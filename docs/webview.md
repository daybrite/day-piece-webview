---
title: "Web view integration"
description: "Load web content in a Day app, handle navigation, and manage view lifetime."
---
<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Web view integration

Use `web_view` for URLs, `web_view_inline` for bundled sites, and
[`web_view_resources`](resource-provider.md) for app-provided bytes. All return a `WebView`
that implements Day's `Piece` and accepts layout, accessibility, and ID decorators.

The examples below belong in a Day app with `day::resources!()`. Names such as
`res::str::go()` and `res::assets::site` stand for the app's generated translations and asset
directories. Define the strings in each supported locale. Do not replace them with hardcoded
UI text or string-based resource lookups.

## A browser with navigation controls

```rust
use day::prelude::*;
use day_piece_webview::{support, web_view};

fn browser() -> impl Piece {
    let url = Signal::new("https://daybrite.dev".to_owned());
    let go = Trigger::new();
    let back = Trigger::new();
    let reload = Trigger::new();
    let history = support() == Support::Native;
    column((
        row((
            button(res::str::back())
                .enabled(move || history)
                .action(move || back.notify()),
            text_field(url).id("browser-url"),
            button(res::str::go()).action(move || go.notify()),
            button(res::str::reload()).action(move || reload.notify()),
        )).spacing(8.0),
        web_view(url).go(go).back(back).reload(reload).id("browser"),
    ))
}
```

The initial URL loads when the view is created. Editing the signal alone does not navigate;
notify the `go` trigger to load its current value. Native navigation updates the signal with
the reported URL. web-dom does not report iframe URL changes.

| Modifier | On trigger notification |
|---|---|
| `.go(trigger)` | Load the current URL signal |
| `.back(trigger)` / `.forward(trigger)` | Move through browser history |
| `.stop(trigger)` | Stop the current load |
| `.reload(trigger)` | Reload; web-dom instead reloads the last URL supplied by the app |
| `.force_reload(trigger)` | Bypass the resource cache; check `force_reload_support()` first |

### Live page state

`.page_state(binding)` keeps a `PageState` binding current with what the engine reports:
`url` (including same-document changes such as `history.pushState` and fragments), `title`,
`loading`, `progress` (`0.0` to `1.0`, in hundredths) and `can_go_back` / `can_go_forward`.
Nothing is polled. Each backend reports from its engine's own notifications, the reports
arriving within one frame are coalesced into one write, and an unchanged state is not
written again.

```rust
use day::prelude::*;
use day_piece_webview::{PageState, web_view};

fn tab(url: Signal<String>) -> impl Piece {
    let page = Signal::new(PageState::default());
    let back = Trigger::new();
    column((
        row((
            button(res::str::back())
                .enabled(move || page.get().can_go_back)
                .action(move || back.notify()),
            label(move || page.get().title).grow(Dimension::Width),
        )),
        when(move || page.get().loading, move || progress(move || page.get().progress)),
        web_view(url).page_state(page).back(back),
    ))
}
```

| Backend | Source of each report | Progress |
|---|---|---|
| AppKit, UIKit, macOS GTK | KVO on WKWebView `URL`, `title`, `loading`, `estimatedProgress`, `canGoBack`, `canGoForward` | Engine estimate |
| Linux GTK | WebKitGTK `notify::uri/title/is-loading/estimated-load-progress`, back-forward list `changed` | Engine estimate |
| Qt | `urlChanged`, `titleChanged`, `loadStarted`, `loadProgress`, `loadFinished` | Engine estimate |
| Android | `onPageStarted`, `onPageFinished`, `doUpdateVisitedHistory`, `onReceivedTitle`, `onProgressChanged` | Engine estimate |
| WebView2 | `NavigationStarting`, `ContentLoading`, `DOMContentLoaded`, `NavigationCompleted`, `DocumentTitleChanged`, `HistoryChanged`, `SourceChanged` | Steps: 0.1, 0.5, 0.8, 1.0 |
| ArkWeb | `onPageBegin`, `onPageEnd`, `onProgressChange`, `onTitleReceive`, `onRefreshAccessedHistory` | Engine estimate |
| web-dom | The iframe's `load` event | `1.0` after each load; no history |
| Servo | `notify_url_changed`, `notify_page_title_changed`, `notify_load_status_changed`, `notify_history_changed` | Steps: 0.1 (started), 0.5 (head parsed), 1.0 |

On web-dom the iframe reports its URL after each load and, for same-origin frames only, its
title; it never reports history or a load in progress.

A view re-attached to a retained session (`.session`) shows a page that has already settled,
so no engine notification follows. The piece asks the renderer for the current state once the
new node is built and realized, so the binding starts out describing that page.

### Scroll position

`.scroll_state(binding)` keeps a `ScrollState` binding current with where the page is scrolled:
`offset` (points from the top, below `0` or past the end while the page bounces),
`content_height`, `viewport_height`, and `dragging` while a finger is on the page.
`at_top()` and `at_bottom()` answer the two questions a browser's chrome asks before it slides
its bars back. Reports arriving within one frame are coalesced into one write.

