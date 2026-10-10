// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! day-piece-webview, an external Day Piece (DESIGN.md §15) wrapping each toolkit's native web
//! view: WKWebView on AppKit/UIKit, QWebEngineView on Qt, `android.webkit.WebView` on Android. One
//! Rust API registered link-time into each backend's renderer slice without touching day. Alongside
//! the picker it's a reference for pieces that carry both a front-end and their own native backend,
//! here including an Android manifest permission contribution (INTERNET), see docs/extending.md.
//!
//! The view is a growing leaf that fills its space. Navigation is imperative and modeled with
//! `Copy` `Trigger`s (`.go()` loads the bound URL, `.back()`/`.forward()`/`.stop()`/`.reload()`
//! drive history), each `watch`ed to a `WebPatch`. The bound URL is two-way: `.go()` loads it, and
//! native navigation reports the current URL back so a bound text field follows along.

#[cfg(feature = "dom")]
mod browser;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use day_core::{BuildCx, Flex, Piece, RNode, with_tree};
use day_reactive::{Signal, Trigger, watch};
use day_spec::Event;

mod resources;
pub use resources::{ResourceProvider, ResourceRequest, ResourceResponse};

#[cfg(all(
    any(target_os = "macos", target_os = "ios"),
    any(feature = "appkit", feature = "uikit", feature = "gtk")
))]
mod resources_apple;

#[cfg(all(feature = "arkui", target_env = "ohos"))]
mod resources_arkui;

/// Ordinary documents opt in through `on_external_link`. Loading a document, reloading,
/// fragment navigation, and embedded frames must not be mistaken for an outbound click.
#[cfg(any(
    test,
    all(
        any(target_os = "macos", target_os = "ios"),
        any(feature = "appkit", feature = "uikit", feature = "gtk")
    )
))]
fn external_document_link(
    current: Option<&str>,
    target: &str,
    activated: bool,
    new_window: bool,
    subframe: bool,
) -> bool {
    if subframe || target.is_empty() || target == "about:blank" || !(activated || new_window) {
        return false;
    }
    let without_fragment = |url: &str| url.split('#').next().unwrap_or("").to_owned();
    !current.is_some_and(|url| without_fragment(url) == without_fragment(target))
}

pub const KIND: &str = "day.piece.webview";

/// Storage shared by independent views. A private profile never shares persistent data.
/// Configure before creating views. Directory is honored where the engine exposes a public
/// storage-path API; Apple uses an identifier-backed, system-managed location instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebProfile {
    pub name: String,
    pub directory: Option<String>,
    pub private: bool,
}
impl WebProfile {
    pub fn persistent(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
    pub fn private(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            private: true,
            directory: None,
        }
    }
    pub fn directory(mut self, path: impl Into<String>) -> Self {
        self.directory = Some(path.into());
        self
    }
    /// Stable, filesystem-safe identity. Includes privacy mode and requested directory.
    pub fn storage_key(&self) -> String {
        let text = format!("{}:{:?}:{}", self.name, self.directory, self.private);
        let hash = text.bytes().fold(14695981039346656037u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(1099511628211)
        });
        format!("day-{hash:016x}")
    }
}
/// Whether an engine can guarantee an ephemeral, isolated data store. Unsupported private
/// requests are blocked before loading anything; deleting data afterward is not private mode.
pub fn private_browsing_support() -> day_spec::Support {
    if cfg!(any(
        all(feature = "mdc", target_os = "android"),
        all(feature = "dom", target_arch = "wasm32")
    )) {
        day_spec::Support::Unsupported
    } else {
        support()
    }
}
/// Native cookie URL filtering, shared by engines that expose a whole cookie jar.
#[allow(dead_code)]
pub(crate) fn cookie_matches(url: &str, domain: &str, path: &str, secure: bool) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || (secure && url.scheme() != "https") {
        return false;
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    let bare = domain.trim_start_matches('.');
    let domain_match =
        host == bare || (domain.starts_with('.') && host.ends_with(&format!(".{bare}")));
    let path = if path.is_empty() { "/" } else { path };
    domain_match
        && (url.path() == path
            || (url.path().starts_with(path)
                && (path.ends_with('/') || url.path().as_bytes().get(path.len()) == Some(&b'/'))))
}
/// Validate the response origin before native APIs (some accept arbitrary cookie domains).
fn cookie_source_valid(source: &str, header: &str) -> bool {
    if header.contains(['\r', '\n']) {
        return false;
    }
    let Ok(url) = url::Url::parse(source) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let mut fields = header.split(';');
    let Some((name, _)) = fields.next().and_then(|field| field.trim().split_once('=')) else {
        return false;
    };
    let mut secure = false;
    let mut domain_set = false;
    let mut root_path = false;
    for field in fields {
        let (key, value) = field.trim().split_once('=').unwrap_or((field.trim(), ""));
        if key.eq_ignore_ascii_case("domain") {
            domain_set = true;
            let domain = value.trim().trim_start_matches('.').to_ascii_lowercase();
            if psl::suffix(domain.as_bytes())
                .is_some_and(|suffix| suffix.is_known() && suffix.as_bytes() == domain.as_bytes())
            {
                return false;
            }
            if domain.is_empty()
                || !(host == domain
                    || (url::Host::parse(&host).is_ok_and(|h| matches!(h, url::Host::Domain(_)))
                        && host.ends_with(&format!(".{domain}"))))
            {
                return false;
            }
        } else if key.eq_ignore_ascii_case("secure") {
            secure = true;
        } else if key.eq_ignore_ascii_case("path") {
            root_path = value.trim() == "/";
        }
    }
    if secure && url.scheme() != "https" {
        return false;
    }
    if name.starts_with("__Secure-") && !secure {
        return false;
    }
    if name.starts_with("__Host-") && (!secure || domain_set || !root_path) {
        return false;
    }
    true
}
#[cfg(any(
    all(any(feature = "appkit", feature = "gtk"), target_os = "macos"),
    all(feature = "uikit", target_os = "ios")
))]
mod storage_apple;
/// Named profile isolation. ArkWeb exposes app-wide stores only; its default and private
/// contexts share data by origin, but do not implement independent named profiles.
pub fn profile_support() -> day_spec::Support {
    #[cfg(all(feature = "mdc", target_os = "android"))]
    let result = android_impl::capability("supportsProfiles");
    #[cfg(not(all(feature = "mdc", target_os = "android")))]
    let result = {
        #[cfg(any(
            all(any(feature = "appkit", feature = "gtk"), target_os = "macos"),
            all(feature = "uikit", target_os = "ios")
        ))]
        if !storage_apple::named_profiles_available() {
            return day_spec::Support::Emulated;
        }
        if cfg!(all(feature = "arkui", target_env = "ohos")) {
            day_spec::Support::Emulated
        } else if cfg!(all(feature = "dom", target_arch = "wasm32")) {
            day_spec::Support::Unsupported
        } else {
            support()
        }
    };
    result
}
/// Complete deletion of a profile's cookie, cache and persistent site data.
pub fn clear_data_support() -> day_spec::Support {
    #[cfg(all(feature = "mdc", target_os = "android"))]
    let result = android_impl::capability("supportsDataClearing");
    #[cfg(not(all(feature = "mdc", target_os = "android")))]
    let result = if cfg!(any(
        all(feature = "arkui", target_env = "ohos"),
        all(feature = "dom", target_arch = "wasm32")
    )) {
        day_spec::Support::Unsupported
    } else {
        support()
    };
    result
}

const DATA_REQUEST: &str = "day-web-data:";

