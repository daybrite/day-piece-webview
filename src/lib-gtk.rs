// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// GTK: WebKitGTK 6.0 via the `webkit6` crate, a `WebView` widget (a `gtk4::Widget`). Written blind
// (WebKitGTK isn't installed on the reference host); it builds+runs where `webkitgtk-6.0` is present
// (the CI gtk jobs install it). The `uri` property notify reports navigation back via
// `Event::custom("webview:url", …)`, matching the AppKit/Qt renderers.
// ---------------------------------------------------------------------------

use super::*;
use day_spec::NodeId;
use day_toolkit_gtk::Gtk;
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
            day_toolkit_gtk::emit(id, Event::custom("webview:url", uri.to_string()));
        }
    });
    // Live page state (`WebView::page_state`): every property WebKitGTK announces a change of,
    // plus the back-forward list, re-reads the whole state; repeats of the last report are
    // dropped here and bursts are coalesced by the front-end.
    let last_state = Rc::new(RefCell::new(String::new()));
    let report = {
        let last_state = last_state.clone();
        move |wv: &webkit6::WebView| {
            let loading = wv.is_loading();
            let text = super::encode_page_state(&PageState {
                url: wv.uri().map(|s| s.to_string()).unwrap_or_default(),
                title: wv.title().map(|s| s.to_string()).unwrap_or_default(),
                loading,
                progress: if loading {
                    wv.estimated_load_progress()
                } else {
                    1.0
                },
                can_go_back: wv.can_go_back(),
                can_go_forward: wv.can_go_forward(),
            });
            if *last_state.borrow() != text {
                *last_state.borrow_mut() = text.clone();
                day_toolkit_gtk::emit(
                    id,
                    Event::Custom {
                        tag: "webview:state",
                        num: super::PAGE_STATE_REPORT,
                        text,
                    },
                );
            }
        }
    };
    for property in ["uri", "title", "is-loading", "estimated-load-progress"] {
        let report = report.clone();
        wv.connect_notify_local(Some(property), move |wv, _| report(wv));
    }
    if let Some(history) = wv.back_forward_list() {
        let report = report.clone();
        let weak = wv.downgrade();
        // webkit6 binds no `changed` handler (its arguments include a raw list), so connect by
        // name; only the fact of a change matters here.
        history.connect_local("changed", false, move |_| {
            if let Some(wv) = weak.upgrade() {
                report(&wv);
            }
            None
        });
    }
    // WebView::zoom and WebView::javascript, before the first load. Each view has its own
    // WebKitSettings. `enable-javascript-markup` stops the page's own scripts (elements,
    // attributes, `javascript:` URLs) and leaves `evaluate_javascript`, so the app's evaluation
    // and page state keep working; `enable-javascript` would stop both. The zoom level scales
    // the whole page and persists across navigations. Desktop mode needs nothing: WebKitGTK's
    // user agent is a desktop browser's already.
    webkit6::prelude::WebViewExt::set_zoom_level(&wv, p.settings.zoom);
    if !p.settings.javascript
        && let Some(settings) = webkit6::prelude::WebViewExt::settings(&wv)
    {
        settings.set_enable_javascript_markup(false);
    }
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
            day_toolkit_gtk::emit(
                id,
                new_tab_event(
                    uri.to_string(),
                    (modified || action.mouse_button() == 2)
                        && action.modifiers() & gtk4::gdk::ModifierType::SHIFT_MASK.bits() == 0,
                ),
            );
            true
        });
        // A new window WebKit is about to create without a NewWindowAction decision first
        // (script-opened windows on some WebKitGTK versions): report it as a tab request, and
        // create nothing.
        wv.connect_create(move |_, action| {
            if let Some(uri) = action.request().and_then(|r| r.uri()) {
                day_toolkit_gtk::emit(id, new_tab_event(uri.to_string(), false));
            }
            None
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
                        day_toolkit_gtk::emit(id, new_tab_event(url.clone(), background))
                    });
                    menu.prepend(&webkit6::ContextMenuItem::from_gaction(
                        &action, label, None,
                    ));
                }
            }
            false
        });
    }
    let mut first: Option<String> = None;
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
                    day_toolkit_gtk::emit(
                        id,
                        Event::Custom {
                            tag: "webview:link",
                            num: super::LINK_REPORT,
                            text: uri,
                        },
                    );
                    true
                });
                first = Some(start);
            }
            Err(e) => log::warn!("day-piece-webview: inline site {:?}: {e}", p.inline_root),
        }
    } else if !p.url.is_empty() {
        // Only a provider's own site is policed; `WebView::resources` just adds the scheme.
        if let Some(provider) = p.resources.as_ref().filter(|_| p.resource_site) {
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
                day_toolkit_gtk::emit(
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
        first = Some(p.url.clone());
    }
    // Scripts, styles, message handlers and rule lists (`WebView::content_rules` and friends).
    // The first page waits for the rule lists, so it never loads unfiltered.
    let loader = wv.clone();
    page_content::attach(&wv, &p.content, id, move || {
        if let Some(url) = first {
            loader.load_uri(&url);
        }
    });
    wv.upcast()
}

/// Content rule lists, user scripts and styles, and page messages on WebKitGTK (docs/webview.md,
/// "Content blocking and page scripts"). Each view has its own `WebKitUserContentManager`.
mod page_content {
    use super::*;
    use crate::content::Transport;
    use webkit6::javascriptcore;

    /// A view's content state, by the view's address.
    struct ViewContent {
        manager: webkit6::UserContentManager,
        node: NodeId,
        /// The rule-list generations and the first page load they hold.
        rules: crate::content::RuleGate,
        next: Cell<u64>,
        replies: RefCell<HashMap<u64, (webkit6::ScriptMessageReply, javascriptcore::Context)>>,
    }

    day_core::tls_group! {
        static VIEWS: RefCell<HashMap<usize, Rc<ViewContent>>> = RefCell::new(HashMap::new());
    }

    pub(super) fn attach(
        wv: &webkit6::WebView,
        content: &PageContent,
        node: NodeId,
        load: impl FnOnce() + 'static,
    ) {
        let Some(manager) = wv.user_content_manager() else {
            load();
            return;
        };
        // WebKitGTK has real user style sheets, at the user level, in every frame.
        for css in &content.styles {
            manager.add_style_sheet(&webkit6::UserStyleSheet::new(
                css,
                webkit6::UserContentInjectedFrames::AllFrames,
                webkit6::UserStyleLevel::User,
                &[],
                &[],
            ));
        }
        for script in content.page_scripts(Transport::WebKit, true) {
            manager.add_script(&webkit6::UserScript::new(
                &script.source,
                match script.frames {
                    Frames::All => webkit6::UserContentInjectedFrames::AllFrames,
                    Frames::Main => webkit6::UserContentInjectedFrames::TopFrame,
                },
                match script.at {
                    InjectAt::DocumentStart => webkit6::UserScriptInjectionTime::Start,
                    InjectAt::DocumentEnd => webkit6::UserScriptInjectionTime::End,
                },
                &[],
                &[],
            ));
        }
        let state = Rc::new(ViewContent {
            manager: manager.clone(),
            node,
            rules: crate::content::RuleGate::default(),
            next: Cell::new(1),
            replies: RefCell::new(HashMap::new()),
        });
        for name in &content.messages {
            // In the page's own world, where `webkit.messageHandlers[name].postMessage` returns
            // a promise of the reply.
            if !manager.register_script_message_handler_with_reply(name, None) {
                log::warn!("day-piece-webview: script message handler {name:?} not registered");
                continue;
            }
            let weak = Rc::downgrade(&state);
            let handler = name.clone();
            manager.connect_script_message_with_reply_received(
                Some(name),
                move |_, value, reply| {
                    let Some(state) = weak.upgrade() else {
                        return false;
                    };
                    let Some(context) = value.context() else {
                        return false;
                    };
                    let id = state.next.get();
                    state.next.set(id + 1);
                    state
                        .replies
                        .borrow_mut()
                        .insert(id, (reply.clone(), context));
                    day_toolkit_gtk::emit(
                        state.node,
                        crate::script_message_event(
                            id,
                            ScriptMessage {
                                name: handler.clone(),
                                payload: value.to_str().to_string(),
                                // WebKitGTK does not say which frame posted.
                                origin: String::new(),
                                main_frame: false,
                            },
                        ),
                    );
                    true
                },
            );
        }
        VIEWS.with(|views| {
            views
                .borrow_mut()
                .insert(wv.as_ptr() as usize, state.clone())
        });
        if content.rules.is_empty() {
            load();
        } else {
            apply_rules(&state, &content.rules, Some(Box::new(load)));
        }
    }

    /// A navigation the app asked for (`WebPatch::Load`): while the view's first page waits for
    /// its rule lists, it takes that page's place and waits too; otherwise it runs now.
    pub(super) fn navigate(wv: &webkit6::WebView, load: impl FnOnce() + 'static) {
        let state = VIEWS.with(|views| views.borrow().get(&(wv.as_ptr() as usize)).cloned());
        let load: Box<dyn FnOnce()> = Box::new(load);
        let now = match state {
            Some(state) => state.rules.redirect(load),
            None => Some(load),
        };
        if let Some(now) = now {
            now();
        }
    }

    /// Whether the view's first page is still waiting for its rule lists.
    pub(super) fn holding(wv: &webkit6::WebView) -> bool {
        VIEWS.with(|views| {
            views
                .borrow()
                .get(&(wv.as_ptr() as usize))
                .is_some_and(|state| state.rules.is_holding())
        })
    }

    pub(super) fn update(wv: &webkit6::WebView, patch: &WebPatch) {
        let Some(state) = VIEWS.with(|views| views.borrow().get(&(wv.as_ptr() as usize)).cloned())
        else {
            return;
        };
        match patch {
            WebPatch::ContentRules(lists) => apply_rules(&state, lists, None),
            WebPatch::ScriptReply { id, reply } => answer(&state, *id, reply.as_deref()),
            _ => {}
        }
    }

    fn answer(state: &ViewContent, id: u64, reply: Option<&str>) {
        if let Some((handler, context)) = state.replies.borrow_mut().remove(&id) {
            handler.return_value(&match reply {
                Some(text) => javascriptcore::Value::new_string(&context, Some(text)),
                None => javascriptcore::Value::new_null(&context),
            });
        }
    }

    pub(super) fn release(view: usize) {
        if let Some(state) = VIEWS.with(|views| views.borrow_mut().remove(&view)) {
            let waiting: Vec<u64> = state.replies.borrow().keys().copied().collect();
            for id in waiting {
                answer(&state, id, None);
            }
        }
    }

    /// The app's compiled-filter store: `<user data>/<app>/day-web-content-rules`. Lists are
    /// stored under their id, with the version they were compiled from in `<id>.version` beside
    /// them, so a relaunch loads instead of compiling, and a new version replaces the old.
    fn store() -> (webkit6::UserContentFilterStore, std::path::PathBuf) {
        let path = gtk4::glib::user_data_dir()
            .join(
                gtk4::glib::prgname()
                    .unwrap_or_else(|| "day-app".into())
                    .as_str(),
            )
            .join("day-web-content-rules");
        let _ = std::fs::create_dir_all(&path);
        (
            webkit6::UserContentFilterStore::new(&path.to_string_lossy()),
            path,
        )
    }

    fn version_file(dir: &std::path::Path, id: &str) -> std::path::PathBuf {
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        dir.join(format!("{safe}.version"))
    }

    /// Compile and attach `lists`, replacing the view's current ones. `load`, the first page
    /// load, waits in the view until the newest set is attached, whichever call that is.
    fn apply_rules(
        state: &Rc<ViewContent>,
        lists: &[ContentRuleList],
        load: Option<Box<dyn FnOnce()>>,
    ) {
        let generation = state.rules.begin(load);
        let (store, dir) = store();
        let state = state.clone();
        let total = lists.len();
        let lists = lists.to_vec();
        gtk4::glib::MainContext::default().spawn_local(async move {
            let mut filters = Vec::with_capacity(total);
            for list in &lists {
                let version = version_file(&dir, &list.id);
                let current =
                    std::fs::read_to_string(&version).is_ok_and(|v| v.trim() == list.version);
                if current && let Ok(filter) = store.load_future(&list.id).await {
                    filters.push(filter);
                    continue;
                }
                // Compiling runs in WebKit's own worker; this task only awaits it.
                let source = gtk4::glib::Bytes::from(list.json.as_bytes());
                match store.save_future(&list.id, &source).await {
                    Ok(filter) => {
                        let _ = std::fs::write(&version, &list.version);
                        filters.push(filter);
                    }
                    Err(error) => {
                        log::warn!(
                            "day-piece-webview: content rule list {:?} did not compile: {error}",
                            list.id
                        );
                        day_toolkit_gtk::emit(
                            state.node,
                            crate::content_rules_error_event(&list.id, &error.to_string()),
                        );
                    }
                }
            }
            // A superseded set neither attaches nor releases the load: the newer one will.
            if !state.rules.is_current(generation) {
                return;
            }
            state.manager.remove_all_filters();
            for filter in &filters {
                state.manager.add_filter(filter);
            }
            state.rules.finish(generation);
        });
    }
}

fn update(_backend: &mut Gtk, h: &gtk4::Widget, patch: &WebPatch) {
    let Some(wv) = h.downcast_ref::<webkit6::WebView>() else {
        return;
    };
    match patch {
        WebPatch::ContentRules(_) | WebPatch::ScriptReply { .. } => page_content::update(wv, patch),
        // WebKitGTK asks the user itself (`permission-request`'s default prompt).
        WebPatch::MediaDecision { .. } | WebPatch::PromptAnswer { .. } => {}
        // While the first page waits for its rule lists, a navigation takes its place.
        WebPatch::Load(url) => {
            let (loader, url) = (wv.clone(), url.clone());
            page_content::navigate(wv, move || loader.load_uri(&url));
        }
        // Nothing has loaded while the first page waits for its rules; it loads when they do.
        WebPatch::Reload | WebPatch::ForceReload if page_content::holding(wv) => {}
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
        // No retained views: a fresh view reports as its first load proceeds.
        WebPatch::ReportState => {}
        WebPatch::Zoom(zoom) => webkit6::prelude::WebViewExt::set_zoom_level(wv, *zoom),
        // WebKitGTK already presents itself as a desktop browser.
        WebPatch::DesktopMode(_) => {}
        // A running page cannot gain or lose its scripts: change the setting, then reload.
        WebPatch::JavaScript(enabled) => {
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(wv)
                && settings.enables_javascript_markup() != *enabled
            {
                settings.set_enable_javascript_markup(*enabled);
                wv.reload();
            }
        }
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
                        if request["operation"].as_str() == Some("snapshot") {
                            let width = request["width"].as_u64().unwrap_or(0) as i32;
                            let (tx, rx) = day_async::oneshot();
                            wv.snapshot(
                                webkit6::SnapshotRegion::Visible,
                                webkit6::SnapshotOptions::NONE,
                                None::<&gtk4::gio::Cancellable>,
                                move |r| tx.send(r.map_err(|e| e.to_string())),
                            );
                            let texture = rx.await.map_err(|_| "snapshot cancelled")??;
                            let png = snapshot_png(&texture, width);
                            return Ok(serde_json::Value::String(
                                gtk4::glib::base64_encode(&png).to_string(),
                            )
                            .to_string());
                        }
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
                        day_toolkit_gtk::emit(
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
                    day_toolkit_gtk::emit(
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

/// Encode `texture` as PNG, first scaled down to at most `width` pixels wide when it is wider
/// (a box filter over the premultiplied pixels, which keeps edges honest at thumbnail sizes).
fn snapshot_png(texture: &gtk4::gdk::Texture, width: i32) -> Vec<u8> {
    let (w, h) = (texture.width(), texture.height());
    if width <= 0 || w <= width || h <= 0 {
        return texture.save_to_png_bytes().to_vec();
    }
    let stride = w as usize * 4;
    let mut pixels = vec![0u8; stride * h as usize];
    texture.download(&mut pixels, stride);
    let (tw, th) = (
        width as usize,
        ((h as i64 * width as i64) / w as i64).max(1) as usize,
    );
    let mut out = vec![0u8; tw * th * 4];
    for ty in 0..th {
        let (y0, y1) = (
            ty * h as usize / th,
            ((ty + 1) * h as usize / th).max(ty * h as usize / th + 1),
        );
        for tx in 0..tw {
            let (x0, x1) = (
                tx * w as usize / tw,
                ((tx + 1) * w as usize / tw).max(tx * w as usize / tw + 1),
            );
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = y * stride + x * 4;
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(pixels[at + c]);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            let at = (ty * tw + tx) * 4;
            for c in 0..4 {
                out[at + c] = (sum[c] / count) as u8;
            }
        }
    }
    // `download` writes the cairo layout: premultiplied BGRA in native (little-endian) order.
    gtk4::gdk::MemoryTexture::new(
        tw as i32,
        th as i32,
        gtk4::gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &gtk4::glib::Bytes::from_owned(out),
        tw * 4,
    )
    .save_to_png_bytes()
    .to_vec()
}

fn release(_backend: &mut Gtk, h: &gtk4::Widget) {
    NODE_IDS.with(|ids| ids.borrow_mut().remove(&(h.as_ptr() as usize)));
    page_content::release(h.as_ptr() as usize);
}

day_pieces::renderer!(day_toolkit_gtk::RENDERERS, Gtk,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);
