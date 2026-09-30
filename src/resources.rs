// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Scoped, binary resource trees. Providers never run while holding the registry lock.
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
};

/// A normalized path inside the provider, without a leading slash. Query is kept separately;
/// fragments are browser-local. Providers run on workers on native hosts and on the browser
/// thread on wasm. Do not access Day signals or toolkit objects from this callback.
#[derive(Clone, Debug)]
pub struct ResourceRequest {
    pub path: String,
    pub query: Option<String>,
    pub method: String,
}
/// An immutable binary response. Cloning shares the body. GET byte ranges and HEAD are handled
/// by the adapter. Keep responses bounded; this API buffers a resource, never an entire site.
#[derive(Clone, Debug)]
pub struct ResourceResponse {
    pub status: u16,
    pub mime: String,
    pub body: Arc<[u8]>,
    pub headers: Vec<(String, String)>,
}
impl ResourceResponse {
    pub fn new(mime: impl Into<String>, body: impl Into<Arc<[u8]>>) -> Self {
        Self {
            status: 200,
            mime: mime.into(),
            body: body.into(),
            headers: Vec::new(),
        }
    }
    pub fn not_found() -> Self {
        Self::error(404)
    }
    pub fn error(status: u16) -> Self {
        Self {
            status,
            ..Self::new("text/plain", Vec::new())
        }
    }
}
type Handler = dyn Fn(ResourceRequest) -> ResourceResponse + Send + Sync;
struct Provider {
    id: u64,
    handler: Box<Handler>,
}
impl Drop for Provider {
    fn drop(&mut self) {
        registry().lock().unwrap().remove(&self.id);
    }
}
/// A resource namespace, kept alive by this handle and by views displaying it. No listener,
/// HTTP server, temporary extraction directory, or JavaScript/base64 transport is required.
#[derive(Clone)]
pub struct ResourceProvider(Arc<Provider>);
impl std::fmt::Debug for ResourceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ResourceProvider").field(&self.0.id).finish()
    }
}
impl PartialEq for ResourceProvider {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}
impl Eq for ResourceProvider {}
fn registry() -> &'static Mutex<HashMap<u64, Weak<Provider>>> {
    static REG: OnceLock<Mutex<HashMap<u64, Weak<Provider>>>> = OnceLock::new();
    REG.get_or_init(Default::default)
}
impl ResourceProvider {
    pub fn new(
        handler: impl Fn(ResourceRequest) -> ResourceResponse + Send + Sync + 'static,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let p = Arc::new(Provider {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            handler: Box::new(handler),
        });
        registry().lock().unwrap().insert(p.id, Arc::downgrade(&p));
        Self(p)
    }
    pub fn id(&self) -> u64 {
        self.0.id
    }
    /// The engine-appropriate base, with trailing slash. Treat it as opaque.
    pub fn base_url(&self) -> String {
        if cfg!(target_arch = "wasm32") {
            format!(
                "assets/data/day-piece-webview/{}/{}/",
                browser_namespace(),
                self.id()
            )
        } else if cfg!(all(feature = "qt", day_qt_webengine)) {
            format!("day-resource://p{}/", self.id())
        } else if cfg!(all(windows, not(feature = "xaml"))) {
            format!("http://day-resource.p{}/", self.id())
        } else if cfg!(any(target_os = "android", windows, target_env = "ohos")) {
            format!("https://day-resource.invalid/{}/", self.id())
        } else {
            format!("day-resource://p{}/", self.id())
        }
    }
    /// Encode an archive-relative path; query and fragment, if needed, can be appended by the
    /// caller. Literal %, # and ? in file names are preserved through percent encoding.
    pub fn url(&self, path: &str) -> String {
        use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
        format!(
            "{}{}",
            self.base_url(),
            path.split('/')
                .map(|p| utf8_percent_encode(p, NON_ALPHANUMERIC).to_string())
                .collect::<Vec<_>>()
                .join("/")
        )
    }
}

