// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! The browser duties (docs/webview.md) on WebKit, shared by the UIKit and macOS backends: the
//! questions WKWebView asks its delegates, each held until the app answers
//! (`WebPatch::PromptAnswer`), failed loads, and downloads.
//!
//! A backend's delegate class forwards its WKUIDelegate and WKNavigationDelegate methods here
//! with its toolkit's event sender. Everything runs on the main thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use block2::RcBlock;
use day_spec::{Event, NodeId};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSError, NSString, NSURL, NSURLRequest};

use super::DialogKind;

/// A toolkit's event sender (`day_toolkit_uikit::emit`, `day_toolkit_appkit::emit`).
pub(crate) type Emit = fn(NodeId, Event);

/// The view a delegate method was called for: where its reports go, and the key its
/// unanswered questions are dismissed by when it is released.
#[derive(Clone, Copy)]
pub(crate) struct Asking {
    pub emit: Emit,
    pub node: NodeId,
    /// The web view's address, the same key the backend keeps its delegate under.
    pub view: usize,
}

/// WKNavigationActionPolicy and WKNavigationResponsePolicy: Cancel = 0, Allow = 1, and
/// Download = 2 (iOS 14.5, macOS 11.3).
pub(crate) const POLICY_CANCEL: isize = 0;
pub(crate) const POLICY_ALLOW: isize = 1;
pub(crate) const POLICY_DOWNLOAD: isize = 2;

/// NSURLSessionAuthChallengeDisposition.
const CHALLENGE_USE_CREDENTIAL: isize = 0;
const CHALLENGE_DEFAULT: isize = 1;

/// `SecTrustRef`'s pointee, so `msg_send!` sees the type WebKit declares.
#[repr(C)]
struct SecTrust {
    _opaque: [u8; 0],
}
// SAFETY: the encoding of `struct __SecTrust *`, an opaque Core Foundation object.
unsafe impl objc2::encode::RefEncode for SecTrust {
    const ENCODING_REF: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__SecTrust", &[]));
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTrustCopyExceptions(trust: *mut SecTrust) -> *const std::ffi::c_void;
    fn SecTrustSetExceptions(trust: *mut SecTrust, exceptions: *const std::ffi::c_void) -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const std::ffi::c_void);
}

/// A completion handler WebKit is waiting on. WebKit raises an exception when one is released
/// without having been called, so every one of these is called exactly once: by the app's
/// answer, or by `dismiss` when its view goes away.
pub(crate) enum Pending {
    Alert(RcBlock<dyn Fn()>),
    Confirm(RcBlock<dyn Fn(objc2::runtime::Bool)>),
    /// `prompt()`, with the text OK returns when the app sends none.
    Prompt(RcBlock<dyn Fn(*mut NSString)>, String),
    Auth(RcBlock<dyn Fn(isize, *mut AnyObject)>),
    /// Where a download goes, and the download it is for.
    Destination(RcBlock<dyn Fn(*mut NSURL)>),
    /// A file from the app's own resource provider, which WebKit cannot download: this piece
    /// writes it once the app says where.
    ProviderFile {
        provider: u64,
        url: String,
    },
}