```rust
let scroll = Signal::new(ScrollState::default());
let bars_hidden = Signal::new(false);
watch(move || scroll.get(), move |now, before| {
    let delta = now.offset - before.map_or(now.offset, |b| b.offset);
    if now.at_top() || now.at_bottom() || delta < -8.0 {
        bars_hidden.set(false);
    } else if now.dragging && delta > 8.0 {
        bars_hidden.set(true);
    }
});
web_view(url).scroll_state(scroll)
```

| Backend | Source |
|---|---|
| UIKit | the web view's `UIScrollView` delegate: `scrollViewDidScroll:`, drag begin and end; the offset counts past the inset UIKit adds under bars |
| Android | `View.OnScrollChangeListener`, with a touch listener for the drag flag |
| AppKit, GTK, Qt, WebView2, ArkWeb, web-dom, Servo | not reported: desktop browsers keep their bars, and the binding keeps its value |

A view without a `scroll_state` binding sends no scroll reports at all.

`.url_binding()` and `on_load` keep their meaning: the bound URL changes when a navigation
completes, and `on_load` runs then. `PageState::url` also follows same-document changes and
the URL of a load still in progress.

To read the same state once instead, bind a `JsHandle` with `.js(handle)` and await
`handle.navigation_state()`, which returns a `NavigationState` with `url`, `title`,
`can_go_back`, `can_go_forward` and `loading`. Iframe hosts return
`EvalError::Unsupported`, because cross-origin history and page state are unavailable.

### Page snapshots

`handle.snapshot(max_width_px).await` captures what the view shows as PNG bytes, at most
`max_width_px` physical pixels wide, for thumbnails such as a tab overview. It is the
engine's own rendering of the visible page, not a re-render of the document.

| Backend | Capture |
|---|---|
| AppKit, UIKit, macOS GTK | `takeSnapshotWithConfiguration:` at the requested width |
| Linux GTK | `webkit_web_view_get_snapshot` of the visible region, scaled down in Rust |
| Qt | `QWidget::grab` of the engine view, scaled with `Qt::SmoothTransformation` |
| Android | The view drawn into a `Bitmap` at the requested scale |
| WebView2 | `CapturePreview`, scaled through WIC |
| ArkWeb, web-dom | `EvalError::Unsupported` |

A view that is not on screen, such as a background tab, may capture blank or fail with
`EvalError::Threw`, depending on the engine. WebView2 stops drawing a hidden view and never
completes a capture of one, so there the request fails at once with a `SnapshotError`. A capture
started while the view shows still completes after the view is hidden, so an app that wants a
picture of the page being left takes it before it switches away.

### Zoom, desktop sites and page scripts

`.zoom(factor)`, `.desktop_mode(on)` and `.javascript(on)` are the per-page settings a browser
offers in its page menu. Each takes a constant, a signal or a closure. The value the view is
built with applies before its first page loads, and a later change applies when it is made.

```rust
use day::prelude::*;
use day_piece_webview::{desktop_mode_support, web_view};

fn page(url: Signal<String>) -> impl Piece {
    let zoom = Signal::new(1.0);
    let desktop = Signal::new(false);
    let scripts = Signal::new(true);
    column((
        row((
            button(res::str::zoom_out()).action(move || zoom.update(|z| *z -= 0.25)),
            button(res::str::zoom_in()).action(move || zoom.update(|z| *z += 0.25)),
            label(res::str::desktop_site()),
            toggle(desktop).enabled(desktop_mode_support() != Support::Unsupported),
            label(res::str::javascript()),
            toggle(scripts),
        ))
        .spacing(8.0),
        web_view(url).zoom(zoom).desktop_mode(desktop).javascript(scripts),
    ))
}
```

- **Zoom**: `1.0` is 100%. Factors are clamped to `WebSettings::ZOOM_RANGE`, 25% to 500%;
  Safari's steps run from 50% to 300%. The engine keeps the factor as the view navigates, and a
  change zooms the page on screen without reloading it.
- **Desktop mode**: `true` asks sites for the layout they serve a desktop browser, like Safari's
  "Request Desktop Website" and Chrome's "Desktop site". `false` leaves the engine's default,
  which is the desktop layout on a Mac, an iPad and every desktop toolkit. A change reloads the
  page so the site can answer. The reload keeps the page's address: a site that redirected a
  phone to its mobile address (`m.example.com`) stays there until the app loads the main one.
- **Page scripts**: `false` stops the scripts a page contains or loads. A change reloads the
  page, since a running page cannot gain or lose its scripts. On most engines the app's own
  evaluation (`JsHandle::eval`, the dayscript `web_eval` step) keeps working with page scripts
  off, and so do page state, snapshots and cookies. On Android and web-dom evaluation fails
  until scripts are on again.

Check `zoom_support()`, `desktop_mode_support()` and `javascript_support()`, or the
`WebRenderer` methods of the same names for a view with a chosen renderer.

