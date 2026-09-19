// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// XAML: this crate's C++/WinRT shim (src/lib-xaml-shim.cpp) wrapping the UWP-XAML WebView,
// boxed into Day handles via the `day_xaml_box`/`day_xaml_unbox` functions day-xaml-sys
// exports (like the Qt renderer's shim). The shim reports url changes through a C callback →
// `Event::custom("webview:url", …)`. Windows-only, built + verified in CI (not on this host).
// ---------------------------------------------------------------------------

use super::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use day_spec::NodeId;
use day_xaml::{WinHandle, Xaml};

unsafe extern "C" {
    fn day_webview_xaml_new(
        url: *const c_char,
        id: u64,
        cb: extern "C" fn(u64, *const c_char),
        inline_root: *const c_char,
        inline_start: *const c_char,
        inline_dir: *const c_char,
        link_cb: extern "C" fn(u64, *const c_char),
    ) -> *mut c_void;
    fn day_webview_xaml_load(handle: *mut c_void, url: *const c_char);
    fn day_webview_xaml_back(handle: *mut c_void);
    fn day_webview_xaml_forward(handle: *mut c_void);
    fn day_webview_xaml_stop(handle: *mut c_void);
    fn day_webview_xaml_reload(handle: *mut c_void);
    fn day_webview_xaml_set_eval_cb(cb: extern "C" fn(u64, u64, *const c_char));
    fn day_webview_xaml_eval(handle: *mut c_void, req: u64, script: *const c_char);
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
    day_xaml::emit(
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

extern "C" fn on_url(id: u64, url: *const c_char) {
    if url.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(url) }
        .to_string_lossy()
        .into_owned();
    day_xaml::emit(NodeId(id), Event::custom("webview:url", s));
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
    day_xaml::emit(
        NodeId(id),
        Event::Custom {
            tag: "webview:link",
            num: super::LINK_REPORT,
            text: s,
        },
    );
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
    EVAL_CB.call_once(|| unsafe { day_webview_xaml_set_eval_cb(on_eval) });
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
    WinHandle(unsafe {
        day_webview_xaml_new(
            cstr(&p.url).as_ptr(),
            id.0,
            on_url,
            cstr(&p.inline_root).as_ptr(),
            cstr(&p.inline_start).as_ptr(),
            cstr(&dir).as_ptr(),
            on_link,
        )
    })
}

fn update(_backend: &mut Xaml, h: &WinHandle, patch: &WebPatch) {
    unsafe {
        match patch {
            WebPatch::Load(url) => day_webview_xaml_load(h.0, cstr(url).as_ptr()),
            WebPatch::Back => day_webview_xaml_back(h.0),
            WebPatch::Forward => day_webview_xaml_forward(h.0),
            WebPatch::Stop => day_webview_xaml_stop(h.0),
            WebPatch::Reload => day_webview_xaml_reload(h.0),
            WebPatch::Eval { req, script } => {
                day_webview_xaml_eval(h.0, *req, cstr(script).as_ptr())
            }
        }
    }
}

day_pieces::renderer!(day_xaml::RENDERERS, Xaml,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure);