impl Pending {
    /// Answer as the app said, or with what WebKit does when nobody answers.
    fn finish(self, id: u64, answer: &super::browsing::Answer) {
        match self {
            Pending::Alert(done) => done.call(()),
            Pending::Confirm(done) => done.call((objc2::runtime::Bool::new(answer.ok),)),
            Pending::Prompt(done, fallback) => {
                if answer.ok {
                    let text = NSString::from_str(answer.text.as_deref().unwrap_or(&fallback));
                    done.call((Retained::as_ptr(&text) as *mut NSString,));
                } else {
                    done.call((std::ptr::null_mut(),));
                }
            }
            Pending::Auth(done) => match (answer.ok, &answer.user, &answer.password) {
                (true, Some(user), Some(password)) => {
                    // NSURLCredentialPersistenceForSession = 1: asked once per site and run.
                    let credential: Retained<AnyObject> = unsafe {
                        msg_send![objc2::class!(NSURLCredential),
                            credentialWithUser: &*NSString::from_str(user),
                            password: &*NSString::from_str(password),
                            persistence: 1usize]
                    };
                    done.call((
                        CHALLENGE_USE_CREDENTIAL,
                        Retained::as_ptr(&credential) as *mut AnyObject,
                    ));
                }
                // Going on without a credential loads the server's own "unauthorized" page.
                _ => done.call((CHALLENGE_DEFAULT, std::ptr::null_mut())),
            },
            Pending::Destination(done) => match answer.paths.first().filter(|_| answer.ok) {
                Some(path) => {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                    DOWNLOADS.with(|d| {
                        if let Some(download) = d.borrow_mut().iter_mut().find(|x| x.id == id) {
                            download.path = Some(path.clone());
                        }
                    });
                    done.call((Retained::as_ptr(&url) as *mut NSURL,));
                    watch_downloads();
                }
                None => {
                    // A nil destination cancels the download, which reports nothing further.
                    DOWNLOADS.with(|d| d.borrow_mut().retain(|x| x.id != id));
                    done.call((std::ptr::null_mut(),));
                }
            },
            Pending::ProviderFile { provider, url } => {
                let Some(path) = answer.paths.first().filter(|_| answer.ok) else {
                    return;
                };
                let response = super::resources::respond(provider, &url, "GET", "");
                let event = if response.status >= 400 {
                    super::DownloadEvent::Failed(format!("HTTP {}", response.status))
                } else {
                    match std::fs::write(path, &response.body) {
                        Ok(()) => {
                            let size = response.body.len() as u64;
                            super::browsing::download_event(
                                id,
                                super::DownloadEvent::Progress {
                                    received: size,
                                    total: Some(size),
                                },
                            );
                            super::DownloadEvent::Finished {
                                location: path.clone(),
                            }
                        }
                        Err(error) => super::DownloadEvent::Failed(error.to_string()),
                    }
                };
                super::browsing::download_event(id, event);
            }
        }
    }
}

