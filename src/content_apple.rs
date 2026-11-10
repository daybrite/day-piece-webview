// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Content rule lists, user scripts and page messages on WKWebView, shared by AppKit, UIKit and
//! GTK on macOS (docs/webview.md, "Content blocking and page scripts").
//!
//! Every view is created with its own `WKWebViewConfiguration` (resources_apple.rs), and so its
//! own `WKUserContentController`: one view's lists, scripts and handlers never reach another.
//! Compiled rule lists live in `WKContentRuleListStore.defaultStore` under
//! `ContentRuleList::store_identifier`, shared by every view and kept across launches.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use block2::RcBlock;
use day_spec::{Event, NodeId};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSError, NSString};

use crate::content::Transport;
use crate::{ContentRuleList, Frames, InjectAt, PageContent, ScriptMessage, WebPatch};

/// WebKit's reply handler: the reply value, or an error message.
type ReplyBlock = RcBlock<dyn Fn(*mut AnyObject, *mut NSString)>;
/// Reports a list that failed to compile: its id and the engine's message.
type OnError = Rc<dyn Fn(&str, &str)>;
/// What `prepare` hands the compiled lists to.
type Prepared = Rc<RefCell<Option<Box<dyn FnOnce(Vec<Retained<AnyObject>>)>>>>;

struct MessageIvars {
    node: Cell<NodeId>,
    emit: Cell<fn(NodeId, Event)>,
    next: Cell<u64>,
    /// Reply handlers waiting for the app's answer (`WebPatch::ScriptReply`), by message id.
    replies: RefCell<HashMap<u64, ReplyBlock>>,
}

define_class!(
    // A WKScriptMessageHandlerWithReply (macOS 11, iOS 14), and the plain handler for older
    // WebKit, whose `postMessage` then resolves with nothing.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayScriptMessages"]
    #[ivars = MessageIvars]
    struct Messages;

    unsafe impl NSObjectProtocol for Messages {}

    impl Messages {
        #[unsafe(method(userContentController:didReceiveScriptMessage:replyHandler:))]
        fn receive_with_reply(
            &self,
            _controller: &AnyObject,
            message: &AnyObject,
            reply: &block2::DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>,
        ) {
            // SAFETY: WebKit hands over a heap block it expects called exactly once; copying it
            // keeps it alive until the app answers.
            let reply = unsafe {
                RcBlock::copy(reply as *const _ as *mut block2::DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>)
            };
            let id = self.ivars().next.get();
            self.ivars().next.set(id + 1);
            if let Some(reply) = reply {
                self.ivars().replies.borrow_mut().insert(id, reply);
            }
            self.report(id, message);
        }

        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn receive(&self, _controller: &AnyObject, message: &AnyObject) {
            let id = self.ivars().next.get();
            self.ivars().next.set(id + 1);
            self.report(id, message);
        }
    }
);

