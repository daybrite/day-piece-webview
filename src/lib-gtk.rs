// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// GTK: WebKitGTK 6.0 via the `webkit6` crate, a `WebView` widget (a `gtk4::Widget`). Written blind
// (WebKitGTK isn't installed on the reference host); it builds+runs where `webkitgtk-6.0` is present
// (the CI gtk jobs install it). The `uri` property notify reports navigation back via
// `Event::custom("webview:url", …)`, matching the AppKit/Qt renderers.
// ---------------------------------------------------------------------------

use super::*;
use day_gtk::Gtk;
use day_spec::NodeId;
use gtk4::prelude::*;
use webkit6::prelude::*;

// Day assigns the automation ID to GtkWidget:name after make(), so that mutable
// presentation property cannot carry the node ID used to route evaluation replies.
day_core::tls_group! {
    static NETWORKS: RefCell<HashMap<String, webkit6::NetworkSession>> = RefCell::new(HashMap::new());
    static NODE_IDS: RefCell<HashMap<usize, NodeId>> = RefCell::new(HashMap::new());
}

/// Extract an inline site's tree from the GResource blob to the user cache dir, once per
/// process per root (docs/webview.md): WebKitGTK cannot browse a GResource, so the site becomes
/// loose files and the view loads a `file://` URL. Returns the extracted site root.
///
/// Called from `prepare_site()` (the checked, pre-warming route) and lazily from `make` (the
/// direct route). Synchronous: it runs inside a day task poll or at realize, not on a render
/// path; moving large-site extraction to a thread is the noted upgrade.
pub(crate) fn extract_site(root: &str) -> Result<std::path::PathBuf, String> {
    use std::cell::RefCell;
    day_core::tls_group! {
    static DONE: RefCell<std::collections::HashMap<String, std::path::PathBuf>> =
        RefCell::new(std::collections::HashMap::new());
    }
    if let Some(dir) = DONE.with(|m| m.borrow().get(root).cloned()) {
        return Ok(dir);
    }
    let app = gtk4::glib::prgname().unwrap_or_else(|| "day-app".into());
    let dest = gtk4::glib::user_cache_dir()
        .join("day-web")
        .join(app.as_str())
        .join(root);
    // Overwrite-extract once per process: stale caches from an older app build must not linger,
    // and the cost is one pass over a bundled site's files. An empty root is the whole asset
    // tree, which is what a site reading the app's own files (`app_assets`) browses.
    let _ = std::fs::remove_dir_all(&dest);
    let res_dir = match root {
        "" => "/day/assets".to_string(),
        root => format!("/day/assets/{root}"),
    };
    extract_tree(&res_dir, &dest)?;
    DONE.with(|m| m.borrow_mut().insert(root.to_string(), dest.clone()));
    Ok(dest)
}

/// Recursively copy one GResource directory (`res_dir`, absolute resource path) into `dest`.
fn extract_tree(res_dir: &str, dest: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    let children =
        gtk4::gio::resources_enumerate_children(res_dir, gtk4::gio::ResourceLookupFlags::NONE)
            .map_err(|e| format!("enumerate {res_dir}: {e}"))?;
    for child in children {
        let name = child.as_str();
        if let Some(dir_name) = name.strip_suffix('/') {
            extract_tree(&format!("{res_dir}/{dir_name}"), &dest.join(dir_name))?;
        } else {
            let bytes = gtk4::gio::resources_lookup_data(
                &format!("{res_dir}/{name}"),
                gtk4::gio::ResourceLookupFlags::NONE,
            )
            .map_err(|e| format!("read {res_dir}/{name}: {e}"))?;
            std::fs::write(dest.join(name), bytes.as_ref())
                .map_err(|e| format!("write {}: {e}", dest.join(name).display()))?;
        }
    }
    Ok(())
}

