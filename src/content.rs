// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Content blocking and page scripts (docs/webview.md, "Content blocking and page scripts"):
//! the engine-agnostic half of [`crate::WebView::request_filter`],
//! [`crate::WebView::content_rules`], [`crate::WebView::user_script`],
//! [`crate::WebView::user_style`], [`crate::WebView::on_script_message`],
//! [`crate::WebView::on_blocked`] and [`crate::WebView::on_media_request`].
//!
//! These are hooks, not a blocker: the app supplies the decisions, the rule lists and the
//! scripts, and each backend wires them into its engine's own mechanism.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use day_spec::Support;

/// Decides whether a web view may load a request. Engines with a per-request hook (Android
/// WebView, WebView2, QtWebEngine, ArkWeb) call it for every request a page makes, before the
/// request leaves the device.
///
/// It runs on whatever thread the engine calls back on: a WebView background thread on
/// Android, the UI thread on WebView2 and Qt, ArkWeb's IO thread. Keep it fast, in memory and
/// free of locks the UI thread holds, and never touch Day's reactive graph from it. Every
/// request of every page waits for the answer.
pub trait RequestFilter: Send + Sync + 'static {
    /// The verdict for one request.
    fn decide(&self, request: &RequestInfo<'_>) -> RequestDecision;
}

impl<F> RequestFilter for F
where
    F: Fn(&RequestInfo<'_>) -> RequestDecision + Send + Sync + 'static,
{
    fn decide(&self, request: &RequestInfo<'_>) -> RequestDecision {
        self(request)
    }
}

/// One request, as the engine describes it to a [`RequestFilter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestInfo<'a> {
    /// The URL being requested.
    pub url: &'a str,
    /// The page shown in the view (the top-level document). Engines that do not report it
    /// (Android, WebView2) get it from the piece, which follows each main-frame navigation.
    /// `None` before the first page.
    pub top_url: Option<&'a str>,
    /// The URL of the frame making the request, where the engine reports it (ArkWeb).
    pub frame_url: Option<&'a str>,
    /// What the request loads. [`ResourceKind::infer`] guesses it on engines that do not say.
    pub kind: ResourceKind,
    /// Whether this is the top-level frame's own document: a navigation of the whole view.
    pub main_frame: bool,
    /// The HTTP method, upper case (`GET`, `POST`, …).
    pub method: &'a str,
}

/// What a request loads.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    /// A page loaded into a frame; with `main_frame` set, a navigation of the whole view.
    Document,
    /// A page loaded into an iframe.
    Subdocument,
    /// JavaScript.
    Script,
    /// CSS.
    Stylesheet,
    /// An image.
    Image,
    /// A web font.
    Font,
    /// Audio or video.
    Media,
    /// `XMLHttpRequest`.
    Xhr,
    /// `fetch()`.
    Fetch,
    /// A WebSocket connection.
    WebSocket,
    /// A beacon, `<a ping>` or CSP report.
    Ping,
    /// A plug-in object (`<object>`, `<embed>`).
    Object,
    /// Anything else the engine names (a worker, a manifest, a prefetch).
    Other,
    /// The engine does not say, and nothing suggests a kind.
    #[default]
    Unknown,
}

impl ResourceKind {
    /// The kind an engine without request types (Android WebView) implies, from the request's
    /// `Accept` header and the URL's extension. The header wins where it is specific: browsers
    /// send `text/html` for documents, `text/css` for style sheets and `image/…` for images,
    /// while scripts and `fetch` both send `*/*`, so those fall back to the extension.
    pub fn infer(url: &str, accept: Option<&str>, main_frame: bool) -> ResourceKind {
        if main_frame {
            return ResourceKind::Document;
        }
        let accept = accept.unwrap_or_default().to_ascii_lowercase();
        let first = accept.split(',').next().unwrap_or_default().trim();
        if first.starts_with("text/html") || first.starts_with("application/xhtml") {
            return ResourceKind::Subdocument;
        }
        if first.starts_with("text/css") {
            return ResourceKind::Stylesheet;
        }
        if first.starts_with("image/") {
            return ResourceKind::Image;
        }
        if first.starts_with("video/") || first.starts_with("audio/") {
            return ResourceKind::Media;
        }
        if first.starts_with("font/") || first.starts_with("application/font") {
            return ResourceKind::Font;
        }
        if first == "text/event-stream" {
            return ResourceKind::Xhr;
        }
        match ResourceKind::from_extension(url) {
            ResourceKind::Unknown if first.starts_with("application/json") => ResourceKind::Xhr,
            kind => kind,
        }
    }

