<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# day-piece-webview

Embed a real browser view in a Day app.

`WKWebView` on macOS and iOS, WebKitGTK on Linux, `QWebEngineView` on Qt, Android's
`WebView`, and the ArkTS `Web` component on HarmonyOS — one Rust API for loading URLs and
hearing about navigation. Where a platform combination has no workable engine, the piece
reports that plainly instead of bundling a browser of its own.

Pieces are Day's reusable UI components, shipped as ordinary crates: one Rust API in
front, a real native control per platform behind it. Enable the backends you build for
with cargo features, and `day build` wires up the native side automatically.

## Use it

```toml
[dependencies]
day-piece-webview = { git = "https://github.com/daybrite/day-piece-webview.git" }
```

```rust
use day::prelude::*;
use day_piece_webview::web_view;

fn browser() -> impl Piece {
    let url = Signal::new("https://daybrite.dev".to_string());
    let go = Trigger::new();
    column((
        row((
            text_field(url).id("url"),
            button("Go").action(move || go.notify()),
        ))
        .spacing(8.0),
        web_view(url).go(go).id("web"),
    ))
}
```

`day build` turns on the piece's backend for each target, so the dependency line carries no
features. The view grows to fill the space it is offered. A site bundled under
`resource/assets/` loads with `web_view_inline(res::assets::site)`, and `JsHandle::eval` runs
JavaScript in either kind of page. [docs/webview.md](docs/webview.md) covers sessions, the link
policy, and each platform's engine.

## Platforms

| Target | Engine | Remote pages | Bundled site | JavaScript |
|---|---|---|---|---|
| macos-appkit, ios-uikit | `WKWebView` | Native | Native | Native |
| android-mdc | `android.webkit.WebView` | Native | Native | Native |
| linux-qt, macos-qt | `QWebEngineView` | Native | Native | Native |
| linux-gtk | WebKitGTK 6 | Native | Native | Unsupported |
| windows-xaml | WebView2 | Native | Native | Native |
| harmony-arkui | ArkWeb | Native | Native | Native |
| web-dom | `<iframe>` | Emulated | Native | Unsupported |

`support()`, `inline_support()`, and `eval_support()` return these answers at run time, so an app
can disable a control the platform cannot back. macos-gtk and windows-gtk have no WebKitGTK build
and render Day's placeholder. MSYS2 packages no Qt WebEngine, so on windows-qt the view shows the
page's URL as a label. On web-dom the browser keeps a cross-origin frame's history and URL from
the host page, so Back, Forward, and Stop do nothing there.

## Demo

[demo/](demo/) is a one-page app and this crate's on-device test. It shows a site bundled with the
app, runs JavaScript in it, and receives a link the site sends to the app. From `demo/`, run
`day launch -p macos-appkit --script dayscript/webview.yaml`, or the same command with
`-p ios-uikit`, `-p android-mdc`, or `-p web-dom`.

## Compatibility and development

This checkout requires Rust 1.89 or newer and declares compatibility with Day 0.4 in
[Cargo.toml](Cargo.toml). The crate is consumed from Git, not crates.io. Its Day dependencies use
`https://github.com/daybrite/day.git` without a branch, tag, or revision. Use the same source in
your app and keep its `Cargo.lock` to record the resolved revisions. Mixing Day source URLs or
refs can introduce duplicate framework crates and incompatible types.

For a local framework checkout, run `day patch --local ../day` from this repository, or
`day patch --local ../../day` from `demo/`. The demo depends on this crate by path, so it builds
against your working copy. `cargo test` runs the host tests for the JavaScript codec and the
front end.

The crate moved out of the [day repository](https://github.com/daybrite/day) with its history in
2026-09. The JavaScript evaluation contract stays there, in `docs/webview-eval.md`, because the
`web_eval` dayscript step and the day-core hook it drives live in day.

## Part of Day

This crate is one piece of [Day](https://daybrite.dev), a Rust framework for building apps
out of each platform's own widgets — AppKit, UIKit, Android's Material widgets, GTK 4, Qt 6,
XAML, and ArkUI — from one codebase. When you write `button("Save")`, macOS shows an
`NSButton` and Android shows a Material button. The framework also ships the tooling around
the app: the `day` CLI, a VS Code extension, GitHub CI workflows, localization,
accessibility, and dayscript automation.

New to Day? Start at [daybrite.dev](https://daybrite.dev), or browse the
[source repository](https://github.com/daybrite/day).
