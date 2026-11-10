<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: MPL-2.0
-->

# day-piece-webview

Embed web pages and application documents in a Day interface.

[Day](https://daybrite.dev/docs/) is a Rust framework for building apps with native desktop and
mobile controls, plus HTML controls on the web.

Use native browser components. Navigate with triggers, follow the page's title, load progress
and history as the engine reports them, evaluate JavaScript, capture page snapshots, choose
browser profiles, and serve bundled or app-owned resources. Browser builds
use iframes and remain subject to same-origin and embedding restrictions.

## Use

Add `day-piece-webview = "0.*"` to your app’s `[dependencies]`.
Build with the Day CLI to select the matching toolkit integration.

## Documentation

- [Setup and usage guide](https://daybrite.dev/docs/reference/webview/)
- [Rust API](https://docs.rs/day-piece-webview/0.4.4/day_piece_webview/)
- [Crates.io](https://crates.io/crates/day-piece-webview)
- [Browser profiles](https://daybrite.dev/docs/reference/webview/#browser-profiles)
- [Application resources](https://daybrite.dev/docs/reference/webview/#application-resources)
- [JavaScript evaluation](https://daybrite.dev/docs/reference/webview/#javascript-interaction)
- [Demo and comparison coverage](docs/testing.md#renderer-comparison-in-ci)
- [Run the demo](demo/README.md)

[Source](https://github.com/daybrite/day-piece-webview) · [MPL-2.0 license](https://github.com/daybrite/day-piece-webview/blob/main/LICENSE)
