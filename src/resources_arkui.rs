// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! ArkWeb API 12 scheme handler. Binary responses bypass ArkTS and JavaScript.
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
type P = *mut c_void;
#[link(name = "ohweb")]
unsafe extern "C" {
    fn OH_ArkWeb_CreateSchemeHandler(out: *mut P);
    fn OH_ArkWeb_DestroySchemeHandler(p: P);
    fn OH_ArkWebSchemeHandler_SetUserData(p: P, data: P) -> i32;
    fn OH_ArkWebSchemeHandler_GetUserData(p: P) -> P;
    fn OH_ArkWebSchemeHandler_SetOnRequestStart(
        p: P,
        cb: unsafe extern "C" fn(P, P, P, *mut bool),
    ) -> i32;
    fn OH_ArkWebSchemeHandler_SetOnRequestStop(p: P, cb: unsafe extern "C" fn(P, P)) -> i32;
    fn OH_ArkWeb_SetSchemeHandler(scheme: *const c_char, tag: *const c_char, p: P) -> bool;
    fn OH_ArkWeb_ClearSchemeHandlers(tag: *const c_char) -> i32;
    fn OH_ArkWebResourceRequest_GetUrl(p: P, out: *mut *mut c_char);
    fn OH_ArkWebResourceRequest_GetMethod(p: P, out: *mut *mut c_char);
    fn OH_ArkWebResourceRequest_GetRequestHeaders(p: P, out: *mut P);
    fn OH_ArkWebRequestHeaderList_GetSize(p: P) -> i32;
    fn OH_ArkWebRequestHeaderList_GetHeader(
        p: P,
        index: i32,
        key: *mut *mut c_char,
        value: *mut *mut c_char,
    );
    fn OH_ArkWebRequestHeaderList_Destroy(p: P);
    fn OH_ArkWebResourceRequest_Destroy(p: P) -> i32;
    fn OH_ArkWebResourceRequest_GetResourceType(p: P) -> i32;
    fn OH_ArkWebResourceRequest_GetFrameUrl(p: P, out: *mut *mut c_char);
    fn OH_ArkWebResourceRequest_IsMainFrame(p: P) -> bool;
    fn OH_ArkWeb_ReleaseString(s: *mut c_char);
    fn OH_ArkWeb_CreateResponse(out: *mut P);
    fn OH_ArkWeb_DestroyResponse(p: P);
    fn OH_ArkWebResponse_SetStatus(p: P, status: i32) -> i32;
    fn OH_ArkWebResponse_SetMimeType(p: P, s: *const c_char) -> i32;
    fn OH_ArkWebResponse_SetCharset(p: P, s: *const c_char) -> i32;
    fn OH_ArkWebResponse_SetHeaderByName(
        p: P,
        k: *const c_char,
        v: *const c_char,
        overwrite: bool,
    ) -> i32;
    fn OH_ArkWebResourceHandler_DidReceiveResponse(p: P, response: P) -> i32;
    fn OH_ArkWebResourceHandler_DidReceiveData(p: P, bytes: *const u8, len: i64) -> i32;
    fn OH_ArkWebResourceHandler_DidFinish(p: P) -> i32;
    fn OH_ArkWebResourceHandler_Destroy(p: P) -> i32;
}
struct Request {
    handler: usize,
    cancelled: AtomicBool,
}
impl Drop for Request {
    fn drop(&mut self) {
        unsafe {
            OH_ArkWebResourceHandler_Destroy(self.handler as P);
        }
    }
}
/// What each installed handler serves: the provider id (0 for none) and the request filter,
/// by the token its user data carries. Removed when the registration drops, so a callback that
/// races teardown finds nothing and lets the request through.
type Handlers = Mutex<HashMap<usize, (u64, Option<crate::RequestHooks>)>>;
fn handlers() -> &'static Handlers {
    static M: OnceLock<Handlers> = OnceLock::new();
    M.get_or_init(Default::default)
}
fn requests() -> &'static Mutex<HashMap<usize, Arc<Request>>> {
    static M: OnceLock<Mutex<HashMap<usize, Arc<Request>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}
