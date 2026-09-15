<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

## Unreleased

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
