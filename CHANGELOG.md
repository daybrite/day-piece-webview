<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

## Unreleased

- New: `WebView::user_agent(UserAgent::Browser)` sends the platform browser's user agent
  (Chrome's on Android, Safari's on iOS and macOS), derived on the device from the engine's own,
  where a view otherwise identifies itself as embedded in an app. `UserAgent::Custom` sends any
  string.
- `JsHandle::eval` and the `web_eval` step work on pages that forbid `eval` (a Content Security
  Policy without `unsafe-eval`, or Trusted Types): the script is sent once more as source when
  the page refused it before it ran. A script that ran and threw is never sent again.
- New: the browser duties (docs/webview.md, "Browser duties") on WebKit (iOS and macOS) and
  Android. `WebView::on_dialog` answers `alert()`, `confirm()` and `prompt()`, which WKWebView
  otherwise answers itself (confirm as `false`), and every view presents Day's own dialogs
  without it. `on_auth` answers HTTP sign-in. `on_download` takes files with
  `DownloadResponder::save_to` or `save_to_downloads`, progress events that outlive the view,
  `cancel_download`, `open_download` and `remove_download`. `on_load_error` reports failed
  main-frame loads by kind, and `trust_host` excepts a host's certificate until the app exits.
  `on_renderer_gone` reports a lost page process; Android no longer ends the app when its
  renderer dies. Android shows Day's file picker for `<input type=file>`. Addresses no engine
  loads (`mailto:`, app schemes, `intent:`) go to the system or to `on_app_link`.
  `swipe_navigation` turns on WebKit's history swipes. A browsing view on Android pinches to
  zoom, and video plays inline on iPhone.
- The request filter treats a provider's pages alike on every engine: skipped in a view showing
  the provider's own site (`web_view_resources`, `web_view_inline`), filtered in a browsing view
  that borrows a provider with `.resources(...)`. A view whose rule lists change before its first
  page loads now waits for the newest lists on WebKit and WebKitGTK, and a navigation made while
  it waits replaces the startup page instead of being overwritten by it.
- New: content blocking and page script hooks (docs/webview.md, "Content blocking and page
  scripts"). `WebView::request_filter(Arc<dyn RequestFilter>)` decides each request on Android
  (`shouldInterceptRequest`, service workers included), WebView2 (`WebResourceRequested`),
  Qt (`QWebEngineUrlRequestInterceptor`), ArkWeb (the NDK scheme handler) and Servo, with
  `RequestInfo { url, top_url, frame_url, kind, main_frame, method }`; `WebView::on_blocked`
  reports what it blocked in batches. `WebView::content_rules` compiles WebKit content rule
  lists on AppKit, UIKit, macOS GTK and WebKitGTK, keeps them compiled across launches by id and
  version, and holds a new view's first page until they apply; `on_content_rules_error`
  reports a list that fails. `WebView::user_script` and `WebView::user_style` run in every page
  and frame. `WebView::on_script_message(name, handler)` answers `window.dayMessage(name, text)`
  from pages with a reply promise. Probes: `request_filter_support()`, `blocked_report_support()`,
  `content_rules_support()`, `user_script_support()`, `script_message_support()`. Renderers
  receive `WebProps::content` and `WebPatch::{ContentRules, ScriptReply, MediaDecision}`.
- New: Android pages can use the camera and microphone. `onPermissionRequest` grants what the
  app holds, asks for a declared permission it does not hold yet, and denies the rest;
  `WebView::on_media_request` lets the app decide first.
- New: three per-view settings, each taking a constant, a signal or a closure.
  `WebView::zoom(factor)` zooms the page with the engine's own page zoom (WKWebView
  `pageZoom`, WebKitGTK zoom level, QtWebEngine and WebView2 zoom factors, Servo page zoom),
  text zoom on Android and ArkWeb, and CSS `zoom` on WinUI and in same-origin web-dom frames.
  `WebView::desktop_mode(on)` asks sites for their desktop layout: WebKit's desktop content mode
  on iOS, a desktop Chrome user agent with a wide viewport on Android and ArkWeb; desktop
  engines already present themselves as desktop browsers. `WebView::javascript(on)` stops page
  scripts while the app's own evaluation keeps working, except on Android and web-dom. Desktop
  mode and the script switch reload the page when they change. `zoom_support()`,
  `desktop_mode_support()` and `javascript_support()` (also on `WebRenderer`) report each
  backend's support; `WebProps::settings` and `WebPatch::{Zoom, DesktopMode, JavaScript}` carry
  them to renderers.
- New: `WebView::page_state(binding)` keeps a `PageState { url, title, loading, progress,
  can_go_back, can_go_forward }` binding current from the engine's own notifications (KVO on
  WKWebView, WebKitGTK property notifications, QtWebEngine signals, Android's client
  callbacks, WebView2 navigation events, ArkWeb callbacks, Servo's delegate notifications).
  Nothing is polled; reports within one frame are coalesced, and the URL follows
  same-document changes. A view re-attached to a retained session reports its settled page
  once its node is ready. The demo drops its `navigation_state()` polling for it.
