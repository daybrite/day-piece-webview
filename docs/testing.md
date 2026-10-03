<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Testing and CI

The [CI workflow](../.github/workflows/ci.yml) runs on pushes, pull requests, manual dispatch,
and a daily schedule. It tests the crate and a real Day app. The demo resolves current Day
sources through the shared `daybrite/actions` workflow; a framework change can therefore break
this repository's daily run without a new piece commit.

## Coverage by layer

| Layer | What it verifies | What it does not verify |
|---|---|---|
| Rust tests on Linux, macOS, Windows | Request validation, namespace/lifetime handling, shared bodies, HEAD/ranges, response filtering, concurrency/reentrancy, JS reply codec | Native browser request delivery or painting |
| Backend Clippy checks | GTK, Qt, AppKit, XAML; cross-checks for UIKit, Android, WASM | Cross-checks do not run or link a mobile app |
| Node tests | Shipped browser hook, service worker, Harmony controller lifecycle in host harnesses | Real browser SW registration, native ArkWeb requests, GPU rendering |
| Windows C++ tests | UTF-16/UTF-8 evaluation payload conversion | Entire WebView2 lifecycle |
| Qt C++ lifecycle test | Deferred Windows-host startup, latest URL, single initialization, closing before/during startup | Uses a fake WebView2 bridge; does not run COM or the Windows engine |
| Demo dayscript | Actual engine navigation/evaluation and relative resource loading | Exhaustive lifecycle, performance, and protocol conformance |
| Harmony screenshot verifier | Visible green marker painted by the bundled page | Provider-specific rendering or all other target screenshots |

At the audited commit there are 18 Rust unit tests, including six provider tests, and 16 Node tests. The
provider tests cover Unicode/encoded paths, forbidden traversal, method/namespace validation,
shared response bodies, headers, error statuses, provider drop, reentrant/concurrent callbacks,
HEAD, and byte ranges. Tests execute featureless Rust code on host platforms; backend-feature
Clippy checks and demo execution supply separate coverage of the adapters.

The Node service-worker harness executes the shipped worker with real Response/MessageChannel
objects and mocked client/fetch interfaces. It checks binary bytes, MIME/isolation headers,
bundled mount routing, HEAD/ranges, missing owners, and scope/path boundaries. It does not
replace an actual service-worker test in a browser. The Harmony host harness tests command
queueing and disposal, not the NDK scheme handler itself.

The Linux Qt host job also builds and runs `tests/qt` with the offscreen Qt platform. It compiles
the Windows QWidget host against a fake bridge so its scheduling and lifetime rules are tested
without a Windows runner. XAML reparenting and the actual Windows Qt engine still require native
execution; downstream Stanza reader tests cover those paths separately.

## End-to-end resource assertions

[demo/dayscript/webview.yaml](../demo/dayscript/webview.yaml) runs 55 applicable steps on native
renderers and 47 on web-dom. The browser phase exercises native Back/Forward, address entry,
invalid address handling, Home and the collapsible testing drawer.
Its provider phase checks:

1. A relative JavaScript resource sets a document marker.
2. A relative CSS import sets a computed custom property.
3. An SVG image has the expected natural width.
4. A nested document loads and exposes its expected DOM marker.
5. CSS from a bundled directory mounted at `__day_assets/` loads.
6. Reload executes the resource script again.
7. Switching back to the regular bundled view still works.

Earlier steps check bundled JS/CSS, Unicode evaluation, the app-link callback, reload state,
and absence of unsupported placeholders. Screenshots are retained for inspection. Merely
producing a screenshot does not establish that its contents are correct.

The shared demo matrix includes macos-appkit, linux-gtk, linux-qt, windows-xaml, ios-uikit,
android-mdc, harmony-arkui, and web-dom. Browser CI uses **Chromium**. There are no current
matrix rows for macos-gtk, macos-qt, windows-gtk, or windows-qt, and no Firefox/WebKit browser
rows. A shared renderer reduces duplication but does not eliminate host-integration differences.

