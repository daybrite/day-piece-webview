<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# day-piece-webview-servo

Experimental Servo adapter for `day-piece-webview`, on macOS, Linux, Windows (MSVC), and Android.
The OpenHarmony host compiles but is [paused](../docs/servo.md#paused-openharmony) until the
engine's prebuilt SpiderMonkey can be used as shipped there.
Enable the `servo` feature (disabled by default) and call `register()` on the UI thread before
creating views. Day selects the host feature for the chosen toolkit. AppKit uses a native GPU surface;
other hosts present offscreen frames through Day Canvas. These paths remain experimental.

See [setup, registration, demo, and limitations](../docs/servo.md). This package depends on the
native piece; the native piece has no dependency on this package. Applications that expose the
adapter's `servo` feature inherit its resolution constraints even when that build option is off.
