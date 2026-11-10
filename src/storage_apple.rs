//! Public WKWebsiteDataStore profiles, shared by macOS WK hosts and UIKit.
use super::*;
use block2::RcBlock;
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_foundation::{NSArray, NSString, NSURL};

day_core::tls_group! {
    static STORES: RefCell<HashMap<String, Retained<AnyObject>>> = RefCell::new(HashMap::new());
}
pub(crate) fn named_profiles_available() -> bool {
    unsafe {
        msg_send![objc2::class!(WKWebsiteDataStore), respondsToSelector: objc2::sel!(dataStoreForIdentifier:)]
    }
}
pub(crate) fn configure(config: &AnyObject, profile: &WebProfile) {
    let key = profile.storage_key();
    let store = STORES.with(|stores| {
        if let Some(store) = stores.borrow().get(&key) { return store.clone(); }
        let store: Retained<AnyObject> = unsafe {
            if profile.private { msg_send![objc2::class!(WKWebsiteDataStore), nonPersistentDataStore] }
            else if profile == &WebProfile::default() { msg_send![objc2::class!(WKWebsiteDataStore), defaultDataStore] }
            else {
                let class = objc2::class!(WKWebsiteDataStore);
                let available: bool = msg_send![class, respondsToSelector: objc2::sel!(dataStoreForIdentifier:)];
                if available {
                    let hex = key.trim_start_matches("day-");
                    let uuid = NSString::from_str(&format!("{hex}-{hex}"));
                    // The two 64-bit halves form a stable UUID, independent of process hash seeds.
                    let hex = uuid.to_string().replace('-', "");
                    let uuid = NSString::from_str(&format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]));
                    let allocated: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(NSUUID), alloc];
                    let uuid: Retained<AnyObject> = msg_send![allocated, initWithUUIDString: &*uuid];
                    msg_send![class, dataStoreForIdentifier: &*uuid]
                } else {
                    // Never fall back to the global jar: forgetting a site must not erase others.
                    log::warn!("named persistent WebKit profiles require macOS 14 / iOS 17; using isolated memory storage");
                    msg_send![class, nonPersistentDataStore]
                }
            }
        };
        stores.borrow_mut().insert(key, store.clone()); store
    });
    unsafe {
        let _: () = msg_send![config, setWebsiteDataStore: &*store];
    }
}
/// The WKWebView properties [`page_state`] reads, each KVO-observable. `URL` follows
/// same-document changes (pushState, fragments) as well as committed navigations.
const STATE_KEYS: [&str; 6] = [
    "URL",
    "title",
    "loading",
    "estimatedProgress",
    "canGoBack",
    "canGoForward",
];

/// Register `observer` (a navigation delegate implementing
/// `observeValueForKeyPath:ofObject:change:context:`) for every [`STATE_KEYS`] change on `web`.
/// Undo with [`unobserve_state`] before the observer goes away.
pub(crate) fn observe_state(web: &AnyObject, observer: &AnyObject) {
    for key in STATE_KEYS {
        // NSKeyValueObservingOptionNew; the handler reads the web view, not the change dictionary.
        unsafe {
            let _: () = msg_send![web, addObserver: observer, forKeyPath: &*NSString::from_str(key), options: 1usize, context: std::ptr::null_mut::<std::ffi::c_void>()];
        }
    }
}

/// Remove what [`observe_state`] registered. Only call it for a pair that registered: removing
/// an observer that was never added raises an Objective-C exception.
pub(crate) fn unobserve_state(web: &AnyObject, observer: &AnyObject) {
    for key in STATE_KEYS {
        unsafe {
            let _: () =
                msg_send![web, removeObserver: observer, forKeyPath: &*NSString::from_str(key)];
        }
    }
}

/// Zoom `web`'s pages by `zoom` (`WebView::zoom`): `pageZoom`, which WebKit keeps as the view
/// navigates and which reflows the page the way Safari's zoom does. It is public API from
/// macOS 11 and iOS 14; an older WebKit lacks the property and keeps 100%.
pub(crate) fn set_page_zoom(web: &AnyObject, zoom: f64) {
    unsafe {
        let available: bool = msg_send![web, respondsToSelector: objc2::sel!(setPageZoom:)];
        if available {
            let _: () = msg_send![web, setPageZoom: zoom];
        }
    }
}

