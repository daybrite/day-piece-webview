// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! WebView2 child-window host for GTK/Qt on Windows. Wry supplies the COM lifetime and loader;
//! the toolkit supplies a native parent and allocation. No Readium or second GUI event loop.
use super::*;
use day_spec::NodeId;
use std::num::NonZeroIsize;
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
struct Parent(NonZeroIsize);
impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // The toolkit keeps the HWND alive until after the WebView is dropped.
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(self.0)))
        })
    }
}
pub(crate) struct Host {
    pub view: wry::WebView,
    id: NodeId,
    emit: fn(NodeId, Event),
}
impl Host {
    pub fn new(
        hwnd: isize,
        url: String,
        prefix: String,
        id: NodeId,
        emit: fn(NodeId, Event),
    ) -> Result<Self, String> {
        let parent = Parent(NonZeroIsize::new(hwnd).ok_or("No native window")?);
        let provider = prefix
            .strip_prefix("http://day-resource.p")
            .and_then(|s| s.trim_end_matches('/').parse::<u64>().ok())
            .unwrap_or(0);
        let mut builder = wry::WebViewBuilder::new();
        if provider != 0 {
            builder = builder.with_asynchronous_custom_protocol(
                "day-resource".into(),
                move |_, request, responder| {
                    super::resources::background(move || {
                        let url = request.uri().to_string().replacen(
                            &format!("day-resource://p{provider}/"),
                            &format!("http://day-resource.p{provider}/"),
                            1,
                        );
                        let r = super::resources::respond(
                            provider,
                            &url,
                            request.method().as_str(),
                            request
                                .headers()
                                .get("Range")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or(""),
                        );
                        let mut response = wry::http::Response::builder()
                            .status(r.status)
                            .header("Content-Type", &r.mime);
                        for (k, v) in r.headers {
                            response = response.header(k, v);
                        }
                        responder.respond(response.body(r.body.to_vec()).unwrap());
                    });
                },
            );
        }
        let view = builder
            .with_url(url)
            .with_navigation_handler(move |url| {
                let allowed = prefix.is_empty()
                    || url == "about:blank"
                    || url == "about:srcdoc"
                    || url::Url::parse(&url)
                        .ok()
                        .is_some_and(|u| u.as_str().starts_with(&prefix));
                if !allowed {
                    emit(
                        id,
                        Event::Custom {
                            tag: "webview:link",
                            num: LINK_REPORT,
                            text: url,
                        },
                    );
                }
                allowed
            })
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    emit(id, Event::custom("webview:url", url));
                }
            })
            .build_as_child(&parent)
            .map_err(|e| e.to_string())?;
        Ok(Self { view, id, emit })
    }
    pub fn bounds(&self, x: f64, y: f64, w: f64, h: f64) {
        let _ = self.view.set_bounds(wry::Rect {
            position: wry::dpi::LogicalPosition::new(x, y).into(),
            size: wry::dpi::LogicalSize::new(w.max(1.), h.max(1.)).into(),
        });
    }
    pub fn patch(&self, p: &WebPatch) {
        match p {
            WebPatch::Load(url) => {
                let _ = self.view.load_url(url);
            }
            WebPatch::Back => {
                let _ = self.view.go_back();
            }
            WebPatch::Forward => {
                let _ = self.view.go_forward();
            }
            WebPatch::Reload => {
                let _ = self.view.reload();
            }
            WebPatch::Stop => {
                let _ = self.view.evaluate_script("window.stop()");
            }
            WebPatch::Eval { req, script } => {
                let (id, emit, req) = (self.id, self.emit, *req);
                let result = self
                    .view
                    .evaluate_script_with_callback(script, move |json| {
                        let text = serde_json::from_str::<String>(&json).unwrap_or_else(|_| {
                            engine_error("WebView2", "Invalid JavaScript result")
                        });
                        emit(
                            id,
                            Event::Custom {
                                tag: "webview:eval",
                                num: req as f64,
                                text,
                            },
                        );
                    });
                if let Err(e) = result {
                    emit(
                        id,
                        Event::Custom {
                            tag: "webview:eval",
                            num: req as f64,
                            text: engine_error("WebView2", &e.to_string()),
                        },
                    );
                }
            }
        }
    }
}

#[cfg(feature = "qt")]
mod qt_bridge {
    use super::*;
    use std::ffi::{CStr, c_char};
    day_core::tls_group! {static HOSTS:RefCell<HashMap<u64,Rc<Host>>>=RefCell::new(HashMap::new());}
    unsafe fn string(p: *const c_char) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn day_webview_win_create(
        hwnd: isize,
        id: u64,
        url: *const c_char,
        prefix: *const c_char,
    ) -> bool {
        match Host::new(
            hwnd,
            unsafe { string(url) },
            unsafe { string(prefix) },
            NodeId(id),
            day_qt::emit,
        ) {
            Ok(h) => {
                HOSTS.with(|m| m.borrow_mut().insert(id, Rc::new(h)));
                true
            }
            Err(e) => {
                log::error!("WebView2: {e}");
                false
            }
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn day_webview_win_bounds(id: u64, w: f64, h: f64) {
        if let Some(v) = HOSTS.with(|m| m.borrow().get(&id).cloned()) {
            v.bounds(0., 0., w, h)
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn day_webview_win_release(id: u64) {
        let host = HOSTS.with(|m| m.borrow_mut().remove(&id));
        drop(host);
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn day_webview_win_command(id: u64, op: u32, req: u64, text: *const c_char) {
        let p = match op {
            0 => WebPatch::Load(unsafe { string(text) }),
            1 => WebPatch::Back,
            2 => WebPatch::Forward,
            3 => WebPatch::Stop,
            4 => WebPatch::Reload,
            _ => WebPatch::Eval {
                req,
                script: unsafe { string(text) },
            },
        };
        if let Some(host) = HOSTS.with(|m| m.borrow().get(&id).cloned()) {
            host.patch(&p)
        } else if op == 5 {
            day_qt::emit(
                NodeId(id),
                Event::Custom {
                    tag: "webview:eval",
                    num: req as f64,
                    text: engine_error(
                        "WebView2",
                        "View not ready; check WebView2 Runtime installation",
                    ),
                },
            );
        }
    }
}