    /// The kind a URL's file extension suggests, or [`ResourceKind::Unknown`].
    fn from_extension(url: &str) -> ResourceKind {
        let path = url.split(['?', '#']).next().unwrap_or_default();
        let path = path
            .split_once("://")
            .map(|(_, rest)| rest.split_once('/').map(|(_, p)| p).unwrap_or(""))
            .unwrap_or(path);
        let last = path.rsplit('/').next().unwrap_or_default();
        let Some((_, ext)) = last.rsplit_once('.') else {
            return ResourceKind::Unknown;
        };
        match ext.to_ascii_lowercase().as_str() {
            "js" | "mjs" | "cjs" => ResourceKind::Script,
            "css" => ResourceKind::Stylesheet,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" => {
                ResourceKind::Image
            }
            "woff" | "woff2" | "ttf" | "otf" | "eot" => ResourceKind::Font,
            "mp4" | "webm" | "mp3" | "m4a" | "m4v" | "ogg" | "oga" | "wav" | "m3u8" | "mpd" => {
                ResourceKind::Media
            }
            "html" | "htm" => ResourceKind::Subdocument,
            "json" | "xml" => ResourceKind::Xhr,
            _ => ResourceKind::Unknown,
        }
    }

    /// The kind a Chromium `ResourceType` names (QtWebEngine's `ResourceType`, ArkWeb's
    /// `ArkWeb_ResourceType`: the same numbering, 0 = main frame … 20). Qt's later additions
    /// (21 = WebSocket, 22 = JSON) follow it.
    #[allow(dead_code)]
    pub(crate) fn from_chromium(code: i32) -> ResourceKind {
        match code {
            0 | 19 => ResourceKind::Document,
            1 | 20 => ResourceKind::Subdocument,
            2 => ResourceKind::Stylesheet,
            3 => ResourceKind::Script,
            4 | 12 => ResourceKind::Image,
            5 => ResourceKind::Font,
            7 | 17 => ResourceKind::Object,
            8 => ResourceKind::Media,
            13 | 22 => ResourceKind::Xhr,
            14 | 16 => ResourceKind::Ping,
            21 => ResourceKind::WebSocket,
            6 | 9 | 10 | 11 | 15 | 18 => ResourceKind::Other,
            _ => ResourceKind::Unknown,
        }
    }

    /// The kind a WebView2 `COREWEBVIEW2_WEB_RESOURCE_CONTEXT` names.
    #[allow(dead_code)]
    pub(crate) fn from_webview2(context: i32) -> ResourceKind {
        match context {
            1 => ResourceKind::Document,
            2 => ResourceKind::Stylesheet,
            3 => ResourceKind::Image,
            4 => ResourceKind::Media,
            5 => ResourceKind::Font,
            6 => ResourceKind::Script,
            7 => ResourceKind::Xhr,
            8 => ResourceKind::Fetch,
            10 => ResourceKind::Xhr, // EventSource
            11 => ResourceKind::WebSocket,
            14 | 15 => ResourceKind::Ping, // Ping, CspViolationReport
            9 | 12 | 13 | 16 => ResourceKind::Other,
            _ => ResourceKind::Unknown,
        }
    }
}

/// A [`RequestFilter`]'s verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RequestDecision {
    /// Load the request.
    #[default]
    Allow,
    /// Refuse it. The page sees a failed load (an empty `403` where the engine answers with a
    /// response).
    Block,
}

/// A request a [`RequestFilter`] blocked, as [`crate::WebView::on_blocked`] reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockedLoad {
    /// The URL that was not loaded.
    pub url: String,
    /// The page that asked for it: the top-level page the view showed, or the frame's address
    /// where the engine reports one.
    pub page_url: String,
    /// What it would have loaded.
    pub kind: ResourceKind,
}

/// A WebKit content rule list ([`crate::WebView::content_rules`]): a JSON array of
/// `{"trigger": …, "action": …}` rules in WebKit's content blocker format, compiled by the
/// engine once and kept compiled across launches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentRuleList {
    /// Names the list in the engine's compiled store. Keep it stable across launches.
    pub id: String,
    /// The list's version. A compiled list whose version differs is compiled again; one whose
    /// version matches is loaded as it is, without reading `json`.
    pub version: String,
    /// The rules, as JSON text.
    pub json: Arc<str>,
}

impl ContentRuleList {
    /// A list named `id` at `version`.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        json: impl Into<Arc<str>>,
    ) -> Self {
        ContentRuleList {
            id: id.into(),
            version: version.into(),
            json: json.into(),
        }
    }

    /// The identifier the compiled list is stored under: the id and version together, so a
    /// relaunch finds the compiled list for this version and a new version compiles afresh.
    #[allow(dead_code)]
    pub(crate) fn store_identifier(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

/// A content rule list failed to compile ([`crate::WebView::on_content_rules_error`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentRulesError {
    /// The list's [`ContentRuleList::id`].
    pub id: String,
    /// The engine's message.
    pub message: String,
}

/// When a [`UserScript`] runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InjectAt {
    /// As the document is created, before the page's own scripts.
    #[default]
    DocumentStart,
    /// Once the document has been parsed (`DOMContentLoaded`), before subresources finish.
    DocumentEnd,
}

/// Which frames a [`UserScript`] runs in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Frames {
    /// The top-level document and every iframe.
    #[default]
    All,
    /// The top-level document only.
    Main,
}

/// A script the view runs in every page it loads ([`crate::WebView::user_script`]), in the
/// page's own JavaScript world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserScript {
    /// The script.
    pub source: Arc<str>,
    /// When it runs.
    pub at: InjectAt,
    /// Which frames it runs in.
    pub frames: Frames,
}

impl UserScript {
    /// `source`, at document start, in every frame.
    pub fn new(source: impl Into<Arc<str>>) -> Self {
        UserScript {
            source: source.into(),
            at: InjectAt::DocumentStart,
            frames: Frames::All,
        }
    }

    /// Run at `at` instead.
    pub fn at(mut self, at: InjectAt) -> Self {
        self.at = at;
        self
    }

