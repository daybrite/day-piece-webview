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
    fn OH_ArkWeb_ReleaseString(s: *mut c_char);
    fn OH_ArkWeb_CreateResponse(out: *mut P);
    fn OH_ArkWeb_DestroyResponse(p: P);
    fn OH_ArkWebResponse_SetStatus(p: P, status: i32) -> i32;
    fn OH_ArkWebResponse_SetMimeType(p: P, s: *const c_char) -> i32;
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
        if !url.starts_with("https://day-resource.invalid/") {
            *intercept = false;
            return;
        }
        *intercept = true;
        let id = OH_ArkWebSchemeHandler_GetUserData(scheme) as usize as u64;
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
            OH_ArkWebResponse_SetMimeType(response, CString::new(r.mime).unwrap().as_ptr());
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
}
impl Drop for Registration {
    fn drop(&mut self) {
        unsafe {
            OH_ArkWeb_ClearSchemeHandlers(self.tag.as_ptr());
            OH_ArkWeb_DestroySchemeHandler(self.handler);
        }
    }
}
pub(crate) fn install(id: u64, tag: &str) -> Option<Registration> {
    unsafe {
        let tag = CString::new(tag).ok()?;
        let mut handler = std::ptr::null_mut();
        OH_ArkWeb_CreateSchemeHandler(&mut handler);
        OH_ArkWebSchemeHandler_SetUserData(handler, id as usize as P);
        OH_ArkWebSchemeHandler_SetOnRequestStart(handler, start);
        OH_ArkWebSchemeHandler_SetOnRequestStop(handler, stop);
        if !OH_ArkWeb_SetSchemeHandler(c"https".as_ptr(), tag.as_ptr(), handler) {
            OH_ArkWeb_DestroySchemeHandler(handler);
            return None;
        }
        Some(Registration { tag, handler })
    }
}