/// Full props (realize). The initial `url` is loaded when the native view is created.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebProps {
    pub profile: WebProfile,
    pub resources: Option<ResourceProvider>,
    /// On Apple backends, report user-activated links in ordinary documents as well.
    pub external_links: bool,
    pub tabs: Option<TabLinkLabels>,
    pub url: String,
    /// The [`WebSession`] this view belongs to, or `0` for none. A non-zero id asks the backend to
    /// hand back the session's existing native view instead of creating one, so the loaded page
    /// survives being navigated away from (docs/webview.md).
    pub session: u64,
    /// Inline mode (docs/webview.md): the `/`-relative `resource/assets/` path of the bundled
    /// site's root directory, or `""` for remote mode. The arm resolves it to its native
    /// browsable base, loads `<base>/<inline_start>`, and cancels+reports navigations that
    /// leave the site (`Event::Custom` with `num` = the link-report tag).
    pub inline_root: String,
    /// The start page within `inline_root` (`"index.html"` unless overridden), which may carry a
    /// query or fragment (`"player.html?src=hello"`). Empty in remote mode.
    pub inline_start: String,
    /// Inline mode: the page may read the app's whole `resource/assets/` tree rather than only
    /// the site's own directory ([`WebView::app_assets`]). Backends whose browsable base is the
    /// asset tree already (Qt's qrc, Android's `file:///android_asset/`, WebView2's virtual host,
    /// the deployed `assets/data/` on web-dom) read the same either way; it is WebKit's file-URL
    /// read access and the GTK cache extraction that narrow to the site without it.
    pub inline_assets: bool,
    /// Draw no background of the view's own, so a page whose own background is transparent shows
    /// the app behind it ([`WebView::transparent`]). A view normally paints white under the page.
    pub transparent: bool,
}

/// A retained browsing session: the thing that outlives the view showing it.
///
/// Day rebuilds a page's whole subtree on every navigation, so a plain `web_view` gets a brand-new
/// native view each visit and reloads from scratch. A session moves the *engine* out of that
/// lifetime: the piece keeps the native web view alive against this id, and a later `web_view`
/// bound to the same session re-attaches it with its page, scroll position, history and JavaScript
/// context intact.
///
/// This is the shape Apple settled on for the same problem (`WebPage` holds the session and
/// `WebView` renders it), and the reason it works is the same: a web view's content lives in the
/// object and its content process, not in its attachment to a parent view.
///
/// Sessions are keyed by a `&'static str`, so [`WebSession::global`] is idempotent and safe to
/// call from a page function that runs again on every navigation. There is no way to mint an
/// anonymous one: an id that changed per build would retain a new view each visit and leak them
/// all.
///
/// The retained view is never freed: one session is one live web view for the process's lifetime.
/// Use them for pages a user returns to, not per-item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WebSession(u64);

impl WebSession {
    /// The session named `key`, created on first use. Calling this repeatedly with the same key
    /// returns the same session.
    pub fn global(key: &'static str) -> WebSession {
        day_core::tls_group! {
            static KEYS: RefCell<HashMap<&'static str, u64>> = RefCell::new(HashMap::new());
            static NEXT: Cell<u64> = const { Cell::new(1) };
        }
        WebSession(KEYS.with(|m| {
            *m.borrow_mut().entry(key).or_insert_with(|| {
                NEXT.with(|c| {
                    let v = c.get();
                    c.set(v + 1);
                    v
                })
            })
        }))
    }

    /// The raw id, as it reaches a backend arm through [`WebProps::session`].
    pub fn id(self) -> u64 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Inline (app-embedded) sites (docs/webview.md). A directory under `resource/assets/` ships a
// whole site (html/css/js/images, structure preserved, §18.5); the view loads it through the
// backend's own local-content channel (a file URL into the bundle on Apple,
// `file:///android_asset/` on Android, the same-origin `assets/data/` URL on web-dom), so the
// page's relative references resolve natively. Navigations that leave the site are cancelled
// in-view and dispatched per [`LinkPolicy`], the system browser by default.
// ---------------------------------------------------------------------------

/// The `num` the arms tag an external-link report with on the shared `Event::Custom` channel:
/// navigation reports are `0`, eval replies are `≥ 1`, link reports are this.
const LINK_REPORT: f64 = -1.0;

/// What to do with a navigation that leaves an inline site: the answer an
/// [`WebView::on_external_link`] handler returns. Without a handler, every external link is
/// [`LinkPolicy::OpenSystem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPolicy {
    /// Hand the URL to the operating system (`day`'s `open_url`): the default browser for
    /// `https://`, the mail client for `mailto:`, whatever the OS maps the scheme to.
    OpenSystem,
    /// Let the navigation proceed inside the web view after all.
    InView,
    /// Swallow it. The handler has already done whatever the link means in-app (navigate the
    /// day app, record it, show UI).
    Ignore,
}

/// Split a start page into its path and the query/fragment behind it (`"player.html?src=hello"`
/// → `("player.html", "?src=hello")`). The arms that browse a site by URL keep the page whole;
/// the two that build a file URL from a path need the tail back, because `fileURLWithPath:`
/// would percent-encode `?` and `#` into the file name.
// Only the two file-URL arms call this; the test below covers it in every build.
#[allow(dead_code)]
pub(crate) fn split_page(page: &str) -> (&str, &str) {
    match page.find(['?', '#']) {
        Some(i) => page.split_at(i),
        None => (page, ""),
    }
}

/// A bundled site ready for [`web_view_inline`], the marker [`AssetDirSiteExt::prepare_site`]
/// resolves to (or an unchecked one via `From`-style [`IntoInlineSite`] on the raw directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSite {
    root: String,
}

impl InlineSite {
    /// The site root's `/`-relative path under `resource/assets/`.
    pub fn root(&self) -> &str {
        &self.root
    }
}

/// Why [`AssetDirSiteExt::prepare_site`] declined the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// `<root>/index.html` is not among the bundled assets.
    MissingIndex(String),
    /// A backend that serves inline sites from extracted files (GTK today) failed to extract
    /// the tree to the platform cache.
    Extract(String),
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrepareError::MissingIndex(root) => {
                write!(f, "no index.html under resource/assets/{root}")
            }
            PrepareError::Extract(e) => write!(f, "extracting the inline site: {e}"),
        }
    }
}

/// The future [`AssetDirSiteExt::prepare_site`] returns. Today every v1 backend serves the site
/// from where it already lives, so this resolves on first poll; the shape is a future because
/// backends whose engine cannot read embedded stores in place (GTK's GResource, HarmonyOS
/// rawfile) will extract to the platform cache here, and a large site should not block the UI
/// thread when they land.
pub struct PrepareSite {
    result: Option<Result<InlineSite, PrepareError>>,
}

impl Future for PrepareSite {
    type Output = Result<InlineSite, PrepareError>;
    fn poll(mut self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.result.take() {
            Some(r) => Poll::Ready(r),
            // Fused: polled again after completion (which day's task runner never does).
            None => Poll::Pending,
        }
    }
}

/// `res::assets::<dir>.prepare_site()`: validate and prepare a bundled directory as an inline
/// site (docs/webview.md).
pub trait AssetDirSiteExt {
    fn prepare_site(self) -> PrepareSite;
}

impl AssetDirSiteExt for day_core::AssetDir {
    fn prepare_site(self) -> PrepareSite {
        let root = self.as_str().trim_matches('/').to_string();
        // web-dom has no resource opener (the browser fetches the served tree directly), so the
        // index probe is skipped there and a missing index surfaces as the frame's own 404.
        #[cfg(target_arch = "wasm32")]
        let result = Ok(InlineSite { root });
        #[cfg(not(target_arch = "wasm32"))]
        let result = {
            let index = day_spec::AssetName::dynamic(format!("{root}/index.html"));
            match day_spec::resource(index) {
                Some(_) => prepare_backend(root),
                None => Err(PrepareError::MissingIndex(root)),
            }
        };
        PrepareSite {
            result: Some(result),
        }
    }
}

/// The per-backend half of `prepare_site`: where a backend whose engine cannot read the
/// embedded store in place gets the site onto loose files ahead of the view (docs/webview.md).
#[cfg(not(target_arch = "wasm32"))]
fn prepare_backend(root: String) -> Result<InlineSite, PrepareError> {
    // GTK: the assets live in a GResource blob WebKitGTK cannot browse; extract to the user
    // cache now so `make` finds it warm. The lazy `web_view_inline(dir)` route extracts at
    // realize instead.
    #[cfg(all(feature = "gtk", not(target_os = "macos"), not(windows)))]
    if let Err(e) = gtk_impl::extract_site(&root) {
        return Err(PrepareError::Extract(e));
    }
    Ok(InlineSite { root })
}

