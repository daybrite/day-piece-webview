// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// XAML: this crate's C++/WinRT shim (src/lib-xaml-shim.cpp) wrapping the UWP-XAML WebView,
// boxed into Day handles via the `day_xaml_box`/`day_xaml_unbox` functions day-toolkit-xaml-sys
// exports (like the Qt renderer's shim). The shim reports url changes through a C callback →
// `Event::custom("webview:url", …)`. Windows-only, built + verified in CI (not on this host).
// ---------------------------------------------------------------------------

use super::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use day_spec::NodeId;
use day_toolkit_winui::{WinHandle, Xaml};

unsafe extern "C" {
    fn day_webview_xaml_new(
        url: *const c_char,
        id: u64,
        cb: extern "C" fn(u64, *const c_char),
        inline_root: *const c_char,
        inline_start: *const c_char,
        inline_dir: *const c_char,
        link_cb: extern "C" fn(u64, *const c_char),
        transparent: bool,
        session: u64,
        profile: *const c_char,
        directory: *const c_char,
        private: bool,
        resource_provider: u64,
        resource_prefix: *const c_char,
    ) -> *mut c_void;
    fn day_webview_xaml_tabs(
        handle: *mut c_void,
        foreground: *const c_char,
        background: *const c_char,
        callback: extern "C" fn(u64, *const c_char, bool),
    );
    fn day_webview_xaml_release(handle: *mut c_void);
    fn day_webview_xaml_load(handle: *mut c_void, url: *const c_char);
    fn day_webview_xaml_back(handle: *mut c_void);
    fn day_webview_xaml_forward(handle: *mut c_void);
    fn day_webview_xaml_stop(handle: *mut c_void);
    fn day_webview_xaml_reload(handle: *mut c_void);
    fn day_webview_xaml_force_reload(w: *mut c_void);
    fn day_webview_xaml_report_state(handle: *mut c_void);
    fn day_webview_xaml_settings(handle: *mut c_void, zoom: f64, javascript: bool);
    fn day_webview_xaml_set_zoom(handle: *mut c_void, zoom: f64);
    fn day_webview_xaml_set_javascript(handle: *mut c_void, enabled: bool);
    fn day_webview_xaml_set_eval_cb(cb: extern "C" fn(u64, u64, *const c_char));
    fn day_webview_xaml_set_state_cb(cb: extern "C" fn(u64, *const c_char));
    fn day_webview_xaml_eval(handle: *mut c_void, req: u64, script: *const c_char);
    fn day_webview_xaml_set_content_cbs(
        filter: extern "C" fn(u64, *const c_char, *const c_char, i32, bool, *const c_char) -> bool,
        message: extern "C" fn(u64, *const c_char, *const c_char),
    );
    fn day_webview_xaml_content(handle: *mut c_void, hooks: u64, scripts: *const c_char);
    fn day_webview_xaml_post_json(handle: *mut c_void, json: *const c_char);
}

/// Copy a C string the shim lends for the call.
fn text(s: *const c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
    }
}

/// WebView::request_filter, from `WebResourceRequested` on the UI thread: `true` blocks.
/// WebView2 names the resource context but not the page, which the hooks track.
extern "C" fn on_filter(
    hooks: u64,
    url: *const c_char,
    _top: *const c_char,
    context: i32,
    main_frame: bool,
    method: *const c_char,
) -> bool {
    let Some(hooks) = super::content::hooks(hooks) else {
        return false;
    };
    let (url, method) = (text(url), text(method));
    let mut kind = ResourceKind::from_webview2(context);
    if kind == ResourceKind::Document && !main_frame {
        kind = ResourceKind::Subdocument;
    }
    let request = RequestInfo {
        url: &url,
        top_url: None,
        frame_url: None,
        kind,
        main_frame,
        method: &method,
    };
    hooks.decide(&request) == RequestDecision::Block
}

/// One page message from the top-level document: the shim's envelope and the document's URI.
extern "C" fn on_message(id: u64, envelope: *const c_char, source: *const c_char) {
    let Some((name, message, payload)) = super::content::parse_envelope(&text(envelope)) else {
        return;
    };
    let origin = url::Url::parse(&text(source))
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    day_toolkit_winui::emit(
        NodeId(id),
        super::script_message_event(
            message,
            ScriptMessage {
                name,
                payload,
                origin,
                main_frame: true,
            },
        ),
    );
}

