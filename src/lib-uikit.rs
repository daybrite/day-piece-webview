// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// UIKit: WKWebView (WebKit), the same control as AppKit, but a UIView subclass on iOS.
// objc2-web-kit 0.3 only generates the macOS (NSView) WKWebView binding, so here we hand-roll the
// iOS class via `extern_class!` + `msg_send!`. A navigation delegate reports the committed URL back
// through `Event::custom("webview:url", …)`; retained in a thread_local (WKWebView keeps the
// delegate weakly).
// ---------------------------------------------------------------------------

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use block2::RcBlock;
use day_spec::NodeId;
use day_toolkit_uikit::Uikit;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_class, msg_send};
use objc2_foundation::{NSError, NSString, NSURL, NSURLRequest};
use objc2_ui_kit::{UIResponder, UIView};

// WKWebView lives in WebKit.framework. objc2-web-kit force-links it on macOS but only binds the
// AppKit variant, so on iOS we hand-roll the class below. WebKit must be linked or
// `objc_getClass("WKWebView")` returns nil and `alloc` aborts (SIGABRT); it is declared via this
// crate's `[package.metadata.day.ios].frameworks = ["WebKit"]`, which the generated DayPieces
// SwiftPM package links into the app, so neither a runtime `dlopen` nor an xcodeproj edit is
// needed.

// The iOS WKWebView (a UIView subclass). We only need a handful of methods, called via msg_send!.
extern_class!(
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct WKWebView;
);

struct NavIvars {
    /// Mutable: a session-retained view outlives the node that first realized it, and must report
    /// to whichever node is currently showing it.
    node: Cell<NodeId>,
    /// Inline mode (docs/webview.md): the `file://` URL prefix of the bundled site's root.
    /// A main-frame navigation outside it is cancelled and reported (`LINK_REPORT`); `None`
    /// permits ordinary document navigation; `external_links` separately intercepts clicks.
    inline_base: RefCell<Option<String>>,
    external_links: Cell<bool>,
    tabs: RefCell<Option<TabLinkLabels>>,
    /// The last page-state report sent (see the AppKit arm's `report_state`).
    last_state: RefCell<String>,
    /// The node showing the view binds its scroll position (`WebProps::scroll_reports`).
    scroll_reports: Cell<bool>,
    /// `WebView::desktop_mode` and `WebView::javascript`, applied to each navigation's
    /// `WKWebpagePreferences` as the delegate decides it.
    desktop_mode: Cell<bool>,
    javascript: Cell<bool>,
    /// Which browser duties the app takes on (`WebProps::browse`).
    browse: Cell<BrowseProps>,
    /// The view's resource provider (`0` for none), which serves the app's own namespace.
    provider: Cell<u64>,
}