thread_local! {
    /// Questions awaiting the app, by id, with the view that asked (`0` for a download's
    /// destination, which no view owns once the download has begun).
    static PROMPTS: RefCell<HashMap<u64, (usize, Pending)>> = RefCell::new(HashMap::new());
    static NEXT_PROMPT: Cell<u64> = const { Cell::new(1) };
    /// Downloads WebKit is running, kept alive here: WebKit holds a download's delegate weakly
    /// and the download itself only while it runs.
    static DOWNLOADS: RefCell<Vec<Download>> = const { RefCell::new(Vec::new()) };
    static DOWNLOAD_DELEGATE: RefCell<Option<Retained<DownloadDelegate>>> = const { RefCell::new(None) };
    static DOWNLOAD_TIMER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

struct Download {
    /// `0` until WebKit asks where the file goes, which is when the app first hears of it.
    id: u64,
    download: Retained<AnyObject>,
    /// The node whose view started it, which is asked for the destination, and its toolkit's
    /// event sender.
    node: NodeId,
    emit: Emit,
    path: Option<String>,
}

/// Keep `pending` until the app answers, and report the question to `node`.
pub(crate) fn ask(
    emit: Emit,
    node: NodeId,
    view: usize,
    pending: Pending,
    report: impl FnOnce(u64) -> super::browsing::PromptReport,
) {
    let id = NEXT_PROMPT.with(|n| n.replace(n.get() + 1));
    PROMPTS.with(|p| p.borrow_mut().insert(id, (view, pending)));
    emit(node, super::browsing::prompt_event(&report(id)));
}

/// `WebPatch::PromptAnswer`.
pub(crate) fn answer_prompt(id: u64, answer: &str) {
    let pending = PROMPTS.with(|p| p.borrow_mut().remove(&id));
    if let Some((_, pending)) = pending {
        pending.finish(id, &super::browsing::Answer::parse(answer));
    }
}

/// A view is going away: nothing will answer what it still asks.
pub(crate) fn dismiss_prompts(view: usize) {
    let mine: Vec<u64> = PROMPTS.with(|p| {
        p.borrow()
            .iter()
            .filter(|(_, (owner, _))| *owner == view)
            .map(|(id, _)| *id)
            .collect()
    });
    for id in mine {
        answer_prompt(id, "");
    }
}

/// The address of the frame a script dialog came from (`WKFrameInfo.request.URL`).
fn frame_url(frame: &AnyObject) -> String {
    let request: Option<Retained<NSURLRequest>> = unsafe { msg_send![frame, request] };
    request
        .and_then(|r| r.URL())
        .and_then(|u| u.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Why a navigation failed, in the front-end's terms, or `None` for the failures that are not
/// errors: a load the user or a newer navigation canceled, and one that became a download.
fn load_error(error: &NSError) -> Option<super::browsing::Notice> {
    use super::browsing::LoadErrorKind as Kind;
    let domain: Retained<NSString> = unsafe { msg_send![error, domain] };
    let code: isize = unsafe { msg_send![error, code] };
    let domain = domain.to_string();
    let kind = match (domain.as_str(), code) {
        // NSURLErrorCancelled; WebKitErrorFrameLoadInterruptedByPolicyChange (a download, or
        // a link this piece handed elsewhere) and its plug-in sibling.
        ("NSURLErrorDomain", -999) | ("WebKitErrorDomain", 102 | 204) => return None,
        ("NSURLErrorDomain", -1009 | -1020) => Kind::Offline,
        ("NSURLErrorDomain", -1003 | -1006) => Kind::HostNotFound,
        ("NSURLErrorDomain", -1001) => Kind::Timeout,
        ("NSURLErrorDomain", -1004 | -1005) => Kind::Connection,
        ("NSURLErrorDomain", -1206..=-1201) => Kind::Certificate,
        ("NSURLErrorDomain", -1200) => Kind::Secure,
        ("NSURLErrorDomain", -1002) => Kind::Unsupported,
        _ => Kind::Other,
    };
    let text: Retained<NSString> = unsafe { msg_send![error, localizedDescription] };
    let info: Retained<AnyObject> = unsafe { msg_send![error, userInfo] };
    let failing: Option<Retained<AnyObject>> = unsafe {
        msg_send![&info, objectForKey: &*NSString::from_str("NSErrorFailingURLStringKey")]
    };
    let url = failing
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    Some(super::browsing::Notice::LoadError {
        url,
        error: kind,
        code: code as i64,
        text: text.to_string(),
    })
}

define_class!(
    // WKDownloadDelegate for every download: a download outlives the view that started it, so
    // its delegate cannot be the view's.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayWebDownloads"]
    struct DownloadDelegate;

    unsafe impl NSObjectProtocol for DownloadDelegate {}

    impl DownloadDelegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn destination(&self, download: &AnyObject, response: &AnyObject, name: &NSString, done: &block2::DynBlock<dyn Fn(*mut NSURL)>) {
            let key = download as *const AnyObject;
            let node = DOWNLOADS.with(|d| d.borrow().iter().find(|x| Retained::as_ptr(&x.download) == key).map(|x| (x.node, x.emit)));
            let Some((node, emit)) = node else { done.call((std::ptr::null_mut(),)); return; };
            let url: Option<Retained<NSURL>> = unsafe { msg_send![response, URL] };
            let mime: Option<Retained<NSString>> = unsafe { msg_send![response, MIMEType] };
            let size: i64 = unsafe { msg_send![response, expectedContentLength] };
            let (name, done) = (name.to_string(), done.copy());
            ask(emit, node, 0, Pending::Destination(done), |id| {
                DOWNLOADS.with(|d| {
                    if let Some(download) = d.borrow_mut().iter_mut().find(|x| Retained::as_ptr(&x.download) == key) {
                        download.id = id;
                    }
                });
                super::browsing::PromptReport::Download {
                    id,
                    url: url.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default(),
                    name,
                    mime: mime.map(|m| m.to_string()).unwrap_or_default(),
                    size,
                }
            });
        }

        #[unsafe(method(downloadDidFinish:))]
        fn finished(&self, download: &AnyObject) {
            let key = download as *const AnyObject;
            let location = DOWNLOADS
                .with(|d| d.borrow().iter().find(|x| Retained::as_ptr(&x.download) == key).and_then(|x| x.path.clone()))
                .unwrap_or_default();
            // The size as it ended: the timer may not have run since the last bytes arrived.
            let id = DOWNLOADS.with(|d| d.borrow().iter().find(|x| Retained::as_ptr(&x.download) == key).map(|x| x.id));
            let progress: Retained<AnyObject> = unsafe { msg_send![download, progress] };
            let received: i64 = unsafe { msg_send![&progress, completedUnitCount] };
            if let Some(id) = id.filter(|id| *id != 0) {
                let received = received.max(0) as u64;
                super::browsing::download_event(id, super::DownloadEvent::Progress { received, total: Some(received).filter(|r| *r > 0) });
            }
            end_download(download, super::DownloadEvent::Finished { location });
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn failed(&self, download: &AnyObject, error: &NSError, _resume: *mut AnyObject) {
            let text: Retained<NSString> = unsafe { msg_send![error, localizedDescription] };
            end_download(download, super::DownloadEvent::Failed(text.to_string()));
        }
    }
);

/// WebKit turned a navigation into a download: keep it, and be its delegate.
pub(crate) fn adopt_download(emit: Emit, download: &AnyObject, node: NodeId) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let delegate = DOWNLOAD_DELEGATE.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                let this = DownloadDelegate::alloc(mtm).set_ivars(());
                unsafe { msg_send![super(this), init] }
            })
            .clone()
    });
    let _: () = unsafe { msg_send![download, setDelegate: &*delegate] };
    // SAFETY: a live WKDownload handed to its delegate callback.
    let download = unsafe { Retained::retain(download as *const AnyObject as *mut AnyObject) };
    if let Some(download) = download {
        DOWNLOADS.with(|d| {
            d.borrow_mut().push(Download {
                id: 0,
                download,
                node,
                emit,
                path: None,
            })
        });
    }
}