    /// Run in `frames` instead.
    pub fn frames(mut self, frames: Frames) -> Self {
        self.frames = frames;
        self
    }
}

/// A message a page posted with `window.dayMessage(name, payload)`
/// ([`crate::WebView::on_script_message`]).
///
/// Every frame the message shim runs in can post one, ads and other third-party frames
/// included. Treat the payload as untrusted input.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScriptMessage {
    /// The handler's name.
    pub name: String,
    /// What the page sent, as text.
    pub payload: String,
    /// The security origin of the frame that sent it, where the engine vouches for it (WebKit,
    /// Android, WebView2); empty elsewhere.
    #[serde(default)]
    pub origin: String,
    /// Whether the top-level document sent it.
    #[serde(default)]
    pub main_frame: bool,
}

/// A page asked for the camera or microphone (`getUserMedia`), the question
/// [`crate::WebView::on_media_request`] answers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MediaRequest {
    /// The origin of the page asking.
    pub origin: String,
    /// It asks for the camera.
    pub camera: bool,
    /// It asks for the microphone.
    pub microphone: bool,
}

/// The view's request filter as a backend receives it in [`crate::WebProps::content`]. Call
/// [`RequestHooks::decide`] from the engine's request callback on any thread: it fills in the
/// top-level page the piece tracked, never lets the filter block the crate's own resource
/// namespace, and reports what it blocked to [`crate::WebView::on_blocked`].
#[derive(Clone)]
pub struct RequestHooks(Arc<Hooks>);

struct Hooks {
    filter: Arc<dyn RequestFilter>,
    /// The top-level page, as the last allowed main-frame document request (or the backend,
    /// through [`RequestHooks::set_top_url`]) left it.
    top: Mutex<String>,
    blocked: Option<Arc<BlockedSink>>,
    /// The resource namespace is the app's own site here (a provider's site or a bundled one),
    /// so the filter never sees it.
    exempt_internal: bool,
}

impl RequestHooks {
    pub(crate) fn new(
        filter: Arc<dyn RequestFilter>,
        blocked: Option<Arc<BlockedSink>>,
        exempt_internal: bool,
    ) -> Self {
        RequestHooks(Arc::new(Hooks {
            filter,
            top: Mutex::new(String::new()),
            blocked,
            exempt_internal,
        }))
    }

    /// Whether requests in the crate's resource namespace go to the filter too: a view that
    /// browses anywhere with a provider beside it (`WebView::resources`) serves pages that stand
    /// for other sites, while a provider's own site or a bundled site is the app's own.
    pub fn filters_internal(&self) -> bool {
        !self.0.exempt_internal
    }

    /// Ask the filter about `request`. Safe on any thread.
    ///
    /// The crate's own resource namespace (`day-resource:` and `https://day-resource.invalid/`)
    /// is allowed without asking where it is the app's own site ([`Self::filters_internal`]).
    /// A filter that panics allows the request rather than unwinding into the engine.
    pub fn decide(&self, request: &RequestInfo<'_>) -> RequestDecision {
        if self.0.exempt_internal && is_internal(request.url) {
            return RequestDecision::Allow;
        }
        let tracked = match request.top_url {
            Some(_) => None,
            None => Some(lock(&self.0.top).clone()).filter(|top| !top.is_empty()),
        };
        let info = RequestInfo {
            top_url: request.top_url.or(tracked.as_deref()),
            ..*request
        };
        let filter = &self.0.filter;
        let decision =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filter.decide(&info)))
                .unwrap_or_else(|_| {
                    log::warn!(
                        "day-piece-webview: a request filter panicked; allowing {}",
                        info.url
                    );
                    RequestDecision::Allow
                });
        match decision {
            RequestDecision::Allow if info.main_frame && info.kind == ResourceKind::Document => {
                self.set_top_url(info.url);
            }
            RequestDecision::Block => {
                if let Some(sink) = &self.0.blocked {
                    sink.push(BlockedLoad {
                        url: info.url.to_owned(),
                        page_url: info
                            .frame_url
                            .or(info.top_url)
                            .unwrap_or(info.url)
                            .to_owned(),
                        kind: info.kind,
                    });
                }
            }
            RequestDecision::Allow => {}
        }
        decision
    }

    /// Record the page the view now shows, for engines that report main-frame navigations
    /// separately from requests (WebView2's `NavigationStarting`).
    pub fn set_top_url(&self, url: &str) {
        let mut top = lock(&self.0.top);
        if *top != url {
            url.clone_into(&mut top);
        }
    }

    /// The page the view shows, as far as the hooks know.
    pub fn top_url(&self) -> String {
        lock(&self.0.top).clone()
    }
}

impl PartialEq for RequestHooks {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for RequestHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RequestHooks")
    }
}

/// A lock that a panicking holder did not poison for everyone else: the data is plain values.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// URLs in the crate's resource namespace, which a filter never sees.
fn is_internal(url: &str) -> bool {
    url.starts_with("day-resource:") || url.starts_with("https://day-resource.invalid/")
}

/// The most blocked loads kept between two deliveries. A page that blocks more within one
/// delivery period loses the excess from the report (never from blocking).
const BLOCKED_BACKLOG: usize = 2000;