| Backend | Zoom | Desktop mode | Page scripts |
|---|---|---|---|
| AppKit, macOS GTK | WKWebView `pageZoom` | Accepted; a Mac already gets the desktop site | `WKWebpagePreferences.allowsContentJavaScript`, set for each navigation |
| UIKit | WKWebView `pageZoom` | `WKWebpagePreferences.preferredContentMode` `.desktop`, set for each navigation | `allowsContentJavaScript`, as on AppKit |
| Linux GTK | `webkit_web_view_set_zoom_level` | Accepted; WebKitGTK sends a desktop user agent | `enable-javascript-markup` |
| Qt | `QWebEngineView::setZoomFactor` | Accepted; QtWebEngine sends a desktop user agent | The page's `QWebEngineSettings::JavascriptEnabled`; app scripts run in the application world |
| Android | `WebSettings.setTextZoom` | A desktop Chrome user agent, a wide viewport shown zoomed out, pinch zoom | `setJavaScriptEnabled`; evaluation fails with `ScriptsDisabled` |
| WebView2, system XAML | `ICoreWebView2Controller::put_ZoomFactor` | Accepted; WebView2 sends desktop Edge's user agent | `ICoreWebView2Settings::put_IsScriptEnabled` |
| WebView2, WinUI | CSS `zoom` on each document (`Emulated`) | As system XAML | As system XAML |
| ArkWeb | `textZoomRatio` | `setCustomUserAgent` with a desktop Chrome user agent | `javaScriptAccess` |
| web-dom | CSS `zoom`, same-origin frames only (`Emulated`) | `Unsupported` | An iframe `sandbox` without `allow-scripts` (`Emulated`); evaluation fails |
| Servo | `WebView::set_page_zoom` | `Unsupported` | `Unsupported` |

