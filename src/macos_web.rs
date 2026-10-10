// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// AppKit: WKWebView (WebKit). A custom navigation delegate reports the committed URL back via
// `Event::custom("webview:url", …)` so a bound text field follows navigation. WKWebView keeps its
// navigationDelegate weakly, so we retain each delegate in a thread_local for the view's lifetime.
// ---------------------------------------------------------------------------

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use block2::RcBlock;

use day_spec::NodeId;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSMenu, NSMenuItem, NSResponder, NSView};
use objc2_foundation::{NSError, NSObject, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKWebView,
};

struct NavIvars {
    emit: fn(NodeId, Event),
    /// Mutable: a session-retained view outlives the node that first realized it, and must report
    /// to whichever node is currently showing it.
    node: Cell<NodeId>,
    /// Inline mode (docs/webview.md): the `file://` URL prefix of the bundled site's root.
    /// A main-frame navigation outside it is cancelled and reported (`LINK_REPORT`); `None`
    /// permits ordinary document navigation; `external_links` separately intercepts clicks.
    inline_base: RefCell<Option<String>>,
    external_links: Cell<bool>,
    tabs: RefCell<Option<TabLinkLabels>>,
    background: Cell<bool>,
    link_item: RefCell<Option<Retained<NSMenuItem>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayWebNav"]
    #[ivars = NavIvars]
    struct WebNav;

    unsafe impl NSObjectProtocol for WebNav {}

    impl WebNav {
        #[unsafe(method(openBackgroundTab:))]
        fn open_background(&self, _sender: Option<&AnyObject>) {
            let item = self.ivars().link_item.borrow().clone();
            if let Some(item) = item {
                self.ivars().background.set(true);
                unsafe { if let Some(action) = item.action() {
                    NSApplication::sharedApplication(self.mtm()).sendAction_to_from(action, item.target().as_deref(), Some(&item));
                } }
            }
        }
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(&self, _web: &WKWebView, _config: &AnyObject, action: &WKNavigationAction, _features: &AnyObject) -> *mut AnyObject {
            if self.ivars().tabs.borrow().is_some() {
                if let Some(url) = unsafe { action.request() }.URL().and_then(|u|u.absoluteString()) {
                    (self.ivars().emit)(self.ivars().node.get(), new_tab_event(url.to_string(), self.ivars().background.replace(false)));
                }
            }
            std::ptr::null_mut()
        }
    }

    unsafe impl WKNavigationDelegate for WebNav {
        // Fired when a navigation completes: reports the new URL back to the piece.
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Some(url) = current_url(web_view) {
                (self.ivars().emit)(self.ivars().node.get(), Event::custom("webview:url", url));
            }
        }

        // Inline mode's link policy (docs/webview.md): a main-frame navigation leaving the
        // bundled site is cancelled here and reported to the piece, which runs the app's
        // `LinkPolicy` (events are enqueue-only, so the decision cannot come back through this
        // callback; cancel-then-dispatch is the contract). Remote mode allows everything.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            web_view: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let frame = unsafe { action.targetFrame() };
            let subframe = frame.as_ref().is_some_and(|f| !unsafe { f.isMainFrame() });
            let url = unsafe { action.request() }
                .URL()
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let activated = unsafe { action.navigationType() }
                == objc2_web_kit::WKNavigationType::LinkActivated;
            let modifiers: usize = unsafe { msg_send![action, modifierFlags] };
            let button: isize = unsafe { msg_send![action, buttonNumber] };
            if self.ivars().tabs.borrow().is_some() && !subframe &&
                (frame.is_none() || (activated && (modifiers & (1 << 20) != 0 || button == 2))) {
                let background = self.ivars().background.replace(false) ||
                    (activated && (modifiers & (1 << 20) != 0 || button == 2) && modifiers & (1 << 17) == 0);
                (self.ivars().emit)(self.ivars().node.get(), new_tab_event(url, background));
                handler.call((WKNavigationActionPolicy::Cancel,));
                return;
            }
            let external = match &*self.ivars().inline_base.borrow() {
                Some(base) => !subframe && !url.starts_with(base.as_str()) && url != "about:blank",
                None => {
                    self.ivars().external_links.get()
                        && super::external_document_link(
                            current_url(web_view).as_deref(),
                            &url,
                            activated,
                            frame.is_none(),
                            subframe,
                        )
                }
            };
            let policy = if external {
                (self.ivars().emit)(
                    self.ivars().node.get(),
                    Event::Custom {
                        tag: "webview:link",
                        num: super::LINK_REPORT,
                        text: url,
                    },
                );
                WKNavigationActionPolicy::Cancel
            } else {
                WKNavigationActionPolicy::Allow
            };
            handler.call((policy,));
        }
    }
);