/// A download ended: tell its callback, forget it, and remove what a failure left behind.
fn end_download(download: &AnyObject, event: super::DownloadEvent) {
    let key = download as *const AnyObject;
    let ended = DOWNLOADS.with(|d| {
        let mut all = d.borrow_mut();
        all.iter()
            .position(|x| Retained::as_ptr(&x.download) == key)
            .map(|i| all.remove(i))
    });
    let Some(ended) = ended else { return };
    if let (super::DownloadEvent::Failed(_), Some(path)) = (&event, &ended.path) {
        let _ = std::fs::remove_file(path);
    }
    // A download that ends before WebKit asked for its destination was never reported.
    if ended.id != 0 {
        if let Some((_, pending)) = PROMPTS.with(|p| p.borrow_mut().remove(&ended.id)) {
            pending.finish(ended.id, &super::browsing::Answer::default());
        }
        super::browsing::download_event(ended.id, event);
    }
}

/// Report each running download's progress five times a second while any runs. NSProgress
/// changes on WebKit's own threads; reading it from a main-thread timer keeps every callback on
/// the main thread.
fn watch_downloads() {
    if DOWNLOAD_TIMER.with(|t| t.borrow().is_some()) {
        return;
    }
    let tick = RcBlock::new(|_timer: *mut AnyObject| {
        let running: Vec<(u64, Retained<AnyObject>)> = DOWNLOADS.with(|d| {
            d.borrow()
                .iter()
                .filter(|x| x.path.is_some())
                .map(|x| (x.id, x.download.clone()))
                .collect()
        });
        if running.is_empty() {
            if let Some(timer) = DOWNLOAD_TIMER.with(|t| t.borrow_mut().take()) {
                let _: () = unsafe { msg_send![&timer, invalidate] };
            }
            return;
        }
        for (id, download) in running {
            let progress: Retained<AnyObject> = unsafe { msg_send![&download, progress] };
            let received: i64 = unsafe { msg_send![&progress, completedUnitCount] };
            let total: i64 = unsafe { msg_send![&progress, totalUnitCount] };
            super::browsing::download_event(
                id,
                super::DownloadEvent::Progress {
                    received: received.max(0) as u64,
                    total: u64::try_from(total).ok().filter(|total| *total > 0),
                },
            );
        }
    });
    let timer: Retained<AnyObject> = unsafe {
        msg_send![objc2::class!(NSTimer), scheduledTimerWithTimeInterval: 0.2f64, repeats: true, block: &*tick]
    };
    DOWNLOAD_TIMER.with(|t| *t.borrow_mut() = Some(timer));
}

/// `cancel_download`.
pub(crate) fn cancel_download(id: u64) {
    let download = DOWNLOADS.with(|d| {
        d.borrow()
            .iter()
            .find(|x| x.id == id)
            .map(|x| x.download.clone())
    });
    let Some(download) = download else { return };
    // WebKit does not call the delegate for a download it was asked to cancel.
    let ended = download.clone();
    let done = RcBlock::new(move |_resume: *mut AnyObject| {
        end_download(&ended, super::DownloadEvent::Failed("canceled".into()));
    });
    let _: () = unsafe { msg_send![&download, cancel: &*done] };
}