/// How often blocked loads reach [`crate::WebView::on_blocked`], at most: one call per
/// period with everything blocked in it.
pub(crate) const BLOCKED_PERIOD_MS: u32 = 100;

/// Blocked loads on their way to the UI thread. Filled from any thread; drained by the
/// front-end's delivery task, which it wakes when the first load of a batch arrives.
#[derive(Default)]
pub(crate) struct BlockedSink {
    state: Mutex<SinkState>,
}

#[derive(Default)]
struct SinkState {
    loads: Vec<BlockedLoad>,
    wake: Option<day_async::Deliver<()>>,
    dropped: usize,
}

impl BlockedSink {
    pub(crate) fn push(&self, load: BlockedLoad) {
        self.extend(std::iter::once(load));
    }

    pub(crate) fn extend(&self, loads: impl IntoIterator<Item = BlockedLoad>) {
        let wake = {
            let mut state = lock(&self.state);
            for load in loads {
                if state.loads.len() < BLOCKED_BACKLOG {
                    state.loads.push(load);
                } else {
                    state.dropped += 1;
                }
            }
            if state.loads.is_empty() {
                None
            } else {
                state.wake.take()
            }
        };
        if let Some(wake) = wake {
            wake.send(());
        }
    }

    /// Everything waiting, or a future that resolves when something arrives.
    fn take_or_wait(&self) -> Result<Vec<BlockedLoad>, day_async::Oneshot<()>> {
        let mut state = lock(&self.state);
        if !state.loads.is_empty() {
            if state.dropped > 0 {
                log::debug!(
                    "day-piece-webview: {} blocked loads not reported (backlog full)",
                    state.dropped
                );
                state.dropped = 0;
            }
            return Ok(std::mem::take(&mut state.loads));
        }
        let (tx, rx) = day_async::oneshot();
        state.wake = Some(tx);
        Err(rx)
    }
}

/// What [`crate::WebView::on_blocked`] stores.
pub(crate) type BlockedCallback = std::rc::Rc<dyn Fn(&[BlockedLoad])>;

/// Deliver `sink`'s loads to `callback` on the UI thread: one call per
/// [`BLOCKED_PERIOD_MS`] at most, carrying everything blocked since the last one.
pub(crate) fn deliver_blocked(
    sink: Arc<BlockedSink>,
    callback: BlockedCallback,
) -> day_core::TaskHandle {
    day_core::task(async move {
        loop {
            match sink.take_or_wait() {
                Ok(loads) => {
                    callback(&loads);
                    day_core::sleep(BLOCKED_PERIOD_MS).await;
                }
                Err(arrival) => {
                    if arrival.await.is_err() {
                        return;
                    }
                    // Let the rest of the burst arrive before the first delivery.
                    day_core::sleep(BLOCKED_PERIOD_MS).await;
                }
            }
        }
    })
}

/// Request hooks by token, for backends whose engine callback arrives through Java or C
/// carrying a number rather than a Rust reference. The registry holds them weakly: the
/// front-end keeps a view's hooks alive for as long as its node lives, and a callback that
/// arrives after that finds nothing and allows the request.
static TOKENS: Mutex<BTreeMap<u64, std::sync::Weak<Hooks>>> = Mutex::new(BTreeMap::new());

/// Register `hooks` and return the token that finds them again (never 0).
#[allow(dead_code)]
pub(crate) fn register(hooks: &RequestHooks) -> u64 {
    let token = day_async::next_token();
    let mut tokens = lock(&TOKENS);
    tokens.retain(|_, hooks| hooks.strong_count() > 0);
    tokens.insert(token, Arc::downgrade(&hooks.0));
    token
}

/// Forget a [`register`]ed token at once.
#[allow(dead_code)]
pub(crate) fn unregister(token: u64) {
    lock(&TOKENS).remove(&token);
}

/// The hooks a token names, while their view lives.
#[allow(dead_code)]
pub(crate) fn hooks(token: u64) -> Option<RequestHooks> {
    if token == 0 {
        return None;
    }
    lock(&TOKENS)
        .get(&token)
        .and_then(std::sync::Weak::upgrade)
        .map(RequestHooks)
}

/// Everything [`crate::WebView`]'s content hooks hand a backend: part of [`crate::WebProps`].
/// Changes to the rule lists arrive later as [`crate::WebPatch::ContentRules`]; the rest is fixed
/// when the view is built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageContent {
    /// The request filter, wrapped for any-thread use ([`crate::WebView::request_filter`]).
    pub requests: Option<RequestHooks>,
    /// Content rule lists for WebKit engines ([`crate::WebView::content_rules`]).
    pub rules: Vec<ContentRuleList>,
    /// The app's user scripts, in the order they were added ([`crate::WebView::user_script`]).
    pub scripts: Vec<UserScript>,
    /// User style sheets ([`crate::WebView::user_style`]), applied to every frame.
    pub styles: Vec<Arc<str>>,
    /// The names pages may post to with `window.dayMessage(name, payload)`
    /// ([`crate::WebView::on_script_message`]).
    pub messages: Vec<String>,
    /// The app decides camera and microphone requests itself
    /// ([`crate::WebView::on_media_request`]); without it the backend's default applies.
    pub media_handler: bool,
}

impl PageContent {
    /// Whether the view has page scripts of any kind to install.
    #[allow(dead_code)]
    pub(crate) fn has_scripts(&self) -> bool {
        !self.scripts.is_empty() || !self.styles.is_empty() || !self.messages.is_empty()
    }