pub(crate) fn respond(id: u64, url: &str, method: &str, range: &str) -> ResourceResponse {
    let p = registry().lock().unwrap().get(&id).and_then(Weak::upgrade);
    let Some(p) = p else {
        return ResourceResponse::error(410);
    };
    let base = ResourceProvider(p.clone()).base_url();
    // wasm URLs are absolute on arrival; compare their full reserved pathname, not an arbitrary suffix.
    let Ok(parsed) = url::Url::parse(url) else {
        return ResourceResponse::error(400);
    };
    let relative = if cfg!(target_arch = "wasm32") {
        parsed
            .path()
            .split_once(&format!(
                "/assets/data/day-piece-webview/{}/{id}/",
                browser_namespace()
            ))
            .map(|(_, p)| p)
    } else {
        url.strip_prefix(&base)
            .map(|s| s.split(['?', '#']).next().unwrap_or(""))
    };
    let Some(relative) = relative else {
        return ResourceResponse::error(403);
    };
    let Ok(path) = percent_encoding::percent_decode_str(relative).decode_utf8() else {
        return ResourceResponse::error(400);
    };
    if path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path.split('/').any(|s| s == ".." || s == ".")
    {
        return ResourceResponse::error(403);
    }
    if method != "GET" && method != "HEAD" {
        return ResourceResponse::error(405);
    }
    let request = ResourceRequest {
        path: path.into_owned(),
        query: parsed.query().map(str::to_owned),
        method: method.into(),
    };
    let mut r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (p.handler)(request)))
        .unwrap_or_else(|_| ResourceResponse::error(500));
    // Android's WebResourceResponse rejects redirects. Providers return content or errors;
    // navigation belongs to the view, not to an implicit redirect from a resource callback.
    if !(200..=599).contains(&r.status)
        || (300..400).contains(&r.status)
        || r.mime.contains(['\r', '\n', '\0'])
    {
        return ResourceResponse::error(500);
    }
    r.headers.retain(|(k, v)| {
        !k.is_empty()
            && k.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            && v.chars()
                .all(|c| c == '\t' || (' '..='\u{ff}').contains(&c) && c != '\u{7f}')
            && ![
                "content-type",
                "content-length",
                "content-range",
                "accept-ranges",
                "cache-control",
                "x-content-type-options",
            ]
            .iter()
            .any(|n| k.eq_ignore_ascii_case(n))
    });
    r.headers.push(("Cache-Control".into(), "no-store".into()));
    r.headers
        .push(("X-Content-Type-Options".into(), "nosniff".into()));
    if r.status == 204 || r.status == 205 {
        r.body = Arc::from([]);
    }
    let len = r.body.len();
    if r.status == 200 {
        r.headers.push(("Accept-Ranges".into(), "bytes".into()));
        if !range.is_empty() && method == "GET" {
            if let Some((a, b)) = byte_range(range, len) {
                r.status = 206;
                r.body = Arc::from(&r.body[a..=b]);
                r.headers
                    .push(("Content-Range".into(), format!("bytes {a}-{b}/{len}")));
            } else {
                r.status = 416;
                r.body = Arc::from([]);
                r.headers
                    .push(("Content-Range".into(), format!("bytes */{len}")));
            }
        }
    }
    r.headers
        .push(("Content-Length".into(), r.body.len().to_string()));
    if method == "HEAD" {
        r.body = Arc::from([]);
    }
    r
}
fn byte_range(s: &str, len: usize) -> Option<(usize, usize)> {
    if len == 0 {
        return None;
    }
    let (a, b) = s.strip_prefix("bytes=")?.split_once('-')?;
    if a.is_empty() {
        let n = b.parse::<usize>().ok()?;
        return (n > 0).then_some((len.saturating_sub(n), len - 1));
    }
    let a: usize = a.parse().ok()?;
    let b = if b.is_empty() {
        len - 1
    } else {
        b.parse::<usize>().ok()?.min(len - 1)
    };
    (a <= b && a < len).then_some((a, b))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn background(work: impl FnOnce() + Send + 'static) {
    type Work = Box<dyn FnOnce() + Send>;
    static QUEUE: OnceLock<std::sync::mpsc::Sender<Work>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Work>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..4 {
            let rx = rx.clone();
            std::thread::spawn(move || {
                loop {
                    let work = rx.lock().unwrap().recv();
                    let Ok(work) = work else { break };
                    work();
                }
            });
        }
        tx
    });
    // Queue request metadata, not inflated resources. A fixed pool avoids creating a thread
    // for every image in a large document, and enqueueing never blocks the UI.
    let _ = queue.send(Box::new(work));
}