use super::browsing_apple::{self as duties, POLICY_ALLOW, POLICY_CANCEL, POLICY_DOWNLOAD};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayWebNavUIKit"]
    #[ivars = NavIvars]
    struct WebNav;

    unsafe impl NSObjectProtocol for WebNav {}

    impl WebNav {
        // KVO on the page-state keys (`storage_apple::observe_state`).
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(&self, _key: Option<&NSString>, object: Option<&AnyObject>, _change: Option<&AnyObject>, _context: *mut std::ffi::c_void) {
            if let Some(web) = object {
                self.report_state(web);
            }
        }
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(&self, _web: &WKWebView, _configuration: &AnyObject, action: &AnyObject, _features: &AnyObject) -> *mut AnyObject {
            if self.ivars().tabs.borrow().is_some() {
                let req: Retained<NSURLRequest> = unsafe { msg_send![action, request] };
                if let Some(url)=req.URL().and_then(|url|url.absoluteString()) {
                    day_toolkit_uikit::emit(self.ivars().node.get(),new_tab_event(url.to_string(),false));
                }
            }
            std::ptr::null_mut()
        }
        #[unsafe(method(webView:contextMenuConfigurationForElement:completionHandler:))]
        fn link_menu(&self, _web: &WKWebView, element: &AnyObject, done: &block2::DynBlock<dyn Fn(*mut AnyObject)>) {
            let url: Option<Retained<NSURL>> = unsafe { msg_send![element, linkURL] };
            let labels=self.ivars().tabs.borrow().clone();
            let (Some(url),Some(labels))=(url.and_then(|u|u.absoluteString()),labels) else { done.call((std::ptr::null_mut(),)); return; };
            let url=url.to_string(); let id=self.ivars().node.get();
            let provider=RcBlock::new(move |suggested: *mut AnyObject| -> *mut AnyObject {
                let mut actions:Vec<Retained<AnyObject>>=Vec::new();
                for (label,background) in [(&labels.foreground,false),(&labels.background,true)] {
                    let target=url.clone();
                    let handler=RcBlock::new(move |_action:*mut AnyObject| { day_toolkit_uikit::emit(id,new_tab_event(target.clone(),background)); });
                    let action: Retained<AnyObject> = unsafe { msg_send![objc2::class!(UIAction), actionWithTitle: &*NSString::from_str(label), image: std::ptr::null::<AnyObject>(), identifier: std::ptr::null::<AnyObject>(), handler: &*handler] };
                    actions.push(action);
                }
                if !suggested.is_null() {
                    let count:usize=unsafe { msg_send![suggested,count] };
                    for i in 0..count { let action:Retained<AnyObject>=unsafe { msg_send![suggested,objectAtIndex:i] }; actions.push(action); }
                }
                let children=objc2_foundation::NSArray::from_retained_slice(&actions);
                let menu:Retained<AnyObject>=unsafe { msg_send![objc2::class!(UIMenu), menuWithTitle: &*NSString::new(), children: &*children] };
                Retained::autorelease_ptr(menu)
            });
            let config:Retained<AnyObject>=unsafe { msg_send![objc2::class!(UIContextMenuConfiguration), configurationWithIdentifier:std::ptr::null::<AnyObject>(), previewProvider:std::ptr::null::<AnyObject>(), actionProvider:&*provider] };
            done.call((Retained::as_ptr(&config) as *mut AnyObject,));
        }
        // UIScrollViewDelegate, as the web view's scroll view's external delegate (WebKit keeps its
        // own and forwards to this one). The adjusted inset changes when WebKit learns whether the
        // page covers the safe areas; see `refresh_viewport`.
        // Scroll reports (`WebView::scroll_state`): every frame of scrolling, plus the moments
        // a drag starts and ends, so the binding knows whether a finger is on the page.
        #[unsafe(method(scrollViewDidScroll:))]
        fn did_scroll(&self, scroll: &UIView) {
            self.report_scroll(scroll);
        }
        #[unsafe(method(scrollViewWillBeginDragging:))]
        fn will_begin_dragging(&self, scroll: &UIView) {
            self.report_scroll(scroll);
        }
        #[unsafe(method(scrollViewDidEndDragging:willDecelerate:))]
        fn did_end_dragging(&self, scroll: &UIView, _decelerate: bool) {
            self.report_scroll(scroll);
        }
        #[unsafe(method(scrollViewDidChangeAdjustedContentInset:))]
        fn adjusted_inset_changed(&self, scroll: &UIView) {
            // UIScrollViewContentInsetAdjustmentNever: WebKit's choice for a page that covers.
            let behavior: isize = unsafe { msg_send![scroll, contentInsetAdjustmentBehavior] };
            let Some(parent) = scroll.superview() else { return };
            if let (2, Some(web)) = (behavior, parent.downcast_ref::<WKWebView>()) {
                refresh_viewport(web);
            }
        }

        // WKNavigationDelegate's webView:didFinishNavigation:, which WKWebView calls on the
        // object we set as its navigationDelegate; responding to the selector is all that's
        // required.
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: *mut AnyObject) {
            if let Some(url) = current_url(web_view) {
                day_toolkit_uikit::emit(self.ivars().node.get(), Event::custom("webview:url", url));
            }
        }

        // The browser duties (docs/webview.md), each forwarded to `browsing_apple` with this
        // view's node: script dialogs, sign-in and certificates, failed loads, a lost page
        // process, and downloads.
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_alert(&self, web: &WKWebView, message: &NSString, frame: &AnyObject, done: &block2::DynBlock<dyn Fn()>) {
            duties::dialog(self.asking(web), DialogKind::Alert, message, None, frame, duties::Pending::Alert(done.copy()));
        }
        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_confirm(&self, web: &WKWebView, message: &NSString, frame: &AnyObject, done: &block2::DynBlock<dyn Fn(objc2::runtime::Bool)>) {
            duties::dialog(self.asking(web), DialogKind::Confirm, message, None, frame, duties::Pending::Confirm(done.copy()));
        }
        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn js_prompt(&self, web: &WKWebView, message: &NSString, fallback: Option<&NSString>, frame: &AnyObject, done: &block2::DynBlock<dyn Fn(*mut NSString)>) {
            let fallback = fallback.map(|f| f.to_string()).unwrap_or_default();
            duties::dialog(self.asking(web), DialogKind::Prompt, message, Some(fallback.clone()), frame, duties::Pending::Prompt(done.copy(), fallback));
        }
        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn challenge(&self, web: &WKWebView, challenge: &AnyObject, done: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>) {
            duties::challenge(self.asking(web), self.ivars().browse.get().auth, challenge, done);
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_provisional(&self, _web: &WKWebView, _navigation: *mut AnyObject, error: &NSError) {
            duties::navigation_failed(self.emit(), self.ivars().node.get(), error);
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_navigation(&self, _web: &WKWebView, _navigation: *mut AnyObject, error: &NSError) {
            duties::navigation_failed(self.emit(), self.ivars().node.get(), error);
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_gone(&self, web: &WKWebView) {
            let listening = self.ivars().browse.get().renderer_gone;
            if !duties::process_gone(self.asking(web), listening) {
                let _: *mut AnyObject = unsafe { msg_send![web, reload] };
            }
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(&self, web: &WKWebView, response: &AnyObject, handler: &block2::DynBlock<dyn Fn(isize)>) {
            let policy = duties::response_policy(self.asking(web),
                self.ivars().browse.get().downloads, self.ivars().provider.get(), response);
            handler.call((policy,));
        }
        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_became_download(&self, _web: &WKWebView, _response: &AnyObject, download: &AnyObject) {
            duties::adopt_download(self.emit(), download, self.ivars().node.get());
        }
        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_became_download(&self, _web: &WKWebView, _action: &AnyObject, download: &AnyObject) {
            duties::adopt_download(self.emit(), download, self.ivars().node.get());
        }

        // The preferences form of the policy callback, which WebKit calls instead of the
        // two-argument one: the decision is `policy`'s, and the navigation's preferences carry
        // the view's desktop-mode and script settings (`storage_apple::webpage_preferences`).
        #[unsafe(method(webView:decidePolicyForNavigationAction:preferences:decisionHandler:))]
        fn decide_policy(
            &self,
            web_view: &WKWebView,
            action: &AnyObject,
            preferences: &AnyObject,
            handler: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>,
        ) {
            let mut policy = self.policy(web_view, action);
            // A link marked `download`: the app takes the file instead of the view.
            if policy == POLICY_ALLOW && duties::action_downloads(self.ivars().browse.get().downloads, action) {
                policy = POLICY_DOWNLOAD;
            }
            super::storage_apple::webpage_preferences(
                preferences,
                self.ivars().desktop_mode.get(),
                self.ivars().javascript.get(),
            );
            handler.call((policy, preferences as *const AnyObject as *mut AnyObject));
        }
    }
);

impl WebNav {
    /// This toolkit's event sender, for the shared duties.
    fn emit(&self) -> duties::Emit {
        day_toolkit_uikit::emit
    }

    /// This view, as the shared duties address it.
    fn asking(&self, web: &WKWebView) -> duties::Asking {
        duties::Asking {
            emit: self.emit(),
            node: self.ivars().node.get(),
            view: web as *const WKWebView as usize,
        }
    }

    /// Inline mode's link policy, the same contract as the AppKit arm's `policy`: a main-frame
    /// navigation leaving the bundled site is canceled and reported; the piece runs the app's
    /// `LinkPolicy` (events are enqueue-only, the decision can't come back through the delegate
    /// callback). Raw msg_send shapes, like the rest of this hand-rolled arm.
    fn policy(&self, web_view: &WKWebView, action: &AnyObject) -> isize {
        let frame: *mut AnyObject = unsafe { msg_send![action, targetFrame] };
        let subframe = !frame.is_null() && !unsafe { msg_send![&*frame, isMainFrame] };
        let req: Retained<NSURLRequest> = unsafe { msg_send![action, request] };
        let url = req.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
        if self.ivars().tabs.borrow().is_some() && let Some(request)=tab_scheme_request(&url) {
            day_toolkit_uikit::emit(self.ivars().node.get(), new_tab_event(request.url,request.background));
            return POLICY_CANCEL;
        }
        let kind: isize = unsafe { msg_send![action, navigationType] };
        let activated = kind == 0; // WKNavigationTypeLinkActivated
        if self.ivars().tabs.borrow().is_some() && frame.is_null() {
            day_toolkit_uikit::emit(self.ivars().node.get(),new_tab_event(url,false));
            return POLICY_CANCEL;
        }
        // An address no engine loads (mailto:, tel:, an app's scheme) is another program's:
        // cancel and report it from any page. A bundled site reports every address outside
        // it below.
        if self.ivars().inline_base.borrow().is_none() && !super::browsing::engine_scheme(&url) {
            day_toolkit_uikit::emit(self.ivars().node.get(), Event::Custom {
                tag: "webview:link", num: super::LINK_REPORT, text: url,
            });
            return POLICY_CANCEL;
        }
        let external = match &*self.ivars().inline_base.borrow() {
            Some(base) => !subframe && !url.starts_with(base.as_str()) && url != "about:blank",
            None => self.ivars().external_links.get() && super::external_document_link(
                current_url(web_view).as_deref(), &url, activated, frame.is_null(), subframe),
        };
        if external {
            day_toolkit_uikit::emit(self.ivars().node.get(), Event::Custom {
                tag: "webview:link", num: super::LINK_REPORT, text: url,
            });
            POLICY_CANCEL
        } else {
            POLICY_ALLOW
        }
    }

    /// Send where the page is scrolled, when the node showing the view asked for it. The offset
    /// counts from the top of the content, past the inset UIKit adds under the bars.
    fn report_scroll(&self, scroll: &UIView) {
        if !self.ivars().scroll_reports.get() {
            return;
        }
        // SAFETY: UIScrollView's geometry getters, on the main thread its delegate is called on.
        let (offset, size, inset, dragging) = unsafe {
            let offset: objc2_core_foundation::CGPoint = msg_send![scroll, contentOffset];
            let size: objc2_core_foundation::CGSize = msg_send![scroll, contentSize];
            let inset: objc2_ui_kit::UIEdgeInsets = msg_send![scroll, adjustedContentInset];
            let dragging: bool = msg_send![scroll, isDragging];
            (offset, size, inset, dragging)
        };
        let viewport = scroll.bounds().size.height - inset.top - inset.bottom;
        let state = super::ScrollState {
            offset: offset.y + inset.top,
            content_height: size.height,
            viewport_height: viewport.max(0.0),
            dragging,
        };
        day_toolkit_uikit::emit(self.ivars().node.get(), super::scroll_state_event(&state));
    }

    /// Send the web view's current page state unless it equals the last one sent.
    fn report_state(&self, web: &AnyObject) {
        let text = super::encode_page_state(&super::storage_apple::page_state(web));
        if *self.ivars().last_state.borrow() == text {
            return;
        }
        *self.ivars().last_state.borrow_mut() = text.clone();
        day_toolkit_uikit::emit(
            self.ivars().node.get(),
            Event::Custom {
                tag: "webview:state",
                num: super::PAGE_STATE_REPORT,
                text,
            },
        );
    }

    fn new(mtm: MainThreadMarker, node: NodeId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NavIvars {
            node: Cell::new(node),
            inline_base: RefCell::new(None),
            external_links: Cell::new(false),
            tabs: RefCell::new(None),
            last_state: RefCell::new(String::new()),
            scroll_reports: Cell::new(false),
            desktop_mode: Cell::new(false),
            javascript: Cell::new(true),
            browse: Cell::new(BrowseProps::default()),
            provider: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }
}

day_core::tls_group! {
    // Keep each navigation delegate alive as long as its web view (delegate ref is weak).
    static DELEGATES: RefCell<HashMap<usize, Retained<WebNav>>> = RefCell::new(HashMap::new());
    // Session id -> the retained web view. Day drops its own reference when the page is navigated
    // away from, which on UIKit only detaches (`removeFromSuperview`); this reference is what keeps
    // the engine (and so the loaded page and its JavaScript context) alive until the app returns.
    static SESSIONS: RefCell<HashMap<u64, Retained<UIView>>> = RefCell::new(HashMap::new());

}

/// Make WebKit size the page's viewport again. WebKit first insets its scroll view by the safe
/// areas; once it learns that the page declares `viewport-fit=cover`, it drops the inset but, on
/// iOS 17 and 18 (iOS 27 resizes on its own), keeps the viewport it sized with it. A full-screen
/// view on a phone with a home indicator then lays the page out 84 pt short, and anything fixed to
/// the bottom floats above the edge.
/// WebKit sizes the viewport again only when the view's size changes, so the view takes a point
/// off its height and gives it back.
fn refresh_viewport(web: &WKWebView) {
    // The resize itself changes the adjusted inset, which calls back into here.
    thread_local!(static REFRESHING: Cell<bool> = const { Cell::new(false) });
    let frame = web.frame();
    if frame.size.height < 1.0 || REFRESHING.replace(true) {
        return;
    }
    let mut shorter = frame;
    shorter.size.height -= 1.0;
    web.setFrame(shorter);
    web.setFrame(frame);
    REFRESHING.set(false);
}

fn current_url(web: &WKWebView) -> Option<String> {
    let url: Option<Retained<NSURL>> = unsafe { msg_send![web, URL] };
    let s = url?.absoluteString()?;
    Some(s.to_string())
}

fn load_url(web: &WKWebView, url: &str) {
    let ns = NSString::from_str(url);
    let Some(nsurl) = NSURL::URLWithString(&ns) else {
        return;
    };
    let req = NSURLRequest::requestWithURL(&nsurl);
    let _: *mut AnyObject = unsafe { msg_send![web, loadRequest: &*req] };
}

/// A file URL for `page` inside `dir`, where `page` may carry a query or fragment
/// (`"player.html?src=hello"`). `fileURLWithPath:` would percent-encode the `?` into the file
/// name, so the path is built first and the tail re-attached as a relative reference against it,
/// which is the resolution a browser performs for the same string.
fn page_url(dir: &std::path::Path, page: &str) -> objc2::rc::Retained<NSURL> {
    let (path, tail) = super::split_page(page);
    let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.join(path).display().to_string()));
    if tail.is_empty() {
        return url;
    }
    NSURL::URLWithString_relativeToURL(&NSString::from_str(tail), Some(&url))
        .and_then(|u| u.absoluteURL())
        .unwrap_or(url)
}

/// A new view's first page, held until its content rule lists are attached.
type FirstLoad = Box<dyn FnOnce(&WKWebView)>;

fn make(_backend: &mut Uikit, p: &WebProps, id: NodeId) -> Retained<UIView> {
    // A session already holding a view: re-attach it rather than build a new one. Only the node
    // changes: point the delegate at the node now showing it, and do not reload, since the whole
    // purpose is to come back to the page as it was left.
    if p.session != 0
        && let Some(view) = SESSIONS.with(|m| m.borrow().get(&p.session).cloned())
    {
        let key = (&*view as *const UIView) as usize;
        DELEGATES.with(|m| {
            if let Some(nav) = m.borrow().get(&key) {
                nav.ivars().node.set(id);
                nav.ivars().external_links.set(p.external_links);
                nav.ivars().scroll_reports.set(p.scroll_reports);
                nav.ivars().browse.set(p.browse);
                *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
                // The node's settings: zoom at once, the rest from the next navigation on.
                nav.ivars().desktop_mode.set(p.settings.desktop_mode);
                nav.ivars().javascript.set(p.settings.javascript);
                super::storage_apple::set_page_zoom(&view, p.settings.zoom);
                super::content_apple::retarget(key, id, day_toolkit_uikit::emit);
                // The node's own rule lists (its scripts and handlers stay the view's first).
                super::content_apple::update(key, &WebPatch::ContentRules(p.content.rules.clone()));
                // The node now showing the page has heard nothing yet. It is told once its
                // handler exists (`WebPatch::ReportState`): a report from here would be lost.
                nav.ivars().last_state.borrow_mut().clear();
            }
        });
        return view;
    }

    let mtm = MainThreadMarker::new().unwrap();
    let config =
        super::resources_apple::configuration(p.resources.as_ref().map(ResourceProvider::id), mtm);
    super::storage_apple::configure(&config, &p.profile);
    duties::configure_user_agent(&config, &p.user_agent);
    if p.tabs.is_some() {
        unsafe {
            let script: *mut AnyObject = msg_send![objc2::class!(WKUserScript), alloc];
            let script: *mut AnyObject = msg_send![script, initWithSource: &*NSString::from_str(TAB_MODIFIER_SCRIPT), injectionTime: 0isize, forMainFrameOnly: false];
            let script = Retained::from_raw(script).expect("WKUserScript init");
            let controller: Retained<AnyObject> = msg_send![&*config, userContentController];
            let _: () = msg_send![&*controller,addUserScript:&*script];
        }
    }

    let web: Retained<WKWebView> = unsafe {
        msg_send![WKWebView::alloc(mtm), initWithFrame: objc2_foundation::NSRect::ZERO, configuration: &*config]
    };
    duties::apply_user_agent(&web, &p.user_agent);
    if p.transparent {
        // A non-opaque view with a clear background, and the same for the scroll view the page
        // sits in, which paints its own color under an overscroll.
        unsafe {
            let clear: *mut AnyObject = msg_send![objc2::class!(UIColor), clearColor];
            let _: () = msg_send![&web, setOpaque: false];
            let _: () = msg_send![&web, setBackgroundColor: clear];
            let scroll: *mut AnyObject = msg_send![&web, scrollView];
            let _: () = msg_send![scroll, setBackgroundColor: clear];
        }
    }
    let nav = WebNav::new(mtm, id);
    unsafe {
        let _: () = msg_send![&*web, setUIDelegate: &*nav];
    }
    nav.ivars().external_links.set(p.external_links);
    nav.ivars().scroll_reports.set(p.scroll_reports);
    nav.ivars().browse.set(p.browse);
    nav.ivars().provider.set(p.resources.as_ref().map(ResourceProvider::id).unwrap_or(0));
    if p.browse.swipe_navigation {
        // Safari's edge swipes through the view's own history (`WebView::swipe_navigation`).
        let _: () = unsafe { msg_send![&*web, setAllowsBackForwardNavigationGestures: true] };
    }
    *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
    // Set before the first load, whose policy decision already reads them.
    nav.ivars().desktop_mode.set(p.settings.desktop_mode);
    nav.ivars().javascript.set(p.settings.javascript);
    if p.settings.zoom != 1.0 {
        super::storage_apple::set_page_zoom(&web, p.settings.zoom);
    }
    let _: () = unsafe { msg_send![&web, setNavigationDelegate: &*nav] };
    // Live page state (`WebView::page_state`); removed again in `release`.
    super::storage_apple::observe_state(&web, &nav);
    // The first page, held until the view's content rule lists are attached (see below).
    let mut first: Option<FirstLoad> = None;
    // SAFETY: main thread; the scroll view holds its delegate weakly, and DELEGATES keeps `nav`
    // alive as long as the web view.
    unsafe {
        let scroll: *mut AnyObject = msg_send![&web, scrollView];
        let _: () = msg_send![scroll, setDelegate: &*nav];
    }
    if !p.inline_root.is_empty() {
        // Inline mode (docs/webview.md): the assets tree is loose files in the app bundle, so
        // `loadFileURL:allowingReadAccessToURL:` is the whole load path; WebKit resolves the
        // page's relative references natively. Read access covers the site's own directory, or
        // the app's whole asset tree for a site that asked for it (`app_assets`); the page
        // starts inside the site either way, and policing stays on the site's URL prefix.
        if let Some(dir) = day_spec::resolve_asset_dir(&p.inline_root) {
            let site = NSURL::fileURLWithPath(&NSString::from_str(&dir.display().to_string()));
            let read = match p
                .inline_assets
                .then(|| day_spec::resolve_asset_dir(""))
                .flatten()
            {
                Some(assets) => {
                    NSURL::fileURLWithPath(&NSString::from_str(&assets.display().to_string()))
                }
                None => site.clone(),
            };
            let index = page_url(&dir, &p.inline_start);
            if let Some(base) = site.absoluteString() {
                *nav.ivars().inline_base.borrow_mut() = Some(base.to_string());
            }
            first = Some(Box::new(move |web: &WKWebView| {
                let _: *mut AnyObject =
                    unsafe { msg_send![web, loadFileURL: &*index, allowingReadAccessToURL: &*read] };
            }));
        } else {
            log::warn!(
                "day-piece-webview: inline site {:?} not found in the staged assets",
                p.inline_root
            );
        }
    } else if !p.url.is_empty() {
        // Only a provider's own site is policed; `WebView::resources` just adds the scheme.
        if let Some(provider) = p.resources.as_ref().filter(|_| p.resource_site) {
            *nav.ivars().inline_base.borrow_mut() = Some(provider.base_url());
        }
        let url = p.url.clone();
        first = Some(Box::new(move |web: &WKWebView| load_url(web, &url)));
    }
    // Scripts, message handlers and rule lists (`WebView::content_rules` and friends). The first
    // page waits for the rule lists, so it never loads unfiltered.
    let key = (&*web as *const WKWebView) as usize;
    let loader = web.clone();
    super::content_apple::attach(
        mtm,
        key,
        &config,
        &p.content,
        id,
        day_toolkit_uikit::emit,
        move || {
            if let Some(first) = first {
                first(&loader);
            }
        },
    );
    let view: Retained<UIView> = Retained::from(<WKWebView as AsRef<UIView>>::as_ref(&web));
    DELEGATES.with(|m| {
        m.borrow_mut()
            .insert((view.as_ref() as *const UIView) as usize, nav)
    });
    if p.session != 0 {
        SESSIONS.with(|m| m.borrow_mut().insert(p.session, view.clone()));
    }
    view
}

/// The node a realized view belongs to. `update` gets only the native handle, so the id comes back
/// from the navigation delegate retained alongside it.
fn node_of(view: &Retained<UIView>) -> Option<NodeId> {
    let key = (&**view as *const UIView) as usize;
    DELEGATES.with(|m| m.borrow().get(&key).map(|nav| nav.ivars().node.get()))
}

/// Same contract as the AppKit arm (see its `eval`): the front-end's wrapper makes the result
/// always a JS string, so the completion's `id` is an `NSString` and no JSON walk is needed.
fn eval(web: &WKWebView, node: NodeId, req: u64, script: &str) {
    if super::storage_apple::request(web, script, move |text| {
        day_toolkit_uikit::emit(
            node,
            Event::Custom {
                tag: "webview:eval",
                num: req as f64,
                text,
            },
        )
    }) {
        return;
    }
    let js = NSString::from_str(script);
    let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
        let payload = if !result.is_null() {
            // SAFETY: non-null result from WebKit; the wrapper guarantees an NSString.
            unsafe { &*result }
                .downcast_ref::<NSString>()
                .map(|s| s.to_string())
                .unwrap_or_else(|| engine_error("WebKitError", "non-string reply"))
        } else if !error.is_null() {
            // SAFETY: non-null NSError from WebKit.
            engine_error(
                "WebKitError",
                &unsafe { (*error).localizedDescription() }.to_string(),
            )
        } else {
            engine_error("WebKitError", "no result")
        };
        day_toolkit_uikit::emit(
            node,
            Event::Custom {
                tag: "webview:eval",
                num: req as f64,
                text: payload,
            },
        );
    });
    // SAFETY: main thread (a renderer duty); WebKit copies the block before returning.
    let _: () = unsafe { msg_send![web, evaluateJavaScript: &*js, completionHandler: &*handler] };
}

