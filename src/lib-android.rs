// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Android: android.webkit.WebView. The Java factory (`dev.daybrite.day.piece.webview.DayWebView`)
// is bundled with this crate in `src/DayWebView.java` and pulled into the app's Gradle build via
// `[package.metadata.day.android]`, which also contributes the INTERNET permission (the extension
// this piece motivates). The Java reports each finished URL back through DayBridge.nativeOnEvent's
// open Custom-event kind (12), §8.2's piece-defined event channel; the front-end handler maps the
// text payload to the bound URL.
// ---------------------------------------------------------------------------

use super::*;
use day_toolkit_material::jni::Env;
use day_toolkit_material::jni::objects::{JClass, JObject, JString};
use day_toolkit_material::{AHandle, Material, java, string_of};
use day_spec::NodeId;

// This piece's Java class (src/DayWebView.java, on the app classpath at build).
day_toolkit_material::java_class! {
    DayWebView => dev.daybrite.day.piece.webview.DayWebView,
    methods {
        static fn supportsProfiles() -> jboolean,
        static fn supportsDataClearing() -> jboolean,
        static fn makeWebView(id: jlong, url: JString, inline_prefix: JString, provider: jlong,
            profile: JString, zoom: jdouble, desktop: jboolean, javascript: jboolean,
            hooks: jlong, content: JString, media_handler: jboolean, duties: jint,
            user_agent: JString)
            -> android.view.View,
        static fn promptAnswer(view: android.view.View, id: jlong, answer: JString),
        static fn trustHost(host: JString),
        static fn openIntent(url: JString) -> JString,
        static fn cancelDownload(id: jlong),
        static fn openDownload(location: JString) -> jboolean,
        static fn removeDownload(location: JString),
        static fn scriptReply(view: android.view.View, message: jlong, reply: JString, has_reply: jboolean),
        static fn mediaDecision(view: android.view.View, request: jlong, allow: jboolean),
        static fn supportsDocumentStartScripts() -> jboolean,
        static fn setZoom(view: android.view.View, zoom: jdouble),
        static fn setDesktopMode(view: android.view.View, desktop: jboolean),
        static fn setJavaScript(view: android.view.View, enabled: jboolean),
        static fn configureTabs(view: android.view.View, foreground: JString, background: JString),
        static fn setTransparent(view: android.view.View),
        static fn reportScroll(view: android.view.View),
        static fn evalJs(view: android.view.View, req: jdouble, script: JString),
        static fn webCommand(view: android.view.View, code: jint, url: JString),
        static fn release(view: android.view.View),
        // Private in Java: only `resource`'s Rust half below calls it.
        static fn response(mime: JString, status: jint, header_lines: JString, body: jbyte[])
            -> android.webkit.WebResourceResponse,
    },
    native_methods {
        // Called by WebView's IO worker, never by the UI thread. A failure throws into Java,
        // where WebView answers the request as failed.
        static fn resource {
            sig = (provider: jlong, url: JString, method: JString, range: JString)
                -> android.webkit.WebResourceResponse,
            error_policy = day_toolkit_material::jni::errors::ThrowRuntimeExAndDefault,
        },
        // WebView::request_filter, on WebView's IO workers. Java treats a throw as "allow".
        static fn decide {
            sig = (hooks: jlong, url: JString, accept: JString, main_frame: jboolean, method: JString)
                -> jboolean,
            error_policy = day_toolkit_material::jni::errors::ThrowRuntimeExAndDefault,
        },
        // A download's progress (`browsing::download_event`), posted to the UI thread by Java.
        static fn downloadEvent {
            sig = (id: jlong, kind: jint, received: jlong, total: jlong, message: JString) -> (),
            error_policy = day_toolkit_material::jni::errors::ThrowRuntimeExAndDefault,
        },
    },
}

/// `cancel_download`: stop the Java fetch; its failure report ends the download.
pub(crate) fn cancel_download(id: u64) {
    java(|env| DayWebView::cancelDownload(env, id as i64));
}

/// `open_download`: ask the system to show a saved file.
pub(crate) fn open_download(location: &str) -> bool {
    java(|env| {
        let location = JString::from_str(env, location)?;
        DayWebView::openDownload(env, &location)
    })
    .unwrap_or(false)
}

/// `remove_download`: delete a saved file from the Downloads collection or the disk.
pub(crate) fn remove_download(location: &str) {
    java(|env| {
        let location = JString::from_str(env, location)?;
        DayWebView::removeDownload(env, &location)
    });
}

/// `trust_host`: WebView asks about each certificate error on its own thread, so the set of
/// excepted hosts lives in Java.
pub(crate) fn trust_host(host: &str) {
    java(|env| {
        let host = JString::from_str(env, host)?;
        DayWebView::trustHost(env, &host)
    });
}

/// Start what an `intent:` address names; the page to load instead when no app takes it and
/// the address carries a fallback.
pub(crate) fn open_intent(url: &str) -> Option<String> {
    let fallback = java(|env| {
        let url = JString::from_str(env, url)?;
        let fallback = DayWebView::openIntent(env, &url)?;
        string_of(env, &fallback)
    })?;
    (!fallback.is_empty()).then_some(fallback)
}

