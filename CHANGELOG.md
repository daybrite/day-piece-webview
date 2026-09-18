<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

## Unreleased

- New: `WebView::app_assets()` — an inline site that reads the app's own files rather than only
  its own directory. WebKit's file-URL read access widens to the asset tree, and the GTK arm
  extracts the whole tree and allows file-URL fetches; the backends whose browsable base is
  already that tree are unchanged, and navigation policing stays on the site.
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