/// Fill in one navigation's `WKWebpagePreferences` from the view's settings, in the navigation
/// delegate's `decidePolicyForNavigationAction:preferences:decisionHandler:`.
///
/// - `preferredContentMode` asks for the desktop site (`WKContentModeDesktop`, 2) or leaves the
///   engine's recommendation (`WKContentModeRecommended`, 0), which is already the desktop site on
///   a Mac and an iPad. WebKit sends a desktop user agent and lays the page out at desktop width.
/// - `allowsContentJavaScript` stops the scripts the page brings. It does not stop the app's:
///   `evaluateJavaScript` and user scripts still run (WWDC 2020, "Discover WKWebView
///   enhancements"), so `JsHandle::eval`, page state and the tab script keep working. Public from
///   macOS 11 and iOS 14; an older WebKit keeps scripts on.
pub(crate) fn webpage_preferences(preferences: &AnyObject, desktop: bool, javascript: bool) {
    unsafe {
        let _: () =
            msg_send![preferences, setPreferredContentMode: if desktop { 2isize } else { 0isize }];
        let available: bool =
            msg_send![preferences, respondsToSelector: objc2::sel!(setAllowsContentJavaScript:)];
        if available {
            let _: () = msg_send![preferences, setAllowsContentJavaScript: javascript];
        }
    }
}

/// The page state WKWebView holds right now: shared by the KVO reports and the
/// `navigation_state` data request.
pub(crate) fn page_state(web: &AnyObject) -> PageState {
    unsafe {
        let url: Option<Retained<NSURL>> = msg_send![web, URL];
        let url = url
            .and_then(|url| url.absoluteString())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let title: Option<Retained<NSString>> = msg_send![web, title];
        let loading: bool = msg_send![web, isLoading];
        let progress: f64 = msg_send![web, estimatedProgress];
        PageState {
            url,
            title: title.map(|s| s.to_string()).unwrap_or_default(),
            loading,
            // WebKit leaves the last load's estimate behind once it finishes; a finished page
            // reads as complete.
            progress: if loading { progress } else { 1.0 },
            can_go_back: msg_send![web, canGoBack],
            can_go_forward: msg_send![web, canGoForward],
        }
    }
}

/// Capture the visible page as PNG for `JsHandle::snapshot`, at most `width` pixels wide, and
/// answer through `done` with the base64 text as a JSON string.
fn snapshot(web: &AnyObject, width: f64, done: Rc<dyn Fn(String)>) {
    unsafe {
        let config: Retained<AnyObject> = msg_send![objc2::class!(WKSnapshotConfiguration), new];
        let bounds: objc2_foundation::NSRect = msg_send![web, bounds];
        // snapshotWidth is in points; the image comes back at the display's scale.
        #[cfg(target_os = "macos")]
        let scale: f64 = {
            let window: *mut AnyObject = msg_send![web, window];
            if window.is_null() {
                2.0
            } else {
                msg_send![window, backingScaleFactor]
            }
        };
        #[cfg(not(target_os = "macos"))]
        let scale: f64 = {
            let traits: Retained<AnyObject> = msg_send![web, traitCollection];
            let scale: f64 = msg_send![&traits, displayScale];
            if scale > 0.0 { scale } else { 2.0 }
        };
        let points = (width / scale.max(1.0)).min(bounds.size.width).floor();
        if points >= 1.0 {
            let number: Retained<AnyObject> =
                msg_send![objc2::class!(NSNumber), numberWithDouble: points];
            let _: () = msg_send![&config, setSnapshotWidth: &*number];
        }
        let block = RcBlock::new(
            move |image: *mut AnyObject, error: *mut objc2_foundation::NSError| {
                if image.is_null() {
                    let message = if error.is_null() {
                        "no image".to_owned()
                    } else {
                        (*error).localizedDescription().to_string()
                    };
                    done(engine_error("SnapshotError", &message));
                    return;
                }
                #[cfg(target_os = "macos")]
                let png: *mut AnyObject = {
                    let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
                    let rep: *mut AnyObject =
                        msg_send![objc2::class!(NSBitmapImageRep), imageRepWithData: tiff];
                    if rep.is_null() {
                        std::ptr::null_mut()
                    } else {
                        let none: Retained<AnyObject> = msg_send![objc2::class!(NSDictionary), new];
                        // NSBitmapImageFileTypePNG
                        msg_send![rep, representationUsingType: 4usize, properties: &*none]
                    }
                };
                #[cfg(not(target_os = "macos"))]
                let png: *mut AnyObject = UIImagePNGRepresentation(image);
                if png.is_null() {
                    done(engine_error("SnapshotError", "PNG encoding failed"));
                    return;
                }
                let text: Retained<NSString> =
                    msg_send![png, base64EncodedStringWithOptions: 0usize];
                done(format!(
                    "1{SEP}{}",
                    serde_json::Value::String(text.to_string())
                ));
            },
        );
        let _: () =
            msg_send![web, takeSnapshotWithConfiguration: &*config, completionHandler: &*block];
    }
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    /// UIKit's PNG encoder (returns an autoreleased NSData, or nil).
    fn UIImagePNGRepresentation(image: *mut AnyObject) -> *mut AnyObject;
}