- New: `WebView::scroll_state(binding)` keeps a `ScrollState { offset, content_height,
  viewport_height, dragging }` current on iOS and Android, for chrome that slides its bars away
  while the user scrolls down, as Safari and Chrome do.
- New: `WebView::resources(provider)` serves a resource provider's namespace in an ordinary
  view, which keeps browsing anywhere, reporting URLs and opening tabs. `web_view_resources`
  still confines a view to the provider's site.
- New: `JsHandle::snapshot(max_width_px)` captures the visible page as PNG on AppKit, UIKit,
  macOS GTK, Linux GTK, Qt, Android and WebView2. ArkWeb and web-dom answer
  `EvalError::Unsupported`.
- Fixed: Linux GTK reports a script-opened window that skipped the new-window policy decision
  to `on_new_tab` instead of dropping it.
- Removed: the WebView2 hosts (through Wry) for `windows-gtk` and `windows-qt`, which Day no
  longer builds, along with the crate's `wry` and `windows` dependencies.
- Fixed: on iOS 17 and 18, a full-screen page that declares `viewport-fit=cover` was laid out
  short by the top and bottom safe areas, so a toolbar fixed to the bottom of the page floated
  above the home indicator. The web view now has WebKit size the page again once WebKit stops
  insetting it.
- Changed: `web_view(url)` and `.url_binding(url)` take any two-way `Binding<String>` (a
  `Signal<String>` still works; a day-model `Field` does too), and `.on_new_tab`'s two menu
  labels take any text source (`IntoText`), so a `LocalizedText` goes in without `.format()`.
- Changed: the `WebViewBuilder` trait is generated by `#[day_pieces::builder]` from the piece's
  inherent builders; it keeps its name and now also forwards `on_load`, `app_assets` and
  `transparent` through a decorated view.
- Fixed: a dayscript screenshot could capture a Servo canvas before its frame arrived (the
  Android comparison job's Servo proof came out blank while the page's JavaScript had already
  answered). The canvas host now registers a capture hold with Day, so `screenshot` and
  `sample_pixel` wait until every attached Servo view has presented its latest frame; needs a
  Day with `day_spec::capture::register_hold`.
- Changed: the experimental Servo renderer's `harmony-arkui` host is paused: out of the
  comparison CI matrix and the demo's `servo` flavor targets, code retained. The prebuilt
  SpiderMonkey archive needs the external LLVM 19 toolchain's libc++ and its env interposer
  crashes on OpenHarmony's loader; docs/servo.md ("Paused: OpenHarmony") records the three
  findings and what has to change upstream before it returns.
- Fixed: the Servo adapter registered no resource reader on OpenHarmony, where the `servo`
  crate leaves that to the embedder, so the constellation panicked at startup; it now links
  Servo's default reader there. Servo thread panics are logged through Day's logger instead of
  vanishing with the process's stderr on mobile hosts.
- Fixed: Linux GTK JavaScript evaluation timed out on web views with an automation ID.
  Evaluation replies now retain the internal node ID separately from GTK's widget name,
  which Day replaces with the automation ID.
- New: `WebView::app_assets()` — an inline site that reads the app's own files rather than only
  its own directory. WebKit's file-URL read access widens to the asset tree, and the GTK arm
  extracts the whole tree and allows file-URL fetches; the backends whose browsable base is
  already that tree are unchanged, and navigation policing stays on the site.
- Fixed: on Windows, an inline site showed the engine's "can't reach this page"
  (`ERR_NAME_NOT_RESOLVED`). The shim mapped the virtual host to an `assets` directory beside the
  exe, which only a packed app has; a `day launch` run keeps the app's assets in the project and a
  piece's under `build/`, so the mapping was never established. The folder is resolved in Rust now
  (`day_spec::resolve_asset_dir`), and a site that resolves to nothing says so in the log.
- Fixed: a start page with a query or a fragment (`start_page("player.html?src=hello")`) loaded
  nothing on AppKit and UIKit. `fileURLWithPath:` percent-encoded the `?` into the file name, so
  the load asked for a file that does not exist; the tail is now re-attached as a relative
  reference, which is how a browser reads the same string.
- Changed: the Android factory moved from `platform/android/java/…/DayWebView.java` to
  `src/DayWebView.java`, beside the Rust arms. Building for Android now needs a `day` CLI that
  links single-file `java` entries; an older one skips the file, and the app fails when it first
  creates the view.

## 0.4.3

The first release from its own repository. The crate moved out of `daybrite/day`
(`pieces/day-piece-webview`) with its history, and [docs/webview.md](docs/webview.md) came with it.
The JavaScript evaluation contract stays in day, because the `web_eval` dayscript step and the
day-core seam it drives live there (day's `docs/webview-eval.md`).

- Tested against day 0.4.
- New: the `demo/` app and its `dayscript/webview.yaml`: a bundled site in the web view, a
  JavaScript console, and the link policy intercepting an app link.
- Fixed: on macOS, creating the view panicked with "class WKWebView could not be found" in an app
  where nothing else loaded WebKit. The piece now declares WebKit under
  `[package.metadata.day.macos]` as well as for iOS, so the app links it.
- Unchanged: `web_view`, `web_view_inline`, `WebSession`, `JsHandle`, the link policy, and the
  eight backends.