define_class!(
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayTabbedWebView"]
    struct TabWebView;
    unsafe impl NSObjectProtocol for TabWebView {}
    impl TabWebView {
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, event: &NSEvent) {
            unsafe { let _: () = msg_send![super(self), willOpenMenu: menu, withEvent: event]; }
            let delegate = DELEGATES.with(|m|m.borrow().get(&(self as *const Self as usize)).cloned());
            let Some(nav) = delegate else { return; };
            let labels = nav.ivars().tabs.borrow().clone();
            let Some(labels) = labels else { return; };
            // WebKit assigns its public context-menu tags to the native menu. Preserve
            // the engine-owned action and represented object so it resolves the link.
            for item in menu.itemArray().iter() {
                if item.tag() == 1 {
                    item.setTitle(&NSString::from_str(&labels.foreground));
                    *nav.ivars().link_item.borrow_mut() = Some(item.clone());
                    let background = NSMenuItem::new(self.mtm());
                    background.setTitle(&NSString::from_str(&labels.background));
                    unsafe {
                        background.setTarget(Some(&nav));
                        background.setAction(Some(objc2::sel!(openBackgroundTab:)));
                    }
                    menu.insertItem_atIndex(&background, menu.indexOfItem(&item) + 1);
                    break;
                }
            }
        }
    }
);

impl WebNav {
    fn new(mtm: MainThreadMarker, node: NodeId, emit: fn(NodeId, Event)) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NavIvars {
            node: Cell::new(node),
            emit,
            inline_base: RefCell::new(None),
            external_links: Cell::new(false),
            tabs: RefCell::new(None),
            background: Cell::new(false),
            link_item: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }
}

day_core::tls_group! {
    // Keep each navigation delegate alive as long as its web view (delegate ref is weak).
    static DELEGATES: RefCell<HashMap<usize, Retained<WebNav>>> = RefCell::new(HashMap::new());
    // Session id -> the retained web view. Day releases its own reference when the page is
    // navigated away from, which on AppKit only detaches (`removeFromSuperview`); this reference
    // is what keeps the engine (and therefore the loaded page) alive until the app returns.
    static SESSIONS: RefCell<HashMap<u64, Retained<NSView>>> = RefCell::new(HashMap::new());

}

/// The node a realized view belongs to. `update` is handed only the native handle, so the id is
/// recovered from the navigation delegate kept alive alongside it.
fn node_of(view: &Retained<NSView>) -> Option<NodeId> {
    let key = (&**view as *const NSView) as usize;
    DELEGATES.with(|m| m.borrow().get(&key).map(|nav| nav.ivars().node.get()))
}

fn current_url(web: &WKWebView) -> Option<String> {
    let url = unsafe { web.URL() }?;
    let s = url.absoluteString()?;
    Some(s.to_string())
}