/// What `makeWebView` installs before the first load: the page scripts (each composed to run at
/// document start in every frame, since `addDocumentStartJavaScript` has no other mode), the
/// message handler names, and the `dayMessage` shim for web message listeners and for the
/// fallback bridge.
fn content_json(content: &PageContent) -> String {
    if !content.has_scripts() {
        return String::new();
    }
    let mut app = content.clone();
    app.messages.clear();
    let scripts: Vec<String> = app
        .page_scripts(super::content::Transport::Android, false)
        .iter()
        .map(super::content::document_start_everywhere)
        .collect();
    serde_json::json!({
        "scripts": scripts,
        "messages": content.messages,
        "shim": super::content::message_shim(super::content::Transport::Android, &content.messages),
        "fallbackShim": super::content::message_shim(
            super::content::Transport::Envelope("window.__dayMessageHost.post"),
            &content.messages,
        ),
    })
    .to_string()
}

/// Whether document-start scripts run natively on this device's WebView.
pub(crate) fn document_start_scripts() -> bool {
    java(|env| DayWebView::supportsDocumentStartScripts(env)).unwrap_or(false)
}

/// One of DayWebView's capability probes (`supportsProfiles`, `supportsDataClearing`).
pub(crate) fn capability(method: &str) -> day_spec::Support {
    let supported = java(|env| match method {
        "supportsProfiles" => DayWebView::supportsProfiles(env),
        _ => DayWebView::supportsDataClearing(env),
    })
    .unwrap_or(false);
    if supported {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

fn make(_backend: &mut Material, p: &WebProps, id: NodeId) -> AHandle {
    if p.profile.private {
        return AHandle(day_toolkit_material::with_env(|env| {
            day_toolkit_material::placeholder_view(env, "web_view")
        }));
    }
    // Inline mode (docs/webview.md): the assets tree is the APK `assets/` root, and WebView
    // browses it through `file:///android_asset/` (exempt from the API-30 file-access
    // default). The URL is composed here; the Java side polices navigations against the
    // prefix and reports external ones back (LINK_REPORT).
    let (url, prefix) = if p.inline_root.is_empty() {
        (
            p.url.clone(),
            // Only a provider's own site is policed; `WebView::resources` only intercepts.
            p.resources
                .as_ref()
                .filter(|_| p.resource_site)
                .map(ResourceProvider::base_url)
                .unwrap_or_default(),
        )
    } else {
        (
            format!("file:///android_asset/{}/{}", p.inline_root, p.inline_start),
            format!("file:///android_asset/{}/", p.inline_root),
        )
    };
    let provider = p.resources.as_ref().map(ResourceProvider::id).unwrap_or(0) as i64;
    let profile = if p.profile == WebProfile::default() {
        String::new()
    } else {
        p.profile.storage_key()
    };
    // A Java throw (e.g. the staged DayWebView class missing) must not panic inside realize:
    // the panic unwinds the JNI up-call and aborts. Placeholder instead.
    let hooks = p
        .content
        .requests
        .as_ref()
        .map(super::content::register)
        .unwrap_or(0);
    let content = content_json(&p.content);
    let view = day_toolkit_material::realize("web_view", |env| {
        let url = JString::from_str(env, &url)?;
        let prefix = JString::from_str(env, &prefix)?;
        let profile = JString::from_str(env, &profile)?;
        let content = JString::from_str(env, &content)?;
        let user_agent = JString::from_str(env, p.user_agent.wire())?;
        DayWebView::makeWebView(
            env,
            id.0 as i64,
            &url,
            &prefix,
            provider,
            &profile,
            p.settings.zoom,
            p.settings.desktop_mode,
            p.settings.javascript,
            hooks as i64,
            &content,
            p.content.media_handler,
            i32::from(p.browse.auth) | i32::from(p.browse.downloads) << 1,
            &user_agent,
        )
    });
    if p.scroll_reports {
        java(|env| DayWebView::reportScroll(env, view.0.as_obj()));
    }
    if let Some(labels) = &p.tabs {
        java(|env| {
            let foreground = JString::from_str(env, &labels.foreground)?;
            let background = JString::from_str(env, &labels.background)?;
            DayWebView::configureTabs(env, view.0.as_obj(), &foreground, &background)
        });
    }
    if p.transparent {
        java(|env| DayWebView::setTransparent(env, view.0.as_obj()));
    }
    view
}

fn update(_backend: &mut Material, h: &AHandle, patch: &WebPatch) {
    // Evaluation (docs/webview-eval.md): `evaluateJavascript` plus the outer-JSON unquote and
    // the null/empty→engine-error mapping live in DayWebView.evalJs; the reply rides the
    // kind-12 channel keyed by `req`, like every other backend.
    if let WebPatch::Eval { req, script } = patch {
        java(|env| {
            let script = JString::from_str(env, script)?;
            DayWebView::evalJs(env, h.0.as_obj(), *req as f64, &script)
        });
        return;
    }
    // Settings (docs/webview.md): text zoom, a desktop user agent and viewport, and the script
    // switch. The Java side reloads the page when desktop mode or scripts change.
    match patch {
        WebPatch::ScriptReply { id, reply } => {
            java(|env| {
                let text = JString::from_str(env, reply.as_deref().unwrap_or(""))?;
                DayWebView::scriptReply(env, h.0.as_obj(), *id as i64, &text, reply.is_some())
            });
            return;
        }
        WebPatch::MediaDecision { id, allow } => {
            java(|env| DayWebView::mediaDecision(env, h.0.as_obj(), *id as i64, *allow));
            return;
        }
        WebPatch::PromptAnswer { id, answer } => {
            java(|env| {
                let answer = JString::from_str(env, answer)?;
                DayWebView::promptAnswer(env, h.0.as_obj(), *id as i64, &answer)
            });
            return;
        }
        // WebKit's rule lists; Android filters through `request_filter` instead.
        WebPatch::ContentRules(_) => return,
        WebPatch::Zoom(zoom) => {
            java(|env| DayWebView::setZoom(env, h.0.as_obj(), *zoom));
            return;
        }
        WebPatch::DesktopMode(desktop) => {
            java(|env| DayWebView::setDesktopMode(env, h.0.as_obj(), *desktop));
            return;
        }
        WebPatch::JavaScript(enabled) => {
            java(|env| DayWebView::setJavaScript(env, h.0.as_obj(), *enabled));
            return;
        }
        _ => {}
    }
    // Commands cross as (code, url): 0=load, 1=back, 2=forward, 3=stop, 4=reload.
    let (code, url) = match patch {
        WebPatch::Load(u) => (0, u.as_str()),
        WebPatch::Back => (1, ""),
        WebPatch::Forward => (2, ""),
        WebPatch::Stop => (3, ""),
        WebPatch::Reload => (4, ""),
        WebPatch::ForceReload => (5, ""),
        // Handled above; listed to keep the match exhaustive.
        WebPatch::Eval { .. }
        | WebPatch::Zoom(_)
        | WebPatch::DesktopMode(_)
        | WebPatch::JavaScript(_)
        | WebPatch::ContentRules(_)
        | WebPatch::ScriptReply { .. }
        | WebPatch::MediaDecision { .. }
        | WebPatch::PromptAnswer { .. } => return,
        // No retained views: a fresh view reports as its first load proceeds.
        WebPatch::ReportState => return,
    };
    java(|env| {
        let url = JString::from_str(env, url)?;
        DayWebView::webCommand(env, h.0.as_obj(), code, &url)
    });
}

fn release(_backend: &mut Material, handle: &AHandle) {
    java(|env| DayWebView::release(env, handle.0.as_obj()));
}

day_pieces::renderer!(day_toolkit_material::RENDERERS, Material,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);

impl DayWebViewNativeInterface for DayWebViewAPI {
    type Error = day_toolkit_material::jni::errors::Error;

    fn resource<'l>(
        env: &mut Env<'l>,
        _class: JClass<'l>,
        provider: i64,
        url: JString<'l>,
        method: JString<'l>,
        range: JString<'l>,
    ) -> Result<JObject<'l>, Self::Error> {
        let url = string_of(env, &url)?;
        let method = string_of(env, &method)?;
        let range = string_of(env, &range)?;
        let r = super::resources::respond(provider as u64, &url, &method, &range);
        let bytes = env.byte_array_from_slice(&r.body)?;
        let mime = JString::from_str(env, &r.mime)?;
        let headers = JString::from_str(
            env,
            r.headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\n"))
                .collect::<String>(),
        )?;
        DayWebView::response(env, &mime, r.status as i32, &headers, &bytes)
    }

    fn downloadEvent<'l>(
        env: &mut Env<'l>,
        _class: JClass<'l>,
        id: i64,
        kind: i32,
        received: i64,
        total: i64,
        message: JString<'l>,
    ) -> Result<(), Self::Error> {
        let event = match kind {
            0 => DownloadEvent::Progress {
                received: received.max(0) as u64,
                total: u64::try_from(total).ok().filter(|total| *total > 0),
            },
            1 => DownloadEvent::Finished {
                location: string_of(env, &message)?,
            },
            _ => DownloadEvent::Failed(string_of(env, &message)?),
        };
        super::browsing::download_event(id as u64, event);
        Ok(())
    }

    fn decide<'l>(
        env: &mut Env<'l>,
        _class: JClass<'l>,
        hooks: i64,
        url: JString<'l>,
        accept: JString<'l>,
        main_frame: bool,
        method: JString<'l>,
    ) -> Result<bool, Self::Error> {
        let Some(hooks) = super::content::hooks(hooks as u64) else {
            return Ok(false);
        };
        let url = string_of(env, &url)?;
        let accept = string_of(env, &accept)?;
        let method = string_of(env, &method)?;
        // Android names no resource type: the Accept header and the extension stand in.
        let kind = ResourceKind::infer(&url, Some(&accept), main_frame);
        let request = RequestInfo {
            url: &url,
            top_url: None,
            frame_url: None,
            kind,
            main_frame,
            method: &method,
        };
        Ok(hooks.decide(&request) == RequestDecision::Block)
    }
}
