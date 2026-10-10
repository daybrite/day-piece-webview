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
use day_uikit::Uikit;
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
}

/// WKNavigationActionPolicy, hand-rolled like the class itself: Cancel = 0, Allow = 1.
const POLICY_CANCEL: isize = 0;
const POLICY_ALLOW: isize = 1;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayWebNavUIKit"]
    #[ivars = NavIvars]
    struct WebNav;

    unsafe impl NSObjectProtocol for WebNav {}

    impl WebNav {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(&self, _web: &WKWebView, _configuration: &AnyObject, action: &AnyObject, _features: &AnyObject) -> *mut AnyObject {
            if self.ivars().tabs.borrow().is_some() {
                let req: Retained<NSURLRequest> = unsafe { msg_send![action, request] };
                if let Some(url)=req.URL().and_then(|url|url.absoluteString()) {
                    day_uikit::emit(self.ivars().node.get(),new_tab_event(url.to_string(),false));
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
                    let handler=RcBlock::new(move |_action:*mut AnyObject| { day_uikit::emit(id,new_tab_event(target.clone(),background)); });
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
        // WKNavigationDelegate's webView:didFinishNavigation:, which WKWebView calls on the
        // object we set as its navigationDelegate; responding to the selector is all that's
        // required.
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: *mut AnyObject) {
            if let Some(url) = current_url(web_view) {
                day_uikit::emit(self.ivars().node.get(), Event::custom("webview:url", url));
            }
        }

        // Inline mode's link policy, the same contract as the AppKit arm's `decide_policy`: a
        // main-frame navigation leaving the bundled site is canceled and reported; the piece
        // runs the app's `LinkPolicy` (events are enqueue-only, the decision can't come back
        // through this callback). Raw msg_send shapes, like the rest of this hand-rolled arm.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            web_view: &WKWebView,
            action: &AnyObject,
            handler: &block2::DynBlock<dyn Fn(isize)>,
        ) {
            let frame: *mut AnyObject = unsafe { msg_send![action, targetFrame] };
            let subframe = !frame.is_null() && !unsafe { msg_send![&*frame, isMainFrame] };
            let req: Retained<NSURLRequest> = unsafe { msg_send![action, request] };
            let url = req.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            if self.ivars().tabs.borrow().is_some() && let Some(request)=tab_scheme_request(&url) {
                day_uikit::emit(self.ivars().node.get(), new_tab_event(request.url,request.background));
                handler.call((POLICY_CANCEL,)); return;
            }
            let kind: isize = unsafe { msg_send![action, navigationType] };
            let activated = kind == 0; // WKNavigationTypeLinkActivated
            if self.ivars().tabs.borrow().is_some() && frame.is_null() {
                day_uikit::emit(self.ivars().node.get(),new_tab_event(url,false));
                handler.call((POLICY_CANCEL,)); return;
            }
            let external = match &*self.ivars().inline_base.borrow() {
                Some(base) => !subframe && !url.starts_with(base.as_str()) && url != "about:blank",
                None => self.ivars().external_links.get() && super::external_document_link(
                    current_url(web_view).as_deref(), &url, activated, frame.is_null(), subframe),
            };
            let policy = if external {
                day_uikit::emit(self.ivars().node.get(), Event::Custom {
                    tag: "webview:link", num: super::LINK_REPORT, text: url,
                });
                POLICY_CANCEL
            } else {
                POLICY_ALLOW
            };
            handler.call((policy,));
        }
    }
);

impl WebNav {
    fn new(mtm: MainThreadMarker, node: NodeId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NavIvars {
            node: Cell::new(node),
            inline_base: RefCell::new(None),
            external_links: Cell::new(false),
            tabs: RefCell::new(None),
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
                *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
            }
        });
        return view;
    }

    let mtm = MainThreadMarker::new().unwrap();
    let config =
        super::resources_apple::configuration(p.resources.as_ref().map(ResourceProvider::id), mtm);
    super::storage_apple::configure(&config, &p.profile);
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
    *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
    let _: () = unsafe { msg_send![&web, setNavigationDelegate: &*nav] };
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
            let _: *mut AnyObject =
                unsafe { msg_send![&web, loadFileURL: &*index, allowingReadAccessToURL: &*read] };
        } else {
            log::warn!(
                "day-piece-webview: inline site {:?} not found in the staged assets",
                p.inline_root
            );
        }
    } else if !p.url.is_empty() {
        if let Some(provider) = &p.resources {
            *nav.ivars().inline_base.borrow_mut() = Some(provider.base_url());
        }
        load_url(&web, &p.url);
    }
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
        day_uikit::emit(
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
        day_uikit::emit(
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
            WebPatch::Load(url) => load_url(web, url),
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
    DELEGATES.with(|m| {
        m.borrow_mut().remove(&key);
    });
}

day_pieces::renderer!(day_uikit::RENDERERS, Uikit,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);