fn make(_backend: &mut Gtk, p: &WebProps, id: NodeId) -> gtk4::Widget {
    let context = webkit6::WebContext::new();
    if let Some(provider) = &p.resources {
        let provider = provider.id();
        if let Some(security) = context.security_manager() {
            security.register_uri_scheme_as_secure("day-resource");
            security.register_uri_scheme_as_cors_enabled("day-resource");
        }
        context.register_uri_scheme("day-resource", move |request| {
            let uri = request.uri().map(|u| u.to_string()).unwrap_or_default();
            let method = request
                .http_method()
                .map(|u| u.to_string())
                .unwrap_or_else(|| "GET".into());
            let range = request
                .http_headers()
                .and_then(|h| h.one("Range"))
                .map(|s| s.to_string())
                .unwrap_or_default();
            let request = request.clone();
            let (tx, rx) = day_async::oneshot();
            super::resources::background(move || {
                tx.send(super::resources::respond(provider, &uri, &method, &range))
            });
            gtk4::glib::MainContext::default().spawn_local(async move {
                let Ok(r) = rx.await else { return };
                let length = r.body.len() as i64;
                let stream = gtk4::gio::MemoryInputStream::from_bytes(
                    &gtk4::glib::Bytes::from_owned(r.body),
                );
                let response = webkit6::URISchemeResponse::new(&stream, length);
                response.set_content_type(&r.mime);
                response.set_status(r.status as u32, Some("Resource"));
                let headers =
                    webkit6::soup::MessageHeaders::new(webkit6::soup::MessageHeadersType::Response);
                // nosniff checks inspect the HTTP Content-Type header, not only the
                // URISchemeResponse MIME property. Without it WebKit rejects JS/CSS.
                headers.append("Content-Type", &r.mime);
                for (k, v) in r.headers {
                    headers.append(&k, &v);
                }
                response.set_http_headers(headers);
                request.finish_with_response(&response);
            });
        });
    }
    let network = NETWORKS.with(|networks| {
        let key = p.profile.storage_key();
        if let Some(network) = networks.borrow().get(&key) {
            return network.clone();
        }
        let network = if p.profile.private {
            webkit6::NetworkSession::new_ephemeral()
        } else {
            let root = p
                .profile
                .directory
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    gtk4::glib::user_data_dir()
                        .join(
                            gtk4::glib::prgname()
                                .unwrap_or_else(|| "day-app".into())
                                .as_str(),
                        )
                        .join("day-web-profiles")
                });
            let root = root.join(&key);
            let data = root.join("data");
            if let Err(error) = std::fs::create_dir_all(&data) {
                log::warn!("day-piece-webview: could not create profile storage: {error}");
            }
            let session = webkit6::NetworkSession::new(
                Some(&data.to_string_lossy()),
                Some(&root.join("cache").to_string_lossy()),
            );
            if let Some(cookies) = session.cookie_manager() {
                cookies.set_persistent_storage(
                    &data.join("cookies.sqlite").to_string_lossy(),
                    webkit6::CookiePersistentStorage::Sqlite,
                );
            }
            session
        };
        networks.borrow_mut().insert(key, network.clone());
        network
    });
    let wv = webkit6::WebView::builder()
        .web_context(&context)
        .network_session(&network)
        .build();
    if p.transparent {
        // WebKitGTK composites the page over this color; a zero alpha is its documented way to
        // draw no background of its own.
        webkit6::prelude::WebViewExt::set_background_color(
            &wv,
            &gtk4::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0),
        );
    }
    NODE_IDS.with(|ids| ids.borrow_mut().insert(wv.as_ptr() as usize, id));
    // The navigation report also drives on_load: report completion, including
    // same-URL reloads, rather than URI changes before the document is ready.
    wv.connect_load_changed(move |wv, event| {
        if event == webkit6::LoadEvent::Finished
            && let Some(uri) = wv.uri()
        {
            day_gtk::emit(id, Event::custom("webview:url", uri.to_string()));
        }
    });
    // Spelled out because `gtk4::prelude::WidgetExt` has a `settings()` of its own (the widget's
    // GtkSettings) and both traits are in scope here.
    if p.inline_assets
        && let Some(settings) = webkit6::prelude::WebViewExt::settings(&wv)
    {
        // A site that reads the app's files does so from one `file:` URL to another, which
        // WebKitGTK refuses by default whatever the page can load as a subresource. What the
        // grant opens up is the extracted asset tree the view is already showing the site from.
        settings.set_allow_file_access_from_file_urls(true);
    }
    if let Some(labels) = p.tabs.clone() {
        wv.connect_decide_policy(move |_, decision, kind| {
            let Some(action) = decision
                .downcast_ref::<webkit6::NavigationPolicyDecision>()
                .and_then(|d| d.navigation_action())
            else {
                return false;
            };
            let modified = action.modifiers()
                & (gtk4::gdk::ModifierType::CONTROL_MASK.bits()
                    | gtk4::gdk::ModifierType::META_MASK.bits())
                != 0;
            if kind != webkit6::PolicyDecisionType::NewWindowAction
                && !modified
                && action.mouse_button() != 2
            {
                return false;
            }
            let Some(uri) = action.request().and_then(|r| r.uri()) else {
                return false;
            };
            decision.ignore();
            day_gtk::emit(
                id,
                new_tab_event(
                    uri.to_string(),
                    (modified || action.mouse_button() == 2)
                        && action.modifiers() & gtk4::gdk::ModifierType::SHIFT_MASK.bits() == 0,
                ),
            );
            true
        });
        wv.connect_context_menu(move |_, menu, hit| {
            if let Some(url) = hit.link_uri() {
                for (name, label, background) in [
                    ("day-tab-open", &labels.foreground, false),
                    ("day-tab-background", &labels.background, true),
                ] {
                    let action = gtk4::gio::SimpleAction::new(name, None);
                    let url = url.to_string();
                    action.connect_activate(move |_, _| {
                        day_gtk::emit(id, new_tab_event(url.clone(), background))
                    });
                    menu.prepend(&webkit6::ContextMenuItem::from_gaction(
                        &action, label, None,
                    ));
                }
            }
            false
        });
    }
    if !p.inline_root.is_empty() {
        // Inline mode (docs/webview.md): extract-to-cache (above), then a file URL; WebKit
        // resolves the site's relative references natively. The policy handler polices by the
        // canonical file-URL prefix; navigations leaving the site are ignored here and
        // reported, and the Rust front-end runs the app's LinkPolicy (events are enqueue-only,
        // so the verdict cannot come back through this signal).
        // A site reading the app's own files (`app_assets`) extracts the whole asset tree and
        // opens its own directory inside it, so the page's relative references reach both.
        match extract_site(if p.inline_assets { "" } else { &p.inline_root }) {
            Ok(dir) => {
                let dir = dir.canonicalize().unwrap_or(dir);
                let dir = if p.inline_assets {
                    dir.join(&p.inline_root)
                } else {
                    dir
                };
                let base = format!("file://{}/", dir.display());
                let start = format!("{base}{}", p.inline_start);
                let policed = base.clone();
                wv.connect_decide_policy(move |_wv, decision, dtype| {
                    use webkit6::PolicyDecisionType;
                    let uri = match dtype {
                        PolicyDecisionType::NavigationAction => decision
                            .downcast_ref::<webkit6::NavigationPolicyDecision>()
                            .and_then(|d| d.navigation_action())
                            .and_then(|a| a.request())
                            .and_then(|r| r.uri())
                            .map(|u| u.to_string()),
                        // target=_blank / window.open: no new window exists in day's tree, so
                        // the request is external by definition.
                        PolicyDecisionType::NewWindowAction => decision
                            .downcast_ref::<webkit6::NavigationPolicyDecision>()
                            .and_then(|d| d.navigation_action())
                            .and_then(|a| a.request())
                            .and_then(|r| r.uri())
                            .map(|u| u.to_string()),
                        _ => None,
                    };
                    let Some(uri) = uri else { return false };
                    let inside = uri.starts_with(&policed) || uri == "about:blank";
                    if inside && dtype == PolicyDecisionType::NavigationAction {
                        return false; // let WebKit proceed
                    }
                    decision.ignore();
                    day_gtk::emit(
                        id,
                        Event::Custom {
                            tag: "webview:link",
                            num: super::LINK_REPORT,
                            text: uri,
                        },
                    );
                    true
                });
                wv.load_uri(&start);
            }
            Err(e) => log::warn!("day-piece-webview: inline site {:?}: {e}", p.inline_root),
        }
    } else if !p.url.is_empty() {
        if let Some(provider) = &p.resources {
            let base = provider.base_url();
            wv.connect_decide_policy(move |_, decision, kind| {
                if kind != webkit6::PolicyDecisionType::NavigationAction {
                    return false;
                }
                let uri = decision
                    .downcast_ref::<webkit6::NavigationPolicyDecision>()
                    .and_then(|d| d.navigation_action())
                    .and_then(|a| a.request())
                    .and_then(|r| r.uri());
                let Some(uri) = uri else { return false };
                if uri.starts_with(&base) || uri.starts_with("about:") {
                    return false;
                }
                decision.ignore();
                day_gtk::emit(
                    id,
                    Event::Custom {
                        tag: "webview:link",
                        num: super::LINK_REPORT,
                        text: uri.to_string(),
                    },
                );
                true
            });
        }
        wv.load_uri(&p.url);
    }
    wv.upcast()
}