/// What [`web_view_inline`] accepts: a prepared [`InlineSite`], or the raw generated
/// `res::assets::…` directory constant for the lazy path (no ahead-of-time index check; a
/// missing page surfaces in the view itself).
pub trait IntoInlineSite {
    fn into_inline_site(self) -> InlineSite;
}

impl IntoInlineSite for InlineSite {
    fn into_inline_site(self) -> InlineSite {
        self
    }
}

impl IntoInlineSite for day_core::AssetDir {
    fn into_inline_site(self) -> InlineSite {
        InlineSite {
            root: self.as_str().trim_matches('/').to_string(),
        }
    }
}

/// Whether this backend can show an inline (app-embedded) site. A separate axis from
/// [`support`]: web-dom's iframe is `Emulated` for remote browsing but fully capable here:
/// the bundled site is same-origin, so loading, relative navigation and the link policy all
/// work. `Unsupported` means no renderer is compiled for this target.
pub fn inline_support() -> day_spec::Support {
    if cfg!(any(
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(feature = "uikit", target_os = "ios"),
        all(feature = "mdc", target_os = "android"),
        // QWebEngine reads qrc assets; Windows without that engine uses a WebView2 child.
        feature = "qt",
        // WebView2 browses the exe-relative assets tree through a virtual-host mapping
        // (degrades to the URL label when the WebView2 Runtime is absent, like `support()`).
        all(feature = "xaml", windows),
        // GTK hosts extract the bundled site before navigation.
        all(feature = "gtk", not(target_os = "macos")),
        // ArkWeb browses the rawfile-staged tree through `resource://rawfile/` URLs.
        all(feature = "arkui", target_env = "ohos"),
        // The deployed `assets/data/` tree is same-origin with the host page: the browser
        // resolves the site natively and the shim's click hook polices leaving links.
        all(feature = "dom", target_arch = "wasm32"),
    )) {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

/// Native cache-bypassing reload. Cross-origin iframe hosts cannot force cache policy.
pub fn force_reload_support() -> day_spec::Support {
    if cfg!(all(feature = "dom", target_arch = "wasm32")) {
        day_spec::Support::Unsupported
    } else {
        support()
    }
}

/// Sparse imperative commands sent to the native view after creation.
#[derive(Clone, Debug, PartialEq)]
pub enum WebPatch {
    /// Load a URL (from `.go()`).
    Load(String),
    /// History back / forward.
    Back,
    Forward,
    /// Stop the in-flight load (the demo's "cancel").
    Stop,
    /// Reload the current page.
    Reload,
    /// Reload while bypassing the engine resource cache (never clears cookies or site data).
    ForceReload,
    /// Evaluate `script` (already wrapped by [`wrap_script`]) and report the result back as an
    /// `Event::Custom` whose `num` is `req`. See docs/webview-eval.md.
    Eval {
        req: u64,
        script: String,
    },
}

// ---------------------------------------------------------------------------
// JavaScript evaluation (docs/webview-eval.md)
// ---------------------------------------------------------------------------

/// Field separator inside an evaluation reply. A raw 0x1F can never appear inside JSON text
/// (`JSON.stringify` escapes control characters as the six ASCII chars `\u001f`), so splitting on
/// it is unambiguous and needs no JSON parser on the Rust side.
const SEP: char = '\u{1f}';

/// Why an evaluation did not produce a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    /// This backend has no engine to evaluate in (web-dom's `<iframe>`, or no renderer at all).
    Unsupported,
    /// The script threw. `name`/`message` come from the caught exception; a cyclic value that
    /// `JSON.stringify` refuses arrives here too, as a `TypeError`.
    Threw { name: String, message: String },
    /// The web view was never realized, or went away before the reply arrived.
    ViewGone,
    /// The engine ran but the reply did not decode; the raw payload is included.
    Engine(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::Unsupported => write!(f, "javascript evaluation is unsupported here"),
            EvalError::Threw { name, message } => write!(f, "{name}: {message}"),
            EvalError::ViewGone => write!(f, "the web view is gone"),
            EvalError::Engine(raw) => write!(f, "undecodable reply: {raw}"),
        }
    }
}

/// Escape `s` into a JavaScript string literal, quotes included.
///
/// U+2028 and U+2029 get named escapes because they are literal line terminators in JS source and
/// would end the string; everything below U+0020 goes to `\uXXXX` because a raw control character
/// is not legal inside a literal either.
fn js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Wrap a user script so every backend reports errors the same way.
///
/// Qt and Android have no error channel at all (a throw, a syntax error and a `null` result all
/// arrive identically), so the channel is built in JavaScript instead, and every backend then
/// behaves like the ones with real errors. The reply is `1␟<json>` or `0␟<name>␟<message>`.
///
/// The script is passed to `eval` as a **string literal** rather than spliced in as source. That
/// costs one escape pass and buys three things splicing cannot:
///
/// - **Syntax errors become catchable.** Spliced in, the wrapper and the script compile as one
///   unit, so a bad script kills the wrapper too and the backend reports its uninformative
///   "no result". `eval` compiles at run time, inside the `try`.
/// - **Statements work.** `throw new Error("x")` and `var a = 1; a + 1` are statements, so
///   `v = (…)` around them is itself a syntax error. `eval` takes a program and yields its
///   completion value, which is the behavior a console user expects.
/// - **No lexical hazards.** A trailing `//` comment or an unbalanced brace in the script cannot
///   reach the wrapper's own tokens.
///
/// The cost is that a page whose Content-Security-Policy omits `unsafe-eval` refuses to run it;
/// that surfaces as a caught `EvalError`, which is at least legible.
///
/// Two `try` blocks, not one: the second catches `JSON.stringify` refusing a cyclic value, which is
/// a different failure from the script throwing.
fn wrap_script(script: &str) -> String {
    let src = js_string_literal(script);
    format!(
        "(function(){{var v;\
         try{{v=eval({src});}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"Error\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         try{{var s=JSON.stringify(v);return \"1\\u001f\"+(s===undefined?\"null\":s);}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"TypeError\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         }})()"
    )
}

/// Build a reply an arm can send when the engine failed rather than the script: a dead content
/// process, a missing web view, a reply of the wrong type. Shaped like the wrapper's own error arm
/// so [`decode`] needs only one format.
// Used by the per-backend arms, each of which is `#[cfg]`-gated to one toolkit, so on any single
// build all but one caller is compiled out, and on a build whose backend has no eval arm yet there
// are none. Which callers exist is a build-configuration accident, not a sign this is unused.
#[allow(dead_code)]
pub(crate) fn engine_error(name: &str, message: &str) -> String {
    format!("0{SEP}{name}{SEP}{message}")
}

/// Decode one reply produced by [`wrap_script`]. `undefined` and values `JSON.stringify` drops
/// (a function, a symbol) both arrive as `null`; the wrapper normalizes them so the payload is
/// always valid JSON.
fn decode(payload: &str) -> Result<String, EvalError> {
    match payload.split_once(SEP) {
        Some(("1", json)) => Ok(json.to_string()),
        Some(("0", rest)) => {
            let (name, message) = rest.split_once(SEP).unwrap_or(("Error", rest));
            Err(EvalError::Threw {
                name: name.to_string(),
                message: message.to_string(),
            })
        }
        _ => Err(EvalError::Engine(payload.to_string())),
    }
}

struct EvalShared {
    result: RefCell<Option<Result<String, EvalError>>>,
    waker: RefCell<Option<std::task::Waker>>,
}

day_core::tls_group! {
    /// Request ids start at 1 so 0 stays free for the URL report, which shares this channel.
    static PRIVATE_BLOCKED_NODES: RefCell<std::collections::HashSet<RNode>> = RefCell::new(std::collections::HashSet::new());
    static REQUEST_NODES: RefCell<HashMap<u64, RNode>> = RefCell::new(HashMap::new());
    static NEXT_REQ: Cell<u64> = const { Cell::new(1) };
    static PENDING: RefCell<HashMap<u64, Rc<EvalShared>>> = RefCell::new(HashMap::new());
    /// Callback-shaped requests (the dayscript `web_eval` step, via day-core's
    /// `register_piece_operation`): same req space and reply channel as the futures above,
    /// different completion shape.
    static PENDING_CB: RefCell<HashMap<u64, day_core::PieceOperationDone>> =
        RefCell::new(HashMap::new());
}

