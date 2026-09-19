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

[Open the demo website](https://daybrite.github.io/day-piece-webview/),
[try the browser app](https://daybrite.github.io/day-piece-webview/webapp/), or
[compare platform screenshots](https://daybrite.github.io/day-piece-webview/gallery/).

[demo/](demo/) is a one-page app and this crate's on-device test. It shows a site bundled with the
app, runs JavaScript in it, and receives a link the site sends to the app. From `demo/`, run
`day launch -p macos-appkit --script dayscript/webview.yaml`, or the same command with
any other primary target. CI builds and runs the demo on macOS AppKit, Linux GTK and Qt,
Windows XAML, iOS UIKit, Android MDC, HarmonyOS ArkUI, and web-dom. It checks the support
labels, bundled page, JavaScript and app links where supported, and reloads the page.
GTK and web-dom capture the site without evaluation. HarmonyOS now attempts the same real-browser
walkthrough, including on x86_64. The dedicated [harmony webview workflow](.github/workflows/harmony-webview.yml)
requires it to pass: boot errors, timeouts, missing engines, and assertion failures fail the job.
It runs on pushes and pull requests and can also be started manually with **Run workflow**.
The shared app workflow still treats HarmonyOS emulator failures as best-effort; use this separate
check to assess browser support. It uploads the device logs, installed engine ABI report, and
screenshots for diagnosis. [The first strict Harmony run](https://github.com/daybrite/day-piece-webview/actions/runs/35414766914)
confirmed that the stock Oniro v6.1 image contains an ARM64-only ArkWeb package on its x86_64
system: the loader cannot find `libs/x86_64/libarkweb_engine.so`, and only 19 of 27 walkthrough
steps passed. CI now rejects an incompatible engine before launching the app. Rendering needs
an image with a matching x86_64 engine, or validation on a compatible ARM64 device; successful
compilation alone does not establish working rendering.

The shared workflow publishes [demo/website/site.toml](demo/website/site.toml) through daysite,
along with the captured screenshots and web-dom build, after the full primary-platform matrix
passes on the default branch.

## Implementation and dependencies

Each renderer and its supporting code live in this repository: Rust bindings for Apple and GTK,
C++ for Qt and Windows, Java for Android, ArkTS for HarmonyOS, and JavaScript for web-dom.
The browser's bundled-page link handler is in [src/browser.rs](src/browser.rs); Day supplies only
generic element, event, and cleanup hooks.

`day-bridge` declares the Rust/JavaScript boundary and `day-build` generates its module at build
time. `day build` discovers and stages the module automatically. This checkout requires Day's `dayHost.dom` bridge and named piece-operation APIs. Update the CLI,
framework dependencies, and this crate together.
The crate also uses Day's core, reactive, piece, and toolkit APIs, `linkme` for native renderer
registration, and `log` for diagnostics. Platform dependencies include `objc2` and `block2` on
Apple, `gtk4` and `webkit6` on Linux GTK, and the engines listed above. `cc` and `day-toolchain`
support native compilation. See [Cargo.toml](Cargo.toml) for versions and feature gates, and
[the implementation guide](docs/webview.md#browser-implementation) for browser behavior and limits.

For Flatpak, this crate declares the Qt WebEngine BaseApp in
`package.metadata.day.flatpak.bases`. Day includes it when the binary links `libQt6WebEngine*`;
the base version follows the selected Qt runtime. The requirement is owned here, so engine
packaging changes do not require editing Day's packer.

## Compatibility and development

This checkout requires Rust 1.89 or newer and declares compatibility with Day 0.4 in
[Cargo.toml](Cargo.toml). The crate is consumed from Git, not crates.io. Its Day dependencies use
`https://github.com/daybrite/day.git` without a branch, tag, or revision. Use the same source in
your app and keep its `Cargo.lock` to record the resolved revisions. Mixing Day source URLs or
refs can introduce duplicate framework crates and incompatible types.

For a local framework checkout, run `day patch --local ../day` from this repository, or
`day patch --local ../../day` from `demo/`. The demo depends on this crate by path, so it builds
against your working copy. `cargo test` runs the host tests for the JavaScript codec and the
front end. `node --test tests/browser.mjs` checks bundled-page navigation and listener cleanup.

The crate moved out of the [day repository](https://github.com/daybrite/day) with its history in
2026-09. The [JavaScript evaluation guide](docs/webview-eval.md) lives here too. Dayscript keeps its
`web_eval` syntax and calls the piece through Day's general named-operation registry.

## Part of Day

This crate is one piece of [Day](https://daybrite.dev), a Rust framework for building apps
out of each platform's own widgets — AppKit, UIKit, Android's Material widgets, GTK 4, Qt 6,
XAML, and ArkUI — from one codebase. When you write `button("Save")`, macOS shows an
`NSButton` and Android shows a Material button. The framework also ships the tooling around
the app: the `day` CLI, a VS Code extension, GitHub CI workflows, localization,
accessibility, and dayscript automation.

New to Day? Start at [daybrite.dev](https://daybrite.dev), or browse the
[source repository](https://github.com/daybrite/day).
