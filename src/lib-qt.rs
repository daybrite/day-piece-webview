// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Qt: this crate's shim (src/lib-qt-shim.cpp) wrapping QWebEngineView behind a flat C ABI.
// build.rs compiles it and links Qt6WebEngineWidgets (which day-toolkit-qt-sys does not). The shim reports
// url changes through a C callback → `Event::custom("webview:url", …)`.
// ---------------------------------------------------------------------------

use super::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use day_toolkit_qt::{Qt, QtHandle};
use day_spec::NodeId;

unsafe extern "C" {
    fn day_webview_new(
        url: *const c_char,
        id: u64,
        cb: extern "C" fn(u64, *const c_char),
        session: u64,
        profile: *const c_char,
        directory: *const c_char,
        private: bool,
        inline_path_prefix: *const c_char,
        link_cb: extern "C" fn(u64, *const c_char),
        zoom: f64,
        javascript: bool,
        defer_load: bool,
    ) -> *mut c_void;
    fn day_webview_set_content_cbs(
        filter: extern "C" fn(u64, *const c_char, *const c_char, i32, bool, *const c_char) -> bool,
        message: extern "C" fn(u64, *const c_char),
    );
    fn day_webview_content(w: *mut c_void, hooks: u64, scripts: *const c_char);
    fn day_webview_run(w: *mut c_void, script: *const c_char);
    fn day_webview_load_initial(w: *mut c_void, url: *const c_char);
    fn day_webview_tabs(
        w: *mut c_void,
        foreground: *const c_char,
        background: *const c_char,
        callback: extern "C" fn(u64, *const c_char, bool),
    );
    fn day_webview_load(w: *mut c_void, url: *const c_char);
    fn day_webview_back(w: *mut c_void);
    fn day_webview_forward(w: *mut c_void);
    fn day_webview_stop(w: *mut c_void);
    fn day_webview_reload(w: *mut c_void);
    fn day_webview_force_reload(w: *mut c_void);
    fn day_webview_report_state_now(w: *mut c_void);
    fn day_webview_set_transparent(w: *mut c_void);
    fn day_webview_set_zoom(w: *mut c_void, zoom: f64);
    fn day_webview_set_javascript(w: *mut c_void, enabled: bool);
    fn day_webview_set_eval_cb(cb: extern "C" fn(u64, u64, *const c_char));
    fn day_webview_set_state_cb(cb: extern "C" fn(u64, *const c_char));
    fn day_webview_eval(w: *mut c_void, req: u64, script: *const c_char);
}

/// One evaluation reply, keyed by request id. The shim always calls this exactly once per request
/// (including from the no-engine fallback), so a pending future can never be stranded.
extern "C" fn on_eval(id: u64, req: u64, payload: *const c_char) {
    let text = if payload.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(payload) }
            .to_string_lossy()
            .into_owned()
    };
    day_toolkit_qt::emit(
        NodeId(id),
        Event::Custom {
            tag: "webview:eval",
            num: req as f64,
            text,
        },
    );
}

/// One page-state report, already in the front-end's text form (the shim composes it).
extern "C" fn on_state(id: u64, text: *const c_char) {
    if text.is_null() {
        return;
    }
    day_toolkit_qt::emit(
        NodeId(id),
        Event::Custom {
            tag: "webview:state",
            num: super::PAGE_STATE_REPORT,
            text: unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned(),
        },
    );
}

extern "C" fn on_url(id: u64, url: *const c_char) {
    if url.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(url) }
        .to_string_lossy()
        .into_owned();
    day_toolkit_qt::emit(NodeId(id), Event::custom("webview:url", s));
}

/// An inline site's navigation left the site: the shim CANCELLED it (acceptNavigationRequest
/// returned false) and reports it here; the front-end runs the app's `LinkPolicy`.
extern "C" fn on_link(id: u64, url: *const c_char) {
    if url.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(url) }
        .to_string_lossy()
        .into_owned();
    day_toolkit_qt::emit(
        NodeId(id),
        Event::Custom {
            tag: "webview:link",
            num: super::LINK_REPORT,
            text: s,
        },
    );
}

extern "C" fn on_tab(id: u64, url: *const c_char, background: bool) {
    if !url.is_null() {
        day_toolkit_qt::emit(
            NodeId(id),
            new_tab_event(
                unsafe { CStr::from_ptr(url) }
                    .to_string_lossy()
                    .into_owned(),
                background,
            ),
        );
    }
}

/// Copy a C string the shim lends for the call.
fn text(s: *const c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
    }
}

/// WebView::request_filter, from the page's `QWebEngineUrlRequestInterceptor` on the UI thread:
/// `true` blocks. Qt names the resource type (Chromium's numbering) and the page that issued
/// the request (`firstPartyUrl`).
extern "C" fn on_filter(
    hooks: u64,
    url: *const c_char,
    top: *const c_char,
    kind: i32,
    main_frame: bool,
    method: *const c_char,
) -> bool {
    let Some(hooks) = super::content::hooks(hooks) else {
        return false;
    };
    let (url, top, method) = (text(url), text(top), text(method));
    let request = RequestInfo {
        url: &url,
        top_url: (!top.is_empty()).then_some(top.as_str()),
        frame_url: None,
        kind: ResourceKind::from_chromium(kind),
        main_frame,
        method: &method,
    };
    hooks.decide(&request) == RequestDecision::Block
}

/// One page message: the console bridge's envelope. Its reply runs in the top-level document.
extern "C" fn on_message(id: u64, envelope: *const c_char) {
    let Some((name, message, payload)) = super::content::parse_envelope(&text(envelope)) else {
        return;
    };
    day_toolkit_qt::emit(
        NodeId(id),
        super::script_message_event(
            message,
            ScriptMessage {
                name,
                payload,
                origin: String::new(),
                main_frame: false,
            },
        ),
    );
}

