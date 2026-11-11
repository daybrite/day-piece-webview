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
use std::sync::Arc;
use std::task::Poll;

use day_core::{BuildCx, Flex, Piece, RNode};
use day_pieces::{Decorated, IntoReactive, IntoText, Reactive};
use day_reactive::{AnyBinding, Binding, Signal, Trigger, bind_seeded, watch};
use day_spec::Event;

mod browsing;
mod content;
pub mod renderer;
mod resources;
pub use browsing::{
    AuthRequest, AuthResponder, BrowseProps, DialogKind, DialogRequest, DialogResponder,
    DownloadEvent, DownloadRequest, DownloadResponder, LoadError, LoadErrorKind, RendererGone,
    UserAgent, cancel_download, download_support, host_trusted, open_download, present_dialog,
    remove_download, sanitize_file_name, trust_host, unused_path,
};
pub use content::{
    BlockedLoad, ContentRuleList, ContentRulesError, Frames, InjectAt, MediaRequest, PageContent,
    RequestDecision, RequestFilter, RequestHooks, RequestInfo, ResourceKind, ScriptMessage,
    UserScript, blocked_report_support, content_rules_support, request_filter_support,
    script_message_support, user_script_support,
};
pub use resources::{ResourceProvider, ResourceRequest, ResourceResponse};

#[cfg(all(
    any(target_os = "macos", target_os = "ios"),
    any(feature = "appkit", feature = "uikit", feature = "gtk")
))]
mod resources_apple;

#[cfg(all(feature = "arkui", target_env = "ohos"))]
mod resources_arkui;

#[cfg(any(test, all(feature = "arkui", target_env = "ohos")))]
mod resource_content_type;

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
        all(feature = "material", target_os = "android"),
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
mod browsing_apple;
#[cfg(any(
    all(any(feature = "appkit", feature = "gtk"), target_os = "macos"),
    all(feature = "uikit", target_os = "ios")
))]
mod content_apple;
#[cfg(any(
    all(any(feature = "appkit", feature = "gtk"), target_os = "macos"),
    all(feature = "uikit", target_os = "ios")
))]
mod storage_apple;
/// For `tests/content-rules-macos.rs` only: WebKit content rule lists on a real view.
#[cfg(all(any(feature = "appkit", feature = "gtk"), target_os = "macos"))]
#[doc(hidden)]
pub use content_apple::probe as __webkit_content_probe;
/// Named profile isolation. ArkWeb exposes app-wide stores only; its default and private
/// contexts share data by origin, but do not implement independent named profiles.
pub fn profile_support() -> day_spec::Support {
    #[cfg(all(feature = "material", target_os = "android"))]
    let result = android_impl::capability("supportsProfiles");
    #[cfg(not(all(feature = "material", target_os = "android")))]
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
    #[cfg(all(feature = "material", target_os = "android"))]
    let result = android_impl::capability("supportsDataClearing");
    #[cfg(not(all(feature = "material", target_os = "android")))]
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
    pub renderer: WebRenderer,
    pub profile: WebProfile,
    pub resources: Option<ResourceProvider>,
    /// The view shows the provider's own site ([`web_view_resources`]): a main-frame navigation
    /// that leaves `resources`' base is cancelled and reported (`LINK_REPORT`), like an inline
    /// site's. `false` with a provider ([`WebView::resources`]) registers the namespace and
    /// otherwise browses like any remote view.
    pub resource_site: bool,
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
    /// Report the page's scroll position as it moves ([`WebView::scroll_state`]). Off unless the
    /// view has a binding for it, so no view sends a report per frame of scrolling for nothing.
    pub scroll_reports: bool,
    /// Page zoom, desktop-site requests and page scripts, as they stand when the view is
    /// created ([`WebView::zoom`], [`WebView::desktop_mode`], [`WebView::javascript`]). Later
    /// changes arrive as [`WebPatch::Zoom`], [`WebPatch::DesktopMode`] and
    /// [`WebPatch::JavaScript`].
    pub settings: WebSettings,
    /// Request filtering, content rule lists, user scripts and styles, and page message
    /// handlers ([`WebView::request_filter`], [`WebView::content_rules`],
    /// [`WebView::user_script`], [`WebView::on_script_message`]). Rule list changes arrive as
    /// [`WebPatch::ContentRules`].
    pub content: PageContent,
    /// Which browser duties the app takes on, and edge-swipe navigation
    /// ([`WebView::on_auth`], [`WebView::on_download`], [`WebView::on_renderer_gone`],
    /// [`WebView::swipe_navigation`]).
    pub browse: BrowseProps,
    /// What the view tells sites it is ([`WebView::user_agent`]), fixed when the view is
    /// created.
    pub user_agent: UserAgent,
}

/// Per-view browsing settings: what [`WebView::zoom`], [`WebView::desktop_mode`] and
/// [`WebView::javascript`] bind. The default is what a view does without them: 100%, the
/// engine's choice of site layout, and page scripts on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WebSettings {
    /// Page zoom factor, `1.0` for 100%, within [`WebSettings::ZOOM_RANGE`]. The backend keeps it
    /// across navigations.
    pub zoom: f64,
    /// Ask sites for the layout they serve a desktop browser.
    pub desktop_mode: bool,
    /// Scripts that pages load or contain run.
    pub javascript: bool,
}

impl WebSettings {
    /// The zoom factors every engine accepts: QtWebEngine and WebView2 both stop at 25% and
    /// 500%. Safari's steps run from 50% to 300%.
    pub const ZOOM_RANGE: std::ops::RangeInclusive<f64> = 0.25..=5.0;

    /// `zoom` within [`WebSettings::ZOOM_RANGE`]; `1.0` for a value that is not a positive
    /// number.
    pub fn clamp_zoom(zoom: f64) -> f64 {
        if zoom.is_finite() && zoom > 0.0 {
            zoom.clamp(*Self::ZOOM_RANGE.start(), *Self::ZOOM_RANGE.end())
        } else {
            1.0
        }
    }
}

impl Default for WebSettings {
    fn default() -> Self {
        WebSettings {
            zoom: 1.0,
            desktop_mode: false,
            javascript: true,
        }
    }
}

/// A desktop browser's user agent for a Chromium engine (Android WebView, ArkWeb) whose user
/// agent is `mobile`: Chrome's "Request desktop site" string, at the engine's major version.
/// The Java (`DayWebView.desktopAgent`) and ArkTS (`desktopAgent`) arms compose this text
/// themselves; this is the reference their format is pinned against.
#[cfg(test)]
pub(crate) fn desktop_user_agent(mobile: &str) -> String {
    let major = mobile
        .split_once("Chrome/")
        .map(|(_, rest)| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|major| !major.is_empty())
        .unwrap_or_else(|| "120".into());
    format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{major}.0.0.0 Safari/537.36"
    )
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
        all(feature = "material", target_os = "android"),
        // QWebEngine reads qrc assets.
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

/// Whether [`WebView::zoom`] scales the page. `Native` uses the engine's own page zoom (text
/// zoom on Android and ArkWeb, see docs/webview.md). `Emulated` sets CSS `zoom` on each page
/// instead: web-dom's iframe can only do that for same-origin pages, and the WinUI WebView2
/// control does not expose the engine's zoom factor.
pub fn zoom_support() -> day_spec::Support {
    if cfg!(any(
        all(feature = "dom", target_arch = "wasm32"),
        all(feature = "winui", windows)
    )) {
        day_spec::Support::Emulated
    } else {
        support()
    }
}

/// Whether [`WebView::desktop_mode`] is honored. Engines on phones and tablets switch to a
/// desktop user agent and layout; desktop engines already present themselves as desktop
/// browsers and accept the setting as it is. web-dom's iframe cannot change what the host
/// browser sends.
pub fn desktop_mode_support() -> day_spec::Support {
    if cfg!(all(feature = "dom", target_arch = "wasm32")) {
        day_spec::Support::Unsupported
    } else {
        support()
    }
}