/// Deliver a reply to whichever request is waiting on `req`: an awaited [`EvalFuture`] or a
/// `web_eval` step callback. A reply for a dropped future finds nothing pending and is
/// discarded.
fn resolve(req: u64, payload: &str) {
    REQUEST_NODES.with(|p| p.borrow_mut().remove(&req));
    if let Some(done) = PENDING_CB.with(|p| p.borrow_mut().remove(&req)) {
        done(decode(payload).map_err(|e| e.to_string()));
        return;
    }
    let Some(shared) = PENDING.with(|p| p.borrow_mut().remove(&req)) else {
        return;
    };
    *shared.result.borrow_mut() = Some(decode(payload));
    if let Some(waker) = shared.waker.borrow_mut().take() {
        waker.wake();
    }
}

/// Register this piece's evaluator with day-core, for the dayscript `web_eval` step
/// (docs/webview-eval.md). Called from the constructors; idempotent, and an app that never
/// builds a web view never registers, which the step then reports. The provider applies
/// the same [`wrap_script`] envelope and reply channel as [`JsHandle::eval`]; on a backend
/// whose arm answers `eval_support() != Native` it fails the callback immediately rather
/// than letting the step wait out its retry window.
fn register_script_eval() {
    day_core::register_piece_operation(
        KIND,
        "day.webview.eval",
        std::rc::Rc::new(|node, script, done| {
            if PRIVATE_BLOCKED_NODES.with(|nodes| nodes.borrow().contains(&node))
                || eval_support() != day_spec::Support::Native
            {
                done(Err(EvalError::Unsupported.to_string()));
                return;
            }
            let req = NEXT_REQ.with(|c| {
                let v = c.get();
                c.set(v + 1);
                v
            });
            let wrapped = wrap_script(script);
            REQUEST_NODES.with(|p| p.borrow_mut().insert(req, node));
            PENDING_CB.with(|p| p.borrow_mut().insert(req, done));
            with_tree(|t| {
                t.patch(
                    node,
                    Box::new(WebPatch::Eval {
                        req,
                        script: wrapped,
                    }),
                    false,
                )
            });
        }),
    );
}

/// A native snapshot of the current browser page and session history.
/// Query after navigation or periodically while visible; no script or network request runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct NavigationState {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
}

/// A handle for running JavaScript in a web view. `Copy`, like `Trigger`, so it can be captured by
/// several closures. Bind it with [`WebView::js`]; evaluating before the view is realized fails
/// with [`EvalError::ViewGone`].
#[derive(Clone, Copy)]
pub struct JsHandle {
    node: Signal<Option<RNode>>,
    blocked: Signal<bool>,
}

impl Default for JsHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl JsHandle {
    #[track_caller]
    pub fn new() -> Self {
        JsHandle {
            node: Signal::new(None),
            blocked: Signal::new(false),
        }
    }

    fn data_request(&self, operation: &str, url: &str, cookie: &str) -> EvalFuture {
        let mut future = self.eval("");
        future.script = Some(format!(
            "{DATA_REQUEST}{}",
            serde_json::json!({"operation":operation,"url":url,"cookie":cookie})
        ));
        future
    }
    /// Read URL, title, loading status and actual native history availability.
    /// Emulated iframe hosts return Unsupported rather than guessing browser history.
    pub async fn navigation_state(&self) -> Result<NavigationState, EvalError> {
        let reply = self.data_request("navigation", "", "").await?;
        serde_json::from_str(&reply).map_err(|_| EvalError::Unsupported)
    }
    /// Read cookies (including HttpOnly) eligible for this URL from the bound view's profile.
    /// Treat the returned header as a credential; never log it or send it to a different origin.
    pub async fn cookie_header(&self, url: &str) -> Result<String, EvalError> {
        if !url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
            return Err(EvalError::Unsupported);
        }
        let reply = self.data_request("cookies", url, "").await?;
        serde_json::from_str(&reply).map_err(|_| EvalError::Unsupported)
    }
    /// Store an HTTP Set-Cookie response in the browser's own jar.
    pub fn set_cookie(&self, url: &str, cookie: &str) -> EvalFuture {
        let future = self.data_request("set-cookie", url, cookie);
        if !cookie_source_valid(url, cookie) {
            *future.shared.result.borrow_mut() = Some(Err(EvalError::Unsupported));
        }
        future
    }
    /// Clear all browser data in this profile. Use a dedicated site profile to forget one site.
    /// Await completion before reusing its cookies or allowing another request to that site.
    pub fn clear_data(&self) -> EvalFuture {
        self.data_request("clear", "", "")
    }

    /// Evaluate `script` and resolve with its result as JSON text.
    ///
    /// Nothing is dispatched until the future is polled. Dropping it deregisters the request, so a
    /// late reply is discarded, but the script keeps running: no backend can cancel one.
    pub fn eval(&self, script: impl AsRef<str>) -> EvalFuture {
        EvalFuture {
            req: NEXT_REQ.with(|c| {
                let v = c.get();
                c.set(v + 1);
                v
            }),
            node: self.node,
            blocked: self.blocked,
            script: Some(wrap_script(script.as_ref())),
            shared: Rc::new(EvalShared {
                result: RefCell::new(None),
                waker: RefCell::new(None),
            }),
            sent: false,
        }
    }
}

/// The pending result of [`JsHandle::eval`].
pub struct EvalFuture {
    blocked: Signal<bool>,
    req: u64,
    node: Signal<Option<RNode>>,
    script: Option<String>,
    shared: Rc<EvalShared>,
    sent: bool,
}

impl Future for EvalFuture {
    type Output = Result<String, EvalError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if let Some(result) = self.shared.result.borrow_mut().take() {
            return Poll::Ready(result);
        }
        let Some(blocked) = day_reactive::untrack(|| self.blocked.try_get()) else {
            return Poll::Ready(Err(EvalError::ViewGone));
        };
        if blocked {
            return Poll::Ready(Err(EvalError::Unsupported));
        }
        // Dispatch on first poll, not at construction, so an eval that is never awaited never runs.
        if !self.sent {
            self.sent = true;
            if eval_support() != day_spec::Support::Native {
                return Poll::Ready(Err(EvalError::Unsupported));
            }
            let Some(node) = day_reactive::untrack(|| self.node.try_get()).flatten() else {
                return Poll::Ready(Err(EvalError::ViewGone));
            };
            let (req, script) = (self.req, self.script.take().unwrap_or_default());
            REQUEST_NODES.with(|p| p.borrow_mut().insert(req, node));
            PENDING.with(|p| p.borrow_mut().insert(req, self.shared.clone()));
            with_tree(|t| t.patch(node, Box::new(WebPatch::Eval { req, script }), false));
            // An arm that fails a request without reaching the engine replies from inside that
            // patch — before this poll has a waker to wake, so `resolve` found none and left the
            // answer here. Take it now: nothing will ever wake this future on its behalf.
            // (The XAML arm does exactly this while WebView2 is still starting up.)
            if let Some(result) = self.shared.result.borrow_mut().take() {
                return Poll::Ready(result);
            }
        }
        *self.shared.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for EvalFuture {
    fn drop(&mut self) {
        REQUEST_NODES.with(|p| p.borrow_mut().remove(&self.req));
        PENDING.with(|p| p.borrow_mut().remove(&self.req));
    }
}

/// Whether this backend can evaluate JavaScript. A separate axis from [`support`]: web-dom loads
/// pages but can evaluate only same-origin frames.
pub fn eval_support() -> day_spec::Support {
    if cfg!(any(
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(feature = "uikit", target_os = "ios"),
        feature = "qt",
        all(feature = "dom", target_arch = "wasm32"),
        all(feature = "gtk", not(target_os = "macos")),
        all(feature = "xaml", target_os = "windows"),
        // `evaluateJavascript` in DayWebView.evalJs, with the outer-JSON unquote and the
        // null/empty→engine-error mapping (docs/webview-eval.md).
        all(feature = "mdc", target_os = "android"),
        // `runJavaScript` in the ArkTS component, replying on `pieceEvent`'s num slot.
        all(feature = "arkui", target_env = "ohos"),
    )) {
        day_spec::Support::Native
    } else {
        // No renderer with a JavaScript engine on this platform.
        day_spec::Support::Unsupported
    }
}

/// A native web view bound to `url`. Attach command triggers with `.go()/.back()/.forward()/
/// .stop()/.reload()`; fire them (`Trigger::notify`) from buttons.
/// A request from a link, modifier click, or a new-window navigation. The source
/// remains alive; the application creates a separately owned browsing context.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewTabRequest {
    pub url: String,
    pub background: bool,
}
/// App-localized labels for native link menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLinkLabels {
    pub foreground: String,
    pub background: String,
}
const NEW_TAB_REPORT: f64 = -4.0;
#[cfg(any(
    all(target_os = "macos", any(feature = "appkit", feature = "gtk")),
    all(target_os = "ios", feature = "uikit"),
    feature = "qt",
    all(feature = "gtk", not(target_os = "macos"), not(windows)),
    windows
))]
fn new_tab_event(url: String, background: bool) -> Event {
    Event::Custom {
        tag: "webview:new-tab",
        num: NEW_TAB_REPORT,
        text: serde_json::to_string(&NewTabRequest { url, background }).unwrap_or_default(),
    }
}