    /// The scripts a backend installs, in order: the message shim (for `transport`, when pages
    /// may post messages), each user style as a script that adds a `<style>` (unless the engine
    /// has native user style sheets), then the app's scripts.
    #[allow(dead_code)]
    pub(crate) fn page_scripts(
        &self,
        transport: Transport,
        native_styles: bool,
    ) -> Vec<UserScript> {
        let mut scripts = Vec::new();
        if !self.messages.is_empty() {
            scripts.push(UserScript::new(message_shim(transport, &self.messages)));
        }
        if !native_styles {
            scripts.extend(
                self.styles
                    .iter()
                    .map(|css| UserScript::new(style_script(css))),
            );
        }
        scripts.extend(self.scripts.iter().cloned());
        scripts
    }
}

/// How a backend's message shim reaches the app.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    /// WebKit: `webkit.messageHandlers[name]`, a handler with replies, so `postMessage` itself
    /// returns the reply promise.
    WebKit,
    /// Android: `window[name]`, a web message listener object; replies come back as messages
    /// on it, tagged with the request id.
    Android,
    /// WebView2: `chrome.webview.postMessage` with a JSON envelope; replies as web messages.
    WebView2,
    /// Qt and ArkWeb: a host object or console line carrying a JSON envelope; replies arrive
    /// through `window.__dayMessageReply(id, value)`, evaluated in the top-level document.
    Envelope(&'static str),
}

/// `window.dayMessage(name, payload)`: the one page-side API for every engine. It returns a
/// promise of the handler's reply (`null` without one). Where the engine has no object of its
/// own per name, it also defines `window[name].postMessage(text)`, fire-and-forget, so a page
/// script written against WebKit's or Android's spelling works unchanged.
pub(crate) fn message_shim(transport: Transport, names: &[String]) -> String {
    let names = serde_json::to_string(names).unwrap_or_else(|_| "[]".into());
    let send = match transport {
        Transport::WebKit => {
            "const h=window.webkit&&window.webkit.messageHandlers&&window.webkit.messageHandlers[name];\
             if(!h)return Promise.reject(new Error('no handler '+name));\
             return Promise.resolve(h.postMessage(text)).then(v=>v===undefined?null:v);"
                .to_owned()
        }
        Transport::Android => {
            // Replies are messages on the same object, `\u0002<id>\u0002<0|1><text>`.
            "const o=window[name];if(!o||typeof o.postMessage!=='function')return Promise.reject(new Error('no handler '+name));\
             if(!hooked.has(name)){hooked.add(name);o.addEventListener('message',e=>{const d=String(e.data);if(d.charCodeAt(0)!==2)return;\
             const i=d.indexOf('\\u0002',1);settle(+d.slice(1,i),d.charAt(i+1)==='1'?d.slice(i+2):null);});}\
             const id=next++;return new Promise(r=>{waiting.set(id,r);o.postMessage('\\u0002'+id+'\\u0002'+text);});"
                .to_owned()
        }
        Transport::WebView2 => {
            "const w=window.chrome&&window.chrome.webview;if(!w)return Promise.reject(new Error('no host'));\
             if(!hooked.has('')){hooked.add('');w.addEventListener('message',e=>{const d=e.data;if(d&&typeof d==='object'&&'dayMessageReply' in d)settle(d.dayMessageReply,d.value);});}\
             const id=next++;return new Promise(r=>{waiting.set(id,r);w.postMessage(JSON.stringify({dayMessage:name,id:id,payload:text}));});"
                .to_owned()
        }
        // Replies reach the top-level document only, so a frame's promise settles at once.
        Transport::Envelope(post) => format!(
            "const id=next++;const e=JSON.stringify({{dayMessage:name,id:id,payload:text}});\
             if(window!==window.top){{{post}(e);return Promise.resolve(null);}}\
             return new Promise(r=>{{waiting.set(id,r);{post}(e);}});"
        ),
    };
    let aliases = match transport {
        // These engines define `window[name]` (or `webkit.messageHandlers[name]`) natively.
        Transport::WebKit | Transport::Android => "",
        Transport::WebView2 | Transport::Envelope(_) => {
            "for(const n of NAMES){if(!(n in window))Object.defineProperty(window,n,{value:Object.freeze({postMessage:t=>{send(n,String(t)).catch(()=>{});}})});}"
        }
    };
    format!(
        "(()=>{{if(window.dayMessage)return;const NAMES={names};let next=1+Math.floor(Math.random()*1e9);const waiting=new Map();const hooked=new Set();\
         const settle=(id,v)=>{{const r=waiting.get(id);if(r){{waiting.delete(id);r(v===undefined?null:v);}}}};\
         Object.defineProperty(window,'__dayMessageReply',{{value:settle}});\
         const send=(name,text)=>{{{send}}};\
         Object.defineProperty(window,'dayMessage',{{value:(name,payload)=>{{try{{return send(String(name),payload==null?'':String(payload));}}catch(e){{return Promise.reject(e);}}}}}});\
         {aliases}}})();"
    )
}

