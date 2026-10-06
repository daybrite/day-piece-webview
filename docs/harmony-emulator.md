<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Harmony WebView testing

The standard [CI workflow](../.github/workflows/ci.yml) runs the common
[demo walkthrough](../demo/dayscript/webview.yaml) on Harmony phone and tablet emulators.
There is no separate Harmony walkthrough and no exclusion of provider/page assertions.
The [CI audit](testing.md#observed-results) records the actual results and the separate
screenshot-verification gap.

## Engine setup

The shared `daybrite/actions` workflow installs a matching x86_64 ArkWeb runtime, applies the
Oniro namespace/EGL setup, and reboots before launching the demo. An ARM64-only ArkWeb package
on an x86_64 image cannot render the page, regardless of whether the Rust app and HAP compile.
Check the installed engine ABI when diagnosing a blank view.

Runtime sources, checksums, and manual setup commands live with the
[shared setup action](https://github.com/daybrite/actions/tree/main/.github/actions/setup-harmony-webview).
Its Chromium 114 test runtime and namespace workaround are emulator provisions, not guidance
for shipping a production browser runtime. The workaround reduces browser-process isolation;
use disposable emulators and controlled test content.

After preparing a local emulator, run from `demo/`:

```sh
DAY_OHOS_ARCH=x86_64 day launch -p harmony-arkui \
  --ohos-device 127.0.0.1:55556 --script dayscript/webview.yaml
```

## Diagnose execution separately from painting

The walkthrough checks bundled content, evaluation replies, app links, reload, and resource
loading. A JavaScript result can succeed even if the GPU surface is blank. It therefore also
captures `webview-render-proof.png`, where the bundled page paints a distinctive green marker.

With Pillow installed, run from the repository root:

```sh
python3 scripts/verify-webview-render.py demo/build/day/screenshots/harmony-arkui
```

The verifier fails if no marker capture exists or too few expected pixels are visible. It checks
the bundled page, not the later provider screenshot. In CI the `harmony-rendering` job currently
depends on the whole demo matrix; an unrelated target failure can skip this check. Do not count
a skipped verifier as evidence of successful painting.

A screenshot waits for the page's renderer before Day's own ArkUI checkpoint: the module's
`settle` (the `DayPieceModule` hook, docs/extending.md in Day) arms four animation frames in
every live page, lets any CSS transition run out, and polls for the result, capped at two
seconds, because under the emulator's
software GL the painted page trails the DOM a script just changed by seconds, and a capture
taken on the ArkUI checkpoint alone showed the previous state. The cap keeps a hidden or
mid-load page (no frames) from stalling the capture.

For failures, inspect the dayscript report, screenshot artifacts, device logs, runtime ABI
report, and ArkWeb renderer-exit messages. The host-side `tests/harmony-controller.mjs` harness
covers queueing, attachment, errors, disposal, and the settle wait. Native resource interception is exercised
by the emulator walkthrough, not by that Node harness.