fn load_url(web: &WKWebView, url: &str) {
    let ns = NSString::from_str(url);
    let Some(nsurl) = NSURL::URLWithString(&ns) else {
        return;
    };
    let req = NSURLRequest::requestWithURL(&nsurl);
    let _ = unsafe { web.loadRequest(&req) };
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

pub(crate) fn make(
    mtm: MainThreadMarker,
    p: &WebProps,
    id: NodeId,
    emit: fn(NodeId, Event),
    assets: Option<std::path::PathBuf>,
) -> Retained<NSView> {
    // A session already holding a view: re-attach it rather than build a new one. Only the node
    // changes: point the delegate at the node now showing it, and do not reload, since the whole
    // purpose is to come back to the page as it was left.
    if p.session != 0
        && let Some(view) = SESSIONS.with(|m| m.borrow().get(&p.session).cloned())
    {
        let key = (&*view as *const NSView) as usize;
        DELEGATES.with(|m| {
            if let Some(nav) = m.borrow().get(&key) {
                nav.ivars().node.set(id);
                nav.ivars().external_links.set(p.external_links);
                *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
                *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
            }
        });
        return view;
    }

    // SAFETY: creates a WKWebView with a default configuration on the main thread.
    let config =
        super::resources_apple::configuration(p.resources.as_ref().map(ResourceProvider::id), mtm);
    super::storage_apple::configure(&config, &p.profile);
    let this = TabWebView::alloc(mtm).set_ivars(());
    let web: Retained<TabWebView> = unsafe {
        msg_send![super(this), initWithFrame: objc2_foundation::NSRect::ZERO, configuration: &*config]
    };
    let web: Retained<WKWebView> = web.into_super();
    if p.transparent {
        // WKWebView paints a white sheet under the page unless told not to. `drawsBackground` is
        // the key Safari's own transparent views set (there is no public setter on macOS), read
        // through key-value coding so a WebKit without it ignores the call rather than crashing.
        unsafe {
            let no: *mut AnyObject = msg_send![objc2::class!(NSNumber), numberWithBool: false];
            let key = NSString::from_str("drawsBackground");
            let _: () = msg_send![&*web, setValue: no, forKey: &*key];
        }
    }
    let nav = WebNav::new(mtm, id, emit);
    nav.ivars().external_links.set(p.external_links);
    *nav.ivars().tabs.borrow_mut() = p.tabs.clone();
    unsafe {
        let _: () = msg_send![&*web, setUIDelegate: &*nav];
    }
    unsafe { web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav))) };
    if !p.inline_root.is_empty() {
        // Inline mode (docs/webview.md): the bundled site is loose files (the assets tree in
        // the bundle, or the project's `resource/assets/` under `day launch`), so a file URL
        // with read access is the whole load path; WebKit resolves the page's relative
        // references against it natively. What that read access covers is the site's own
        // directory, or the app's whole asset tree for a site that asked for it
        // (`app_assets`) — the page still starts inside the site either way, and the
        // navigation policing below stays on the site's own URL prefix.
        if let Some(dir) = assets
            .clone()
            .or_else(|| day_spec::resolve_asset_dir(&p.inline_root))
        {
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
            let _ = unsafe { web.loadFileURL_allowingReadAccessToURL(&index, &read) };
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
    let view: Retained<NSView> = Retained::from(<WKWebView as AsRef<NSView>>::as_ref(&web));
    DELEGATES.with(|m| {
        m.borrow_mut()
            .insert((view.as_ref() as *const NSView) as usize, nav)
    });
    if p.session != 0 {
        SESSIONS.with(|m| m.borrow_mut().insert(p.session, view.clone()));
    }
    view
}

/// Run `script` and report `1␟<json>` / `0␟<name>␟<message>` back on `node`, keyed by `req`.
///
/// The script is already wrapped by the front-end, so it always evaluates to a JS string and the
/// `id` handed to the completion is an `NSString`: no `NSJSONSerialization` walk, which matters
/// because WebKit hands back cyclic dictionaries that would hang it.
///
/// A `nil` result with a `nil` error cannot happen here for the same reason (the wrapper always
/// returns a string), so anything else is WebKit itself failing, most often a dead content
/// process, which reports as `JavaScriptResultTypeIsUnsupported` with no exception message.
fn eval(web: &WKWebView, node: NodeId, req: u64, script: &str, emit: fn(NodeId, Event)) {
    if super::storage_apple::request(web, script, move |text| {
        emit(
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
            // SAFETY: non-null result from WebKit; the wrapper guarantees it is an NSString.
            let obj = unsafe { &*result };
            obj.downcast_ref::<NSString>()
                .map(|s| s.to_string())
                .unwrap_or_else(|| engine_error("WebKitError", "non-string reply"))
        } else if !error.is_null() {
            // SAFETY: non-null NSError from WebKit.
            let msg = unsafe { (*error).localizedDescription() }.to_string();
            engine_error("WebKitError", &msg)
        } else {
            engine_error("WebKitError", "no result")
        };
        emit(
            node,
            Event::Custom {
                tag: "webview:eval",
                num: req as f64,
                text: payload,
            },
        );
    });
    // SAFETY: main thread (a renderer duty), and WebKit copies the block before returning.
    unsafe { web.evaluateJavaScript_completionHandler(&js, Some(&handler)) };
}

pub(crate) fn update(h: &Retained<NSView>, patch: &WebPatch) {
    let Some(web) = h.downcast_ref::<WKWebView>() else {
        return;
    };
    match patch {
        WebPatch::Eval { req, script } => {
            if let Some(node) = node_of(h) {
                let key = (&**h as *const NSView) as usize;
                if let Some(emit) = DELEGATES.with(|m| m.borrow().get(&key).map(|n| n.ivars().emit))
                {
                    eval(web, node, *req, script, emit);
                }
            }
        }
        WebPatch::Load(url) => load_url(web, url),
        WebPatch::Back => {
            let _ = unsafe { web.goBack() };
        }
        WebPatch::Forward => {
            let _ = unsafe { web.goForward() };
        }
        WebPatch::Stop => unsafe { web.stopLoading() },
        WebPatch::ForceReload => {
            let _ = unsafe { web.reloadFromOrigin() };
        }
        WebPatch::Reload => {
            let _ = unsafe { web.reload() };
        }
    }
}

/// Drop the retained navigation delegate when the view goes away.
///
/// Without this the map grows by one entry per realized web view, and, worse, its key is the
/// view's address, which the allocator reuses: a later view landing on a freed address would
/// inherit the dead node's id and misroute its events.
pub(crate) fn release(h: &Retained<NSView>) {
    let key = (&**h as *const NSView) as usize;
    // A session-retained view is not going away; day is only detaching it from the page being
    // torn down, and the next visit re-attaches it. Its delegate has to outlive this node too, or
    // the returning view would report navigations to nobody.
    let retained = SESSIONS.with(|m| {
        m.borrow()
            .values()
            .any(|v| (&**v as *const NSView) as usize == key)
    });
    if retained {
        return;
    }
    DELEGATES.with(|m| {
        m.borrow_mut().remove(&key);
    });
}
