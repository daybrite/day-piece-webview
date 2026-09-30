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

There is no public `can_go_back` or loading-progress signal. `support() == Native` indicates a
compiled command implementation, not that history is nonempty or the engine started successfully.

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

## Choose the right data channel

Use [JavaScript evaluation](webview-eval.md) for small commands and structured state, such as a
chart dataset, selected item, or reader position. Use [resource providers](resource-provider.md)
for documents and binary resources. For an EPUB, serve one requested chapter or image at a time;
do not serialize the entire book into a JavaScript call.

The public API does not currently provide a general load-completion event, download manager,
cookie-store API, arbitrary request-header injection, or a general JavaScript message bridge.
Build a readiness convention for bundled pages and handle evaluation errors during navigation.
[Platform notes](platforms.md) describe the remaining differences.

`on_load(callback)` runs on the UI thread after a native backend reports completed navigation,
including a same-URL reload. Pair it with `JsHandle` to apply the latest UI state after loading;
use a reactive watcher for subsequent changes to avoid reloading and losing reading position.
Emulated iframe hosts do not report navigation and therefore do not invoke this callback.