/// Whether this system has WKDownload (iOS 14.5, macOS 11.3).
pub(crate) fn downloads_available() -> bool {
    objc2::runtime::AnyClass::get(c"WKDownload").is_some()
}

/// `runJavaScript…Panel`: report a script dialog and keep its completion handler until the app
/// answers. Without these delegate methods WebKit answers every dialog itself: alert as if
/// dismissed, confirm false, prompt null.
pub(crate) fn dialog(
    at: Asking,
    kind: DialogKind,
    message: &NSString,
    fallback: Option<String>,
    frame: &AnyObject,
    pending: Pending,
) {
    let (message, url) = (message.to_string(), frame_url(frame));
    ask(at.emit, at.node, at.view, pending, |id| {
        super::browsing::PromptReport::Dialog {
            id,
            dialog: kind,
            message,
            default_text: fallback.unwrap_or_default(),
            url,
        }
    });
}

/// `webView:didReceiveAuthenticationChallenge:completionHandler:`: a server asking to be
/// trusted or to be signed in to. A host the user chose to visit despite its certificate
/// (`trust_host`) is trusted; a name-and-password challenge goes to the app
/// (`WebView::on_auth`, when `auth`); everything else is the system's to decide.
pub(crate) fn challenge(
    at: Asking,
    auth: bool,
    challenge: &AnyObject,
    done: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>,
) {
    // WebKit hands over a wrapper that forwards to the NSURLAuthenticationChallenge inside it;
    // `msg_send!` checks a method against the receiver's own class, so the messages go to the
    // challenge itself.
    let inner: Option<Retained<AnyObject>> =
        unsafe { msg_send![challenge, forwardingTargetForSelector: objc2::sel!(protectionSpace)] };
    let Some(challenge) = inner else {
        done.call((CHALLENGE_DEFAULT, std::ptr::null_mut()));
        return;
    };
    let space: Retained<AnyObject> = unsafe { msg_send![&challenge, protectionSpace] };
    let method: Retained<NSString> = unsafe { msg_send![&space, authenticationMethod] };
    let host: Retained<NSString> = unsafe { msg_send![&space, host] };
    let (method, host) = (method.to_string(), host.to_string());
    if method == "NSURLAuthenticationMethodServerTrust" {
        let trust: *mut SecTrust = unsafe { msg_send![&space, serverTrust] };
        if !trust.is_null() && super::browsing::host_trusted(&host) {
            // SAFETY: `trust` is the challenge's live SecTrust; the exceptions data is released
            // after the trust object has copied it.
            unsafe {
                let exceptions = SecTrustCopyExceptions(trust);
                if !exceptions.is_null() {
                    SecTrustSetExceptions(trust, exceptions);
                    CFRelease(exceptions);
                }
            }
            let credential: Retained<AnyObject> =
                unsafe { msg_send![objc2::class!(NSURLCredential), credentialForTrust: trust] };
            done.call((
                CHALLENGE_USE_CREDENTIAL,
                Retained::as_ptr(&credential) as *mut AnyObject,
            ));
        } else {
            done.call((CHALLENGE_DEFAULT, std::ptr::null_mut()));
        }
        return;
    }
    let password = matches!(
        method.as_str(),
        "NSURLAuthenticationMethodHTTPBasic"
            | "NSURLAuthenticationMethodHTTPDigest"
            | "NSURLAuthenticationMethodNTLM"
    );
    if !password || !auth {
        done.call((CHALLENGE_DEFAULT, std::ptr::null_mut()));
        return;
    }
    let realm: Option<Retained<NSString>> = unsafe { msg_send![&space, realm] };
    let proxy: bool = unsafe { msg_send![&space, isProxy] };
    let failures: isize = unsafe { msg_send![&challenge, previousFailureCount] };
    let realm = realm.map(|r| r.to_string()).unwrap_or_default();
    ask(
        at.emit,
        at.node,
        at.view,
        Pending::Auth(done.copy()),
        |id| super::browsing::PromptReport::Auth {
            id,
            host,
            realm,
            proxy,
            failures: failures.max(0) as u32,
        },
    );
}

/// `webView:didFail(Provisional)Navigation:withError:`: a load that failed before or after its
/// first byte (`WebView::on_load_error`).
pub(crate) fn navigation_failed(emit: Emit, node: NodeId, error: &NSError) {
    if let Some(notice) = load_error(error) {
        emit(node, super::browsing::notice_event(&notice));
    }
}