fn update(_backend: &mut Gtk, h: &gtk4::Widget, patch: &WebPatch) {
    let Some(wv) = h.downcast_ref::<webkit6::WebView>() else {
        return;
    };
    match patch {
        WebPatch::Load(url) => wv.load_uri(url),
        WebPatch::Back => {
            if wv.can_go_back() {
                wv.go_back();
            }
        }
        WebPatch::Forward => {
            if wv.can_go_forward() {
                wv.go_forward();
            }
        }
        WebPatch::Stop => wv.stop_loading(),
        WebPatch::Reload => wv.reload(),
        WebPatch::ForceReload => wv.reload_bypass_cache(),
        // WebKitGTK replies asynchronously; forward the wrapped string on the request channel.
        WebPatch::Eval { req, script } => {
            let Some(id) = NODE_IDS.with(|ids| ids.borrow().get(&(wv.as_ptr() as usize)).copied())
            else {
                return;
            };
            if let Some(request) = script.strip_prefix(DATA_REQUEST) {
                let request =
                    serde_json::from_str::<serde_json::Value>(request).unwrap_or_default();
                let id = NODE_IDS.with(|ids| ids.borrow().get(&(wv.as_ptr() as usize)).copied());
                let req = *req;
                let wv = wv.clone();
                gtk4::glib::MainContext::default().spawn_local(async move {
                    let result: Result<String, String> = async {
                        if request["operation"].as_str() == Some("navigation") {
                            return Ok(serde_json::json!({"url":wv.uri().map(|s|s.to_string()).unwrap_or_default(),"title":wv.title().map(|s|s.to_string()).unwrap_or_default(),"can_go_back":wv.can_go_back(),"can_go_forward":wv.can_go_forward(),"loading":wv.is_loading()}).to_string());
                        }
                        let network = wv.network_session().ok_or("profile unavailable")?;
                        let jar = network.cookie_manager().ok_or("cookies unavailable")?;
                        match request["operation"].as_str().unwrap_or_default() {
                            "cookies" => {
                                let url = request["url"].as_str().unwrap_or_default();
                                let mut cookies =
                                    jar.cookies_future(url).await.map_err(|e| e.to_string())?;
                                let header = cookies
                                    .iter_mut()
                                    .map(|c| {
                                        format!(
                                            "{}={}",
                                            c.name().unwrap_or_default(),
                                            c.value().unwrap_or_default()
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("; ");
                                Ok(serde_json::to_string(&header).unwrap())
                            }
                            "set-cookie" => {
                                let uri = gtk4::glib::Uri::parse(
                                    request["url"].as_str().unwrap_or_default(),
                                    gtk4::glib::UriFlags::NONE,
                                )
                                .map_err(|e| e.to_string())?;
                                let cookie = webkit6::soup::Cookie::parse(
                                    request["cookie"].as_str().unwrap_or_default(),
                                    Some(&uri),
                                )
                                .ok_or("invalid cookie")?;
                                jar.add_cookie_future(&cookie)
                                    .await
                                    .map_err(|e| e.to_string())?;
                                Ok("true".into())
                            }
                            "clear" => {
                                let manager = network
                                    .website_data_manager()
                                    .ok_or("storage unavailable")?;
                                let (tx, rx) = day_async::oneshot();
                                manager.clear(
                                    webkit6::WebsiteDataTypes::all(),
                                    gtk4::glib::TimeSpan::from_microseconds(0),
                                    None::<&gtk4::gio::Cancellable>,
                                    move |r| tx.send(r.map_err(|e| e.to_string())),
                                );
                                rx.await.map_err(|_| "clear cancelled")??;
                                Ok("true".into())
                            }
                            _ => Err("unknown storage operation".into()),
                        }
                    }
                    .await;
                    if let Some(id) = id {
                        let text = match result {
                            Ok(json) => format!("1{SEP}{json}"),
                            Err(error) => engine_error("StorageError", &error),
                        };
                        day_gtk::emit(
                            id,
                            Event::Custom {
                                tag: "webview:eval",
                                num: req as f64,
                                text,
                            },
                        );
                    }
                });
                return;
            }
            let req = *req;
            wv.evaluate_javascript(
                script,
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    let payload = match result {
                        Ok(value) => value.to_str().to_string(),
                        Err(error) => super::engine_error("WebKitError", &error.to_string()),
                    };
                    day_gtk::emit(
                        id,
                        Event::Custom {
                            tag: "webview:eval",
                            num: req as f64,
                            text: payload,
                        },
                    );
                },
            );
        }
    }
}

fn release(_backend: &mut Gtk, h: &gtk4::Widget) {
    NODE_IDS.with(|ids| ids.borrow_mut().remove(&(h.as_ptr() as usize)));
}

day_pieces::renderer!(day_gtk::RENDERERS, Gtk,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);