fn update(_backend: &mut Uikit, h: &Retained<UIView>, patch: &WebPatch) {
    let Some(web) = (**h).downcast_ref::<WKWebView>() else {
        return;
    };
    unsafe {
        match patch {
            WebPatch::Eval { req, script } => {
                if let Some(node) = node_of(h) {
                    eval(web, node, *req, script);
                }
            }
            WebPatch::ContentRules(_)
            | WebPatch::ScriptReply { .. }
            | WebPatch::MediaDecision { .. } => {
                super::content_apple::update((&**h as *const UIView) as usize, patch);
            }
            WebPatch::PromptAnswer { id, answer } => duties::answer_prompt(*id, answer),
            // While the first page waits for its rule lists, a navigation takes its place.
            WebPatch::Load(url) => {
                let (loader, url) = (objc2::Message::retain(web), url.clone());
                super::content_apple::navigate((&**h as *const UIView) as usize, move || {
                    load_url(&loader, &url)
                });
            }
            // Nothing has loaded while the first page waits for its rules; it loads when they do.
            WebPatch::Reload | WebPatch::ForceReload
                if super::content_apple::holding((&**h as *const UIView) as usize) => {}
            WebPatch::Back => {
                let _: *mut AnyObject = msg_send![web, goBack];
            }
            WebPatch::Forward => {
                let _: *mut AnyObject = msg_send![web, goForward];
            }
            WebPatch::Stop => {
                let _: () = msg_send![web, stopLoading];
            }
            WebPatch::ForceReload => {
                let _: *mut AnyObject = msg_send![web, reloadFromOrigin];
            }
            WebPatch::Reload => {
                let _: *mut AnyObject = msg_send![web, reload];
            }
            WebPatch::ReportState => {
                let key = (&**h as *const UIView) as usize;
                DELEGATES.with(|m| {
                    if let Some(nav) = m.borrow().get(&key) {
                        nav.ivars().last_state.borrow_mut().clear();
                        nav.report_state(web);
                    }
                });
            }
            WebPatch::Zoom(zoom) => super::storage_apple::set_page_zoom(web, *zoom),
            // Both settings reach WebKit through the next navigation's preferences, so the
            // page reloads to take them: the site answers the desktop request, or the page
            // comes back without (or with) its scripts.
            WebPatch::DesktopMode(on) | WebPatch::JavaScript(on) => {
                let key = (&**h as *const UIView) as usize;
                let changed = DELEGATES.with(|m| {
                    m.borrow().get(&key).is_some_and(|nav| {
                        let setting = if matches!(patch, WebPatch::DesktopMode(_)) {
                            &nav.ivars().desktop_mode
                        } else {
                            &nav.ivars().javascript
                        };
                        setting.replace(*on) != *on
                    })
                });
                if changed {
                    let _: *mut AnyObject = msg_send![web, reload];
                }
            }
        }
    }
}

/// Drop the retained navigation delegate when the view goes away.
///
/// Without this the map grows by one entry per realized web view, and, worse, its key is the
/// view's address, which the allocator reuses: a later view landing on a freed address would
/// inherit the dead node's id and misroute its events.
fn release(_backend: &mut Uikit, h: &Retained<UIView>) {
    let key = (&**h as *const UIView) as usize;
    // A session-retained view is not going away; day is only detaching it from the page being
    // torn down, and the next visit re-attaches it. Its delegate has to outlive this node too, or
    // the returning view would report navigations to nobody.
    let retained = SESSIONS.with(|m| {
        m.borrow()
            .values()
            .any(|v| (&**v as *const UIView) as usize == key)
    });
    if retained {
        return;
    }
    super::content_apple::release(key);
    // WebKit is still waiting on whatever this view asked and nobody answered.
    duties::dismiss_prompts(key);
    let nav = DELEGATES.with(|m| m.borrow_mut().remove(&key));
    if let (Some(nav), Some(web)) = (nav, (**h).downcast_ref::<WKWebView>()) {
        super::storage_apple::unobserve_state(web, &nav);
    }
}

day_pieces::renderer!(day_toolkit_uikit::RENDERERS, Uikit,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);