/// Whether [`WebView::javascript`] can stop page scripts. `Emulated` on web-dom, which sandboxes
/// the iframe without `allow-scripts` and so also blocks the app's own evaluation in the frame.
pub fn javascript_support() -> day_spec::Support {
    if cfg!(all(feature = "dom", target_arch = "wasm32")) {
        day_spec::Support::Emulated
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
    /// Report the page state now, even when it has not changed since the last report. Sent once,
    /// a turn after the view is built, so a node re-attaching a retained session learns where
    /// the settled page stands. A report made while the view was being created would reach a
    /// node whose handler is not installed yet and be lost. Renderers that never retain a view
    /// ignore it: a fresh view reports as its first load proceeds.
    ReportState,
    /// Zoom the page to this factor ([`WebView::zoom`]), already within
    /// [`WebSettings::ZOOM_RANGE`]. Applies at once and to later pages.
    Zoom(f64),
    /// Ask for desktop (`true`) or the engine's default layout ([`WebView::desktop_mode`]). The
    /// backend reloads the current page so the site can answer.
    DesktopMode(bool),
    /// Let page scripts run, or stop them ([`WebView::javascript`]). The backend reloads the
    /// current page, since a page's scripts cannot be turned on or off while it runs.
    JavaScript(bool),
    /// Replace the view's content rule lists ([`WebView::content_rules`]). The backend compiles
    /// what it has not compiled before, off the UI thread, and swaps the lists once all are ready.
    ContentRules(Vec<ContentRuleList>),
    /// Answer the page message the backend reported under `id` (`SCRIPT_MESSAGE`): the
    /// handler's reply, or `None` for none. The page's `dayMessage` promise resolves with it.
    ScriptReply {
        id: u64,
        reply: Option<String>,
    },
    /// Answer the camera and microphone request the backend reported under `id`
    /// (`MEDIA_REPORT`) with what the app allows ([`WebView::on_media_request`]).
    MediaDecision {
        id: u64,
        allow: bool,
    },
    /// Answer the question the backend reported under `id` (`PAGE_PROMPT`): a script dialog, a
    /// sign-in, a file choice or a download's destination. `answer` is the JSON of the
    /// crate's `browsing::Answer`.
    PromptAnswer {
        id: u64,
        answer: String,
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
    // A page that refuses `eval` refuses every string, so a harmless one is tried first: its
    // failure is the refusal, reported under a name no script throws ([`EVAL_REFUSED`]) and
    // retried in the second form. The script itself then reaches `eval` exactly as written
    // (a leading directive, its completion value and its own names are all its own), and
    // whatever it throws, even an `EvalError`, is its error from its one run.
    format!(
        "(function(){{var v;\
         try{{eval(\"0\");}}\
         catch(e){{return \"0\\u001f{EVAL_REFUSED}\\u001f\"+((e&&e.message)||String(e));}}\
         try{{v=eval({src});}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"Error\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         try{{var s=JSON.stringify(v);return \"1\\u001f\"+(s===undefined?\"null\":s);}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"TypeError\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         }})()"
    )
}

/// The error name [`wrap_script`] reports when the page refused `eval` before the script ran.
const EVAL_REFUSED: &str = "DayEvalRefused";

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
/// The wrapper for a page that refuses `eval`: the script is part of the wrapper's own source,
/// as the expression it then has to be. A page's Content Security Policy without `unsafe-eval`,
/// or with Trusted Types required, stops `eval(text)` inside the page even though the engine
/// runs the wrapper itself; such a page answers [`wrap_script`]'s form with [`EVAL_REFUSED`],
/// and the request is sent once more in this form ([`resolve`]). A script that is a list of
/// statements cannot be written this way and fails to parse, which is the error the caller
/// then sees. The line breaks keep a trailing `//` comment from swallowing the wrapper.
fn wrap_expression(script: &str) -> String {
    format!(
        "(function(){{var v;\
         try{{v=(\n{script}\n);}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"Error\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         try{{var s=JSON.stringify(v);return \"1\\u001f\"+(s===undefined?\"null\":s);}}\
         catch(e){{return \"0\\u001f\"+((e&&e.name)||\"TypeError\")+\"\\u001f\"+((e&&e.message)||String(e));}}\
         }})()"
    )
}

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
    /// Each request's second form ([`wrap_expression`]), kept until its first answer.
    static FALLBACKS: RefCell<HashMap<u64, String>> = RefCell::new(HashMap::new());
}

/// Deliver a reply to whichever request is waiting on `req`: an awaited [`EvalFuture`] or a
/// `web_eval` step callback. A reply for a dropped future finds nothing pending and is
/// discarded.
fn resolve(req: u64, payload: &str) {
    // A page that refuses `eval` gets the request once more with the script as source.
    let fallback = FALLBACKS.with(|f| f.borrow_mut().remove(&req));
    let refused =
        matches!(decode(payload), Err(EvalError::Threw { ref name, .. }) if name == EVAL_REFUSED);
    if refused
        && let Some(script) = fallback
        && let Some(node) = REQUEST_NODES.with(|p| p.borrow().get(&req).copied())
    {
        renderer::send_patch(node, WebPatch::Eval { req, script });
        return;
    }
    // Refused, with no second form left: the caller hears it under the engine's own name.
    let renamed;
    let payload = if refused {
        renamed = payload.replacen(EVAL_REFUSED, "EvalError", 1);
        renamed.as_str()
    } else {
        payload
    };
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
    let handler: day_core::PieceOperationFn = std::rc::Rc::new(|node, script, done| {
        if PRIVATE_BLOCKED_NODES.with(|nodes| nodes.borrow().contains(&node))
            || (!renderer::surface::owns(node) && eval_support() != day_spec::Support::Native)
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
        FALLBACKS.with(|f| f.borrow_mut().insert(req, wrap_expression(script)));
        PENDING_CB.with(|p| p.borrow_mut().insert(req, done));
        renderer::send_patch(
            node,
            WebPatch::Eval {
                req,
                script: wrapped,
            },
        );
    });
    day_core::register_piece_operation(KIND, "day.webview.eval", handler.clone());
    day_core::register_piece_operation(
        renderer::surface::KIND,
        "day.webview.eval",
        Rc::new(move |node, script, done| {
            if renderer::surface::owns(node) {
                handler(node, script, done);
            } else {
                done(Err(EvalError::Unsupported.to_string()));
            }
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

/// The live state of the page a view shows, kept current by the engine's own notifications
/// ([`WebView::page_state`]). Unlike [`JsHandle::navigation_state`] nothing is polled: each
/// backend reports a change when its engine announces one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageState {
    /// The engine's current URL, including same-document changes (`history.pushState`, a
    /// fragment). A provider-served page reports its provider URL.
    pub url: String,
    /// The document title, or empty before the page sets one.
    pub title: String,
    /// A main-frame load is in progress.
    pub loading: bool,
    /// Estimated load progress from `0.0` to `1.0`, in hundredths. It reaches `1.0` when a load
    /// finishes. Engines without a progress estimate step through fixed points (docs/webview.md).
    pub progress: f64,
    /// Session history has an entry behind the current one.
    pub can_go_back: bool,
    /// Session history has an entry ahead of the current one.
    pub can_go_forward: bool,
}

/// The `num` that tags a page-state report on the shared `Event::Custom` channel.
const PAGE_STATE_REPORT: f64 = -5.0;

/// Encode a page-state report: `<loading><back><forward>␟<progress>␟<url>␟<title>`, each flag a
/// `0`/`1` digit and the progress a decimal fraction. The title goes last so that a title holding
/// the separator still decodes; a URL never holds a raw control character. Java, C++ and ArkTS
/// compose the same text, so the format stays this plain.
// The Rust arms call this; a build whose arm composes the text natively compiles it out.
#[allow(dead_code)]
pub(crate) fn encode_page_state(state: &PageState) -> String {
    let flag = |b: bool| if b { '1' } else { '0' };
    format!(
        "{}{}{}{SEP}{:.2}{SEP}{}{SEP}{}",
        flag(state.loading),
        flag(state.can_go_back),
        flag(state.can_go_forward),
        state.progress.clamp(0.0, 1.0),
        state.url,
        state.title
    )
}

/// A page-state report as an event, for the arms that emit from Rust.
#[allow(dead_code)]
pub(crate) fn page_state_event(state: &PageState) -> Event {
    Event::Custom {
        tag: "webview:state",
        num: PAGE_STATE_REPORT,
        text: encode_page_state(state),
    }
}

/// Where the page is scrolled, kept current as it moves ([`WebView::scroll_state`]): what a
/// browser's chrome reads to slide its bars away while the user scrolls down a page, as Safari
/// and Chrome do, and back when they scroll up or reach either end.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollState {
    /// How far the page is scrolled down, in points from its top: `0` at the top. It can go
    /// below `0` or past the end while the platform's scroll bounces.
    pub offset: f64,
    /// The page's full height, in points.
    pub content_height: f64,
    /// The height of the part of the page the view shows, in points.
    pub viewport_height: f64,
    /// The user's finger is on the page (a drag, not a fling or a programmatic scroll).
    pub dragging: bool,
}

impl ScrollState {
    /// The page is at its top, or bounced above it.
    pub fn at_top(&self) -> bool {
        self.offset <= 0.5
    }

    /// The page is scrolled to its end, or bounced past it.
    pub fn at_bottom(&self) -> bool {
        self.content_height > self.viewport_height
            && self.offset + self.viewport_height >= self.content_height - 0.5
    }
}

/// The `num` that tags a scroll report on the shared `Event::Custom` channel.
const SCROLL_REPORT: f64 = -6.0;

/// The `num` that tags a blocked-load report: a JSON array of [`BlockedLoad`]s (or one).
const BLOCKED_REPORT: f64 = -7.0;
/// The `num` that tags a page message: a JSON [`content::MessageReport`].
const SCRIPT_MESSAGE: f64 = -8.0;
/// The `num` that tags a content rule list that failed to compile: a JSON [`ContentRulesError`].
const CONTENT_RULES_ERROR: f64 = -9.0;
/// The `num` that tags a camera or microphone request: a JSON [`content::MediaReport`].
const MEDIA_REPORT: f64 = -10.0;

/// A blocked-load report as an event, for arms that report through the event channel.
#[allow(dead_code)]
pub(crate) fn blocked_event(loads: &[BlockedLoad]) -> Event {
    Event::Custom {
        tag: "webview:blocked",
        num: BLOCKED_REPORT,
        text: serde_json::to_string(loads).unwrap_or_default(),
    }
}

/// A page message as an event: its reply goes back as [`WebPatch::ScriptReply`] with `id`.
#[allow(dead_code)]
pub(crate) fn script_message_event(id: u64, message: ScriptMessage) -> Event {
    Event::Custom {
        tag: "webview:message",
        num: SCRIPT_MESSAGE,
        text: serde_json::to_string(&content::MessageReport { id, message }).unwrap_or_default(),
    }
}

/// A content rule list compile failure as an event.
#[allow(dead_code)]
pub(crate) fn content_rules_error_event(id: &str, message: &str) -> Event {
    Event::Custom {
        tag: "webview:rules",
        num: CONTENT_RULES_ERROR,
        text: serde_json::to_string(&ContentRulesError {
            id: id.to_owned(),
            message: message.to_owned(),
        })
        .unwrap_or_default(),
    }
}

/// A camera or microphone request as an event: the answer goes back as
/// [`WebPatch::MediaDecision`] with `id`.
#[allow(dead_code)]
pub(crate) fn media_event(id: u64, request: MediaRequest) -> Event {
    Event::Custom {
        tag: "webview:media",
        num: MEDIA_REPORT,
        text: serde_json::to_string(&content::MediaReport { id, request }).unwrap_or_default(),
    }
}

/// Encode a scroll report: `<offset>␟<content height>␟<viewport height>␟<dragging>`, in points
/// rounded to tenths, the flag a `0`/`1` digit. Java composes the same text.
#[allow(dead_code)]
pub(crate) fn encode_scroll_state(state: &ScrollState) -> String {
    format!(
        "{:.1}{SEP}{:.1}{SEP}{:.1}{SEP}{}",
        state.offset,
        state.content_height,
        state.viewport_height,
        if state.dragging { '1' } else { '0' }
    )
}

/// A scroll report as an event, for the arms that emit from Rust.
#[allow(dead_code)]
pub(crate) fn scroll_state_event(state: &ScrollState) -> Event {
    Event::Custom {
        tag: "webview:scroll",
        num: SCROLL_REPORT,
        text: encode_scroll_state(state),
    }
}

fn decode_scroll_state(text: &str) -> Option<ScrollState> {
    let mut fields = text.split(SEP);
    let mut number = || -> Option<f64> {
        let v: f64 = fields.next()?.trim().parse().ok()?;
        v.is_finite().then_some(v)
    };
    let (offset, content_height, viewport_height) = (number()?, number()?, number()?);
    let dragging = match fields.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some(ScrollState {
        offset,
        content_height,
        viewport_height,
        dragging,
    })
}

/// Decode [`encode_page_state`]'s text. Progress is rounded to hundredths, so a stream of tiny
/// estimate changes collapses into equal states that the front-end does not re-publish.
fn decode_page_state(text: &str) -> Option<PageState> {
    let mut fields = text.splitn(4, SEP);
    let flags = fields.next()?.as_bytes();
    let progress: f64 = fields.next()?.trim().parse().ok()?;
    let url = fields.next()?.to_owned();
    let title = fields.next().unwrap_or_default().to_owned();
    if flags.len() != 3 || !progress.is_finite() {
        return None;
    }
    Some(PageState {
        url,
        title,
        loading: flags[0] == b'1',
        progress: (progress.clamp(0.0, 1.0) * 100.0).round() / 100.0,
        can_go_back: flags[1] == b'1',
        can_go_forward: flags[2] == b'1',
    })
}

/// Decode standard base64 (with or without padding; whitespace ignored), as every engine's
/// snapshot reply encodes its PNG. `None` on any other character.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A handle for running JavaScript in a web view. `Copy`, like `Trigger`, so it can be captured by
/// several closures. Bind it with [`WebView::js`]; evaluating before the view is realized fails
/// with [`EvalError::ViewGone`].
#[derive(Clone, Copy)]
pub struct JsHandle {
    node: Signal<Option<RNode>>,
    blocked: Signal<bool>,
    renderer: Signal<WebRenderer>,
}

impl Default for JsHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl JsHandle {
    /// Engine currently attached to this handle.
    pub fn renderer(&self) -> WebRenderer {
        self.renderer.get_untracked()
    }
    #[track_caller]
    pub fn new() -> Self {
        JsHandle {
            node: Signal::new(None),
            blocked: Signal::new(false),
            renderer: Signal::new(WebRenderer::default()),
        }
    }

    fn data_request(&self, operation: &str, url: &str, cookie: &str) -> EvalFuture {
        self.data_request_value(
            serde_json::json!({"operation":operation,"url":url,"cookie":cookie}),
        )
    }
    fn data_request_value(&self, request: serde_json::Value) -> EvalFuture {
        let mut future = self.eval("");
        future.script = Some(format!("{DATA_REQUEST}{request}"));
        // A request to the backend is no page script and has no second form.
        future.fallback = None;
        future
    }
    /// Capture the visible page as PNG bytes, at most `max_width_px` physical pixels wide (the
    /// height follows the view's aspect ratio). Meant for thumbnails such as a tab overview.
    ///
    /// The capture is the engine's own rendering of what the view shows, not a re-render of the
    /// document. A view that is not on screen (a background tab) may capture blank or fail with
    /// [`EvalError::Threw`], depending on the engine. Engines without a capture API (ArkWeb,
    /// web-dom's iframe) fail with [`EvalError::Unsupported`]; see docs/webview.md.
    pub async fn snapshot(&self, max_width_px: u32) -> Result<Vec<u8>, EvalError> {
        let reply = self
            .data_request_value(serde_json::json!({"operation":"snapshot","width":max_width_px}))
            .await
            .map_err(|error| match error {
                EvalError::Threw { name, .. } if name == "Unsupported" => EvalError::Unsupported,
                error => error,
            })?;
        let encoded: String =
            serde_json::from_str(&reply).map_err(|_| EvalError::Engine(reply.clone()))?;
        base64_decode(&encoded).ok_or(EvalError::Engine(reply))
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
            fallback: Some(wrap_expression(script.as_ref())),
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
    /// The script in [`wrap_expression`]'s form, for a page that refuses `eval`.
    fallback: Option<String>,
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
            let Some(node) = day_reactive::untrack(|| self.node.try_get()).flatten() else {
                return Poll::Ready(Err(EvalError::ViewGone));
            };
            if !renderer::surface::owns(node) && eval_support() != day_spec::Support::Native {
                return Poll::Ready(Err(EvalError::Unsupported));
            }
            let (req, script) = (self.req, self.script.take().unwrap_or_default());
            REQUEST_NODES.with(|p| p.borrow_mut().insert(req, node));
            if let Some(fallback) = self.fallback.take() {
                FALLBACKS.with(|f| f.borrow_mut().insert(req, fallback));
            }
            PENDING.with(|p| p.borrow_mut().insert(req, self.shared.clone()));
            renderer::send_patch(node, WebPatch::Eval { req, script });
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
        FALLBACKS.with(|f| f.borrow_mut().remove(&self.req));
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
        all(feature = "material", target_os = "android"),
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
type MessageHandler = Rc<dyn Fn(ScriptMessage) -> Option<String>>;
type RulesErrorHandler = Rc<dyn Fn(&ContentRulesError)>;
type MediaHandler = Rc<dyn Fn(&MediaRequest) -> bool>;

pub struct WebView {
    renderer: WebRenderer,
    profile: WebProfile,
    resources: Option<ResourceProvider>,
    resource_site: bool,
    page_state: Option<AnyBinding<PageState>>,
    scroll_state: Option<AnyBinding<ScrollState>>,
    url: AnyBinding<String>,
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
    zoom: Option<Reactive<f64>>,
    desktop_mode: Option<Reactive<bool>>,
    javascript: Option<Reactive<bool>>,
    request_filter: Option<Arc<dyn RequestFilter>>,
    content_rules: Option<Reactive<Vec<ContentRuleList>>>,
    user_scripts: Vec<UserScript>,
    user_styles: Vec<Arc<str>>,
    messages: Vec<(String, MessageHandler)>,
    on_blocked: Option<content::BlockedCallback>,
    on_rules_error: Option<RulesErrorHandler>,
    on_media: Option<MediaHandler>,
    on_app_link: Option<LinkDecider>,
    browse: browsing::Handlers,
    swipe_navigation: bool,
    user_agent: UserAgent,
}

/// `web_view(url)`: a native web view showing `url`. The initial value loads on creation; call
/// `.go(trigger)` and fire the trigger to (re)load whatever `url` currently holds.
pub fn web_view(url: impl Binding<String>) -> WebView {
    // Self-register the web renderer. wasm has no link-time renderer slice, and a constructor is
    // the earliest point the piece is known to be in play, always before its node is realized.
    #[cfg(all(feature = "dom", target_arch = "wasm32"))]
    dom_impl::register();
    // Same earliest-point reasoning for the dayscript `web_eval` registration: a step can only
    // target a webview some constructor built, so registration here is always in time.
    register_script_eval();
    WebView {
        renderer: WebRenderer::default(),
        profile: WebProfile::default(),
        url: AnyBinding::new(url),
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
        resource_site: false,
        page_state: None,
        scroll_state: None,
        zoom: None,
        desktop_mode: None,
        javascript: None,
        request_filter: None,
        content_rules: None,
        user_scripts: Vec::new(),
        user_styles: Vec::new(),
        messages: Vec::new(),
        on_blocked: None,
        on_rules_error: None,
        on_media: None,
        on_app_link: None,
        browse: browsing::Handlers::default(),
        swipe_navigation: false,
        user_agent: UserAgent::Engine,
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
    view.resource_site = true;
    view
}

#[day_pieces::builder(WebViewBuilder)]
impl WebView {
    /// Select an engine for this context, failing instead of falling back silently.
    /// Rebuild the piece at its current URL to switch an existing page's renderer.
    pub fn renderer(mut self, renderer: WebRenderer) -> Result<Self, RendererUnavailable> {
        if !renderer.available() {
            return Err(RendererUnavailable(renderer));
        }
        self.renderer = renderer;
        Ok(self)
    }
    /// Share cookies, local storage, cache and engine site data with other views using this profile.
    pub fn profile(mut self, profile: WebProfile) -> Self {
        self.profile = profile;
        self
    }
    /// Bind the actual page URL, including native navigation reports, to application state.
    /// Useful for an address bar on bundled/resource views. Notify `.go()` to load the value.
    pub fn url_binding(mut self, url: impl Binding<String>) -> Self {
        let url = AnyBinding::new(url);
        if url.get_untracked().is_empty() {
            url.set(self.url.get_untracked());
        }
        self.url = url;
        self
    }
    /// Keep `state` current with the page's URL, title, load progress and history, as the
    /// engine reports them (see [`PageState`]). The view writes the binding; the app reads it to
    /// drive a title, a progress line, or Back/Forward buttons.
    ///
    /// Reports are pushed by the engine's own notifications, not polled, and those arriving
    /// within one frame (a burst of progress estimates) are coalesced into one write. The
    /// binding is only written when the state changed. web-dom's iframe reports only the URL and
    /// title of same-origin frames after each load.
    pub fn page_state(mut self, state: impl Binding<PageState>) -> Self {
        self.page_state = Some(AnyBinding::new(state));
        self
    }
    /// Keep `state` current with where the page is scrolled (see [`ScrollState`]): what a
    /// browser's chrome reads to slide its bars away as the user scrolls down, the way Safari
    /// and Chrome do on a phone. Reports arriving within one frame are coalesced into one write.
    ///
    /// Reported on iOS (the web view's scroll view) and Android (the view's scroll listener),
    /// the platforms whose browsers hide their bars; elsewhere the binding keeps its value.
    pub fn scroll_state(mut self, state: impl Binding<ScrollState>) -> Self {
        self.scroll_state = Some(AnyBinding::new(state));
        self
    }
    /// Serve `provider`'s namespace in this view, which otherwise browses like any remote view:
    /// it navigates anywhere, reports URLs, opens new tabs, and keeps its history across
    /// provider pages and remote ones. Pages are reached at [`ResourceProvider::url`] (or under
    /// [`ResourceProvider::base_url`]); a query reaches the handler as
    /// [`ResourceRequest::query`].
    ///
    /// [`web_view_resources`] is the other shape: a view confined to the provider's site, whose
    /// links out of it go through [`WebView::on_external_link`]. A provider given here replaces
    /// any provider the view already had.
    pub fn resources(mut self, provider: ResourceProvider) -> Self {
        self.resources = Some(provider);
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
    /// Zoom the page by `factor`, `1.0` being 100%: a constant, a signal or a closure. The engine
    /// keeps the factor as the view navigates, the way a browser's zoom control does, and a
    /// change applies to the page on screen at once. Factors are clamped to
    /// [`WebSettings::ZOOM_RANGE`]. Check [`zoom_support`]: Android and ArkWeb scale text, and
    /// web-dom zooms same-origin pages only (docs/webview.md).
    pub fn zoom<M>(mut self, factor: impl IntoReactive<f64, M>) -> Self {
        self.zoom = Some(factor.into_reactive());
        self
    }
    /// Ask sites for the layout they serve a desktop browser, like Safari's "Request Desktop
    /// Website" and Chrome's "Desktop site". When the value changes, the view reloads its page so
    /// the site can answer. `false` leaves the engine's choice, which is already the desktop
    /// layout on a Mac, an iPad and every desktop toolkit. Check [`desktop_mode_support`].
    pub fn desktop_mode<M>(mut self, desktop: impl IntoReactive<bool, M>) -> Self {
        self.desktop_mode = Some(desktop.into_reactive());
        self
    }
    /// Let pages run their scripts (`true`, the default) or stop them. When the value changes,
    /// the view reloads its page, because a page cannot lose or gain its scripts while it runs.
    ///
    /// The app's own evaluation ([`JsHandle::eval`]) keeps working with page scripts off on the
    /// WebKit, WebKitGTK, Qt, WebView2 and ArkWeb engines. Android WebView and web-dom have no
    /// such split: there, evaluation fails until scripts are on again. Check
    /// [`javascript_support`] and docs/webview.md.
    pub fn javascript<M>(mut self, enabled: impl IntoReactive<bool, M>) -> Self {
        self.javascript = Some(enabled.into_reactive());
        self
    }
    /// Ask `filter` about every request the view's pages make, before it leaves the device
    /// (docs/webview.md, "Content blocking and page scripts"). A [`RequestDecision::Block`]
    /// fails the request; [`WebView::on_blocked`] hears about it.
    ///
    /// Runs on the engine's own thread: a WebView worker on Android, the UI thread on WebView2
    /// and Qt, ArkWeb's IO thread. Check [`request_filter_support`]: WebKit engines have no
    /// per-request hook and take [`WebView::content_rules`] instead.
    pub fn request_filter(mut self, filter: Arc<dyn RequestFilter>) -> Self {
        self.request_filter = Some(filter);
        self
    }
    /// Hear about the requests [`WebView::request_filter`] blocked, on the UI thread, in
    /// batches: at most one call every 100 ms, holding every load blocked since the last one.
    pub fn on_blocked(mut self, f: impl Fn(&[BlockedLoad]) + 'static) -> Self {
        self.on_blocked = Some(Rc::new(f));
        self
    }
    /// Block and hide content with WebKit content rule lists: a constant, a signal or a
    /// closure. Each list is compiled once, off the UI thread, and kept compiled across launches
    /// under its id and version; a change swaps the view's lists once the new ones are ready.
    /// A view built with lists holds its first page until they are attached. Check
    /// [`content_rules_support`]: only the WebKit engines apply them.
    pub fn content_rules<M>(mut self, lists: impl IntoReactive<Vec<ContentRuleList>, M>) -> Self {
        self.content_rules = Some(lists.into_reactive());
        self
    }
    /// Hear about a content rule list that failed to compile (it is also logged). The other
    /// lists still apply.
    pub fn on_content_rules_error(mut self, f: impl Fn(&ContentRulesError) + 'static) -> Self {
        self.on_rules_error = Some(Rc::new(f));
        self
    }
    /// Run `script` in every page the view loads, in the page's own JavaScript world. Call it
    /// again for more; they run in the order added. Check [`user_script_support`].
    pub fn user_script(mut self, script: UserScript) -> Self {
        self.user_scripts.push(script);
        self
    }
    /// Apply `css` to every page and frame the view loads, as a user style sheet where the engine
    /// has one (WebKitGTK) and as a `<style>` element added at document start elsewhere.
    pub fn user_style(mut self, css: impl Into<Arc<str>>) -> Self {
        self.user_styles.push(css.into());
        self
    }
    /// Answer page messages posted to `name`: a page calls `window.dayMessage(name, text)` and
    /// gets a promise of the handler's reply (`null` for `None`). The handler runs on the UI
    /// thread. Any frame can post, third-party ones included: treat [`ScriptMessage::payload`]
    /// as untrusted input. Check [`script_message_support`].
    pub fn on_script_message(
        mut self,
        name: impl Into<String>,
        handler: impl Fn(ScriptMessage) -> Option<String> + 'static,
    ) -> Self {
        let name = name.into();
        self.messages.retain(|(known, _)| *known != name);
        self.messages.push((name, Rc::new(handler)));
        self
    }
    /// Decide whether a page may use the camera or microphone (`getUserMedia`). Android only
    /// asks this: without it, a page gets what the app itself may use, the system asking the
    /// user once for a permission the app declares but does not hold yet. The other engines
    /// ask the user themselves.
    pub fn on_media_request(mut self, f: impl Fn(&MediaRequest) -> bool + 'static) -> Self {
        self.on_media = Some(Rc::new(f));
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
    pub fn on_new_tab<M1, M2>(
        mut self,
        foreground_label: impl IntoText<M1>,
        background_label: impl IntoText<M2>,
        f: impl Fn(&NewTabRequest) + 'static,
    ) -> Self {
        self.tabs = Some((
            TabLinkLabels {
                foreground: foreground_label.into_text().initial(),
                background: background_label.into_text().initial(),
            },
            Rc::new(f),
        ));
        self
    }
    pub fn on_external_link(mut self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self {
        self.on_link = Some(Rc::new(f));
        self
    }
    /// Decide what happens to a navigation whose address another program opens: `mailto:`,
    /// `tel:`, an app's own scheme, Android's `intent:`. The view never loads such an address;
    /// without this (and without [`WebView::on_external_link`]) each goes to the system, which
    /// is [`LinkPolicy::OpenSystem`]. A browser asks the user here before an unfamiliar scheme
    /// leaves for another app. [`LinkPolicy::InView`] is treated as [`LinkPolicy::Ignore`].
    pub fn on_app_link(mut self, f: impl Fn(&str) -> LinkPolicy + 'static) -> Self {
        self.on_app_link = Some(Rc::new(f));
        self
    }
    /// Answer a page's `alert()`, `confirm()` and `prompt()`. The page's script waits for the
    /// responder. Without this the view presents Day's own dialogs, titled with the site that
    /// asked.
    pub fn on_dialog(mut self, f: impl Fn(DialogRequest, DialogResponder) + 'static) -> Self {
        self.browse.dialog = Some(Rc::new(f));
        self
    }
    /// Answer a server's or proxy's request for a name and password (HTTP Basic, Digest, NTLM).
    /// Without this the sign-in is canceled and the server's "unauthorized" page loads.
    pub fn on_auth(mut self, f: impl Fn(AuthRequest, AuthResponder) + 'static) -> Self {
        self.browse.auth = Some(Rc::new(f));
        self
    }
    /// Take the files the engine offers to save rather than show. The handler names where a
    /// file goes ([`DownloadResponder::save_to`]) or declines it; without a handler nothing is
    /// downloaded. See [`download_support`].
    pub fn on_download(mut self, f: impl Fn(DownloadRequest, DownloadResponder) + 'static) -> Self {
        self.browse.download = Some(Rc::new(f));
        self
    }
    /// Hear that a page did not load: no connection, no such server, an untrusted certificate.
    /// The view is left on whatever the engine shows for a failed load, so a browser covers it
    /// with its own error page until the next load starts.
    pub fn on_load_error(mut self, f: impl Fn(&LoadError) + 'static) -> Self {
        self.browse.load_error = Some(Rc::new(f));
        self
    }
    /// Hear that the process running this view's page is gone. On Android the view is dead from
    /// then on and ignores every command: build a new one in its place. On Apple platforms the
    /// view survives and a reload brings the page back; without this handler WebKit views
    /// reload themselves.
    pub fn on_renderer_gone(mut self, f: impl Fn(&RendererGone) + 'static) -> Self {
        self.browse.renderer_gone = Some(Rc::new(f));
        self
    }
    /// What the view tells sites it is: the engine's own string (the default), the platform
    /// browser's ([`UserAgent::Browser`], which a browser wants), or a string of the app's.
    /// It is read when the view is created; build a new view to change it. WebKit and Android
    /// honor it, and a view in [`WebView::desktop_mode`] sends the desktop string instead.
    pub fn user_agent(mut self, user_agent: UserAgent) -> Self {
        self.user_agent = user_agent;
        self
    }
    /// Swiping in from a screen edge goes back and forward through the view's history (WebKit).
    /// Off unless asked for, since an app's own navigation may own those edges.
    pub fn swipe_navigation(mut self, on: bool) -> Self {
        self.swipe_navigation = on;
        self
    }
}

/// Hand an address no engine loads to the program that opens it: `mailto:`, `tel:`, an app's
/// own scheme. Android's `intent:` addresses name an app, a package to install it from and a
/// web page to fall back to; the backend starts whichever applies, and the fallback page comes
/// back here for the caller to load. This is what [`LinkPolicy::OpenSystem`] does, for an app
/// that answers [`WebView::on_app_link`] later than the handler's return (after asking the
/// user).
pub fn open_app_link(url: &str) -> Option<String> {
    #[cfg(all(target_os = "android", feature = "material"))]
    if url.starts_with("intent:") {
        return android_impl::open_intent(url);
    }
    day_core::open_url(url);
    None
}

/// [`LinkPolicy::OpenSystem`]: hand `url` to the system, and load an `intent:` address's
/// fallback page in the view through `send`.
fn open_system(url: &str, send: &impl Fn(WebPatch)) {
    if let Some(fallback) = open_app_link(url) {
        send(WebPatch::Load(fallback));
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
        all(feature = "material", target_os = "android"),
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

/// The front-end half of [`WebView::page_state`] and [`WebView::scroll_state`]: the latest
/// unpublished report and the timer that publishes it.
struct LatestSink<T: Clone + PartialEq + 'static> {
    binding: AnyBinding<T>,
    pending: RefCell<Option<T>>,
    flush: Cell<Option<day_core::TaskHandle>>,
}

impl<T: Clone + PartialEq + 'static> LatestSink<T> {
    /// A sink writing `binding`, whose pending publish is cancelled with the current scope.
    fn new(binding: AnyBinding<T>) -> Rc<Self> {
        let sink = Rc::new(LatestSink {
            binding,
            pending: RefCell::new(None),
            flush: Cell::new(None),
        });
        let cleanup = sink.clone();
        day_reactive::Scope::current().on_cleanup(move || {
            if let Some(flush) = cleanup.flush.take() {
                flush.abort();
            }
        });
        sink
    }

    /// Keep `state` as the latest report and publish it on the next frame, unless a publish is
    /// already due: the reports arriving before it are superseded, never queued.
    fn report(self: &Rc<Self>, state: T) {
        *self.pending.borrow_mut() = Some(state);
        if let Some(flush) = self.flush.get()
            && !flush.is_finished()
        {
            return;
        }
        let sink = self.clone();
        self.flush.set(Some(day_core::task(async move {
            day_core::sleep(16).await;
            if let Some(state) = sink.pending.borrow_mut().take()
                && sink.binding.get_untracked() != state
            {
                sink.binding.set(state);
            }
        })));
    }
}

impl Piece for WebView {
    fn build(self, cx: &mut BuildCx) -> RNode {
        let WebView {
            renderer,
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
            resource_site,
            page_state,
            scroll_state,
            zoom,
            desktop_mode,
            javascript,
            request_filter,
            content_rules,
            user_scripts,
            user_styles,
            messages,
            on_blocked,
            on_rules_error,
            on_media,
            on_app_link,
            browse,
            swipe_navigation,
            user_agent,
        } = self;
        let settings = WebSettings {
            zoom: zoom
                .as_ref()
                .map_or(1.0, |zoom| WebSettings::clamp_zoom(zoom.get_untracked())),
            desktop_mode: desktop_mode
                .as_ref()
                .is_some_and(|desktop| desktop.get_untracked()),
            javascript: javascript
                .as_ref()
                .is_none_or(|javascript| javascript.get_untracked()),
        };
        // Blocked loads reach `on_blocked` through this sink, from whichever thread the engine
        // filters on (`RequestHooks::decide`) or from the event channel (`BLOCKED_REPORT`).
        let blocked_sink = on_blocked
            .as_ref()
            .map(|_| Arc::new(content::BlockedSink::default()));
        let content = PageContent {
            // A provider's own site and a bundled site are the app's: the filter never sees them.
            // A view that only borrows a provider for some pages filters those like any other.
            requests: request_filter.map(|filter| {
                RequestHooks::new(
                    filter,
                    blocked_sink.clone(),
                    resource_site || inline.is_some(),
                )
            }),
            rules: content_rules
                .as_ref()
                .map(|rules| rules.get_untracked())
                .unwrap_or_default(),
            scripts: user_scripts,
            styles: user_styles,
            messages: messages.iter().map(|(name, _)| name.clone()).collect(),
            media_handler: on_media.is_some(),
        };
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
                static SESSION_RESOURCES: RefCell<HashMap<(WebRenderer, u64), (ResourceProvider, String)>> = RefCell::new(HashMap::new());
            }
            let retained = SESSION_RESOURCES.with(|m| {
                m.borrow_mut()
                    .entry((renderer, session_id))
                    .or_insert_with(|| (provider.clone(), initial_url.clone()))
                    .clone()
            });
            resources = Some(retained.0);
            initial_url = retained.1;
            url.set(initial_url.clone());
        }
        let blocked_private = (profile.private
            && renderer.private_browsing_support() == day_spec::Support::Unsupported)
            || (renderer != WebRenderer::WebView
                && renderer.profile_support() == day_spec::Support::Unsupported
                && profile != WebProfile::default());
        let initial = WebProps {
            renderer,
            profile,
            resources: if blocked_private {
                None
            } else {
                resources.clone()
            },
            resource_site: resource_site && resources.is_some() && !blocked_private,
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
            scroll_reports: scroll_state.is_some(),
            settings,
            content: if blocked_private {
                PageContent::default()
            } else {
                content
            },
            browse: browse.props(swipe_navigation),
            user_agent,
        };
        // A web view has no intrinsic size; it fills whatever space its container offers.
        let surface = renderer::surface::get(renderer);
        let flex = Flex {
            grow_w: true,
            grow_h: true,
            ..Default::default()
        };
        let node = if surface.is_some() {
            cx.leaf(
                renderer::surface::KIND,
                &day_spec::props::CanvasProps::default(),
                flex,
            )
        } else {
            cx.leaf(KIND, &initial, flex)
        };

        if let Some(host) = surface {
            renderer::surface::attach(host, cx, node, &initial);
        }

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
            renderer::send_patch(node, patch);
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

        // Settings: seeded with what the props already carried, so a binding sends a patch only
        // when its value changes from the one the view has, and never at build time.
        if let Some(zoom) = zoom {
            bind_seeded(
                settings.zoom,
                move || WebSettings::clamp_zoom(zoom.get()),
                move |zoom: &f64| send(WebPatch::Zoom(*zoom)),
            );
        }
        if let Some(desktop) = desktop_mode {
            bind_seeded(
                settings.desktop_mode,
                move || desktop.get(),
                move |desktop: &bool| send(WebPatch::DesktopMode(*desktop)),
            );
        }
        if let Some(javascript) = javascript {
            bind_seeded(
                settings.javascript,
                move || javascript.get(),
                move |enabled: &bool| send(WebPatch::JavaScript(*enabled)),
            );
        }
        if let Some(rules) = content_rules {
            bind_seeded(
                initial.content.rules.clone(),
                move || rules.get(),
                move |rules: &Vec<ContentRuleList>| send(WebPatch::ContentRules(rules.clone())),
            );
        }
        if let (Some(sink), Some(callback)) = (&blocked_sink, on_blocked) {
            let delivery = content::deliver_blocked(sink.clone(), callback);
            day_reactive::Scope::current().on_cleanup(move || delivery.abort());
        }

        // Bind the eval handle to the realized node so `handle.eval(…)` knows where to send.
        if let Some(js) = js {
            js.renderer.set(renderer);
            js.blocked.set(blocked_private);
            js.node.set(Some(node));
            day_reactive::Scope::current().on_cleanup(move || {
                if js.node.try_get() == Some(Some(node)) {
                    js.node.set(None);
                }
            });
        }

        // Page-state reports (`PAGE_STATE_REPORT`) land in `pending` and are published at most
        // once a frame: an engine announces each progress estimate, title and history change
        // separately, and the app only needs the state they settle on.
        let has_page_state = page_state.is_some();
        let page_states = page_state.map(LatestSink::new);
        // Scroll reports (`SCROLL_REPORT`) arrive once per frame of scrolling, coalesced alike.
        let scroll_states = scroll_state.map(LatestSink::new);
        // Several kinds of report share this node's `Event::Custom` channel, told apart by `num`:
        // 0 is navigation (the URL, so a bound text field follows along), positive is an
        // evaluation reply keyed by its request id, and the negative tags are the link, new-tab
        // and page-state reports. In-process backends also tag them, but a
        // cross-boundary Custom (JNI, C-ABI) carries only `num`/`text`, so `num` is the
        // discriminator that works everywhere (§8.2's opened event channel).
        #[cfg(all(feature = "arkui", target_env = "ohos"))]
        let registration = RefCell::new(None);
        #[cfg(all(feature = "arkui", target_env = "ohos"))]
        let resource_start = initial.url.clone();
        // Backends reach the request hooks by token through Java or C, and the token registry
        // holds them weakly: this handler keeps them alive exactly as long as the node.
        let keep_hooks = initial.content.requests.clone();
        cx.on(node, move |ev| {
            let _keep_resources_alive = &resources;
            let _keep_hooks_alive = &keep_hooks;
            if let Event::Custom { num, text, .. } = ev {
                #[cfg(all(feature = "arkui", target_env = "ohos"))]
                if *num == -3.0 {
                    // The controller is attached: install the scheme handler that serves the
                    // provider and asks the request filter, then load the first page, which
                    // waited for it.
                    let provider = resources.as_ref().map(ResourceProvider::id).unwrap_or(0);
                    *registration.borrow_mut() =
                        resources_arkui::install(provider, keep_hooks.clone(), text);
                    if provider != 0 {
                        if registration.borrow().is_some() {
                            send(WebPatch::Load(resource_start.clone()));
                        }
                    } else if keep_hooks.is_some() {
                        if registration.borrow().is_none() {
                            log::warn!("day-piece-webview: ArkWeb scheme handler unavailable; the request filter is off");
                        }
                        // The ArkTS half loads its own first page for an empty URL.
                        send(WebPatch::Load(String::new()));
                    }
                    return;
                }
                if *num >= 1.0 {
                    resolve(*num as u64, text);
                } else if *num == PAGE_STATE_REPORT {
                    if let (Some(sink), Some(state)) = (&page_states, decode_page_state(text)) {
                        sink.report(state);
                    }
                } else if *num == SCROLL_REPORT {
                    if let (Some(sink), Some(state)) = (&scroll_states, decode_scroll_state(text)) {
                        sink.report(state);
                    }
                } else if *num == BLOCKED_REPORT {
                    if let Some(sink) = &blocked_sink {
                        match serde_json::from_str::<Vec<BlockedLoad>>(text) {
                            Ok(loads) => sink.extend(loads),
                            Err(_) => {
                                if let Ok(load) = serde_json::from_str::<BlockedLoad>(text) {
                                    sink.push(load);
                                }
                            }
                        }
                    }
                } else if *num == SCRIPT_MESSAGE {
                    if let Ok(report) = serde_json::from_str::<content::MessageReport>(text) {
                        let reply = messages
                            .iter()
                            .find(|(name, _)| *name == report.message.name)
                            .and_then(|(_, handler)| handler(report.message));
                        send(WebPatch::ScriptReply {
                            id: report.id,
                            reply,
                        });
                    }
                } else if *num == CONTENT_RULES_ERROR {
                    if let (Some(callback), Ok(error)) = (
                        &on_rules_error,
                        serde_json::from_str::<ContentRulesError>(text),
                    ) {
                        callback(&error);
                    }
                } else if *num == MEDIA_REPORT {
                    if let Ok(report) = serde_json::from_str::<content::MediaReport>(text) {
                        let allow = on_media.as_ref().is_some_and(|f| f(&report.request));
                        send(WebPatch::MediaDecision {
                            id: report.id,
                            allow,
                        });
                    }
                } else if *num == browsing::PAGE_PROMPT {
                    browse.prompt(text, move |id, answer| {
                        send(WebPatch::PromptAnswer {
                            id,
                            answer: serde_json::to_string(&answer).unwrap_or_default(),
                        });
                    });
                } else if *num == browsing::PAGE_NOTICE {
                    browse.notice(text);
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
                    // An address no engine loads (`mailto:`, an app's scheme) is reported from
                    // any page, and `on_app_link` decides it where the app set one.
                    let app_link = !browsing::engine_scheme(text);
                    let policy = on_app_link
                        .as_ref()
                        .filter(|_| app_link)
                        .or(on_link.as_ref())
                        .map(|f| f(text))
                        .unwrap_or(LinkPolicy::OpenSystem);
                    match policy {
                        LinkPolicy::OpenSystem => open_system(text, &send),
                        LinkPolicy::InView if !app_link => {
                            send(WebPatch::Load(text.clone()));
                        }
                        LinkPolicy::InView | LinkPolicy::Ignore => {}
                    }
                } else if *num == 0.0 {
                    url.set(text.clone());
                    if let Some(callback) = &on_load {
                        callback();
                    }
                }
            }
        });
        // A retained session re-attached to this node has a settled page and no reason to
        // report again: ask for its state once the handler above exists and the node is realized.
        if has_page_state {
            let ask = day_core::task(async move {
                day_core::sleep(0).await;
                send(WebPatch::ReportState);
            });
            day_reactive::Scope::current().on_cleanup(move || ask.abort());
        }
        node
    }
}

// ---------------------------------------------------------------------------
// Per-toolkit native renderers, one file per backend (this crate is a reference implementation,
// so each toolkit is split out for clarity). Each module registers a `Renderer` link-time into its
// backend's `RENDERERS` slice; `#[cfg]` gates each to its feature + target, and `#[path]` keeps the
// files grouped next to lib.rs.
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "macos", feature = "gtk"))]
mod gtk_assets;
#[cfg(all(target_os = "macos", feature = "gtk"))]
#[path = "lib-gtk-macos.rs"]
mod gtk_macos;
#[cfg(all(target_os = "macos", any(feature = "appkit", feature = "gtk")))]
mod macos_web;

day_pieces::glue_modules!(appkit, qt, uikit, material, xaml, arkui, dom);

#[cfg(any(test, all(feature = "xaml", windows)))]
mod xaml_path;

// Linux GTK uses WebKitGTK; the other GTK hosts are registered above.
#[cfg(all(feature = "gtk", not(target_os = "macos"), not(windows)))]
#[path = "lib-gtk.rs"]
mod gtk_impl;

/// Rendering engine selected independently for each view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WebRenderer {
    /// The toolkit's embedded browser; the default.
    #[default]
    WebView,
    /// Experimental engine supplied by the separately registered `day-piece-webview-servo` package.
    Servo,
}
impl WebRenderer {
    /// Whether this build can create this renderer, without initializing an engine.
    pub fn available(self) -> bool {
        match self {
            Self::WebView => support() != day_spec::Support::Unsupported,
            Self::Servo => renderer::capabilities(self).is_some(),
        }
    }
    /// Whether isolated private browsing contexts are supported.
    pub fn private_browsing_support(self) -> day_spec::Support {
        match self {
            Self::WebView => private_browsing_support(),
            Self::Servo => renderer::capabilities(self)
                .map_or(day_spec::Support::Unsupported, |caps| caps.private_browsing),
        }
    }
    /// Whether named storage profiles are supported.
    pub fn profile_support(self) -> day_spec::Support {
        match self {
            Self::WebView => profile_support(),
            Self::Servo => renderer::capabilities(self)
                .map_or(day_spec::Support::Unsupported, |caps| caps.profiles),
        }
    }
    /// Whether all data in a profile can be deleted.
    pub fn clear_data_support(self) -> day_spec::Support {
        match self {
            Self::WebView => clear_data_support(),
            Self::Servo => renderer::capabilities(self)
                .map_or(day_spec::Support::Unsupported, |caps| caps.clear_data),
        }
    }
    /// Whether [`WebView::zoom`] scales the page. The Servo adapter sets Servo's page zoom.
    pub fn zoom_support(self) -> day_spec::Support {
        match self {
            Self::WebView => zoom_support(),
            Self::Servo => renderer::capabilities(self)
                .map_or(day_spec::Support::Unsupported, |_| {
                    day_spec::Support::Native
                }),
        }
    }
    /// Whether [`WebView::desktop_mode`] is honored. Servo 0.6 has no per-view user agent.
    pub fn desktop_mode_support(self) -> day_spec::Support {
        match self {
            Self::WebView => desktop_mode_support(),
            Self::Servo => day_spec::Support::Unsupported,
        }
    }
    /// Whether [`WebView::request_filter`] runs. The Servo adapter filters each request it is
    /// asked to load.
    pub fn request_filter_support(self) -> day_spec::Support {
        match self {
            Self::WebView => request_filter_support(),
            Self::Servo => renderer::capabilities(self)
                .map_or(day_spec::Support::Unsupported, |_| {
                    day_spec::Support::Native
                }),
        }
    }
    /// Whether [`WebView::javascript`] can stop page scripts. Servo 0.6 has no per-view switch.
    pub fn javascript_support(self) -> day_spec::Support {
        match self {
            Self::WebView => javascript_support(),
            Self::Servo => day_spec::Support::Unsupported,
        }
    }
}
/// An explicitly selected renderer is unavailable in this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererUnavailable(pub WebRenderer);
impl std::fmt::Display for RendererUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} renderer is unavailable in this build", self.0)
    }
}
impl std::error::Error for RendererUnavailable {}

#[cfg(test)]
mod tests {
    #[test]
    fn the_expression_form_carries_the_script_as_source() {
        let wrapped = wrap_expression("document.title // the title");
        assert!(wrapped.contains("v=(\ndocument.title // the title\n);"));
        assert!(!wrapped.contains("eval("));
        // The first form evaluates text, which a page's policy can refuse.
        assert!(wrap_script("document.title").contains("eval("));
    }

    /// The wrapper, run for real (with `node`, skipped without it). A script that throws its
    /// own `EvalError` after a side effect ran once and is reported as thrown, so it is never
    /// sent again. An `eval` that refuses before anything ran is reported as the refusal.
    #[test]
    fn only_a_refused_eval_is_reported_as_one() {
        let throws = "globalThis.count=(globalThis.count||0)+1; throw new EvalError('mine')";
        let program = format!(
            "var first={};\
             var refused=(function(eval){{return {}}})(function(){{throw new EvalError('refused')}});\
             console.log(JSON.stringify([first,globalThis.count,refused,globalThis.touched===undefined,{}]))",
            wrap_script(throws),
            wrap_script("globalThis.touched=1"),
            wrap_expression("1+1")
        );
        let output = match std::process::Command::new("node")
            .arg("-e")
            .arg(&program)
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => {
                eprintln!("node did not run; skipping");
                return;
            }
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let values: Vec<serde_json::Value> = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(values[0], format!("0{SEP}EvalError{SEP}mine"));
        assert_eq!(values[1], 1, "the script ran once");
        assert_eq!(values[2], format!("0{SEP}{EVAL_REFUSED}{SEP}refused"));
        assert_eq!(values[3], true, "a refused script did not run");
        assert_eq!(values[4], format!("1{SEP}2"));
    }

    /// The wrapper leaves the script's meaning alone (with `node`, skipped without it): a
    /// leading `"use strict"` still governs it, a script with no value reports null, and the
    /// script may use any name, the wrapper's own included.
    #[test]
    fn the_wrapper_runs_the_script_as_written() {
        let scripts = [
            "'use strict'; (function(){ return this === undefined; })()",
            "",
            "var declared = 1;",
            "let ran = 2, v = 3, e = 4, s = 5; ran + v + e + s",
            "document = undefined; typeof eval",
        ];
        let wrapped: Vec<String> = scripts.iter().map(|s| wrap_script(s)).collect();
        let program = format!("console.log(JSON.stringify([{}]))", wrapped.join(","));
        let output = match std::process::Command::new("node")
            .arg("-e")
            .arg(&program)
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => {
                eprintln!("node did not run; skipping");
                return;
            }
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let values: Vec<String> = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(values[0], format!("1{SEP}true"), "strict mode kept");
        assert_eq!(values[1], format!("1{SEP}null"));
        assert_eq!(values[2], format!("1{SEP}null"));
        assert_eq!(values[3], format!("1{SEP}14"));
        assert_eq!(values[4], format!("1{SEP}\"function\""));
    }

    #[test]
    fn servo_selection_is_explicit_and_does_not_claim_native_profiles() {
        assert_eq!(super::WebRenderer::default(), super::WebRenderer::WebView);
        assert!(!super::WebRenderer::Servo.available());
        assert_eq!(
            super::WebRenderer::Servo.private_browsing_support(),
            day_spec::Support::Unsupported
        );
        assert_eq!(
            super::WebRenderer::Servo.profile_support(),
            day_spec::Support::Unsupported
        );
    }

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
    fn scroll_reports_round_trip_and_reject_garbage() {
        let state = ScrollState {
            offset: 1234.5,
            content_height: 8000.0,
            viewport_height: 700.0,
            dragging: true,
        };
        assert_eq!(
            decode_scroll_state(&encode_scroll_state(&state)),
            Some(state)
        );
        // Java's `%.1f` under Locale.ROOT, and a bounce above the top.
        let java = decode_scroll_state("-12.0\u{1f}900.0\u{1f}700.0\u{1f}0").unwrap();
        assert!(java.at_top() && !java.dragging);
        assert!(!java.at_bottom());
        let end = ScrollState {
            offset: 200.0,
            content_height: 900.0,
            viewport_height: 700.0,
            dragging: false,
        };
        assert!(end.at_bottom());
        assert_eq!(decode_scroll_state("1,5\u{1f}900\u{1f}700\u{1f}0"), None);
        assert_eq!(decode_scroll_state("1\u{1f}NaN\u{1f}700\u{1f}0"), None);
        assert_eq!(decode_scroll_state("1\u{1f}2\u{1f}3\u{1f}yes"), None);
        assert_eq!(decode_scroll_state("1\u{1f}2"), None);
    }

    #[test]
    fn page_state_reports_round_trip_with_a_separator_in_the_title() {
        let state = PageState {
            url: "https://example.test/a?b=c#d".into(),
            title: reply(&["two", "parts"]),
            loading: true,
            progress: 0.4567,
            can_go_back: true,
            can_go_forward: false,
        };
        let decoded = decode_page_state(&encode_page_state(&state)).unwrap();
        assert_eq!(decoded.title, state.title);
        assert_eq!(decoded.url, state.url);
        assert!(decoded.loading && decoded.can_go_back && !decoded.can_go_forward);
        assert_eq!(decoded.progress, 0.46);
        // What the Java, C++ and ArkTS arms compose by hand.
        let native = decode_page_state(&reply(&["011", "1", "about:blank", ""])).unwrap();
        assert_eq!(native.progress, 1.0);
        assert!(!native.loading && native.can_go_back && native.can_go_forward);
        assert_eq!(decode_page_state(&reply(&["01", "1", "x", ""])), None);
        assert_eq!(decode_page_state(&reply(&["000", "NaN", "x", ""])), None);
        assert_eq!(
            decode_page_state(&reply(&["000", "7", "x", "t"]))
                .unwrap()
                .progress,
            1.0
        );
    }

    #[test]
    fn zoom_factors_clamp_to_what_every_engine_accepts() {
        assert_eq!(WebSettings::clamp_zoom(1.25), 1.25);
        assert_eq!(WebSettings::clamp_zoom(0.1), 0.25);
        assert_eq!(WebSettings::clamp_zoom(12.0), 5.0);
        for nonsense in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            assert_eq!(WebSettings::clamp_zoom(nonsense), 1.0, "{nonsense}");
        }
    }

    /// The Java (`DayWebView.desktopAgent`) and ArkTS (`desktopAgent`) arms compose this text too.
    #[test]
    fn desktop_agents_keep_the_engine_major_version() {
        let android = "Mozilla/5.0 (Linux; Android 14; Pixel 7 Build/UQ1A; wv) AppleWebKit/537.36 \
                       (KHTML, like Gecko) Version/4.0 Chrome/131.0.6778.135 Mobile Safari/537.36";
        assert_eq!(
            desktop_user_agent(android),
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/131.0.0.0 Safari/537.36"
        );
        assert!(desktop_user_agent("ArkWeb/4.1 Mobile").contains(" Chrome/120.0.0.0 "));
    }

    #[test]
    fn snapshot_replies_decode_base64() {
        assert_eq!(base64_decode("iVBORw0KGgo=").unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(""), Some(Vec::new()));
        assert_eq!(base64_decode("a*b"), None);
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
