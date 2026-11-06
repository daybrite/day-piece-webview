<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: MPL-2.0
-->

# day-piece-webview-servo

Render a Day web view with the experimental Servo engine.

[Day](https://daybrite.dev/docs/) is a Rust framework for building apps with native desktop and
mobile controls, plus HTML controls on the web.

Register the adapter during UI startup and select Servo per view. Native views remain available
alongside it. Review the platform requirements and missing browser-host features before use.

## Use

Add the optional adapter dependency, enable its `servo` feature, and register it on the UI
thread. Follow the setup guide for the required toolkit features and engine toolchain.

## Documentation

- [Setup and usage guide](../docs/servo.md)
- [Rust API](https://docs.rs/day-piece-webview-servo/0.4.3/day_piece_webview_servo/)
- [Crates.io](https://crates.io/crates/day-piece-webview-servo)

[Source](https://github.com/daybrite/day-piece-webview/tree/main/servo) · [MPL-2.0 license](https://github.com/daybrite/day-piece-webview/blob/main/LICENSE)