// Owned C ABI response used by Qt, WebView2, JNI and ArkWeb. Buffers remain valid until free.
#[repr(C)]
pub(crate) struct WireResponse {
    pub status: u16,
    pub mime: std::ffi::CString,
    pub headers: std::ffi::CString,
    pub response: ResourceResponse,
}
pub(crate) fn wire(r: ResourceResponse) -> Box<WireResponse> {
    let headers = r
        .headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect::<String>();
    Box::new(WireResponse {
        status: r.status,
        mime: std::ffi::CString::new(r.mime.as_str()).unwrap_or_default(),
        headers: std::ffi::CString::new(headers).unwrap_or_default(),
        response: r,
    })
}
unsafe fn cstr<'a>(s: *const std::ffi::c_char) -> &'a str {
    if s.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_str()
            .unwrap_or("")
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_read(
    id: u64,
    url: *const std::ffi::c_char,
    method: *const std::ffi::c_char,
    range: *const std::ffi::c_char,
) -> *mut WireResponse {
    Box::into_raw(wire(respond(
        id,
        unsafe { cstr(url) },
        unsafe { cstr(method) },
        unsafe { cstr(range) },
    )))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_free(r: *mut WireResponse) {
    if !r.is_null() {
        drop(unsafe { Box::from_raw(r) })
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_status(r: &WireResponse) -> u16 {
    r.status
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_mime(r: &WireResponse) -> *const std::ffi::c_char {
    r.mime.as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_headers(r: &WireResponse) -> *const std::ffi::c_char {
    r.headers.as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_data(r: &WireResponse) -> *const u8 {
    r.response.body.as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_len(r: &WireResponse) -> usize {
    r.response.body.len()
}

#[cfg(not(target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_start(
    id: u64,
    url: *const std::ffi::c_char,
    method: *const std::ffi::c_char,
    range: *const std::ffi::c_char,
    context: *mut std::ffi::c_void,
    done: extern "C" fn(*mut std::ffi::c_void, *mut WireResponse),
) {
    let (url, method, range) = unsafe {
        (
            cstr(url).to_owned(),
            cstr(method).to_owned(),
            cstr(range).to_owned(),
        )
    };
    let context = context as usize;
    background(move || {
        let r = Box::into_raw(wire(respond(id, &url, &method, &range)));
        done(context as *mut _, r);
    });
}

#[cfg(all(feature = "dom", target_arch = "wasm32"))]
fn browser_namespace() -> String {
    crate::browser::namespace()
}
#[cfg(not(all(feature = "dom", target_arch = "wasm32")))]
fn browser_namespace() -> String {
    String::new()
}

impl ResourceProvider {
    /// Mount a bundled directory at `__day_assets/` alongside dynamic resources. Pass the
    /// generated `res::assets::<directory>` constant. Paths inside the directory remain
    /// document-relative. The directory is trusted app content; the callback owns other paths.
    pub fn with_site(
        site: impl crate::IntoInlineSite,
        handler: impl Fn(ResourceRequest) -> ResourceResponse + Send + Sync + 'static,
    ) -> Self {
        let root = site.into_inline_site().root;
        Self::new(move |request| {
            let Some(path) = request.path.strip_prefix("__day_assets/") else {
                return handler(request);
            };
            let name = format!("{root}/{path}");
            #[cfg(target_arch = "wasm32")]
            {
                let mut r = ResourceResponse::new("application/octet-stream", Vec::new());
                r.headers.push(("X-Day-Bundled-Asset".into(), name));
                r
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                match day_spec::resource(day_spec::AssetName::dynamic(name)) {
                    Some(resource) => {
                        ResourceResponse::new(mime_for_path(path), resource.as_slice().to_vec())
                    }
                    None => ResourceResponse::not_found(),
                }
            }
        })
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn mime_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_validation_precedes_callback_and_preserves_queries() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let p = ResourceProvider::new(move |r| {
            count.fetch_add(1, Ordering::Relaxed);
            assert_eq!(r.path, "text/café #1.xhtml");
            assert_eq!(r.query.as_deref(), Some("mode=print"));
            ResourceResponse::new("text/html", Vec::new())
        });
        let other = ResourceProvider::new(|_| unreachable!("wrong provider"));
        for path in [
            "%2E%2E/private",
            "a/%2e%2e/b",
            "%2Fprivate",
            "a%5Cb",
            "%00",
            "%FF",
        ] {
            assert!(respond(p.id(), &format!("{}{path}", p.base_url()), "GET", "").status >= 400);
        }
        assert_eq!(respond(p.id(), &other.url("x"), "GET", "").status, 403);
        assert_eq!(respond(p.id(), &p.url("x"), "POST", "").status, 405);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            respond(
                p.id(),
                &(p.url("text/café #1.xhtml") + "?mode=print#anchor"),
                "GET",
                ""
            )
            .status,
            200
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn shared_bytes_head_and_response_headers() {
        let bytes: Arc<[u8]> = Arc::from([0, 1, 255]);
        let body = bytes.clone();
        let p = ResourceProvider::new(move |_| {
            let mut r = ResourceResponse::new("application/octet-stream", body.clone());
            r.headers = vec![
                ("X-Fixture".into(), "yes".into()),
                ("Content-Length".into(), "999".into()),
                ("Bad:Name".into(), "x".into()),
                ("X-Bad".into(), "x\r\ny".into()),
            ];
            r
        });
        let r = respond(p.id(), &p.url("bytes"), "GET", "");
        assert!(Arc::ptr_eq(&r.body, &bytes));
        let h: HashMap<_, _> = r.headers.into_iter().collect();
        assert_eq!(h["Content-Length"], "3");
        assert_eq!(h["X-Fixture"], "yes");
        assert!(!h.contains_key("Bad:Name") && !h.contains_key("X-Bad"));
        let head = respond(p.id(), &p.url("bytes"), "HEAD", "");
        assert!(head.body.is_empty());
        assert!(
            head.headers
                .contains(&("Content-Length".into(), "3".into()))
        );
    }
    #[test]
    fn provider_can_use_registry_reentrantly_from_parallel_requests() {
        let p = ResourceProvider::new(|_| {
            let nested = ResourceProvider::new(|_| ResourceResponse::not_found());
            assert_eq!(
                respond(nested.id(), &nested.url("missing"), "GET", "").status,
                404
            );
            ResourceResponse::new("text/plain", vec![42])
        });
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || respond(p.id(), &p.url("fixture"), "GET", "").body)
            })
            .collect();
        for task in tasks {
            assert_eq!(&*task.join().unwrap(), &[42]);
        }
    }
    #[test]
    fn errors_and_empty_statuses_are_portable() {
        for status in [100, 302, 600] {
            let p = ResourceProvider::new(move |_| ResourceResponse::error(status));
            assert_eq!(respond(p.id(), &p.url("fixture"), "GET", "").status, 500);
        }
        for status in [204, 205] {
            let p = ResourceProvider::new(move |_| ResourceResponse {
                status,
                ..ResourceResponse::new("text/plain", vec![1])
            });
            assert!(
                respond(p.id(), &p.url("fixture"), "GET", "")
                    .body
                    .is_empty()
            );
        }
        let p = ResourceProvider::new(|_| panic!("synthetic provider failure"));
        assert_eq!(respond(p.id(), &p.url("fixture"), "GET", "").status, 500);
    }
    #[test]
    fn relative_paths_binary_ranges_and_lifetime() {
        let p = ResourceProvider::new(|r| {
            assert_eq!(r.path, "images/a b.png");
            ResourceResponse::new("image/png", vec![0, 1, 2, 255])
        });
        let u = p.url("images/a b.png");
        assert_eq!(&*respond(p.id(), &u, "GET", "").body, &[0, 1, 2, 255]);
        let r = respond(p.id(), &u, "GET", "bytes=1-2");
        assert_eq!(r.status, 206);
        assert_eq!(&*r.body, &[1, 2]);
        assert_eq!(respond(p.id(), &u, "GET", "bytes=99-").status, 416);
        assert!(respond(p.id(), &u, "HEAD", "").body.is_empty());
        assert_eq!(
            respond(
                p.id(),
                &format!("{}%2e%2e/private", p.base_url()),
                "GET",
                ""
            )
            .status,
            403
        );
        assert_eq!(
            respond(p.id(), "https://elsewhere.invalid/", "GET", "").status,
            403
        );
        let id = p.id();
        drop(p);
        assert_eq!(respond(id, &u, "GET", "").status, 410);
    }
    #[test]
    fn ranges() {
        assert_eq!(byte_range("bytes=-2", 4), Some((2, 3)));
        assert_eq!(byte_range("bytes=1-", 4), Some((1, 3)));
        assert_eq!(byte_range("bytes=1-2,3-4", 8), None);
    }
}