impl Messages {
    fn new(mtm: MainThreadMarker, node: NodeId, emit: fn(NodeId, Event)) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MessageIvars {
            node: Cell::new(node),
            emit: Cell::new(emit),
            next: Cell::new(1),
            replies: RefCell::new(HashMap::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn report(&self, id: u64, message: &AnyObject) {
        // SAFETY: WKScriptMessage's documented properties, on the main thread.
        let (name, payload, origin, main_frame) = unsafe {
            let name: Retained<NSString> = msg_send![message, name];
            let body: *mut AnyObject = msg_send![message, body];
            let payload = body
                .as_ref()
                .map(|body| match body.downcast_ref::<NSString>() {
                    Some(text) => text.to_string(),
                    None => {
                        let text: Retained<NSString> = msg_send![body, description];
                        text.to_string()
                    }
                })
                .unwrap_or_default();
            let frame: *mut AnyObject = msg_send![message, frameInfo];
            let (origin, main_frame) = match frame.as_ref() {
                Some(frame) => {
                    let main: bool = msg_send![frame, isMainFrame];
                    let security: *mut AnyObject = msg_send![frame, securityOrigin];
                    (security.as_ref().map(origin_text).unwrap_or_default(), main)
                }
                None => (String::new(), false),
            };
            (name.to_string(), payload, origin, main_frame)
        };
        (self.ivars().emit.get())(
            self.ivars().node.get(),
            crate::script_message_event(
                id,
                ScriptMessage {
                    name,
                    payload,
                    origin,
                    main_frame,
                },
            ),
        );
    }

    fn answer(&self, id: u64, reply: Option<&str>) {
        let Some(handler) = self.ivars().replies.borrow_mut().remove(&id) else {
            return;
        };
        match reply {
            Some(text) => {
                let text = NSString::from_str(text);
                handler.call((
                    Retained::as_ptr(&text) as *mut AnyObject,
                    std::ptr::null_mut(),
                ));
            }
            None => handler.call((std::ptr::null_mut(), std::ptr::null_mut())),
        }
    }
}

/// `scheme://host[:port]` from a WKSecurityOrigin.
fn origin_text(origin: &AnyObject) -> String {
    // SAFETY: WKSecurityOrigin's properties.
    unsafe {
        let scheme: Retained<NSString> = msg_send![origin, protocol];
        let host: Retained<NSString> = msg_send![origin, host];
        let port: isize = msg_send![origin, port];
        if port > 0 {
            format!("{scheme}://{host}:{port}")
        } else {
            format!("{scheme}://{host}")
        }
    }
}

/// One view's content state, by the view's address.
struct ViewContent {
    controller: Retained<AnyObject>,
    messages: Option<Retained<Messages>>,
    node: Cell<NodeId>,
    emit: fn(NodeId, Event),
    /// The rule-list generations and the first page load they hold.
    rules: crate::content::RuleGate,
}

day_core::tls_group! {
    static VIEWS: RefCell<HashMap<usize, Rc<ViewContent>>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// The rule list store views compile into: `None` for WebKit's default store. Tests set
    /// their own directory (`probe`), so they never touch the user's store.
    static STORE_DIRECTORY: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
}

/// Install `content`'s scripts and message handlers into the configuration's user content
/// controller, then attach its rule lists and call `load` once they are attached (or have
/// failed): a page loaded before them would load unfiltered. `load` runs at once when there are
/// no lists.
pub(crate) fn attach(
    mtm: MainThreadMarker,
    view: usize,
    config: &AnyObject,
    content: &PageContent,
    node: NodeId,
    emit: fn(NodeId, Event),
    load: impl FnOnce() + 'static,
) {
    // SAFETY: WKWebViewConfiguration's controller, on the main thread.
    let controller: Retained<AnyObject> = unsafe { msg_send![config, userContentController] };
    for script in content.page_scripts(Transport::WebKit, false) {
        add_script(&controller, &script.source, script.at, script.frames);
    }
    let messages = (!content.messages.is_empty()).then(|| {
        let handler = Messages::new(mtm, node, emit);
        for name in &content.messages {
            add_handler(&controller, &handler, name);
        }
        handler
    });
    let state = Rc::new(ViewContent {
        controller,
        messages,
        node: Cell::new(node),
        emit,
        rules: crate::content::RuleGate::default(),
    });
    VIEWS.with(|views| views.borrow_mut().insert(view, state.clone()));
    if content.rules.is_empty() {
        load();
    } else {
        apply_rules(&state, &content.rules, Some(Box::new(load)));
    }
}

fn add_script(controller: &AnyObject, source: &str, at: InjectAt, frames: Frames) {
    // WKUserScriptInjectionTimeAtDocumentStart = 0, AtDocumentEnd = 1. This initializer runs in
    // the page's own world, where a script sees and patches the page's globals.
    let time: isize = match at {
        InjectAt::DocumentStart => 0,
        InjectAt::DocumentEnd => 1,
    };
    // SAFETY: WKUserScript's designated initializer and the controller's `addUserScript:`.
    unsafe {
        let script: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(WKUserScript), alloc];
        let script: Option<Retained<AnyObject>> = msg_send![script, initWithSource: &*NSString::from_str(source), injectionTime: time, forMainFrameOnly: frames == Frames::Main];
        if let Some(script) = script {
            let _: () = msg_send![controller, addUserScript: &*script];
        }
    }
}

fn add_handler(controller: &AnyObject, handler: &Messages, name: &str) {
    let name = NSString::from_str(name);
    // SAFETY: the controller's handler registration, on the main thread. WebKit retains the
    // handler for as long as it stays registered.
    unsafe {
        let replies: bool = msg_send![controller, respondsToSelector: objc2::sel!(addScriptMessageHandlerWithReply:contentWorld:name:)];
        if replies {
            let world: Retained<AnyObject> = msg_send![objc2::class!(WKContentWorld), pageWorld];
            let _: () = msg_send![controller, addScriptMessageHandlerWithReply: handler, contentWorld: &*world, name: &*name];
        } else {
            let _: () = msg_send![controller, addScriptMessageHandler: handler, name: &*name];
        }
    }
}

/// A navigation the app asked for (`WebPatch::Load`). While the view's first page waits for its
/// rule lists, this navigation takes its place and waits too, so the page the app asked for
/// last loads, filtered; otherwise it runs now.
pub(crate) fn navigate(view: usize, load: impl FnOnce() + 'static) {
    let state = VIEWS.with(|views| views.borrow().get(&view).cloned());
    let load: Box<dyn FnOnce()> = Box::new(load);
    let now = match state {
        Some(state) => state.rules.redirect(load),
        None => Some(load),
    };
    if let Some(now) = now {
        now();
    }
}

/// Whether the view's first page is still waiting for its rule lists: a reload then has nothing
/// to reload, and the held load will run anyway.
pub(crate) fn holding(view: usize) -> bool {
    VIEWS.with(|views| {
        views
            .borrow()
            .get(&view)
            .is_some_and(|state| state.rules.is_holding())
    })
}

/// Point a retained view's reports at the node now showing it.
pub(crate) fn retarget(view: usize, node: NodeId, emit: fn(NodeId, Event)) {
    VIEWS.with(|views| {
        if let Some(state) = views.borrow().get(&view) {
            state.node.set(node);
            if let Some(messages) = &state.messages {
                messages.ivars().node.set(node);
                messages.ivars().emit.set(emit);
            }
        }
    });
}

/// Apply a content patch; `false` for the patches that are not content's.
pub(crate) fn update(view: usize, patch: &WebPatch) -> bool {
    let Some(state) = VIEWS.with(|views| views.borrow().get(&view).cloned()) else {
        return matches!(
            patch,
            WebPatch::ContentRules(_)
                | WebPatch::ScriptReply { .. }
                | WebPatch::MediaDecision { .. }
        );
    };
    match patch {
        WebPatch::ContentRules(lists) => apply_rules(&state, lists, None),
        WebPatch::ScriptReply { id, reply } => {
            if let Some(messages) = &state.messages {
                messages.answer(*id, reply.as_deref());
            }
        }
        // WebKit asks the user itself (WKUIDelegate's media capture prompt).
        WebPatch::MediaDecision { .. } => {}
        _ => return false,
    }
    true
}

/// Forget a released view. Unanswered messages resolve with nothing.
pub(crate) fn release(view: usize) {
    if let Some(state) = VIEWS.with(|views| views.borrow_mut().remove(&view))
        && let Some(messages) = &state.messages
    {
        let waiting: Vec<u64> = messages.ivars().replies.borrow().keys().copied().collect();
        for id in waiting {
            messages.answer(id, None);
        }
    }
}

/// The store compiled lists live in: the default store, or a test's own directory.
fn store(directory: Option<&std::path::Path>) -> Option<Retained<AnyObject>> {
    // SAFETY: WKContentRuleListStore's class constructors.
    unsafe {
        match directory {
            None => msg_send![objc2::class!(WKContentRuleListStore), defaultStore],
            Some(directory) => {
                let url = objc2_foundation::NSURL::fileURLWithPath_isDirectory(
                    &NSString::from_str(&directory.display().to_string()),
                    true,
                );
                msg_send![objc2::class!(WKContentRuleListStore), storeWithURL: &*url]
            }
        }
    }
}

/// Compile and attach `lists`, replacing the view's current ones. `load`, the first page load,
/// waits in the view until the newest set is attached, whichever call that is.
fn apply_rules(
    state: &Rc<ViewContent>,
    lists: &[ContentRuleList],
    load: Option<Box<dyn FnOnce()>>,
) {
    let generation = state.rules.begin(load);
    let state = state.clone();
    let directory = STORE_DIRECTORY.with(|d| d.borrow().clone());
    let Some(store) = store(directory.as_deref()) else {
        log::warn!("day-piece-webview: WKContentRuleListStore is unavailable");
        state.rules.finish(generation);
        return;
    };
    let on_error = {
        let state = state.clone();
        move |id: &str, message: &str| {
            (state.emit)(
                state.node.get(),
                crate::content_rules_error_event(id, message),
            );
        }
    };
    prepare(&store, lists, Rc::new(on_error), move |compiled| {
        // A superseded set neither attaches nor releases the load: the newer one will.
        if !state.rules.is_current(generation) {
            return;
        }
        // SAFETY: the view's controller, on the main thread completions arrive on.
        unsafe {
            let _: () = msg_send![&*state.controller, removeAllContentRuleLists];
            for list in &compiled {
                let _: () = msg_send![&*state.controller, addContentRuleList: &**list];
            }
        }
        state.rules.finish(generation);
    });
}

/// Look up or compile each list in `store`, then hand the compiled lists (those that compiled)
/// to `done`, in order. Lookups are keyed by id and version, so a list compiled by an earlier
/// launch loads without its JSON being read; a list compiled afresh removes the versions of the
/// same id it replaces. Everything completes on the main thread, and nothing waits on it.
fn prepare(
    store: &Retained<AnyObject>,
    lists: &[ContentRuleList],
    on_error: OnError,
    done: impl FnOnce(Vec<Retained<AnyObject>>) + 'static,
) {
    if lists.is_empty() {
        done(Vec::new());
        return;
    }
    type Slots = Rc<RefCell<Vec<Option<Retained<AnyObject>>>>>;
    let slots: Slots = Rc::new(RefCell::new(vec![None; lists.len()]));
    let remaining = Rc::new(Cell::new(lists.len()));
    let done: Prepared = Rc::new(RefCell::new(Some(Box::new(done))));
    let finish_one = {
        let slots = slots.clone();
        move || {
            remaining.set(remaining.get() - 1);
            if remaining.get() == 0
                && let Some(done) = done.borrow_mut().take()
            {
                let compiled = slots.borrow_mut().drain(..).flatten().collect();
                done(compiled);
            }
        }
    };
    let finish_one: Rc<dyn Fn()> = Rc::new(finish_one);
    for (index, list) in lists.iter().enumerate() {
        let identifier = NSString::from_str(&list.store_identifier());
        let (store2, list2, slots2, finish2, on_error2) = (
            store.clone(),
            list.clone(),
            slots.clone(),
            finish_one.clone(),
            on_error.clone(),
        );
        let found = RcBlock::new(move |compiled: *mut AnyObject, _error: *mut NSError| {
            // SAFETY: a WKContentRuleList, retained for as long as the slot holds it.
            if let Some(compiled) = unsafe { Retained::retain(compiled) } {
                slots2.borrow_mut()[index] = Some(compiled);
                finish2();
                return;
            }
            compile(&store2, &list2, index, &slots2, &finish2, &on_error2);
        });
        // SAFETY: WKContentRuleListStore lookup; WebKit copies the block.
        unsafe {
            let _: () = msg_send![&**store, lookUpContentRuleListForIdentifier: &*identifier, completionHandler: &*found];
        }
    }
}

fn compile(
    store: &Retained<AnyObject>,
    list: &ContentRuleList,
    index: usize,
    slots: &Rc<RefCell<Vec<Option<Retained<AnyObject>>>>>,
    finish_one: &Rc<dyn Fn()>,
    on_error: &OnError,
) {
    let identifier = list.store_identifier();
    let (store2, slots, finish_one, on_error) = (
        store.clone(),
        slots.clone(),
        finish_one.clone(),
        on_error.clone(),
    );
    let id = list.id.clone();
    let keep = identifier.clone();
    let compiled = RcBlock::new(move |compiled: *mut AnyObject, error: *mut NSError| {
        // SAFETY: the compiled list, or the NSError explaining why there is none.
        match unsafe { Retained::retain(compiled) } {
            Some(compiled) => {
                slots.borrow_mut()[index] = Some(compiled);
                remove_stale(&store2, &id, &keep);
            }
            None => {
                let message = unsafe { error.as_ref() }
                    .map(|error| error.localizedDescription().to_string())
                    .unwrap_or_else(|| "compilation failed".into());
                log::warn!(
                    "day-piece-webview: content rule list {id:?} did not compile: {message}"
                );
                on_error(&id, &message);
            }
        }
        finish_one();
    });
    // SAFETY: compilation runs out of process; WebKit copies the block and calls it on the
    // main thread.
    unsafe {
        let _: () = msg_send![&**store, compileContentRuleListForIdentifier: &*NSString::from_str(&identifier), encodedContentRuleList: &*NSString::from_str(&list.json), completionHandler: &*compiled];
    }
}

/// Remove compiled versions of `id` other than `keep` from the store.
fn remove_stale(store: &Retained<AnyObject>, id: &str, keep: &str) {
    let prefix = format!("{id}@");
    let (store2, keep) = (store.clone(), keep.to_owned());
    let listed = RcBlock::new(move |identifiers: *mut NSArray<NSString>| {
        // SAFETY: the store's identifiers, an NSArray of NSString.
        let Some(identifiers) = (unsafe { identifiers.as_ref() }) else {
            return;
        };
        for identifier in identifiers {
            let text = identifier.to_string();
            if text.starts_with(&prefix) && text != keep {
                let ignore = RcBlock::new(|_error: *mut NSError| {});
                unsafe {
                    let _: () = msg_send![&*store2, removeContentRuleListForIdentifier: &*identifier, completionHandler: &*ignore];
                }
            }
        }
    });
    // SAFETY: the store's listing; WebKit copies the block.
    unsafe {
        let _: () = msg_send![&**store, getAvailableContentRuleListIdentifiers: &*listed];
    }
}

/// An end-to-end check of rule lists on a real WKWebView, for `tests/content-rules-macos.rs`
/// alone (it must run on the main thread with a run loop, which a library test cannot). It
/// compiles `lists` into its own store at `directory`, keeping the user's default store clean,
/// attaches them to a fresh view and loads `page`; the test spins the run loop and reads back
/// what the page wrote into its title.
#[cfg(target_os = "macos")]
pub mod probe {
    use super::*;