/// The page scripts as the shim's JSON array of sources, each composed to run with its own
/// timing and frames, since WebView2 runs every script at document creation in every frame.
fn scripts_json(content: &PageContent) -> String {
    let scripts: Vec<String> = content
        .page_scripts(super::content::Transport::WebView2, false)
        .iter()
        .map(super::content::document_start_everywhere)
        .collect();
    serde_json::to_string(&scripts).unwrap_or_default()
}

/// One evaluation reply, keyed by request id. The shim calls this exactly once per request
/// (including from the no-engine path), so a pending future can never be stranded.
extern "C" fn on_eval(id: u64, req: u64, payload: *const c_char) {
    let text = if payload.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(payload) }
            .to_string_lossy()
            .into_owned()
    };
    day_toolkit_winui::emit(
        NodeId(id),
        Event::Custom {
            tag: "webview:eval",
            // `num` is what tells an eval reply from the URL readback sharing this channel:
            // URL reports keep 0, requests start at 1 (docs/webview-eval.md).
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
    day_toolkit_winui::emit(
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
    day_toolkit_winui::emit(NodeId(id), Event::custom("webview:url", s));
}

/// An inline site's navigation left the site: the shim CANCELLED it (NavigationStarting /
/// NewWindowRequested) and reports it here; the front-end runs the app's `LinkPolicy`.
extern "C" fn on_link(id: u64, url: *const c_char) {
    if url.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(url) }
        .to_string_lossy()
        .into_owned();
    day_toolkit_winui::emit(
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
        day_toolkit_winui::emit(
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
fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

/// The local folder the virtual host maps to: the asset tree that holds the site, so that
/// `https://day-assets.example/<inline_root>/<page>` resolves to a real file.
///
/// Resolved here rather than in the shim, because only `day_spec` knows where the tree is. A
/// packed app has one `assets` directory beside the exe; a `day launch` run leaves the app's
/// assets in the project and stages a piece's under `build/`, and points an environment variable
/// at each ([`day_spec::resolve_asset_dir`] probes both). An empty answer leaves the shim on its
/// exe-relative default.
fn inline_dir(p: &WebProps) -> String {
    if p.inline_root.is_empty() {
        return String::new();
    }
    let Some(site) = day_spec::resolve_asset_dir(&p.inline_root) else {
        return String::new();
    };
    // The URL keeps `<host>/<inline_root>/<page>`, so the mapped folder is the site's directory
    // with its own path taken back off: one parent per segment of `inline_root`.
    let mut base = site;
    for _ in p.inline_root.split('/').filter(|seg| !seg.is_empty()) {
        let Some(parent) = base.parent().map(|dir| dir.to_path_buf()) else {
            return String::new();
        };
        base = parent;
    }
    // resolve_asset_dir canonicalizes, which adds a Windows verbatim prefix. WebView2's
    // virtual-host loader needs the ordinary DOS/UNC spelling of the same resolved folder.
    super::xaml_path::mapping_folder(&base)
}

fn make(_backend: &mut Xaml, p: &WebProps, id: NodeId) -> WinHandle {
    // The eval callback is a single file-static in the shim, shared by every web view (each reply
    // carries its own node id), so register it once rather than per view.
    static EVAL_CB: std::sync::Once = std::sync::Once::new();
    EVAL_CB.call_once(|| unsafe {
        day_webview_xaml_set_eval_cb(on_eval);
        day_webview_xaml_set_state_cb(on_state);
        day_webview_xaml_set_content_cbs(on_filter, on_message);
    });
    // Inline mode (docs/webview.md): the shim maps the asset tree holding the site under a
    // virtual host and browses `https://day-assets.example/<root>/<start>`; the engine resolves
    // the site's relative references itself, and the shim polices top-level navigations against
    // that prefix. The folder comes from `inline_dir` below, because where that tree is depends
    // on how the app was built and run (Windows keeps assets as loose files, resources/xaml.rs).
    let dir = inline_dir(p);
    if !p.inline_root.is_empty() && dir.is_empty() {
        // Without a folder there is nothing to map the host to, and the site's first navigation
        // fails DNS resolution in the engine rather than here: say so while it can still be tied
        // to the cause.
        log::warn!(
            "day-piece-webview: inline site {:?} not found in the staged assets",
            p.inline_root
        );
    }
    // A provider's requests are intercepted by id; only its own site is policed (by its base).
    let provider = p.resources.as_ref().map(ResourceProvider::id).unwrap_or(0);
    let resource_prefix = p
        .resources
        .as_ref()
        .filter(|_| p.resource_site)
        .map(ResourceProvider::base_url)
        .unwrap_or_default();
    let handle = WinHandle(unsafe {
        day_webview_xaml_new(
            cstr(&p.url).as_ptr(),
            id.0,
            on_url,
            cstr(&p.inline_root).as_ptr(),
            cstr(&p.inline_start).as_ptr(),
            cstr(&dir).as_ptr(),
            on_link,
            p.transparent,
            p.session,
            cstr(&p.profile.storage_key()).as_ptr(),
            cstr(p.profile.directory.as_deref().unwrap_or("")).as_ptr(),
            p.profile.private,
            provider,
            cstr(&resource_prefix).as_ptr(),
        )
    });
    // WebView::zoom and WebView::javascript: kept until the engine is up, applied before its
    // first navigation (or at once to a retained session's engine). Desktop mode needs nothing:
    // WebView2's user agent is desktop Edge's already.
    unsafe { day_webview_xaml_settings(handle.0, p.settings.zoom, p.settings.javascript) };
    // WebView::request_filter and the page scripts, kept until the engine is up and installed
    // before its first navigation.
    let hooks = p
        .content
        .requests
        .as_ref()
        .map(super::content::register)
        .unwrap_or(0);
    if hooks != 0 || p.content.has_scripts() {
        unsafe {
            day_webview_xaml_content(handle.0, hooks, cstr(&scripts_json(&p.content)).as_ptr())
        };
    }
    if let Some(labels) = &p.tabs {
        unsafe {
            day_webview_xaml_tabs(
                handle.0,
                cstr(&labels.foreground).as_ptr(),
                cstr(&labels.background).as_ptr(),
                on_tab,
            );
        }
    }
    handle
}

fn update(_backend: &mut Xaml, h: &WinHandle, patch: &WebPatch) {
    unsafe {
        match patch {
            WebPatch::Load(url) => day_webview_xaml_load(h.0, cstr(url).as_ptr()),
            WebPatch::Back => day_webview_xaml_back(h.0),
            WebPatch::Forward => day_webview_xaml_forward(h.0),
            WebPatch::Stop => day_webview_xaml_stop(h.0),
            WebPatch::Reload => day_webview_xaml_reload(h.0),
            WebPatch::ForceReload => day_webview_xaml_force_reload(h.0),
            WebPatch::ReportState => day_webview_xaml_report_state(h.0),
            WebPatch::Eval { req, script } => {
                day_webview_xaml_eval(h.0, *req, cstr(script).as_ptr())
            }
            WebPatch::Zoom(zoom) => day_webview_xaml_set_zoom(h.0, *zoom),
            // WebView2 already presents itself as desktop Edge.
            WebPatch::DesktopMode(_) => {}
            WebPatch::JavaScript(enabled) => day_webview_xaml_set_javascript(h.0, *enabled),
            WebPatch::ScriptReply { id, reply } => {
                let json = serde_json::json!({ "dayMessageReply": id, "value": reply });
                day_webview_xaml_post_json(h.0, cstr(&json.to_string()).as_ptr());
            }
            // WebKit's rule lists; WebView2 filters through `request_filter`, and asks the user
            // about capture devices itself.
            WebPatch::ContentRules(_)
            | WebPatch::MediaDecision { .. }
            | WebPatch::PromptAnswer { .. } => {}
        }
    }
}

fn release(_backend: &mut Xaml, h: &WinHandle) {
    // Unloaded also occurs during reparenting. Release unmounts or closes the session here.
    unsafe { day_webview_xaml_release(h.0) };
}

day_pieces::renderer!(day_toolkit_winui::RENDERERS, Xaml,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);