// A document-start fallback for engines that don't expose pointer modifiers in
// navigation requests. It carries only link intent; normal navigation remains native.
#[cfg(all(target_os = "ios", feature = "uikit"))]
const TAB_MODIFIER_SCRIPT: &str = r#"(() => {
  const click = e => {
    const a = e.composedPath().find(n => n?.tagName === 'A' && n.href);
    if (!a || !(e.metaKey || e.ctrlKey || e.button === 1)) return;
    e.preventDefault(); e.stopImmediatePropagation();
    location.href = 'day-tab://open?url=' + encodeURIComponent(a.href) + '&background=' + (e.shiftKey ? '0' : '1');
  };
  addEventListener('click', click, true); addEventListener('auxclick', click, true);
})();"#;
#[cfg(all(target_os = "ios", feature = "uikit"))]
fn tab_scheme_request(input: &str) -> Option<NewTabRequest> {
    let url = url::Url::parse(input).ok()?;
    if url.scheme() != "day-tab" {
        return None;
    }
    let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
    Some(NewTabRequest {
        url: pairs.get("url")?.clone(),
        background: pairs.get("background").is_some_and(|v| v == "1"),
    })
}

/// What `.on_link(…)` stores: decides what a navigation to a URL should do.
type LinkDecider = Rc<dyn Fn(&str) -> LinkPolicy>;
type NewTabHandler = (TabLinkLabels, Rc<dyn Fn(&NewTabRequest)>);

pub struct WebView {
    profile: WebProfile,
    resources: Option<ResourceProvider>,
    url: Signal<String>,
    go: Option<Trigger>,
    back: Option<Trigger>,
    forward: Option<Trigger>,
    stop: Option<Trigger>,
    reload: Option<Trigger>,
    force_reload: Option<Trigger>,
    js: Option<JsHandle>,
    session: Option<WebSession>,
    inline: Option<InlineSite>,
    inline_start: String,
    inline_assets: bool,
    transparent: bool,
    on_link: Option<LinkDecider>,
    tabs: Option<NewTabHandler>,
    on_load: Option<Rc<dyn Fn()>>,
}

/// `web_view(url)`: a native web view showing `url`. The initial value loads on creation; call
/// `.go(trigger)` and fire the trigger to (re)load whatever `url` currently holds.
pub fn web_view(url: Signal<String>) -> WebView {
    // Self-register the web renderer. wasm has no link-time renderer slice, and a constructor is
    // the earliest point the piece is known to be in play, always before its node is realized.
    #[cfg(all(feature = "dom", target_arch = "wasm32"))]
    dom_impl::register();
    // Same earliest-point reasoning for the dayscript `web_eval` registration: a step can only
    // target a webview some constructor built, so registration here is always in time.
    register_script_eval();
    WebView {
        profile: WebProfile::default(),
        url,
        go: None,
        back: None,
        forward: None,
        stop: None,
        reload: None,
        force_reload: None,
        js: None,
        session: None,
        inline: None,
        inline_start: String::new(),
        inline_assets: false,
        transparent: false,
        on_link: None,
        tabs: None,
        on_load: None,
        resources: None,
    }
}

/// `web_view_inline(site)`: a web view showing a site bundled inside the app
/// (docs/webview.md): a directory under `resource/assets/`, shipped whole. Relative references
/// within the site resolve natively; navigations that leave it are cancelled and dispatched
/// per [`LinkPolicy`], the system browser unless [`WebView::on_external_link`] says otherwise.
///
/// Takes a prepared [`InlineSite`] (`res::assets::<dir>.prepare_site().await?`, the checked
/// route) or the raw `res::assets::<dir>` constant (lazy; a missing page surfaces in the view).
/// Gate on [`inline_support`].
pub fn web_view_inline(site: impl IntoInlineSite) -> WebView {
    let mut v = web_view(Signal::new(String::new()));
    v.inline = Some(site.into_inline_site());
    v.inline_start = "index.html".into();
    v
}

/// Display an application-provided resource tree. Relative references are resolved by the engine.
pub fn web_view_resources(provider: ResourceProvider, page: &str) -> WebView {
    let mut view = web_view(Signal::new(provider.url(page)));
    view.resources = Some(provider);
    view
}