pub(crate) fn request(web: &AnyObject, request: &str, done: impl Fn(String) + 'static) -> bool {
    let Some(request) = request.strip_prefix(DATA_REQUEST) else {
        return false;
    };
    let done: Rc<dyn Fn(String)> = Rc::new(done);
    let Ok(data) = serde_json::from_str::<serde_json::Value>(request) else {
        done(engine_error("StorageError", "invalid request"));
        return true;
    };
    let operation = data["operation"].as_str().unwrap_or_default();
    let url = data["url"].as_str().unwrap_or_default().to_owned();
    unsafe {
        let config: Retained<AnyObject> = msg_send![web, configuration];
        let store: Retained<AnyObject> = msg_send![&config, websiteDataStore];
        let jar: Retained<AnyObject> = msg_send![&store, httpCookieStore];
        match operation {
            "navigation" => {
                let state = page_state(web);
                done(format!(
                    "1{SEP}{}",
                    serde_json::json!({"url":state.url,"title":state.title,"can_go_back":state.can_go_back,"can_go_forward":state.can_go_forward,"loading":state.loading})
                ));
            }
            "snapshot" => snapshot(web, data["width"].as_f64().unwrap_or(0.0), done),
            "clear" => {
                let types: Retained<AnyObject> =
                    msg_send![objc2::class!(WKWebsiteDataStore), allWebsiteDataTypes];
                let date: Retained<AnyObject> = msg_send![objc2::class!(NSDate), distantPast];
                let block = RcBlock::new(move || done(format!("1{SEP}true")));
                let _: () = msg_send![&store, removeDataOfTypes: &*types, modifiedSince: &*date, completionHandler: &*block];
            }
            "cookies" => {
                let block = RcBlock::new(move |cookies: *mut NSArray<AnyObject>| {
                    let mut header = Vec::new();
                    if let Some(cookies) = cookies.as_ref() {
                        for cookie in cookies {
                            let domain: Retained<NSString> = msg_send![&*cookie, domain];
                            let path: Retained<NSString> = msg_send![&*cookie, path];
                            let secure: bool = msg_send![&*cookie, isSecure];
                            if super::cookie_matches(
                                &url,
                                &domain.to_string(),
                                &path.to_string(),
                                secure,
                            ) {
                                let name: Retained<NSString> = msg_send![&*cookie, name];
                                let value: Retained<NSString> = msg_send![&*cookie, value];
                                header.push((path.length(), format!("{name}={value}")));
                            }
                        }
                    }
                    header.sort_by_key(|(length, _)| std::cmp::Reverse(*length));
                    let header = header
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect::<Vec<_>>()
                        .join("; ");
                    done(format!("1{SEP}{}", serde_json::to_string(&header).unwrap()));
                });
                let _: () = msg_send![&jar, getAllCookies: &*block];
            }
            "set-cookie" => {
                let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) else {
                    done(engine_error("StorageError", "invalid URL"));
                    return true;
                };
                let headers: Retained<AnyObject> =
                    msg_send![objc2::class!(NSMutableDictionary), new];
                let cookie = NSString::from_str(data["cookie"].as_str().unwrap_or_default());
                let _: () = msg_send![&headers, setObject: &*cookie, forKey: &*NSString::from_str("Set-Cookie")];
                let cookies: Retained<NSArray<AnyObject>> = msg_send![objc2::class!(NSHTTPCookie), cookiesWithResponseHeaderFields: &*headers, forURL: &*url];
                let count = Rc::new(Cell::new(cookies.len()));
                if cookies.is_empty() {
                    done(engine_error("StorageError", "invalid cookie"));
                }
                for cookie in &cookies {
                    let count = count.clone();
                    let done = done.clone();
                    let block = RcBlock::new(move || {
                        count.set(count.get() - 1);
                        if count.get() == 0 {
                            done(format!("1{SEP}true"));
                        }
                    });
                    let _: () = msg_send![&jar, setCookie: &*cookie, completionHandler: &*block];
                }
            }
            _ => done(engine_error("StorageError", "unknown operation")),
        }
    }
    true
}