Android and ArkWeb zoom text, the setting Chrome on Android calls "Text scaling". Their other
zoom controls (`zoomBy`, `setInitialScale`, the ArkWeb controller's `zoom`) magnify without
reflowing and start over on each page, so a column of text would run off the screen. Images and
boxes sized in pixels keep their size there.

The WinUI WebView2 control does not expose the engine's zoom factor, so that build sets CSS
`zoom` on each document as it is created. A page that sets `zoom` on its root element itself
overrides it.

A view re-attached to a retained session (`.session`) takes the new view's settings: zoom at
once, desktop mode and page scripts from the page's next navigation.

### App-served pages in an ordinary view

`web_view(url).resources(provider)` registers a [resource provider](resource-provider.md)'s
namespace on a view that otherwise browses like any remote view: it navigates anywhere,
reports URLs and page state, opens new tabs, and keeps one history across provider pages and
remote ones. This is the shape of a browser's test mode, which serves recorded pages for
web addresses. `web_view_resources(provider, page)` is the other shape: a view kept inside the
provider's site, whose links out of it go through `on_external_link`.

A provider page is addressed under `provider.base_url()`, whose form differs by engine:
`day-resource://p<id>/` on Apple, GTK and Qt, `https://day-resource.invalid/<id>/` on
Android, WebView2 and ArkWeb. Relative links, `<form method="get">` and `location` assignments
resolve against the page as usual, and the query reaches `ResourceRequest::query`. A
root-relative URL (`/about`) resolves against the base's root rather than the provider's
namespace, so pages that map web addresses into the namespace should link relatively.

For inline or resource-provider views, `.url_binding(address_signal)` exposes the reported URL
to an address bar while preserving the constructor's initial page when the signal is empty.
Editing that signal alone still does not navigate; notify `.go(trigger)` explicitly.

The web view grows in both directions. A `column` containing controls followed by the web view
lets it use the remaining height. Give an embedded preview a bounded frame; avoid putting an
unbounded web view inside a scrolling container. Native child-window hosts need particular care
with clipping and overlapping controls; see [Platforms](platforms.md).

## Bundled help, editors, and players

Place the complete web document tree under a generated asset directory:

```text
resource/assets/site/
  index.html
  css/style.css
  js/main.js
  pages/shortcuts.html
```

Open it with its generated constant:

```rust
use day::prelude::*;
use day_piece_webview::web_view_inline;

fn shortcuts() -> impl Piece {
    web_view_inline(res::assets::site)
        .start_page("pages/shortcuts.html?mode=compact#keyboard")
        .id("shortcuts")
}
```

`.start_page()` applies only to inline sites. Without it the first page is `index.html`.
The query and fragment reach the page unchanged. Within HTML and CSS, use relative links such
as `../css/style.css`; do not construct platform-specific bundle URLs.

For an explicit preparation step:

```rust
use day_piece_webview::{AssetDirSiteExt, PrepareError, WebView, web_view_inline};

async fn prepared_help() -> Result<WebView, PrepareError> {
    let site = res::assets::site.prepare_site().await?;
    Ok(web_view_inline(site))
}
```

`prepare_site()` checks for `index.html` on native targets and prepares Linux GTK's extracted
cache. Its current implementation performs the work synchronously and returns an immediately
ready future; `await` does not move extraction off the UI thread. web-dom skips the presence
check, so missing pages fail when the iframe loads. Map preparation errors to localized UI
messages; the error's `Display` text is a diagnostic, not a translated label.

### Share other bundled assets

A player can opt into reading sibling asset directories:

```rust
use day::prelude::*;
use day_piece_webview::web_view_inline;

fn player() -> impl Piece {
    web_view_inline(res::assets::player).app_assets().transparent()
}
```

`.app_assets()` widens Apple's file read-access root and GTK's extraction scope. Other engines
already expose the staged asset tree. It is not a portable filesystem sandbox. File-origin
`fetch` and XMLHttpRequest rules also vary by engine. Use a
[resource provider with a bundled shell](resource-provider.md#bundled-shell-and-dynamic-content)
when the page needs app-owned data at relative URLs.

`.transparent()` clears the native view background. The document must also leave its own
background transparent, for example `html, body { background: transparent; }`. An opaque CSS
background remains opaque. Test compositing on the intended platforms; the walkthrough does
not currently assert transparency.

## Links from web content into the app

Inline and resource views keep internal navigation in the web view. By default, links that
leave their content root open through the operating system. Override that policy for routes
owned by the application:

```rust
use day::prelude::*;
use day_piece_webview::{LinkPolicy, web_view_inline};

fn help_with_settings(open_settings: impl Fn() + 'static) -> impl Piece {
    web_view_inline(res::assets::site).on_external_link(move |url| {
        if url == "myapp://settings" {
            open_settings();
            LinkPolicy::Ignore
        } else if url.starts_with("https://") || url.starts_with("mailto:") {
            LinkPolicy::OpenSystem
        } else {
            LinkPolicy::Ignore
        }
    })
}
```

The bundled page can link to `myapp://settings`. This is an in-view message convention, not
OS-level scheme registration. The callback runs on the UI thread. Match known routes and validate
parameters before acting on them.

| Policy | Effect |
|---|---|
| `OpenSystem` | Open using the OS URL handler, usually a browser or mail client |
| `Ignore` | Cancel navigation; the callback can handle the request itself |
| `InView` | Request navigation inside the web view |

Ordinary `web_view(url)` documents opt into this callback on AppKit/UIKit when a handler is
provided. User-activated links and new-window requests (`target="_blank"`) are cancelled
before the UI-thread callback runs. Initial loads, `.go()` reloads, embedded frames, and
same-document fragments remain in the web view. `LinkPolicy::InView` deliberately reissues
the request without re-triggering a click callback. Session reuse updates the opt-in.
Other backends retain their ordinary-document navigation behavior; use inline/resource
mode for portable site-boundary policies.

## Retain a page across app navigation

```rust
use day::prelude::*;
use day_piece_webview::{WebSession, web_view_inline};

fn retained_editor() -> impl Piece {
    web_view_inline(res::assets::editor)
        .session(WebSession::global("app.editor"))
}
```

A normal view is recreated when its Day subtree is rebuilt. Supported backends retain a
session's browser engine, including page state, scroll position, JavaScript context, and history.
Use one stable key for a persistent surface. Attach it to only one visible view at a time.
Hoist any associated Day signals separately; retaining the browser does not retain page-local
Rust state.

Retention is implemented for AppKit, UIKit, macOS GTK, and Qt WebEngine. Linux GTK, Android,
Windows XAML, Wry hosts, Harmony, and web-dom currently ignore the engine-retention request.
There is no disposal API: a session lasts for the process lifetime. Do not allocate one per
book, list item, or transient preview. Resource sessions also pin their first provider and
initial URL; passing a different provider under the same key does not replace that content.

## Content blocking and page scripts

A browser's content blocker, reader mode or password filler needs the same few engine hooks:
refuse a request before it leaves the device, hide elements, run a script in every page, and
let that script talk back to the app. The piece exposes those hooks on every engine that has
them. It ships no blocking rules: the app supplies the decisions, the rule lists and the
scripts.

```rust
use std::sync::Arc;
use day::prelude::*;
use day_piece_webview::{
    ContentRuleList, RequestDecision, RequestInfo, UserScript, content_rules_support,
    request_filter_support, web_view,
};

fn guarded(url: Signal<String>, rules: Signal<Vec<ContentRuleList>>) -> impl Piece {
    let blocked = Signal::new(0usize);
    web_view(url)
        // Android, WebView2, Qt and ArkWeb ask this for every request, on their own thread.
        .request_filter(Arc::new(|request: &RequestInfo<'_>| {
            if request.url.contains("://ads.example/") {
                RequestDecision::Block
            } else {
                RequestDecision::Allow
            }
        }))
        .on_blocked(move |loads| blocked.update(|n| *n += loads.len()))
        // WebKit engines compile these instead (AppKit, UIKit, GTK).
        .content_rules(rules)
        .user_style(".cookie-banner { display: none !important }")
        .user_script(UserScript::new("window.readerReady = true;"))
        .on_script_message("reader", |message| {
            Some(format!("{} bytes", message.payload.len()))
        })
}
```

A page script calls `window.dayMessage("reader", text)` and gets a promise of the handler's
reply, or `null` when it returns `None`. Check `request_filter_support()`,
`content_rules_support()`, `user_script_support()` and `script_message_support()` before
offering a feature that depends on one of them.

### Request filters

`.request_filter(Arc<dyn RequestFilter>)` sees each request as a `RequestInfo`: `url`,
`top_url` (the page the view shows), `frame_url` where the engine reports it, `kind` (a
`ResourceKind` such as `Script`, `Image` or `Subdocument`), `main_frame` and `method`. It returns
`RequestDecision::Allow` or `Block`. A closure `Fn(&RequestInfo) -> RequestDecision + Send + Sync`
is a filter.

Every request waits for the answer, on whatever thread the engine calls back on. Keep the filter
in memory and fast, and never touch signals or other reactive state from it. A filter that panics
allows the request.

The piece's own resource namespace (`day-resource:`, `https://day-resource.invalid/`, and
`http://day-resource.p<id>/` on Windows without WebView2) is the app's own site in a view made with
`web_view_resources` or `web_view_inline`, and the filter never sees it there. A view that browses
anywhere and borrows a provider for some pages with `.resources(provider)` sends those pages through
the filter like any other site. Every engine with a request filter follows the same rule
(`RequestHooks::filters_internal`).

`.on_blocked(callback)` hears about blocked requests on the UI thread, as a slice of
`BlockedLoad { url, page_url, kind }`. Blocks are batched: one call every 100 ms at most,
carrying everything blocked since the last call, so a page that blocks 300 requests costs a
few calls rather than 300. A batch keeps at most 2,000 loads; blocking continues past that,
and only the report drops the rest.

### Content rule lists

WebKit has no per-request hook. It compiles declarative JSON rule lists (Safari's content
blocker format) and applies them inside the engine. `.content_rules(lists)` takes a constant, a
signal or a closure of `Vec<ContentRuleList { id, version, json }>`.

- Each list compiles once, in WebKit's own process, and stays compiled across launches under
  its `id` and `version`. A relaunch with the same version loads the compiled list without
  reading `json`; a new version compiles again and replaces the old one.
- A view built with lists holds its first page until they are attached, so the first page is
  filtered too. If the lists change before they finish compiling, the first page waits for the
  newest ones: an older set that finishes first is dropped and does not release it. A
  navigation the app asks for meanwhile (a new URL in the binding) takes the first page's place
  and loads, filtered, once the lists are attached; a reload meanwhile does nothing. A later
  change swaps the lists once all the new ones are ready. Nothing waits on
  the UI thread.
- A list that does not compile is logged and reported to `.on_content_rules_error(callback)` as
  `ContentRulesError { id, message }`; the other lists still apply. WebKit allows at most
  150,000 rules per list and supports a subset of regular expressions (no `|`, no `{n,m}`, no
  `\d` or `\w`).
- Apple engines keep compiled lists in `WKContentRuleListStore.defaultStore`. WebKitGTK keeps
  them in `<user data dir>/<app>/day-web-content-rules`.
- WebKit reports nothing about what a rule blocked, so `.on_blocked` stays silent there.

### User scripts and styles

`.user_script(UserScript)` runs a script in the page's own JavaScript world, where it can read
and patch the page's globals. `UserScript::new(source)` runs at document start in every frame;
`.at(InjectAt::DocumentEnd)` waits for `DOMContentLoaded`, and `.frames(Frames::Main)` skips
iframes. Call it again for more scripts; they run in the order added.

`.user_style(css)` applies a style sheet to every page and frame. WebKitGTK installs it as a
user style sheet, which overrides the page's own rules. Other engines add a `<style>` element
at document start, so a page rule with higher specificity or `!important` can still win.

Engines that only offer "every frame, at document start" (Android, WebView2, ArkWeb) run a
document-end or main-frame script wrapped in a check, which makes its top-level `let` and
`const` declarations local to the script.

### Page messages

`.on_script_message(name, handler)` lets pages post text to the app. The page calls
`window.dayMessage(name, text)`; the handler receives a `ScriptMessage { name, payload, origin,
main_frame }` on the UI thread and returns the reply. Engines that define a channel object per
name also accept a direct `postMessage`: `webkit.messageHandlers[name].postMessage(text)` on
WebKit and `window[name].postMessage(text)` on Android. On WebView2, Qt and ArkWeb the shim
defines `window[name].postMessage(text)` itself, without a reply.

Every frame running the shim can post, third-party iframes included. Treat `payload` as
untrusted input, and check `origin` before acting on a message where the engine supplies it.

### Camera and microphone

WebKit, WebView2 and Qt show their own prompt when a page calls `getUserMedia`. Android WebView
shows none and denies every request unless the app answers it, so the piece answers. A page gets
the camera or microphone when the app holds the matching permission (`CAMERA`,
`RECORD_AUDIO`). When the app declares the permission but does not hold it yet, the system asks
the user first. Permissions the app does not declare are never granted, and other resources
(protected media IDs, MIDI) are denied. `.on_media_request(|request: &MediaRequest| -> bool)`
decides first, given the page's origin and the devices it asked for; `true` continues with the
same permission check. ArkWeb denies capture requests from pages.

### Backends

| Backend | Request filter | Filter thread | Rule lists | Scripts and styles | Page messages |
|---|---|---|---|---|---|
| AppKit, UIKit, macOS GTK | Unsupported | | `WKContentRuleListStore`, `WKUserContentController` | `WKUserScript`; styles as scripts | `WKScriptMessageHandlerWithReply`, page world |
| Linux GTK | Unsupported | | `WebKitUserContentFilterStore` | `WebKitUserScript`, `WebKitUserStyleSheet` | `register_script_message_handler_with_reply` |
| Android | `shouldInterceptRequest`, service workers through `ServiceWorkerClientCompat` | WebView worker | Unsupported | `addDocumentStartJavaScript`; older WebViews run them at `onPageStarted`, main frame only (`Emulated`) | `addWebMessageListener`; older WebViews an `addJavascriptInterface` bridge |
| WebView2 | `WebResourceRequested`, filter `*`, all source kinds | UI | Unsupported | `AddScriptToExecuteOnDocumentCreated` | `chrome.webview.postMessage`, top-level document only |
| Qt | `QWebEngineUrlRequestInterceptor` on the page | UI | Unsupported | `QWebEngineScript`, main world | Console bridge; replies reach the top-level document |
| ArkWeb | NDK scheme handler (`OH_ArkWeb_SetSchemeHandler`, http and https) | IO | Unsupported | `javaScriptOnDocumentStart` | `javaScriptProxy`; replies reach the top-level document |
| web-dom | Unsupported | | Unsupported | Unsupported | Unsupported |
| Servo | `load_web_resource` | UI | Unsupported | Unsupported | Unsupported |

Engine notes:

- **Android** names no resource type. The piece infers `kind` from the request's `Accept` header
  and the URL's extension (`ResourceKind::infer`) and tracks `top_url` from main-frame requests.
  A blocked request receives an empty `403`. Service-worker requests have no view, so they go
  through the filter of the newest view that has one. WebSocket handshakes are not offered to
  `shouldInterceptRequest`.
- **Android's fallback message bridge** (WebViews without web message listeners) is an object
  every frame of every page in the view can call, and WebView does not say which origin called
  it, so `origin` is empty there.
- **WebView2** names the resource context but not the frame or the page. The piece tracks the
  page from top-level `NavigationStarting`, and a document request for that URL counts as the
  main frame. Each filtered request crosses to the UI thread.
- **Qt** pages post through a console line rather than `QWebChannel`, which needs moc-generated
  object metadata (this crate's shim builds without moc) and an asynchronous handshake before
  the first message. A message from an iframe resolves its promise with `null` at once.
- **ArkWeb** requests that the filter blocks get an empty `403`. ArkWeb's own ad blocker
  (`AdsBlockManager`) is not used.
- **WebView2 is unverified**: it compiles only on Windows, and this code has not been built or run
  there yet.

## Browser duties

A view that browses the web is asked for more than a view showing an app's own pages. A page
opens a dialog, a server asks for a password, a link names a file to save, a load fails, the
process running the page ends. An engine does not handle these by itself: WKWebView answers a
`confirm()` nobody implements with `false`, and Android's WebView ends the whole app when its
renderer goes unhandled. These hooks cover them on WebKit (iOS and macOS) and on Android.

```rust
web_view(url)
    .on_dialog(|request, responder| { /* alert, confirm, prompt */ })
    .on_auth(|request, responder| responder.sign_in("ada", "secret"))
    .on_download(|request, responder| {
        responder.save_to_downloads(&request.suggested_name, |event| { /* progress */ });
    })
    .on_load_error(|error| show_error_page(error))
    .on_renderer_gone(|_| rebuild_the_view())
    .on_app_link(|url| LinkPolicy::OpenSystem)
    .swipe_navigation(true)
```

Every question hands the app a responder. Answer it once; a responder dropped unanswered
cancels what was asked, so a page is never left waiting.

### Script dialogs

`alert()`, `confirm()` and `prompt()` reach `.on_dialog(|request: DialogRequest, responder:
DialogResponder|)`, with the kind, the page's message, a prompt's default text and the address
of the frame that asked. The page's script waits until the responder is told `accept()`,
`accept_with(text)` or `dismiss()`.

Without a handler the view presents Day's own alert, confirm or prompt, titled with the host that
asked, so every Day app with a web view gets working dialogs. `present_dialog(request,
responder)` is that default, for a handler that changes the request first or shows only some
dialogs. On Android, a page's "leave this page?" question (`beforeunload`) keeps WebView's own
dialog.

### Sign-in

A server or proxy that answers with an HTTP Basic, Digest or NTLM challenge reaches
`.on_auth(|request: AuthRequest, responder: AuthResponder|)`: the host, the realm the server
named, whether a proxy is asking, and how many answers were already refused. `sign_in(user,
password)` sends the credentials, which the engine keeps for the site until the app exits;
`cancel()` loads the server's own "unauthorized" page. Without a handler every challenge is
canceled.

### Downloads

A response the engine cannot show, one the server marked `Content-Disposition: attachment`, and
a link with the `download` attribute are files to save. With `.on_download(|request:
DownloadRequest, responder: DownloadResponder|)` the app decides each one: the request carries
the address, a file name safe to join onto a directory, the type and the announced size.

- `responder.save_to(path, on_event)` saves at a path the app chose.
- `responder.save_to_downloads(name, on_event)` saves where the user looks for downloads: the
  system's Downloads collection on Android, a `Downloads` folder in the app's documents on iOS
  (which the Files app shows once the app's `Info.plist` sets `UIFileSharingEnabled` and
  `LSSupportsOpeningDocumentsInPlace`), and the user's Downloads folder on a desktop. A name
  already taken becomes `report (2).pdf`.
- `responder.cancel()` declines it.

`on_event` hears `DownloadEvent::Progress { received, total }`, then `Finished { location }` or
`Failed(reason)`, on the main thread. It keeps running after the view that started the download
is gone. `cancel_download(id)` stops one, `open_download(location)` shows a finished file in the
app the system opens it with (the share sheet on iOS), and `remove_download(location)` deletes
it. `download_support()` says whether a backend ever asks. Without a handler nothing is
downloaded. On the web, `open_download` returns `false`: the page cannot open a native
downloaded file.

Android's WebView only announces a download, so the piece fetches the file itself, with the
page's cookies, user agent and referrer. A `blob:` address exists only inside its page, which
reads it and hands the bytes over, naming the transfer by a random token only that page was given, and the transfer ends if the page is left (one the app answers after the page was left fails without starting); `data:` addresses are decoded byte for byte. A fetched download sends the Referer a browser's default policy would: the page's address without its fragment to its own origin, the origin alone to another, and none from an HTTPS page to an HTTP address, at every redirect. WebView does not pass along a
link's `download` attribute, so a page script notes the name as the link is clicked. A file served by the
view's own [resource provider](resource-provider.md) is saved from the provider on both
platforms.

### Failed loads and certificates

`.on_load_error(|error: &LoadError|)` hears every main-frame load that fails: `error.kind` is
`Offline`, `HostNotFound`, `Timeout`, `Connection`, `Certificate`, `Secure`, `Unsupported` or
`Other`, beside the address, the engine's error number and its description. The view is left on
whatever the engine shows for a failed load (WebKit keeps the page before it, Android shows a
page of its own), so a browser covers the view with its own error page until the next load
starts. Loading the failed address again retries; a reload would reload the page before it on
WebKit.

A certificate the system does not trust fails with `LoadErrorKind::Certificate`.
`trust_host(host)` lets pages from that host load despite it, in every view, until the app
exits; nothing is stored. Call it only after the user has said so, then load the address again.

### A lost page process

The process that runs a page can crash, or be taken back by the system for memory.
`.on_renderer_gone(|report: &RendererGone|)` hears it. On Android the view is dead from then on
and ignores every command, so build a new one in its place (rebuild the piece, say under a
`when`). One renderer serves every view of an Android app, so each view reports. On WebKit the
view survives and a load brings the page back; without the handler WebKit views reload
themselves. Android without the handler is left with a blank view, which is still better than
WebView's default of ending the app.

### File inputs

Android's WebView has no file chooser of its own. The piece shows Day's
[file picker](https://daybrite.dev/docs/reference/files/) for `<input type=file>`, filtered by the
input's `accept` list where it names known types, and allowing several files for `multiple`.
WebKit shows the system's chooser itself. The `capture` attribute is not honored yet.

### Links to other apps

An address no engine loads (`mailto:`, `tel:`, another app's scheme, Android's `intent:`) is
never loaded in the view. Each is reported like a link out of a bundled site and goes to the
system unless the app decides otherwise in `.on_app_link(|url| -> LinkPolicy)`. A browser
answers `LinkPolicy::Ignore` there, asks the user, and then calls `open_app_link(url)`. For an
`intent:` address with no app to take it, that returns the fallback page the address names, for
the app to load.

### Gestures and media

`.swipe_navigation(true)` turns on WebKit's swipes back and forward through the view's history,
from the screen's edges on iOS and with two fingers on a Mac trackpad. A view browsing the web
on Android pinches to zoom where the page allows it; a bundled site keeps the scale it was
designed for. Video plays inline on iPhone rather than taking over the screen.

### The user agent

An engine's own user agent says the view is embedded in an app: Android's carries `; wv` and
`Version/4.0`, and WebKit's stops before Safari's `Version/… Safari/…`. Sites act on it. Some
refuse sign-in to embedded views, and some push their own app. `.user_agent(UserAgent::Browser)`
sends what the platform's own browser sends, built from the engine's string on the device, so
it follows the installed engine and never goes stale: Chrome's on Android (reduced as Chrome reduces it, to `Android 10; K` and the major version), Safari's on iOS and
macOS. `UserAgent::Custom(text)` sends exactly `text`, and `UserAgent::Engine` (the default)
leaves the engine's own.

The setting is read when the view is created; build a new view to change it. A view in
[desktop mode](#zoom-desktop-sites-and-page-scripts) sends the desktop string instead. The
other engines keep their own string.

### Backends

| Duty | UIKit, AppKit, macOS GTK | Android | Others |
|---|---|---|---|
| Script dialogs | `WKUIDelegate` panels | `WebChromeClient.onJsAlert`, `onJsConfirm`, `onJsPrompt` | The engine's own dialogs |
| Sign-in | `didReceiveAuthenticationChallenge` | `onReceivedHttpAuthRequest` | The engine's default |
| Downloads | `WKDownload` (iOS 14.5, macOS 11.3) | `DownloadListener`, fetched by the piece | Unsupported |
| Failed loads | `didFailProvisionalNavigation`, `didFailNavigation` | `onReceivedError`, `onReceivedSslError` | Not reported |
| Certificate exceptions | `SecTrust` exceptions on the challenge | `SslErrorHandler.proceed` | Unsupported |
| Lost page process | `webViewWebContentProcessDidTerminate` | `onRenderProcessGone` | Not reported |
| File inputs | The system's chooser | `onShowFileChooser` and Day's picker | The engine's chooser |
| Links to other apps | Navigation policy | `shouldOverrideUrlLoading` | The engine's default |
| User agent | `applicationNameForUserAgent`, `customUserAgent` | `setUserAgentString` | The engine's own |

## Choose the right data channel

Use [JavaScript evaluation](webview-eval.md) for small commands and structured state, such as a
chart dataset, selected item, or reader position. Use [resource providers](resource-provider.md)
for documents and binary resources. For an EPUB, serve one requested chapter or image at a time;
do not serialize the entire book into a JavaScript call.

The public API does not currently provide arbitrary request-header injection. Files a page
offers to save reach the app through [downloads](#downloads), and pages reach the app through
[page messages](#page-messages).
Build a readiness convention for bundled pages and handle evaluation errors during navigation.
[Platform notes](platforms.md) describe the remaining differences.

`on_load(callback)` runs on the UI thread after a native backend reports completed navigation,
including a same-URL reload. Pair it with `JsHandle` to apply the latest UI state after loading;
use a reactive watcher for subsequent changes to avoid reloading and losing reading position.
Emulated iframe hosts do not report navigation and therefore do not invoke this callback.

## Shared logins and private browsing

Use `.profile(WebProfile::persistent(name))` on independent views to share browser storage, or
`WebProfile::private(name)` for an isolated ephemeral store. A session retains a loaded page;
a profile shares its data without retaining a page. See [profiles and privacy](profiles.md)
for native cookie access, complete data deletion, capability checks and platform limits.

## Force reload

Force reload keeps cookies, logins and site storage intact. Apple uses WKWebView's
`reloadFromOrigin`, GTK uses `reload_bypass_cache`, Qt uses `ReloadAndBypassCache`, and
WebView2 uses `Page.reload` with `ignoreCache`. Android temporarily applies `LOAD_NO_CACHE`
and restores its previous mode after completion or Stop. ArkWeb must evict the application's
shared resource cache before refreshing; other WebViews may consequently refetch resources.
The iframe backend reports Unsupported because it cannot guarantee cache bypass.

## Document tabs

The demo uses Day's `TabSet` and `document_tabs`. Each document owns its browser,
address editor, navigation history, profile choice and console state. Switching
or moving a tab retains its native webview; closing it disposes the page scope
and resolves outstanding evaluations with `ViewGone`. Private tabs pass their
profile choice to newly opened tabs and are excluded from Reopen Closed Tab.

`WebView::on_new_tab(foreground_label, background_label, callback)` installs an
application callback receiving `NewTabRequest { url, background }`. Both menu
labels take what `label` takes (a string, a `LocalizedText` from the application's
generated accessors, a signal or a closure) and are resolved when the piece is
built. This event
is separate from `on_external_link`: a new-tab request must not replace the
originating document. The app owns creating and selecting the new tab.

The adapters use engine new-window/navigation events and native link menus where
available. `target="_blank"` links and `window.open` reach the callback on every native
backend; Qt, Android and ArkWeb pass on only those a user gesture started (WebView2 and the
WebKit backends also report script-started ones), so a page cannot open tabs unprompted.
A tab built in the background loads its page while hidden (`dayscript/background-tab.yaml`).
DOM handles modifier clicks and link menus only in same-origin frames;
browser origin restrictions prevent intercepting remote iframe content. A URL
request does not transfer a JavaScript opener, POST body, or live popup browsing
context. Native macOS groups support the system tab tear-off and merge affordances. Other
backends currently reorder within a collection; pinned tabs and persisted session
restoration remain application policy. `dayscript/tabs.yaml` tests the
demo's native-page retention and independent state.

The persisted **Native tabs** preference changes presentation live. On macOS, AppKit,
Qt and GTK use real `NSWindowTabGroup` document windows; the existing webview moves
between its document window and the emulated host without reloading. Elsewhere,
Qt, libadwaita and WinUI use their document-tab controls, and Android uses Material
TabLayout. UIKit deliberately uses the emulated document strip: `UITabBarController`
represents application sections, and UIKit exposes no public Safari browser-tab
container. The ArkUI NDK and DOM backends also use the emulated strip.

Apps can supply `DocumentTabs::chrome` and `layout` to control the entire emulated
strip and its placement. `TabActions` supplies shared add/close commands and scoped
drag/drop reordering. The preference changes chrome and window placement; it does
not recreate document scopes, browser history, page JavaScript, or undo state.

Settings uses Day's singleton preferences-window API with a compact independent
window on desktop. Mobile platforms use their separate-window mechanism when
available; iPhone falls back to a page sheet.

## Optional Servo engine

The separate `day-piece-webview-servo` package adds a per-view engine choice on macos-appkit.
Its `servo` feature is disabled by default; native consumers do not depend on this package.
Native WebView remains available alongside it. Existing platform capability tables
on this page describe native integrations. See [Servo](servo.md) for engine selection,
coexistence, build requirements, profile restrictions, and current host limitations.