unsafe fn take(s: *mut c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
        unsafe { OH_ArkWeb_ReleaseString(s) };
        text
    }
}
unsafe extern "C" fn start(scheme: P, request: P, handler: P, intercept: *mut bool) {
    unsafe {
        let mut url = std::ptr::null_mut();
        OH_ArkWebResourceRequest_GetUrl(request, &mut url);
        let url = take(url);
        let token = OH_ArkWebSchemeHandler_GetUserData(scheme) as usize;
        let Some((id, hooks)) = handlers().lock().ok().and_then(|h| h.get(&token).cloned()) else {
            *intercept = false;
            return;
        };
        if id == 0 || !url.starts_with("https://day-resource.invalid/") {
            *intercept = match hooks {
                Some(hooks) => filter(&hooks, request, handler, &url),
                None => false,
            };
            return;
        }
        // The provider's pages too, as on every engine: `decide` passes them where the provider
        // is the app's own site and asks the filter where the view only borrows it.
        if let Some(hooks) = &hooks
            && filter(hooks, request, handler, &url)
        {
            *intercept = true;
            return;
        }
        *intercept = true;
        let mut method = std::ptr::null_mut();
        OH_ArkWebResourceRequest_GetMethod(request, &mut method);
        let method = take(method);
        let mut headers = std::ptr::null_mut();
        OH_ArkWebResourceRequest_GetRequestHeaders(request, &mut headers);
        let mut range = String::new();
        if !headers.is_null() {
            for i in 0..OH_ArkWebRequestHeaderList_GetSize(headers) {
                let (mut k, mut v) = (std::ptr::null_mut(), std::ptr::null_mut());
                OH_ArkWebRequestHeaderList_GetHeader(headers, i, &mut k, &mut v);
                let (k, v) = (take(k), take(v));
                if k.eq_ignore_ascii_case("range") {
                    range = v;
                }
            }
            OH_ArkWebRequestHeaderList_Destroy(headers);
        }
        let pending = Arc::new(Request {
            handler: handler as usize,
            cancelled: AtomicBool::new(false),
        });
        requests()
            .lock()
            .unwrap()
            .insert(request as usize, pending.clone());
        super::resources::background(move || {
            if pending.cancelled.load(Ordering::Acquire) {
                return;
            }
            let r = super::resources::respond(id, &url, &method, &range);
            if pending.cancelled.load(Ordering::Acquire) {
                return;
            }
            let h = pending.handler as P;
            let mut response = std::ptr::null_mut();
            OH_ArkWeb_CreateResponse(&mut response);
            OH_ArkWebResponse_SetStatus(response, r.status as i32);
            // ArkWeb expects a bare MIME type, not an HTTP Content-Type value. Passing
            // "text/html; charset=utf-8" here makes the reader display literal HTML.
            let (mime, charset) = super::resource_content_type::parse(&r.mime);
            OH_ArkWebResponse_SetMimeType(response, CString::new(mime).unwrap().as_ptr());
            if let Some(charset) = charset {
                OH_ArkWebResponse_SetCharset(response, CString::new(charset).unwrap().as_ptr());
            }
            OH_ArkWebResponse_SetHeaderByName(
                response,
                c"Content-Type".as_ptr(),
                CString::new(r.mime).unwrap().as_ptr(),
                true,
            );
            for (k, v) in r.headers {
                OH_ArkWebResponse_SetHeaderByName(
                    response,
                    CString::new(k).unwrap().as_ptr(),
                    CString::new(v).unwrap().as_ptr(),
                    true,
                );
            }
            OH_ArkWebResourceHandler_DidReceiveResponse(h, response);
            OH_ArkWeb_DestroyResponse(response);
            for bytes in r.body.chunks(64 * 1024) {
                if pending.cancelled.load(Ordering::Acquire) {
                    return;
                }
                OH_ArkWebResourceHandler_DidReceiveData(h, bytes.as_ptr(), bytes.len() as i64);
            }
            if !pending.cancelled.load(Ordering::Acquire) {
                OH_ArkWebResourceHandler_DidFinish(h);
            }
        });
    }
}
/// WebView::request_filter, on ArkWeb's IO thread: `true` when the request was blocked and
/// answered here with an empty 403. ArkWeb names the resource type (Chromium's numbering) and the
/// requesting frame.
unsafe fn filter(hooks: &crate::RequestHooks, request: P, handler: P, url: &str) -> bool {
    unsafe {
        let mut method = std::ptr::null_mut();
        OH_ArkWebResourceRequest_GetMethod(request, &mut method);
        let method = take(method);
        let mut frame = std::ptr::null_mut();
        OH_ArkWebResourceRequest_GetFrameUrl(request, &mut frame);
        let frame = take(frame);
        let main_frame = OH_ArkWebResourceRequest_IsMainFrame(request);
        let kind =
            crate::ResourceKind::from_chromium(OH_ArkWebResourceRequest_GetResourceType(request));
        let info = crate::RequestInfo {
            url,
            top_url: None,
            frame_url: (!frame.is_empty() && !main_frame).then_some(frame.as_str()),
            kind,
            main_frame,
            method: &method,
        };
        if hooks.decide(&info) != crate::RequestDecision::Block {
            return false;
        }
        // Kept until `stop`, which destroys the handler, as for a served request.
        requests().lock().unwrap().insert(
            request as usize,
            Arc::new(Request {
                handler: handler as usize,
                cancelled: AtomicBool::new(false),
            }),
        );
        let mut response = std::ptr::null_mut();
        OH_ArkWeb_CreateResponse(&mut response);
        OH_ArkWebResponse_SetStatus(response, 403);
        OH_ArkWebResponse_SetMimeType(response, c"text/plain".as_ptr());
        OH_ArkWebResourceHandler_DidReceiveResponse(handler, response);
        OH_ArkWeb_DestroyResponse(response);
        OH_ArkWebResourceHandler_DidFinish(handler);
        true
    }
}
unsafe extern "C" fn stop(_scheme: P, request: P) {
    if let Some(p) = requests().lock().unwrap().remove(&(request as usize)) {
        p.cancelled.store(true, Ordering::Release);
    }
    unsafe {
        OH_ArkWebResourceRequest_Destroy(request);
    }
}
pub(crate) struct Registration {
    tag: CString,
    handler: P,
    token: usize,
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut handlers) = handlers().lock() {
            handlers.remove(&self.token);
        }
        unsafe {
            OH_ArkWeb_ClearSchemeHandlers(self.tag.as_ptr());
            OH_ArkWeb_DestroySchemeHandler(self.handler);
        }
    }
}
/// Install the view's scheme handler: `https` for the provider's namespace (`id`, 0 for none),
/// and `http` and `https` for the request filter when there is one.
pub(crate) fn install(
    id: u64,
    hooks: Option<crate::RequestHooks>,
    tag: &str,
) -> Option<Registration> {
    if id == 0 && hooks.is_none() {
        return None;
    }
    unsafe {
        let tag = CString::new(tag).ok()?;
        let token = day_async::next_token() as usize;
        let filtered = hooks.is_some();
        handlers().lock().ok()?.insert(token, (id, hooks));
        let mut handler = std::ptr::null_mut();
        OH_ArkWeb_CreateSchemeHandler(&mut handler);
        OH_ArkWebSchemeHandler_SetUserData(handler, token as P);
        OH_ArkWebSchemeHandler_SetOnRequestStart(handler, start);
        OH_ArkWebSchemeHandler_SetOnRequestStop(handler, stop);
        let registration = Registration {
            tag,
            handler,
            token,
        };
        if !OH_ArkWeb_SetSchemeHandler(c"https".as_ptr(), registration.tag.as_ptr(), handler) {
            return None;
        }
        if filtered
            && !OH_ArkWeb_SetSchemeHandler(c"http".as_ptr(), registration.tag.as_ptr(), handler)
        {
            log::warn!(
                "day-piece-webview: ArkWeb did not take an http scheme handler; http requests go unfiltered"
            );
        }
        Some(registration)
    }
}