/// The page scripts as the shim's JSON: the dayMessage shim over the console bridge, user styles
/// as scripts (QtWebEngine has no user style sheets), then the app's scripts.
fn scripts_json(content: &PageContent) -> String {
    let scripts = content.page_scripts(
        super::content::Transport::Envelope("(t=>console.debug('\\u0002dayMessage'+t))"),
        false,
    );
    let scripts: Vec<_> = scripts
        .iter()
        .map(|script| {
            serde_json::json!({
                "source": &*script.source,
                "end": script.at == InjectAt::DocumentEnd,
                "main": script.frames == Frames::Main,
            })
        })
        .collect();
    serde_json::Value::Array(scripts).to_string()
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

fn make(_backend: &mut Qt, p: &WebProps, id: NodeId) -> QtHandle {
    // The eval callback is a single file-static in the shim, shared by every web view (the reply
    // carries its own node id), so register it once rather than per view.
    static EVAL_CB: std::sync::Once = std::sync::Once::new();
    EVAL_CB.call_once(|| unsafe {
        day_webview_set_eval_cb(on_eval);
        day_webview_set_state_cb(on_state);
        day_webview_set_content_cbs(on_filter, on_message);
    });
    // Inline mode (docs/webview.md): the asset tree is compiled into the qrc blob
    // (`:/day/assets/<path>`, resources/qt.rs), and QWebEngine reads the qrc scheme natively,
    // so the load is one URL and the engine resolves the site's relative references itself.
    // The shim polices navigation by (scheme, path-prefix), not by raw string: Chromium
    // normalizes qrc URL spellings, and a string compare would cancel the site's own first
    // load (the AppKit arm learned the same lesson with file URLs).
    let (url, path_prefix) = if p.inline_root.is_empty() {
        (
            p.url.clone(),
            // Only a provider's own site is policed; `WebView::resources` just serves it.
            p.resources
                .as_ref()
                .filter(|_| p.resource_site)
                .map(ResourceProvider::base_url)
                .unwrap_or_default(),
        )
    } else {
        (
            format!("qrc:/day/assets/{}/{}", p.inline_root, p.inline_start),
            format!("/day/assets/{}/", p.inline_root),
        )
    };
    let hooks = p
        .content
        .requests
        .as_ref()
        .map(super::content::register)
        .unwrap_or(0);
    let has_content = hooks != 0 || p.content.has_scripts();
    let handle = QtHandle(unsafe {
        day_webview_new(
            cstr(&url).as_ptr(),
            id.0,
            on_url,
            p.session,
            cstr(&p.profile.storage_key()).as_ptr(),
            cstr(p.profile.directory.as_deref().unwrap_or("")).as_ptr(),
            p.profile.private,
            cstr(&path_prefix).as_ptr(),
            on_link,
            p.settings.zoom,
            p.settings.javascript,
            has_content,
        )
    });
    if has_content {
        // Hooks and scripts first, so the first page is filtered and scripted. A retained
        // session's view (which `day_webview_new` re-attached without loading) only takes the
        // new node's hooks; loading `url` into it is skipped below.
        unsafe {
            day_webview_content(handle.0, hooks, cstr(&scripts_json(&p.content)).as_ptr());
        }
        if p.session == 0 || !RETAINED.with(|r| r.borrow().contains(&p.session)) {
            unsafe { day_webview_load_initial(handle.0, cstr(&url).as_ptr()) };
        }
    }
    if p.session != 0 {
        RETAINED.with(|r| r.borrow_mut().insert(p.session));
    }
    if let Some(labels) = &p.tabs {
        unsafe {
            day_webview_tabs(
                handle.0,
                cstr(&labels.foreground).as_ptr(),
                cstr(&labels.background).as_ptr(),
                on_tab,
            )
        };
    }
    if p.transparent {
        unsafe { day_webview_set_transparent(handle.0) };
    }
    handle
}

day_core::tls_group! {
    /// Sessions whose engine view exists already: re-attaching one must not load again.
    static RETAINED: std::cell::RefCell<std::collections::HashSet<u64>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

fn update(_backend: &mut Qt, h: &QtHandle, patch: &WebPatch) {
    unsafe {
        match patch {
            WebPatch::ScriptReply { id, reply } => day_webview_run(
                h.0,
                cstr(&super::content::reply_script(*id, reply.as_deref())).as_ptr(),
            ),
            // WebKit's rule lists; Qt filters through `request_filter`. QtWebEngine asks the
            // user about capture devices through its own permission flow.
            WebPatch::ContentRules(_)
            | WebPatch::MediaDecision { .. }
            | WebPatch::PromptAnswer { .. } => {}
            WebPatch::Load(url) => day_webview_load(h.0, cstr(url).as_ptr()),
            WebPatch::Back => day_webview_back(h.0),
            WebPatch::Forward => day_webview_forward(h.0),
            WebPatch::Stop => day_webview_stop(h.0),
            WebPatch::Reload => day_webview_reload(h.0),
            WebPatch::ForceReload => day_webview_force_reload(h.0),
            WebPatch::ReportState => day_webview_report_state_now(h.0),
            WebPatch::Eval { req, script } => day_webview_eval(h.0, *req, cstr(script).as_ptr()),
            WebPatch::Zoom(zoom) => day_webview_set_zoom(h.0, *zoom),
            // QtWebEngine already presents itself as a desktop browser; its user agent is also
            // per profile, not per view, so the setting changes nothing here.
            WebPatch::DesktopMode(_) => {}
            WebPatch::JavaScript(enabled) => day_webview_set_javascript(h.0, *enabled),
        }
    }
}

day_pieces::renderer!(day_toolkit_qt::RENDERERS, Qt,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure);