/// `webViewWebContentProcessDidTerminate:`: the page's process is gone
/// (`WebView::on_renderer_gone`). `true` when the app was told and rebuilds or reloads the
/// view; `false` when nobody listens and the caller reloads rather than leave it blank.
pub(crate) fn process_gone(at: Asking, listening: bool) -> bool {
    dismiss_prompts(at.view);
    if listening {
        let notice = super::browsing::Notice::RendererGone { crashed: true };
        (at.emit)(at.node, super::browsing::notice_event(&notice));
    }
    listening
}

/// `webView:decidePolicyForNavigationResponse:decisionHandler:`: a response WebKit cannot show,
/// or one the server marked as an attachment, becomes a download where the app takes them
/// (`WebView::on_download`, when `downloads`); every other response loads as it would without
/// the delegate method. A file from the app's own resource provider is not WebKit's to
/// download: it stops there, and this module saves it once the app says where.
pub(crate) fn response_policy(
    at: Asking,
    downloads: bool,
    provider: u64,
    response: &AnyObject,
) -> isize {
    if !downloads || !downloads_available() {
        return POLICY_ALLOW;
    }
    let shows: bool = unsafe { msg_send![response, canShowMIMEType] };
    let inner: Retained<AnyObject> = unsafe { msg_send![response, response] };
    let http: bool = unsafe { msg_send![&inner, isKindOfClass: objc2::class!(NSHTTPURLResponse)] };
    let attachment = http && {
        let disposition: Option<Retained<NSString>> = unsafe {
            msg_send![&inner, valueForHTTPHeaderField: &*NSString::from_str("Content-Disposition")]
        };
        disposition.is_some_and(|d| {
            d.to_string()
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
        })
    };
    if shows && !attachment {
        return POLICY_ALLOW;
    }
    let url: Option<Retained<NSURL>> = unsafe { msg_send![&inner, URL] };
    let url = url
        .and_then(|u| u.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if provider == 0 || !url.starts_with("day-resource:") {
        return POLICY_DOWNLOAD;
    }
    let mime: Option<Retained<NSString>> = unsafe { msg_send![&inner, MIMEType] };
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let pending = Pending::ProviderFile {
        provider,
        url: url.clone(),
    };
    ask(at.emit, at.node, at.view, pending, |id| {
        super::browsing::PromptReport::Download {
            id,
            url,
            name,
            mime: mime.map(|m| m.to_string()).unwrap_or_default(),
            size: -1,
        }
    });
    POLICY_CANCEL
}

/// Whether a navigation action is a link marked `download`, which the app takes as a file
/// (`WebView::on_download`, when `downloads`).
pub(crate) fn action_downloads(downloads: bool, action: &AnyObject) -> bool {
    downloads && downloads_available() && unsafe { msg_send![action, shouldPerformDownload] }
}

/// `WebView::user_agent` on a configuration, before its view exists. The platform browser's
/// string is Safari's: WebKit writes the device part itself and takes the rest as the
/// application name, so the view keeps answering desktop mode and iPad's desktop-class
/// browsing the way Safari does.
pub(crate) fn configure_user_agent(config: &AnyObject, user_agent: &super::UserAgent) {
    if *user_agent != super::UserAgent::Browser {
        return;
    }
    let info: Retained<AnyObject> = unsafe { msg_send![objc2::class!(NSProcessInfo), processInfo] };
    let version: objc2_foundation::NSOperatingSystemVersion =
        unsafe { msg_send![&info, operatingSystemVersion] };
    let version = format!("{}.{}", version.majorVersion, version.minorVersion);
    let name = super::browsing::safari_application_name(&version, cfg!(target_os = "ios"));
    let _: () =
        unsafe { msg_send![config, setApplicationNameForUserAgent: &*NSString::from_str(&name)] };
}

/// `WebView::user_agent` on a view just created: a custom string replaces WebKit's whole.
pub(crate) fn apply_user_agent(web: &AnyObject, user_agent: &super::UserAgent) {
    if let super::UserAgent::Custom(text) = user_agent
        && !text.trim().is_empty()
    {
        let _: () =
            unsafe { msg_send![web, setCustomUserAgent: &*NSString::from_str(text.trim())] };
    }
}
