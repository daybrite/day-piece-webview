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
                let url: Option<Retained<NSURL>> = msg_send![web, URL];
                let url = url
                    .map(|url| {
                        let text: Retained<NSString> = msg_send![&*url, absoluteString];
                        text.to_string()
                    })
                    .unwrap_or_default();
                let title: Option<Retained<NSString>> = msg_send![web, title];
                let back: bool = msg_send![web, canGoBack];
                let forward: bool = msg_send![web, canGoForward];
                let loading: bool = msg_send![web, isLoading];
                done(format!(
                    "1{SEP}{}",
                    serde_json::json!({"url":url,"title":title.map(|s|s.to_string()).unwrap_or_default(),"can_go_back":back,"can_go_forward":forward,"loading":loading})
                ));
            }
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
