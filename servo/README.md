<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# day-piece-webview-servo

Experimental Servo adapter for `day-piece-webview`, initially on macOS AppKit.
Enable the `servo` feature (disabled by default) and call `register()` on the UI thread before
creating views. Day selects the `appkit` host feature when building for `macos-appkit`.

See [setup, registration, demo, and limitations](../docs/servo.md). This package depends on the
native piece; the native piece has no dependency on this package. Applications that expose the
adapter's `servo` feature inherit its resolution constraints even when that build option is off.