/// A script that applies `css` to the document as a `<style>` element: user styles on engines
/// without a native user style sheet. It runs at document start and adds the element as soon
/// as the document has a root to add it to.
pub(crate) fn style_script(css: &str) -> String {
    let css = crate::js_string_literal(css);
    format!(
        "(()=>{{const css={css};const add=()=>{{const r=document.head||document.documentElement;if(!r)return false;\
         const s=document.createElement('style');s.setAttribute('data-day-user-style','');s.textContent=css;r.appendChild(s);return true;}};\
         if(!add())document.addEventListener('readystatechange',add,{{once:true}});}})();"
    )
}

/// `script` as a script that engines with only "every frame, at document start" injection
/// (Android's `addDocumentStartJavaScript`, WebView2, ArkWeb) run with its own timing and frames:
/// a main-frame script returns early in iframes, and a document-end script waits for
/// `DOMContentLoaded`.
#[allow(dead_code)]
pub(crate) fn document_start_everywhere(script: &UserScript) -> String {
    let mut source = script.source.to_string();
    if script.at == InjectAt::DocumentEnd {
        source = format!(
            "(()=>{{const run=()=>{{{source}\n}};if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',run,{{once:true}});else run();}})();"
        );
    }
    if script.frames == Frames::Main {
        source = format!("if(window===window.top){{{source}\n}}");
    }
    source
}

/// The JavaScript that settles request `id` in the top-level document with `reply`, for the
/// envelope transports.
#[allow(dead_code)]
pub(crate) fn reply_script(id: u64, reply: Option<&str>) -> String {
    let value = reply.map_or_else(|| "null".to_owned(), crate::js_string_literal);
    format!("window.__dayMessageReply&&window.__dayMessageReply({id},{value})")
}

/// A message as a backend reports it on the event channel (`SCRIPT_MESSAGE`): the message and
/// the id its reply ([`crate::WebPatch::ScriptReply`]) goes back under.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MessageReport {
    pub id: u64,
    #[serde(flatten)]
    pub message: ScriptMessage,
}

/// A media request as a backend reports it on the event channel (`MEDIA_REPORT`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MediaReport {
    pub id: u64,
    #[serde(flatten)]
    pub request: MediaRequest,
}

/// Split an envelope a shim posted (`{"dayMessage":name,"id":n,"payload":text}`).
#[allow(dead_code)]
pub(crate) fn parse_envelope(text: &str) -> Option<(String, u64, String)> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(rename = "dayMessage")]
        name: String,
        id: u64,
        #[serde(default)]
        payload: String,
    }
    let envelope: Envelope = serde_json::from_str(text).ok()?;
    Some((envelope.name, envelope.id, envelope.payload))
}

/// Whether [`crate::WebView::request_filter`] runs on this build's engine. `Native` where the
/// engine calls the app for each request (Android WebView, WebView2, QtWebEngine, ArkWeb);
/// WebKit has no such hook (use [`content_rules_support`]), and an iframe sees nothing.
pub fn request_filter_support() -> Support {
    if cfg!(any(
        all(feature = "material", target_os = "android"),
        all(feature = "xaml", windows),
        feature = "qt",
        all(feature = "arkui", target_env = "ohos"),
    )) {
        Support::Native
    } else {
        Support::Unsupported
    }
}

/// Whether [`crate::WebView::on_blocked`] reports anything: wherever the request filter runs.
pub fn blocked_report_support() -> Support {
    request_filter_support()
}

/// Whether [`crate::WebView::content_rules`] applies: the WebKit engines (AppKit, UIKit, GTK on
/// macOS through WKWebView, WebKitGTK on Linux).
pub fn content_rules_support() -> Support {
    if cfg!(any(
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(feature = "uikit", target_os = "ios"),
        all(feature = "gtk", not(target_os = "macos"), not(windows)),
    )) {
        Support::Native
    } else {
        Support::Unsupported
    }
}

/// Whether [`crate::WebView::user_script`] and [`crate::WebView::user_style`] run in pages.
/// `Emulated` on Android WebViews too old for document-start scripts, which the backend runs
/// as each page starts instead (main frame only, possibly after the page's first scripts).
pub fn user_script_support() -> Support {
    #[cfg(all(feature = "material", target_os = "android"))]
    if !crate::android_impl::document_start_scripts() {
        return Support::Emulated;
    }
    if cfg!(any(
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(feature = "uikit", target_os = "ios"),
        all(feature = "gtk", not(target_os = "macos"), not(windows)),
        all(feature = "material", target_os = "android"),
        all(feature = "xaml", windows),
        feature = "qt",
        all(feature = "arkui", target_env = "ohos"),
    )) {
        Support::Native
    } else {
        Support::Unsupported
    }
}

/// Whether pages can reach [`crate::WebView::on_script_message`] handlers.
pub fn script_message_support() -> Support {
    user_script_support()
}

/// A view's rule-list generations and its held first page load, for the engines that compile
/// rule lists asynchronously (WebKit, WebKitGTK). Every rule change starts a generation; only
/// the newest one's completion attaches its lists and releases the load. A superseded compile
/// that finishes first neither attaches nor loads, so the first page never loads under rules
/// that are about to be replaced, or none.
#[derive(Default)]
// Only the WebKit backends compile rule lists; other builds keep it for its tests.
#[allow(dead_code)]
pub(crate) struct RuleGate {
    generation: std::cell::Cell<u64>,
    pending_load: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
}