    /// What the probe has seen so far.
    #[derive(Debug, Default)]
    pub struct Probe {
        /// The lists that compiled, or the errors, once compilation finished.
        pub compiled: Option<Result<usize, Vec<crate::ContentRulesError>>>,
        /// The page's title, as last read.
        pub title: String,
        web: Option<Retained<AnyObject>>,
    }

    /// Start: compile, then build a view, attach the lists and load `page` (a file URL whose
    /// directory the page may read).
    pub fn start(
        directory: &std::path::Path,
        lists: &[ContentRuleList],
        page: &std::path::Path,
    ) -> Rc<RefCell<Probe>> {
        let probe = Rc::new(RefCell::new(Probe::default()));
        let Some(mtm) = MainThreadMarker::new() else {
            probe.borrow_mut().compiled = Some(Err(Vec::new()));
            return probe;
        };
        let Some(store) = store(Some(directory)) else {
            probe.borrow_mut().compiled = Some(Err(Vec::new()));
            return probe;
        };
        let errors = Rc::new(RefCell::new(Vec::new()));
        let on_error = {
            let errors = errors.clone();
            move |id: &str, message: &str| {
                errors.borrow_mut().push(crate::ContentRulesError {
                    id: id.to_owned(),
                    message: message.to_owned(),
                });
            }
        };
        let (state, page) = (probe.clone(), page.to_owned());
        let wanted = lists.len();
        prepare(&store, lists, Rc::new(on_error), move |compiled| {
            let errors = errors.take();
            if compiled.len() != wanted || !errors.is_empty() {
                state.borrow_mut().compiled = Some(Err(errors));
                return;
            }
            let config = crate::resources_apple::configuration(None, mtm);
            // SAFETY: a plain WKWebView on the main thread, given the compiled lists.
            unsafe {
                let controller: Retained<AnyObject> = msg_send![&*config, userContentController];
                for list in &compiled {
                    let _: () = msg_send![&*controller, addContentRuleList: &**list];
                }
                let web: objc2::rc::Allocated<AnyObject> =
                    msg_send![objc2::class!(WKWebView), alloc];
                let frame = objc2_foundation::NSRect::new(
                    objc2_foundation::NSPoint::new(0.0, 0.0),
                    objc2_foundation::NSSize::new(320.0, 240.0),
                );
                let web: Retained<AnyObject> =
                    msg_send![web, initWithFrame: frame, configuration: &*config];
                let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                    &page.display().to_string(),
                ));
                let read = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                    &page.parent().unwrap_or(&page).display().to_string(),
                ));
                let _: *mut AnyObject =
                    msg_send![&*web, loadFileURL: &*url, allowingReadAccessToURL: &*read];
                let mut state = state.borrow_mut();
                state.compiled = Some(Ok(compiled.len()));
                state.web = Some(web);
            }
        });
        probe
    }

    /// Build a view the way the backends do (`attach`, with `lists` compiling into `directory`)
    /// whose first page is `initial`, then, while the lists are still compiling, navigate to
    /// `requested` the way `WebPatch::Load` does (`navigate`). The view must end on `requested`.
    /// [`Probe::compiled`] is `Ok(1)` when the navigation arrived while the first page was
    /// held, which is what this checks.
    pub fn navigate_while_compiling(
        directory: &std::path::Path,
        lists: &[ContentRuleList],
        initial: &std::path::Path,
        requested: &std::path::Path,
    ) -> Rc<RefCell<Probe>> {
        let probe = Rc::new(RefCell::new(Probe::default()));
        let Some(mtm) = MainThreadMarker::new() else {
            probe.borrow_mut().compiled = Some(Err(Vec::new()));
            return probe;
        };
        STORE_DIRECTORY.with(|d| *d.borrow_mut() = Some(directory.to_owned()));
        let config = crate::resources_apple::configuration(None, mtm);
        // SAFETY: a plain WKWebView built on the main thread from that configuration.
        let web: Retained<AnyObject> = unsafe {
            let web: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(WKWebView), alloc];
            let frame = objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(0.0, 0.0),
                objc2_foundation::NSSize::new(320.0, 240.0),
            );
            msg_send![web, initWithFrame: frame, configuration: &*config]
        };
        let load_file = |web: Retained<AnyObject>, page: std::path::PathBuf| {
            move || {
                let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                    &page.display().to_string(),
                ));
                let read = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                    &page.parent().unwrap_or(&page).display().to_string(),
                ));
                // SAFETY: the view's file loader, on the main thread.
                let _: *mut AnyObject = unsafe {
                    msg_send![&*web, loadFileURL: &*url, allowingReadAccessToURL: &*read]
                };
            }
        };
        let key = Retained::as_ptr(&web) as usize;
        let content = PageContent {
            rules: lists.to_vec(),
            ..PageContent::default()
        };
        fn ignore(_: NodeId, _: Event) {}
        attach(
            mtm,
            key,
            &config,
            &content,
            NodeId(0),
            ignore,
            load_file(web.clone(), initial.to_owned()),
        );
        let held = holding(key);
        navigate(key, load_file(web.clone(), requested.to_owned()));
        STORE_DIRECTORY.with(|d| *d.borrow_mut() = None);
        let mut state = probe.borrow_mut();
        state.compiled = Some(if held { Ok(1) } else { Err(Vec::new()) });
        state.web = Some(web);
        drop(state);
        probe
    }

    /// Ask the page for its title; the answer lands in [`Probe::title`].
    pub fn read_title(probe: &Rc<RefCell<Probe>>) {
        let Some(web) = probe.borrow().web.clone() else {
            return;
        };
        let state = probe.clone();
        let done = RcBlock::new(move |value: *mut AnyObject, _error: *mut NSError| {
            if let Some(text) = unsafe { value.as_ref() }.and_then(|v| v.downcast_ref::<NSString>())
            {
                state.borrow_mut().title = text.to_string();
            }
        });
        // SAFETY: main thread; WebKit copies the block.
        unsafe {
            let _: () = msg_send![&*web, evaluateJavaScript: &*NSString::from_str("document.title"), completionHandler: &*done];
        }
    }
}