impl WebView {
    /// Share cookies, local storage, cache and engine site data with other views using this profile.
    pub fn profile(mut self, profile: WebProfile) -> Self {
        self.profile = profile;
        self
    }
    /// Bind the actual page URL, including native navigation reports, to application state.
    /// Useful for an address bar on bundled/resource views. Notify `.go()` to load the value.
    pub fn url_binding(mut self, url: Signal<String>) -> Self {
        if url.get_untracked().is_empty() {
            url.set(self.url.get_untracked());
        }
        self.url = url;
        self
    }
    /// Called on the UI thread after the backend reports a completed navigation. Useful for
    /// applying the latest presentation state through `JsHandle`, including same-URL reloads.
    /// Available on native backends that report navigation; not on emulated iframe hosts.
    pub fn on_load(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_load = Some(Rc::new(callback));
        self
    }
    /// Load the current value of the bound `url` whenever `trigger` fires.
    pub fn go(mut self, trigger: Trigger) -> Self {
        self.go = Some(trigger);
        self
    }
    /// Navigate back in history whenever `trigger` fires.
    pub fn back(mut self, trigger: Trigger) -> Self {
        self.back = Some(trigger);
        self
    }
    /// Navigate forward in history whenever `trigger` fires.
    pub fn forward(mut self, trigger: Trigger) -> Self {
        self.forward = Some(trigger);
        self
    }
    /// Stop the current load whenever `trigger` fires.
    pub fn stop(mut self, trigger: Trigger) -> Self {
        self.stop = Some(trigger);
        self
    }
    /// Reload the current page whenever `trigger` fires.
    pub fn reload(mut self, trigger: Trigger) -> Self {
        self.reload = Some(trigger);
        self
    }
    /// Reload from the server, bypassing cached resources. Check [`force_reload_support`].
    pub fn force_reload(mut self, trigger: Trigger) -> Self {
        self.force_reload = Some(trigger);
        self
    }
    /// Bind a [`JsHandle`] so `handle.eval(…)` runs in this view (docs/webview-eval.md).
    pub fn js(mut self, handle: JsHandle) -> Self {
        self.js = Some(handle);
        self
    }
    /// Show a retained [`WebSession`], so the loaded page survives navigating away and back.
    pub fn session(mut self, session: WebSession) -> Self {
        self.session = Some(session);
        self
    }
    /// Inline mode only: the page within the site to open first, instead of `index.html`. A
    /// query or fragment is part of the page (`"player.html?src=hello"`), and the site reads it
    /// back through `location.search` / `location.hash` as it would on a server.
    pub fn start_page(mut self, page: impl Into<String>) -> Self {
        self.inline_start = page.into();
        self
    }
    /// Inline mode only: let the site read the app's whole `resource/assets/` tree, not just its
    /// own directory.
    ///
    /// A self-contained site needs nothing here. Ask for it when the page's job is to show the
    /// app's own files — a player, a viewer, a renderer — because those files live in the app's
    /// asset namespace and the site's relative URLs have to reach them
    /// (`day-piece-lottie`'s player is this: one page, any of the app's animations).
    pub fn app_assets(mut self) -> Self {
        self.inline_assets = true;
        self
    }
    /// Draw no background of the view's own: where the page's own background is transparent, the
    /// app shows through, the way an image with an alpha channel sits on whatever is behind it.
    ///
    /// What a web view is for here is content that belongs to the app's own surface (an
    /// animation player, a rendered diagram, a badge), where the view's usual white sheet reads as
    /// a hole in the window. A page that sets a background of its own still draws it. Every arm
    /// honors it (docs/webview.md "A transparent view").
    pub fn transparent(mut self) -> Self {
        self.transparent = true;
        self
    }
    /// Decide what happens to a navigation that leaves an inline/resource site. On Apple
    /// backends, also opts ordinary documents into reporting user-activated links and new
    /// windows; initial loads, subframes and same-document fragments stay in view. Runs on the
    /// main thread with the target URL; without it every external link is
    /// [`LinkPolicy::OpenSystem`]. The closure may do arbitrary in-app work (navigate the day
    /// app, log) and return [`LinkPolicy::Ignore`].
    /// Handle a request for a separate tab without replacing the source document.
    /// Labels must be localized by the application. Background requests preserve focus.
    pub fn on_new_tab(
        mut self,
        foreground_label: impl Into<String>,
        background_label: impl Into<String>,
        f: impl Fn(&NewTabRequest) + 'static,
    ) -> Self {
        self.tabs = Some((
            TabLinkLabels {
                foreground: foreground_label.into(),
                background: background_label.into(),
            },
            Rc::new(f),
        ));
        self
    }
    pub fn on_external_link(mut self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self {
        self.on_link = Some(Rc::new(f));
        self
    }
}

/// What this backend realizes. `Native` is a real embedded browser engine with the full command
/// set; `Emulated` loads pages but cannot drive history or report navigation back (web-dom's
/// `<iframe>`, see docs/webview.md); `Unsupported` renders day's placeholder leaf.
///
/// Gate history controls on this: `.back()`, `.forward()` and `.stop()` are no-ops below `Native`,
/// so an app should disable those buttons rather than offer ones that do nothing.
pub fn support() -> day_spec::Support {
    // Browser frames retain remote-navigation restrictions. Native hosts supply an engine.
    if cfg!(all(feature = "dom", target_arch = "wasm32")) {
        day_spec::Support::Emulated
    } else if cfg!(any(
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(feature = "uikit", target_os = "ios"),
        all(feature = "mdc", target_os = "android"),
        all(feature = "gtk", not(target_os = "macos")),
        feature = "qt",
        all(feature = "xaml", windows),
        all(feature = "arkui", target_env = "ohos"),
    )) {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

impl Piece for WebView {
    fn build(self, cx: &mut BuildCx) -> RNode {
        let WebView {
            profile,
            url,
            go,
            back,
            forward,
            stop,
            reload,
            force_reload,
            js,
            session,
            inline,
            inline_start,
            inline_assets,
            transparent,
            on_link,
            tabs,
            on_load,
            mut resources,
        } = self;
        let session_id = session.map(WebSession::id).unwrap_or(0);
        let session_id = if session_id == 0 || profile == WebProfile::default() {
            session_id
        } else {
            profile.storage_key().bytes().fold(session_id, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(1099511628211)
            })
        };
        let mut initial_url = url.get_untracked();
        if let (Some(_session), Some(provider)) = (session, &resources) {
            // A retained engine still needs its original namespace while detached. Like the
            // retained page itself, the first provider wins until process shutdown.
            day_core::tls_group! {
                static SESSION_RESOURCES: RefCell<HashMap<u64, (ResourceProvider, String)>> = RefCell::new(HashMap::new());
            }
            let retained = SESSION_RESOURCES.with(|m| {
                m.borrow_mut()
                    .entry(session_id)
                    .or_insert_with(|| (provider.clone(), initial_url.clone()))
                    .clone()
            });
            resources = Some(retained.0);
            initial_url = retained.1;
            url.set(initial_url.clone());
        }
        let blocked_private =
            profile.private && private_browsing_support() == day_spec::Support::Unsupported;
        let initial = WebProps {
            profile,
            resources: if blocked_private {
                None
            } else {
                resources.clone()
            },
            external_links: on_link.is_some(),
            tabs: tabs.as_ref().map(|(labels, _)| labels.clone()),
            url: if blocked_private {
                "about:blank".into()
            } else {
                initial_url
            },
            session: session_id,
            inline_root: if blocked_private {
                String::new()
            } else {
                inline.as_ref().map(|s| s.root.clone()).unwrap_or_default()
            },
            inline_start: if inline.is_some() {
                inline_start
            } else {
                String::new()
            },
            inline_assets: inline.is_some() && inline_assets,
            transparent,
        };
        // A web view has no intrinsic size; it fills whatever space its container offers.
        let node = cx.leaf(
            KIND,
            &initial,
            Flex {
                grow_w: true,
                grow_h: true,
                ..Default::default()
            },
        );

        day_reactive::Scope::current().on_cleanup(move || {
            let requests = REQUEST_NODES.with(|p| {
                p.borrow()
                    .iter()
                    .filter_map(|(req, owner)| (*owner == node).then_some(*req))
                    .collect::<Vec<_>>()
            });
            for req in requests {
                REQUEST_NODES.with(|p| p.borrow_mut().remove(&req));
                if let Some(done) = PENDING_CB.with(|p| p.borrow_mut().remove(&req)) {
                    done(Err(EvalError::ViewGone.to_string()));
                }
                if let Some(shared) = PENDING.with(|p| p.borrow_mut().remove(&req)) {
                    *shared.result.borrow_mut() = Some(Err(EvalError::ViewGone));
                    if let Some(waker) = shared.waker.borrow_mut().take() {
                        waker.wake();
                    }
                }
            }
        });
        if blocked_private {
            PRIVATE_BLOCKED_NODES.with(|nodes| nodes.borrow_mut().insert(node));
            day_reactive::Scope::current().on_cleanup(move || {
                PRIVATE_BLOCKED_NODES.with(|nodes| nodes.borrow_mut().remove(&node));
            });
        }
        let send = move |patch: WebPatch| {
            if blocked_private {
                if let WebPatch::Eval { req, .. } = patch {
                    resolve(
                        req,
                        &engine_error("Unsupported", "private browsing unavailable"),
                    );
                }
                return;
            }
            with_tree(|t| t.patch(node, Box::new(patch), false));
        };

        // Each command trigger → one patch. `watch` never fires for the initial value, so wiring
        // these does not issue a spurious command at build time (the initial URL loads via props).
        if let Some(go) = go {
            watch(
                move || go.track(),
                move |_, _| send(WebPatch::Load(url.get_untracked())),
            );
        }
        if let Some(back) = back {
            watch(move || back.track(), move |_, _| send(WebPatch::Back));
        }
        if let Some(forward) = forward {
            watch(move || forward.track(), move |_, _| send(WebPatch::Forward));
        }
        if let Some(stop) = stop {
            watch(move || stop.track(), move |_, _| send(WebPatch::Stop));
        }
        if let Some(reload) = reload {
            watch(move || reload.track(), move |_, _| send(WebPatch::Reload));
        }

        if let Some(force_reload) = force_reload {
            watch(
                move || force_reload.track(),
                move |_, _| send(WebPatch::ForceReload),
            );
        }

        // Bind the eval handle to the realized node so `handle.eval(…)` knows where to send.
        if let Some(js) = js {
            js.blocked.set(blocked_private);
            js.node.set(Some(node));
        }

        // Two kinds of report share this node's `Event::Custom` channel, told apart by `num`:
        // 0 is navigation (the URL, so a bound text field follows along), anything else is an
        // evaluation reply keyed by its request id. In-process backends also tag them, but a
        // cross-boundary Custom (JNI, C-ABI) carries only `num`/`text`, so `num` is the
        // discriminator that works everywhere (§8.2's opened event channel).
        #[cfg(all(feature = "arkui", target_env = "ohos"))]
        let registration = RefCell::new(None);
        #[cfg(all(feature = "arkui", target_env = "ohos"))]
        let resource_start = initial.url.clone();
        cx.on(node, move |ev| {
            let _keep_resources_alive = &resources;
            if let Event::Custom { num, text, .. } = ev {
                #[cfg(all(feature = "arkui", target_env = "ohos"))]
                if *num == -3.0 {
                    if let Some(provider) = &resources {
                        *registration.borrow_mut() = resources_arkui::install(provider.id(), text);
                        if registration.borrow().is_some() {
                            send(WebPatch::Load(resource_start.clone()));
                        }
                    }
                    return;
                }
                if *num >= 1.0 {
                    resolve(*num as u64, text);
                } else if *num == NEW_TAB_REPORT {
                    if let Some((_, callback)) = &tabs
                        && let Ok(request) = serde_json::from_str::<NewTabRequest>(text)
                    {
                        callback(&request);
                    }
                } else if *num == LINK_REPORT {
                    // An inline site's navigation left the site: the arm already canceled it
                    // (§8.3 events are enqueue-only, so the native side can't ask), and the
                    // policy runs here. `InView` re-issues the load as a command.
                    let policy = on_link
                        .as_ref()
                        .map(|f| f(text))
                        .unwrap_or(LinkPolicy::OpenSystem);
                    match policy {
                        LinkPolicy::OpenSystem => day_core::open_url(text),
                        LinkPolicy::InView => {
                            with_tree(|t| {
                                t.patch(node, Box::new(WebPatch::Load(text.clone())), false)
                            });
                        }
                        LinkPolicy::Ignore => {}
                    }
                } else if *num == 0.0 {
                    url.set(text.clone());
                    if let Some(callback) = &on_load {
                        callback();
                    }
                }
            }
        });
        node
    }
}

// ---------------------------------------------------------------------------
// Per-toolkit native renderers, one file per backend (this crate is a reference implementation,
// so each toolkit is split out for clarity). Each module registers a `Renderer` link-time into its
// backend's `RENDERERS` slice; `#[cfg]` gates each to its feature + target, and `#[path]` keeps the
// files grouped next to lib.rs.
// ---------------------------------------------------------------------------

#[cfg(all(any(target_os = "macos", windows), feature = "gtk"))]
mod gtk_assets;
#[cfg(all(target_os = "macos", feature = "gtk"))]
#[path = "lib-gtk-macos.rs"]
mod gtk_macos;
#[cfg(all(windows, feature = "gtk"))]
#[path = "lib-gtk-windows.rs"]
mod gtk_windows;
#[cfg(all(target_os = "macos", any(feature = "appkit", feature = "gtk")))]
mod macos_web;
#[cfg(all(windows, any(feature = "gtk", feature = "qt")))]
mod windows_web;

day_pieces::glue_modules!(appkit, qt, uikit, mdc, xaml, arkui, dom);

#[cfg(any(test, all(feature = "xaml", windows)))]
mod xaml_path;

// Linux GTK uses WebKitGTK; the other GTK hosts are registered above.
#[cfg(all(feature = "gtk", not(target_os = "macos"), not(windows)))]
#[path = "lib-gtk.rs"]
mod gtk_impl;

// --- Typed builders, forwarded through `Decorated` (docs/api-style.md) ---

/// [`WebView`]'s own builders, reachable through a decoration (§5.2): `day_pieces::Decorated` forwards them
/// to the piece it wraps, so generic modifiers and typed ones chain in any order.
pub trait WebViewBuilder: Sized {
    fn url_binding(self, url: Signal<String>) -> Self;
    fn profile(self, profile: WebProfile) -> Self;
    fn go(self, trigger: Trigger) -> Self;
    fn back(self, trigger: Trigger) -> Self;
    fn forward(self, trigger: Trigger) -> Self;
    fn stop(self, trigger: Trigger) -> Self;
    fn reload(self, trigger: Trigger) -> Self;
    fn force_reload(self, trigger: Trigger) -> Self;
    fn js(self, handle: JsHandle) -> Self;
    fn session(self, session: WebSession) -> Self;
    fn start_page(self, page: impl Into<String>) -> Self;
    fn on_external_link(self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self;
    fn on_new_tab(
        self,
        foreground: impl Into<String>,
        background: impl Into<String>,
        f: impl Fn(&NewTabRequest) + 'static,
    ) -> Self;
}

impl WebViewBuilder for WebView {
    fn on_new_tab(
        self,
        foreground: impl Into<String>,
        background: impl Into<String>,
        f: impl Fn(&NewTabRequest) + 'static,
    ) -> Self {
        WebView::on_new_tab(self, foreground, background, f)
    }
    fn url_binding(self, url: Signal<String>) -> Self {
        WebView::url_binding(self, url)
    }
    fn profile(self, profile: WebProfile) -> Self {
        WebView::profile(self, profile)
    }
    fn go(self, trigger: Trigger) -> Self {
        WebView::go(self, trigger)
    }
    fn back(self, trigger: Trigger) -> Self {
        WebView::back(self, trigger)
    }
    fn forward(self, trigger: Trigger) -> Self {
        WebView::forward(self, trigger)
    }
    fn stop(self, trigger: Trigger) -> Self {
        WebView::stop(self, trigger)
    }
    fn reload(self, trigger: Trigger) -> Self {
        WebView::reload(self, trigger)
    }
    fn force_reload(self, trigger: Trigger) -> Self {
        WebView::force_reload(self, trigger)
    }
    fn js(self, handle: JsHandle) -> Self {
        WebView::js(self, handle)
    }
    fn session(self, session: WebSession) -> Self {
        WebView::session(self, session)
    }
    fn start_page(self, page: impl Into<String>) -> Self {
        WebView::start_page(self, page)
    }
    fn on_external_link(self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self {
        WebView::on_external_link(self, f)
    }
}

impl<Inner: WebViewBuilder + day_pieces::prelude::Piece> WebViewBuilder
    for day_pieces::Decorated<Inner>
{
    fn on_new_tab(
        self,
        foreground: impl Into<String>,
        background: impl Into<String>,
        f: impl Fn(&NewTabRequest) + 'static,
    ) -> Self {
        self.map_inner(|inner| inner.on_new_tab(foreground, background, f))
    }
    fn url_binding(self, url: Signal<String>) -> Self {
        self.map_inner(|inner| inner.url_binding(url))
    }
    fn profile(self, profile: WebProfile) -> Self {
        self.map_inner(|inner| inner.profile(profile))
    }
    fn go(self, trigger: Trigger) -> Self {
        self.map_inner(|inner_piece| inner_piece.go(trigger))
    }
    fn back(self, trigger: Trigger) -> Self {
        self.map_inner(|inner_piece| inner_piece.back(trigger))
    }
    fn forward(self, trigger: Trigger) -> Self {
        self.map_inner(|inner_piece| inner_piece.forward(trigger))
    }
    fn stop(self, trigger: Trigger) -> Self {
        self.map_inner(|inner_piece| inner_piece.stop(trigger))
    }
    fn reload(self, trigger: Trigger) -> Self {
        self.map_inner(|inner_piece| inner_piece.reload(trigger))
    }
    fn force_reload(self, trigger: Trigger) -> Self {
        self.map_inner(|inner| inner.force_reload(trigger))
    }
    fn js(self, handle: JsHandle) -> Self {
        self.map_inner(|inner_piece| inner_piece.js(handle))
    }
    fn session(self, session: WebSession) -> Self {
        self.map_inner(|inner_piece| inner_piece.session(session))
    }
    fn start_page(self, page: impl Into<String>) -> Self {
        self.map_inner(|inner_piece| inner_piece.start_page(page))
    }
    fn on_external_link(self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self {
        self.map_inner(|inner_piece| inner_piece.on_external_link(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unpolled_evaluation_survives_its_tab_being_closed() {
        let scope = day_reactive::Scope::child();
        let mut future = scope.enter(|| JsHandle::new().eval("1 + 1"));
        scope.dispose();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            Pin::new(&mut future).poll(&mut context),
            Poll::Ready(Err(EvalError::ViewGone))
        ));
    }

    #[test]
    fn profiles_keep_identity_and_privacy_separate() {
        let profile = WebProfile::persistent("publication");
        assert_eq!(
            profile.storage_key(),
            WebProfile::persistent("publication").storage_key()
        );
        assert_ne!(
            profile.storage_key(),
            WebProfile::private("publication").storage_key()
        );
        assert_ne!(
            profile.storage_key(),
            WebProfile::persistent("other").storage_key()
        );
        assert_ne!(
            profile.clone().directory("/fixture/a").storage_key(),
            profile.directory("/fixture/b").storage_key()
        );
    }
    #[test]
    fn response_cookies_cannot_impersonate_another_origin() {
        assert!(cookie_source_valid(
            "https://news.example.com/story",
            "session=fixture; Domain=.example.com; Secure; HttpOnly"
        ));
        assert!(!cookie_source_valid(
            "https://news.example.co.uk/",
            "session=fixture; Domain=co.uk"
        ));
        assert!(!cookie_source_valid(
            "https://project.github.io/",
            "session=fixture; Domain=github.io"
        ));
        assert!(!cookie_source_valid(
            "https://unrelated.test/",
            "session=fixture; Domain=example.com"
        ));
        assert!(!cookie_source_valid(
            "https://example.com/",
            "session=fixture\r\nInjected: header"
        ));
        assert!(!cookie_source_valid(
            "http://example.com/",
            "session=fixture; Secure"
        ));
        assert!(!cookie_source_valid(
            "https://example.com/",
            "__Host-session=fixture; Secure; Domain=example.com; Path=/"
        ));
        assert!(cookie_source_valid(
            "https://example.com/",
            "__Host-session=fixture; Secure; Path=/"
        ));
    }
    #[test]
    fn cookie_credentials_respect_host_path_and_transport_boundaries() {
        assert!(cookie_matches(
            "https://news.example.com/account/page",
            ".example.com",
            "/account",
            true
        ));
        assert!(!cookie_matches(
            "http://news.example.com/account",
            ".example.com",
            "/account",
            true
        ));
        assert!(!cookie_matches(
            "https://evil-example.com/account",
            ".example.com",
            "/account",
            false
        ));
        assert!(!cookie_matches(
            "https://news.example.com/accounting",
            ".example.com",
            "/account",
            false
        ));
        assert!(!cookie_matches(
            "https://news.example.com/",
            "example.com",
            "/",
            false
        ));
        assert!(!cookie_matches(
            "file:///account",
            ".example.com",
            "/",
            false
        ));
        assert!(cookie_matches(
            "https://EXAMPLE.com/",
            "example.com",
            "",
            false
        ));
    }

    /// Build a reply the way an arm would, without a literal control char in the test source.
    fn reply(parts: &[&str]) -> String {
        parts.join(&SEP.to_string())
    }

    #[test]
    fn decodes_a_value() {
        assert_eq!(decode(&reply(&["1", "2"])), Ok("2".into()));
        assert_eq!(decode(&reply(&["1", "null"])), Ok("null".into()));
        let obj = r#"{"a":1,"b":"x"}"#;
        assert_eq!(decode(&reply(&["1", obj])), Ok(obj.into()));
    }

    #[test]
    fn decodes_a_throw() {
        assert_eq!(
            decode(&reply(&["0", "TypeError", "boom"])),
            Err(EvalError::Threw {
                name: "TypeError".into(),
                message: "boom".into(),
            })
        );
    }

    /// Only the first separator splits the name off, so a message carrying more stays intact.
    #[test]
    fn a_message_may_contain_the_separator() {
        assert_eq!(
            decode(&reply(&["0", "Error", "a", "b"])),
            Err(EvalError::Threw {
                name: "Error".into(),
                message: reply(&["a", "b"]),
            })
        );
    }

    /// JSON text can never hold a raw separator (`JSON.stringify` escapes control characters as
    /// six ASCII chars), so splitting on it cannot corrupt a value. This pins that.
    #[test]
    fn an_escaped_separator_inside_json_survives() {
        let json = r#""a\u001fb""#;
        assert_eq!(decode(&reply(&["1", json])), Ok(json.into()));
    }

    #[test]
    fn an_undecodable_reply_is_an_engine_error() {
        assert_eq!(decode(""), Err(EvalError::Engine(String::new())));
        // What a backend reports when the wrapper never ran at all.
        assert_eq!(decode("null"), Err(EvalError::Engine("null".into())));
    }

    /// The arms build engine failures with `engine_error`; it must decode like any other throw.
    #[test]
    fn engine_errors_round_trip() {
        assert_eq!(
            decode(&engine_error("WebKitError", "process gone")),
            Err(EvalError::Threw {
                name: "WebKitError".into(),
                message: "process gone".into(),
            })
        );
    }

    /// The script rides inside a string literal, so nothing in it can reach the wrapper's own
    /// tokens: a trailing line comment, an unbalanced brace, a quote, a newline.
    #[test]
    fn the_wrapper_is_lexically_sealed() {
        for hostile in [
            "1 + 1 // add",
            "\"unterminated",
            "}}})(){{{",
            "a\nb",
            "x\\y",
        ] {
            let js = wrap_script(hostile);
            assert!(
                js.trim_end().ends_with("})()"),
                "wrapper not closed for {hostile:?}: {js}"
            );
            assert!(
                !js.contains("\n"),
                "raw newline leaked for {hostile:?}: {js}"
            );
        }
    }

    #[test]
    fn a_start_page_keeps_its_query_out_of_the_path() {
        assert_eq!(split_page("index.html"), ("index.html", ""));
        assert_eq!(
            split_page("player.html?src=hello&loop=1"),
            ("player.html", "?src=hello&loop=1")
        );
        assert_eq!(
            split_page("docs/index.html#top"),
            ("docs/index.html", "#top")
        );
    }

    #[test]
    fn escapes_a_script_into_a_literal() {
        assert_eq!(js_string_literal("a"), "\"a\"");
        assert_eq!(js_string_literal("a\"b"), "\"a\\\"b\"");
        assert_eq!(js_string_literal("a\\b"), "\"a\\\\b\"");
        assert_eq!(js_string_literal("a\nb"), "\"a\\nb\"");
        // A literal line terminator would end the string mid-source.
        assert_eq!(js_string_literal("a\u{2028}b"), "\"a\\u2028b\"");
        assert_eq!(js_string_literal("a\u{1}b"), "\"a\\u0001b\"");
    }
}

#[cfg(test)]
mod external_link_tests {
    use super::external_document_link as external;
    #[test]
    fn user_navigation_leaves_reader_but_loading_and_anchors_do_not() {
        let page = Some("file:///tmp/fixture.html");
        assert!(external(
            page,
            "https://example.test/story",
            true,
            false,
            false
        ));
        assert!(external(
            page,
            "https://example.test/story",
            true,
            true,
            false
        ));
        assert!(external(
            page,
            "https://example.test/story",
            false,
            true,
            false
        ));
        assert!(!external(
            page,
            "file:///tmp/fixture.html#note",
            true,
            false,
            false
        ));
        assert!(!external(
            page,
            "file:///tmp/next.html",
            false,
            false,
            false
        ));
        assert!(!external(
            page,
            "https://example.test/frame",
            true,
            false,
            true
        ));
        assert!(!external(page, "about:blank", false, true, false));
        assert!(external(
            Some("https://example.test/story?a=1#x"),
            "https://example.test/story?a=2",
            true,
            false,
            false
        ));
    }
}