#[allow(dead_code)]
impl RuleGate {
    /// Start a generation for a new set of lists, holding `load` (the first page load) when
    /// there is one. Returns the generation to hand to [`RuleGate::finish`].
    pub(crate) fn begin(&self, load: Option<Box<dyn FnOnce()>>) -> u64 {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if let Some(load) = load {
            *self.pending_load.borrow_mut() = Some(load);
        }
        generation
    }

    /// Whether `generation` is still the newest: only then may its lists attach.
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }

    /// Whether the first page load is still held for the rule lists.
    pub(crate) fn is_holding(&self) -> bool {
        self.pending_load.borrow().is_some()
    }

    /// A navigation the app asked for (`WebPatch::Load`). While the first page load is held, it
    /// takes that load's place, so the page the app asked for last is the one that loads, and
    /// only once the newest rules are attached; `None` then. Otherwise it comes back to run now.
    pub(crate) fn redirect(&self, load: Box<dyn FnOnce()>) -> Option<Box<dyn FnOnce()>> {
        let mut pending = self.pending_load.borrow_mut();
        if pending.is_some() {
            *pending = Some(load);
            None
        } else {
            Some(load)
        }
    }

    /// Release the held load, once `generation`'s lists are attached (or could not be). Does
    /// nothing for a superseded generation; the newer one releases it.
    pub(crate) fn finish(&self, generation: u64) {
        if !self.is_current(generation) {
            return;
        }
        // Taken before the call: the load may change the rules again.
        let load = self.pending_load.borrow_mut().take();
        if let Some(load) = load {
            load();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_rules_release_the_first_load() {
        use std::cell::Cell;
        use std::rc::Rc;
        let gate = RuleGate::default();
        let loaded = Rc::new(Cell::new(false));
        let first = gate.begin(Some(Box::new({
            let loaded = loaded.clone();
            move || loaded.set(true)
        })));
        // New lists arrive before the first compile finishes.
        let second = gate.begin(None);
        // The superseded compile finishes first: no attach, no load.
        assert!(!gate.is_current(first));
        gate.finish(first);
        assert!(!loaded.get());
        // The newest finishes: it attaches and the page loads, once.
        assert!(gate.is_current(second));
        gate.finish(second);
        assert!(loaded.get());
        loaded.set(false);
        gate.finish(second);
        assert!(!loaded.get());
    }

    #[test]
    fn a_navigation_while_held_replaces_the_first_load() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let gate = RuleGate::default();
        let page = Rc::new(RefCell::new(Vec::new()));
        let load = |name: &'static str| -> Box<dyn FnOnce()> {
            let page = page.clone();
            Box::new(move || page.borrow_mut().push(name))
        };
        let generation = gate.begin(Some(load("initial")));
        assert!(gate.is_holding());
        // Navigations while the rules compile wait, and the last one wins.
        assert!(gate.redirect(load("first")).is_none());
        assert!(gate.redirect(load("requested")).is_none());
        assert!(page.borrow().is_empty());
        gate.finish(generation);
        assert_eq!(*page.borrow(), ["requested"]);
        // Once the page is out, a navigation runs at once.
        assert!(!gate.is_holding());
        match gate.redirect(load("later")) {
            Some(now) => now(),
            None => panic!("a navigation after the first load was held"),
        }
        assert_eq!(*page.borrow(), ["requested", "later"]);
    }

    #[test]
    fn kinds_follow_the_accept_header_then_the_extension() {
        let infer = |url, accept| ResourceKind::infer(url, accept, false);
        assert_eq!(
            ResourceKind::infer("https://a.test/", Some("text/html"), true),
            ResourceKind::Document
        );
        assert_eq!(
            infer(
                "https://a.test/frame",
                Some("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            ),
            ResourceKind::Subdocument
        );
        assert_eq!(
            infer("https://a.test/x?y", Some("text/css,*/*;q=0.1")),
            ResourceKind::Stylesheet
        );
        assert_eq!(
            infer("https://a.test/pixel", Some("image/avif,image/webp,*/*")),
            ResourceKind::Image
        );
        // Scripts and fetch send */*: the extension decides, and nothing is a guess.
        assert_eq!(
            infer("https://ads.test/tag.JS?v=2#x", Some("*/*")),
            ResourceKind::Script
        );
        assert_eq!(infer("https://a.test/font.woff2", None), ResourceKind::Font);
        assert_eq!(
            infer("https://a.test/clip.m3u8", Some("*/*")),
            ResourceKind::Media
        );
        assert_eq!(
            infer("https://a.test/api/feed.json", Some("*/*")),
            ResourceKind::Xhr
        );
        assert_eq!(
            infer("https://a.test/api/feed", Some("application/json")),
            ResourceKind::Xhr
        );
        assert_eq!(
            infer("https://a.test/collect", Some("*/*")),
            ResourceKind::Unknown
        );
        assert_eq!(infer("https://a.test", None), ResourceKind::Unknown);
        assert_eq!(infer("https://a.test/dir.v2/", None), ResourceKind::Unknown);
    }

    #[test]
    fn engine_type_codes_map_to_kinds() {
        assert_eq!(ResourceKind::from_chromium(0), ResourceKind::Document);
        assert_eq!(ResourceKind::from_chromium(1), ResourceKind::Subdocument);
        assert_eq!(ResourceKind::from_chromium(3), ResourceKind::Script);
        assert_eq!(ResourceKind::from_chromium(13), ResourceKind::Xhr);
        assert_eq!(ResourceKind::from_chromium(21), ResourceKind::WebSocket);
        assert_eq!(ResourceKind::from_chromium(255), ResourceKind::Unknown);
        assert_eq!(ResourceKind::from_webview2(8), ResourceKind::Fetch);
        assert_eq!(ResourceKind::from_webview2(11), ResourceKind::WebSocket);
        assert_eq!(ResourceKind::from_webview2(0), ResourceKind::Unknown);
    }

    #[test]
    fn hooks_track_the_top_page_and_never_filter_the_resource_namespace() {
        let filter = |request: &RequestInfo<'_>| {
            if request.url.contains("ads") || request.url.starts_with("day-resource:") {
                RequestDecision::Block
            } else {
                RequestDecision::Allow
            }
        };
        let sink = Arc::new(BlockedSink::default());
        let hooks = RequestHooks::new(Arc::new(filter), Some(sink.clone()), true);
        fn request(url: &str, kind: ResourceKind, main_frame: bool) -> RequestInfo<'_> {
            RequestInfo {
                url,
                top_url: None,
                frame_url: None,
                kind,
                main_frame,
                method: "GET",
            }
        }
        assert_eq!(
            hooks.decide(&request("https://news.test/", ResourceKind::Document, true)),
            RequestDecision::Allow
        );
        assert_eq!(hooks.top_url(), "https://news.test/");
        assert_eq!(
            hooks.decide(&request(
                "https://ads.test/a.js",
                ResourceKind::Script,
                false
            )),
            RequestDecision::Block
        );
        assert_eq!(
            hooks.decide(&request(
                "day-resource://p1/ads.html",
                ResourceKind::Subdocument,
                false
            )),
            RequestDecision::Allow
        );
        // A blocked navigation does not become the page.
        assert_eq!(
            hooks.decide(&request("https://ads.test/", ResourceKind::Document, true)),
            RequestDecision::Block
        );
        assert_eq!(hooks.top_url(), "https://news.test/");
        let loads = sink.take_or_wait().ok().unwrap();
        assert_eq!(
            loads[0],
            BlockedLoad {
                url: "https://ads.test/a.js".into(),
                page_url: "https://news.test/".into(),
                kind: ResourceKind::Script,
            }
        );
        assert_eq!(loads.len(), 2);
        assert!(sink.take_or_wait().is_err());
        // A view browsing with a provider beside it filters the provider's pages too.
        let sees_all = RequestHooks::new(
            Arc::new(|r: &RequestInfo<'_>| {
                if r.url.contains("ads") {
                    RequestDecision::Block
                } else {
                    RequestDecision::Allow
                }
            }),
            None,
            false,
        );
        assert!(sees_all.filters_internal());
        assert_eq!(
            sees_all.decide(&request(
                "https://day-resource.invalid/p1/https/ads.example/a.js",
                ResourceKind::Script,
                false
            )),
            RequestDecision::Block
        );
        // A panicking filter allows rather than unwinding into the engine.
        let panicky = RequestHooks::new(
            Arc::new(|_: &RequestInfo<'_>| -> RequestDecision { panic!("filter bug") }),
            None,
            true,
        );
        assert_eq!(
            panicky.decide(&request("https://x.test/", ResourceKind::Image, false)),
            RequestDecision::Allow
        );
        // Engines call the filter from their own threads.
        let workers: Vec<_> = (0..3)
            .map(|worker| {
                let hooks = hooks.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let url = format!("https://ads.test/{worker}/{i}.js");
                        assert_eq!(
                            hooks.decide(&request(&url, ResourceKind::Script, false)),
                            RequestDecision::Block
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(sink.take_or_wait().ok().map(|loads| loads.len()), Some(300));
        let token = register(&hooks);
        assert!(hooks_eq(super::hooks(token), &hooks));
        unregister(token);
        assert!(super::hooks(token).is_none());
    }

    fn hooks_eq(found: Option<RequestHooks>, hooks: &RequestHooks) -> bool {
        found.is_some_and(|found| found == *hooks)
    }

    #[test]
    fn envelopes_round_trip_and_shims_name_their_handlers() {
        assert_eq!(
            parse_envelope(r#"{"dayMessage":"dayAdblock","id":7,"payload":"{\"a\":1}"}"#),
            Some(("dayAdblock".into(), 7, "{\"a\":1}".into()))
        );
        assert_eq!(parse_envelope("plain text"), None);
        let names = vec!["dayAdblock".to_owned()];
        for transport in [
            Transport::WebKit,
            Transport::Android,
            Transport::WebView2,
            Transport::Envelope("console.debug"),
        ] {
            let shim = message_shim(transport, &names);
            assert!(shim.contains("\"dayAdblock\""), "{shim}");
            assert!(
                shim.starts_with("(()=>{") && shim.ends_with("})();"),
                "{shim}"
            );
        }
        assert_eq!(
            reply_script(3, None),
            "window.__dayMessageReply&&window.__dayMessageReply(3,null)"
        );
        assert!(reply_script(3, Some("a\"b")).ends_with("(3,\"a\\\"b\")"));
        assert!(style_script("a{b:'c'}\n").contains("\"a{b:'c'}\\n\""));
    }
}