## Observed results

Audit snapshot: **2026-09-30**, commit `eab52ce`,
[run 36671367605](https://github.com/daybrite/day-piece-webview/actions/runs/36671367605).
These are observed walkthrough results, not a promise about subsequent revisions.

| Demo target | Result |
|---|---|
| macos-appkit | 37/37 |
| ios-uikit, iPhone simulator | 37/37 |
| android-mdc, Pixel 7 / API 36 | 37/37 |
| linux-qt | 37/37 |
| windows-xaml | 37/37 |
| harmony-arkui, phone and tablet emulators | 37/37 on each |
| web-dom, Chromium | 37/37 |
| linux-gtk | **33/37; failed** |

All host and cross-check jobs passed. Linux GTK built successfully, then failed in the provider
phase: the script marker, provider CSS marker, and bundled CSS marker checks timed out; after
reload the script marker was `null`. The image and nested-document assertions passed, and the
regular bundled view worked again after switching back. The logs identify resource-loading
failures but do not establish their root cause. This run does not validate the Linux GTK
provider as working correctly.

The separate `harmony-rendering` job was **skipped**, because it depends on the combined `demo`
job and that matrix failed. Harmony's 37/37 script results establish page execution, but this
run did not independently pass the pixel verifier. Its green marker is painted before switching
to the provider page, so even a successful verifier would not prove provider-specific painting.

## Gaps and next tests

The suite gives useful coverage of the common path and has already caught a native integration
failure. It is not yet a complete resource-provider compatibility suite. Priorities are:

1. Diagnose the Linux GTK script/CSS failure and keep those assertions required.
2. Run screenshot verification whenever the relevant artifacts exist, even if another target
   fails, and add a marker/geometry check to the provider page itself.
3. Exercise native error responses, HEAD, ranges, MIME/header handling, and same-origin fetch.
   Host tests cover the dispatcher; Qt requires explicitly different expectations.
4. Test removal during in-flight work, rapid replacement/reload, multiple providers/views,
   session reuse, and repeated open/close for leaked request objects or providers.
5. Add realistic fonts and larger binary resources, resource-size/memory checks, and loading
   latency measurements. The small demo does not establish EPUB-scale performance.
6. Add browser WebKit/Firefox, subdirectory deployment and worker-restart tests, plus the
   macOS GTK/Qt and Windows GTK/Qt host combinations.

These are coverage recommendations, not tests already implemented. Tests in downstream apps
such as Stanza-Redux are additional evidence, not part of this repository's CI contract.

## Run locally

At the repository root:

```sh
cargo test
node --test tests/*.mjs
cargo fmt --all -- --check
```

In `demo/`, choose a target with the required tools installed:

```sh
day launch -p macos-appkit --script dayscript/webview.yaml
day launch -p ios-uikit --script dayscript/webview.yaml
day launch -p android-mdc --script dayscript/webview.yaml
day launch -p web-dom --script dayscript/webview.yaml
```

Screenshots are written beneath `demo/build/day/screenshots/<target>/`. To verify the browser
paint marker, install Pillow and run from the repository root:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 scripts/verify-webview-render.py demo/build/day/screenshots/harmony-arkui
```

See [Harmony setup](harmony-emulator.md) for its emulator requirements. For a new regression,
prefer a deterministic fixture and a `web_eval` assertion that checks the loaded resource's
observable effect. Add protocol edge cases to Rust/worker tests and retain native execution
for anything involving engine delivery, lifecycle, focus, or painting.

## macOS GTK native scrolling

`tests/macos-gtk/scroll.swift` posts real system wheel events to an active test window.
It accepts a process ID, screen x/y coordinates, delta (negative scrolls down), and
`pixel` or `line`. Use isolated demo data; the probe moves the system pointer and
activates that window. macOS must permit event posting by the terminal/test runner.

For example, after opening a long article in Day-News:

```sh
swift tests/macos-gtk/scroll.swift PID SCREEN_X SCREEN_Y -40 pixel
```

Use `JsHandle::eval` or dayscript `web_eval` to assert `window.scrollY > 0` after a
short pause. Repeat from the top with `-1 line`, and with positive deltas to check
upward scrolling. For a nested `overflow:auto` element, assert its `scrollTop`
changes while `window.scrollY` stays unchanged. Check a sibling GTK list stays
stationary, then hide/reopen the web view and repeat. GTK widget snapshots omit
Cocoa sibling views; inspect an actual macOS window capture for this host.

Local validation on 2026-10-03: Day-News/macOS GTK passed 201 applicable seed and
walkthrough steps (11 platform skips), with the keyboard-selected row inspected in
both GTK and native macOS captures. System pixel and line wheel probes changed the
article scroll position. An inner overflow fixture moved by 240 pixels while the
outer document remained at zero. The piece's 19 Rust tests and GTK Clippy passed.
These observations cover macOS GTK; they do not add Linux or Windows runtime coverage.

## Document readiness and same-URL reloads

`on_load` is a completed-document notification. Linux GTK now reports WebKitGTK's
`LoadEvent::Finished`; Qt reports successful `loadFinished`; Windows Wry filters its
page-load callback to `PageLoadEvent::Finished`. This permits apps to start JavaScript
work after each reload of a stable application resource URL. URI-change/start events
are too early and may not occur on a same-URL reload. Day-News's
`dayscript/inline-reader.yaml` exercises automatic extraction across repeated resource
reloads, and checks that manual inline extraction retains the same document.


## Profile and privacy validation (2026-10-03)

The shared storage API adds profile-identity, cookie-origin, cookie-path and secure-transport
tests (22 Rust tests total). The ArkWeb controller harness checks that native cookie calls
select the persistent or incognito jar and that unavailable complete clearing never calls
global deletion (18 Node tests total). These are host tests, not ArkWeb execution.

The demo `dayscript/privacy.yaml` passed 21/21 checks on macOS AppKit, demonstrating private
localStorage isolation, restoration of persistent storage after switching back, and complete
data clearing followed by a fresh browser view. The
downstream Day-News synthetic login fixture passed 53/53 applicable checks on macOS AppKit,
GTK and Qt: browser login saved an HttpOnly cookie, native reader extraction used it, article
deselection and feed reselection opened dashboards, and forgetting cleared cookies,
localStorage and IndexedDB. AppKit also verified private mode does not use the persistent
login, and persistence across an application restart (15/15 restore checks).

AppKit, macOS GTK and Qt Clippy checks passed; UIKit and configured DOM cross-checks passed.
The Android demo APK built against the profile and deletion APIs. Windows adaptations were
reviewed against WebView2/Wry APIs but were not built or executed on this Mac. Harmony's Rust
component compiled, but hvigor's application build was blocked by the installed SDK's changed
management mode. No Harmony emulator was run, and its Web component remains unverified here.
Linux GTK's NetworkSession adapter was reviewed against the installed Rust bindings, including
explicit SQLite cookie persistence, but needs a Linux native build/run.

## Browser demo validation (2026-10-03)

The conventional browser layout passed its walkthrough on macOS AppKit, macOS GTK and
macOS Qt (55/55 each), and web-dom with the headless WebKit driver (47/47, native history
checks skipped). The iOS simulator walkthrough (55/55) and AppKit privacy walkthrough also passed.
Android built, but no connected device was available for runtime tests;
Harmony's Rust, ArkTS and signed HAP build passed against the installed API 18 SDK without
running an emulator. Windows and Linux adapters need their native CI runners.

Back/Forward now use Qt's native history object directly rather than its view actions; the
walkthrough reproduces the local-page navigation case where those actions remained disabled.
