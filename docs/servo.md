<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Experimental Servo renderer

The separate [`day-piece-webview-servo`](../servo/) package adds Servo 0.6 to the hosts listed [below](#platforms).
Its `servo` Cargo feature is disabled by default; the native piece has no dependency on it.
The toolkit-native WebView remains available in the same build. Each piece selects its renderer independently;
Servo and WebView pages can coexist in tabs or separate windows of one app.

## Enable it

```toml
[dependencies]
day-piece-webview = "0.4"
day-piece-webview-servo = { version = "0.4", optional = true, default-features = false }

[features]
servo = ["dep:day-piece-webview-servo", "day-piece-webview-servo/servo"]
```

Day enables the adapter's toolkit feature (`appkit`, `gtk`, `qt`, `xaml`, `winui`, `material`, or
`arkui`). For direct Cargo checks, enable the matching adapter feature as well. Register on the UI thread before constructing views,
then select the engine per piece:

```rust
use day_piece_webview::{WebRenderer, web_view};

#[cfg(feature = "servo")]
day_piece_webview_servo::register()?;

let native = web_view(native_url);
let experimental = web_view(servo_url).renderer(WebRenderer::Servo)?;
```

`renderer` returns `RendererUnavailable` if the adapter has not been registered or its host is
unavailable. It never silently substitutes the native renderer. `WebRenderer::available()` checks availability
without initializing Servo. `JsHandle::renderer()` reports the attached engine. Existing
crate-level support functions describe the toolkit's native integration; query the selected
renderer for private browsing, named-profile, and data-clearing support.

## Dependency boundary

The Cargo dependency direction is:

```text
native app ────────────────> day-piece-webview ──> Day / native toolkits
mixed-engine app ──────────> day-piece-webview
                 └────────> day-piece-webview-servo ──> day-piece-webview
                                                   └─> Servo / SpiderMonkey
```

The adapter lives under `servo/` with an independent manifest and workspace. The native package
has no dependency on it, including optional and development dependencies. A native consumer's
fresh lockfile therefore excludes Servo and SpiderMonkey, and those packages do not enter its
`cargo vendor` output or constrain its ICU resolution. CI checks this with
`python3 scripts/check-native-resolution.py`; inspecting only an enabled `cargo tree` would miss
optional dependencies retained in a lockfile.

An application that exposes the adapter's `servo` feature still resolves its engine dependencies
even with that feature disabled in the selected build. That includes this comparison demo. Such applications still inherit
SpiderMonkey's ICU 2.1 constraint and the larger lockfile/vendor graph. Enabling `servo` controls
compilation and linking; omitting the adapter dependency provides resolution isolation.

## Rollout

Day's matching changes must land first. This piece's CI resolves Day from Git main, while an
installed older CLI cannot generate the new hosts or interpret new manifest fields. A shared
`0.5` compatibility label does not establish that those changes are present. Validate the
piece against the updated Day revision and provide a CLI release containing the changes
before downstream apps adopt the manifest fields or remove their native host projects.

Keep the experimental engine integration and the demo's generated-host migration separately
reviewable. The package split isolates native consumers, but does not remove the Day release
prerequisite for the generated hosts or experimental linker metadata.

## Run the demo

From `demo/`, use the supplied Day flavor:

```sh
day launch -t macos-appkit --flavor servo
```

This enables the demo's optional adapter dependency and the adapter's `servo` feature. A launch
without `--flavor servo` builds the ordinary native demo. Initial Servo compilation is substantially
larger than a native build. The supplied flavor targets macOS 15.5+, matching the published
SpiderMonkey objects, and requires Python 3.11+ and LLVM/Clang 19+ in addition to Day's
Apple toolchain. Day sanitizes the Apple build environment; supply dependency tools explicitly.
For Homebrew installations:

```sh
DAY_BUILD_TOOL_PATH="$(brew --prefix python@3.12)/libexec/bin:$(brew --prefix llvm)/bin" \
  day launch -t macos-appkit --flavor servo
```

For local framework development, replace `day` with
`cargo run --manifest-path /path/to/day/Cargo.toml -q -p day-cli --` using the path to your Day
checkout. Keep the app and piece patched to that same checkout.

Settings includes a **Default renderer** preference, which applies to newly created tabs.
The toolbar's **WebView** or **Servo** button shows the current renderer. Clicking it rebuilds
that tab at its current URL using the other renderer. Other tabs keep their renderer and
browsing context. Switching loses the current JavaScript heap, scroll state, and history;
it does not transfer cookies or logins between engines.

To compare a website, open `https://daybrite.dev` in two tabs and switch one to Servo. With
native window tabs enabled, drag one tab out of its tab bar to create a separate window.
Each window retains its renderer selection. Compare release builds for timing measurements;
first engine startup, disk/network caches, and debug compilation can dominate small differences.

## Platforms

| Day targets | Host | Requirements / validation |
| --- | --- | --- |
| `macos-appkit` | Native NSView / Surfman surface | arm64, macOS 15.5+, Python 3.11+, LLVM 19+; existing runtime regression |
| `macos-gtk`, `macos-qt` | Offscreen Day Canvas | Same Apple minimum; toolkit and native browser libraries; GTK and Qt lifecycle/comparison tests passed locally |
| `linux-gtk`, `linux-qt` | Offscreen Day Canvas | EGL/OpenGL, fontconfig, FreeType, LLVM 19+, Python 3.11+; Xvfb/Mesa for CI |
| `windows-winui` | Offscreen Day Canvas / ANGLE | MSVC, LLVM, Python; GNU GTK/Qt targets are not supported by this adapter |
| `android-material` | Offscreen Day Canvas / EGL | NDK 28.2.13676358; matching rustls Kotlin verifier AAR; APK, lifecycle/comparison tests, and live HTTPS smoke test passed locally |
| `harmony-arkui` | Offscreen Day Canvas / EGL | **Paused** (see [below](#paused-openharmony)): the host code compiles and ran once on the 7.0 emulator with local workarounds, but the published SpiderMonkey archive cannot be used as shipped |

The comparison CI matrix builds and runs both dayscripts for every row and publishes paired
captures by target. Adding a matrix row is not evidence of a successful run: Windows and Linux
full runtime validation is still pending a successful CI run.
Servo has no iOS or WebAssembly host here; those targets retain their native integration.

## Paused: OpenHarmony

The `harmony-arkui` row is out of the comparison matrix and out of `demo/Day-servo.toml`'s
targets as of 2026-10-13. The adapter's ArkUI host (`renderer::surface` on a Day canvas) stays
in the tree and compiles with `--features servo,arkui`; it is the engine's dependencies that
block a supportable build. Three problems stood between `day launch -t harmony-arkui --flavor
servo` and a Servo tab on the OpenHarmony 7.0 x86_64 emulator; the first two are why this is
paused rather than fixed.

1. **libc++ ABI mismatch with the prebuilt SpiderMonkey.** mozjs-sys downloads
   `libmozjs-x86_64-unknown-linux-ohos.tar.gz`, which the mozjs project builds with
   `CARGO_OHOS_DOWNLOAD_PREBUILT=19`, the openharmony-rs LLVM 19 toolchain, because every
   published SDK's clang (15.0.4 in the 5.1, 6.0.0.1, and 7.0 SDKs) is too old for Servo. That
   toolchain's libc++ uses the `std::__h` ABI namespace; the SDK's `libc++_shared.so` uses
   `std::__n1`. A HAP linked with the SDK clang therefore fails to load:
   `Error relocating libentry.so: _ZNSt3__h11__call_onceERVmPvPFvS2_E: symbol not found`, which
   the ArkTS host reports as `Cannot read property setEnv of undefined`. This is the failure in
   the comparison job's log. Building with the same external toolchain and bundling its
   `libc++.so` works; the Day CLI now stages whatever cargo-ohos reports under
   `runtime_libraries`, so `CARGO_OHOS_DOWNLOAD_PREBUILT=19` (plus a cache of
   `~/.cache/cargo-ohos/ohos-llvm`, about 2 GB) is the whole workflow-side change for this one.
2. **mozglue's env interposer crashes on OpenHarmony.** The same archive carries
   `mozglue/interposers/env_interposer.cpp`, which overrides `setenv`, `getenv`, `putenv`,
   `unsetenv`, and `clearenv` process-wide and resolves the real functions with
   `dlsym(RTLD_NEXT, …)`. That lookup fails on OHOS's musl loader, and `InterposerHelper.h`'s
   `dlopen("libc.so")` fallback is compiled only `#if defined(ANDROID)`, so the first `setenv`
   (Day's ArkTS host applying the launch environment) is a `MOZ_CRASH`: `SIGSEGV` at address 0
   in `get_real_symbol` under `std::env::set_var`. The only local workaround is binary surgery:
   `llvm-ar d js/src/build/libjs_static.a Unified_cpp_mozglue_interposers0.o` on a copy of the
   archive, handed to mozjs-sys through `MOZJS_ARCHIVE` (which it ignores whenever a tarball is
   already cached in its `OUT_DIR`). Not something to carry in CI.
3. **No resource reader on OpenHarmony.** The `servo` crate links `servo-default-resources`
   on every platform except `target_env = "ohos"`, where it expects the embedder to register one,
   so the constellation panicked at startup ("No resource reader registered"). Fixed: the
   adapter links the default reader there, and a panic hook now routes Servo thread panics to
   the logger, where they were previously lost with the process's stderr.

With all three addressed locally (release profile, since the 232 MB debug HAP exceeds the
emulator's install reserve), a Servo tab loaded the bundled site and answered `web_eval` on the
7.0 emulator. The full `experiments/servo.yaml` and `renderer-comparison.yaml` walkthroughs
have not been run there.

**To resume:** (a) a mozjs-sys release whose OpenHarmony archive no longer crashes in the env
interposer — upstream, extend the Android fallback in `mozglue/interposers/InterposerHelper.h`
to `__OHOS__`, or leave the interposers out of the standalone OHOS build — and (b) agreement to
build with the external LLVM 19 toolchain until an OpenHarmony SDK ships a new enough clang.
Then put the matrix row and the flavor target back, add `CARGO_OHOS_DOWNLOAD_PREBUILT=19` and
the toolchain cache to the "Install Servo OpenHarmony native SDK" step, and run both
walkthroughs on the emulator. `scripts/install-servo-ohos-sdk.sh` and
`scripts/boot-servo-ohos-ci.sh` are kept for that.

CI uses an isolated Python environment for screenshot checks. Untitled pixel-proof captures
are validated alongside the published pairs but do not require gallery titles or captions.
The paint proofs poll for the resource document to finish loading, including its script and
stylesheet markers, before changing the background. They then wait through two animation-frame
callbacks before capturing. An engine/user-agent reply alone is not a page-readiness signal:
Android [comparison job 119242971635](https://github.com/daybrite/day-piece-webview/actions/runs/39477891745/job/119242971635)
accepted the screenshot checkpoint roughly 250 ms before the resource page finished loading.
Servo logged "Suppressing pre-load-event reflow"; its capture was white even though every
JavaScript assertion passed. These checks
apply to both renderers on every comparison target; the pixel verifier still rejects blank
or stale captures.
Windows explicitly selects the MSVC linker to avoid Git Bash's unrelated `link.exe`.
The Windows comparison jobs build a local clone at a short physical path on the runner.
mozangle 0.7.1 passes all ANGLE object paths on one command line, exceeding Windows' 32,767
character process limit under the default workspace path. A `subst` drive does not help:
Day canonicalizes the project root back to its physical path. The cache, capture checks,
size measurements, and artifact uploads all use the short checkout until mozangle supports
linker response files.

After building on Windows, stage ANGLE beside the executable before launching (from the
repository root):

```sh
python scripts/stage-windows-angle.py demo/build/day/flavors/servo/cargo/windows-winui/debug/debug
```

Use Python matching the application's architecture. The helper copies `libEGL.dll` and
`libGLESv2.dll` from the newest complete mozangle build in that profile directory (a cached target
directory can hold stale ones) and checks their native loader dependencies and EGL entry
points. CI runs it before the comparison scripts. Day does not currently stage
these dynamically loaded libraries; without them, Surfman panics with `egl function was not
loaded` on the first Servo view. Repeat after rebuilding ANGLE, and ship both DLLs beside
the executable when distributing a Windows Servo app.

Selection uses the DLL modification times, keeping both DLLs from the same build. If you
switch back to an older dependency resolution locally, clean that Cargo target directory and
rebuild first; a reused older build may not have the newest timestamps.

Harmony installs the API-20 native compiler separately from Day's API-18 packaging SDK;
installing both SDKs into the same directory mixes incompatible component versions.
The Linux GTK fixture job disables WebKit's sandbox only in that CI process, because hosted
runner restrictions prevent its bubblewrap network namespace from starting.

Use `day launch -t <target> --flavor servo` for any listed target. Android's certificate verifier
is initialized with the application context from `day-platform-android`'s `DayRuntime` before
starting Servo; it does not read context from the Material widget bridge. Its Maven contribution
ships the matching Kotlin implementation. SpiderMonkey's pointer encoding also requires the adapter's
feature-gated `disable-native-heap-pointer-tagging` contribution. That changes the entire
Android process while Servo is enabled; Android warns that this compatibility escape hatch
may not exist on future hardware ([tagged pointers](https://source.android.com/docs/security/test/tagged-pointers)).
Native builds keep Android's default. These metadata and runtime-library packaging changes
require the matching Day CLI update before this adapter can run on Android.

Mobile bundled pages use an intercepted HTTPS
origin because packaged assets are not ordinary files. Storage uses Day's app-writable cache.

See upstream [Linux](https://book.servo.org/building/linux.html),
[Windows](https://book.servo.org/building/windows.html),
[Android](https://book.servo.org/building/android.html), and
[OpenHarmony](https://book.servo.org/building/openharmony.html) requirements.

## Host and lifecycle

One lazily initialized Servo engine serves all Servo views on the Day UI thread. AppKit
views have a Surfman/WebRender rendering context attached to their NSView. Other toolkits
use offscreen contexts and present decoded frames in a Day canvas. Day's existing piece
renderer registration supplies the native view; there is no Servo dependency in day-core.

On desktop Linux, the adapter preserves and restores the toolkit's GLX or EGL binding around
Servo initialization, commands, painting and release. EGL cannot become current while a GLX
context remains bound through libglvnd. The standalone `tests/linux/gl-context.rs` regression
uses real Mesa contexts under Xvfb to reproduce that conflict and check restoration, nested
calls and unwinding without building the Servo engine.

`day_piece_webview_servo::register()` selects a static host descriptor for the active toolkit,
using `day_piece_webview::renderer::appkit` or `renderer::surface`. Re-registering the same descriptor is harmless; replacing
a registered host or overriding the native renderer returns an error. Registration is permanent
for the process and does not initialize the engine. Availability and storage capabilities come
from that descriptor. The AppKit piece dispatches creation by renderer and routes updates and
release to the host that owns the view. The adapter uses shared helpers for events, evaluation
replies, and resource IO rather than copying the piece's private wire format. This adapter API
is experimental. The portable `renderer::surface` descriptor attaches to a canvas node and
receives the same commands. Ordinary canvases do not gain browser evaluation operations.

Servo wakes Day's main queue when it has work. Wakes are coalesced. Painting follows
Servo's frame-ready notifications; the adapter does not subscribe to Day's frame clock or
install an idle polling timer. The canvas host registers a screenshot hold
(`day_spec::capture::register_hold`, Day's docs/window-image.md): a dayscript `screenshot` or
`sample_pixel` waits until every attached Servo view has presented its latest frame, since the
toolkit's own render checkpoint cannot see a frame that is still being read back or decoded.
This hold drains frames already announced by Servo; it does not establish document readiness
or wait for future JavaScript/layout work. Walkthroughs must assert their page-specific readiness
conditions before capturing.
A page that animates continuously keeps a capture waiting until the step times out. AppKit forwards resizing, backing scale, focus, mouse, scroll,
and keyboard events. Closing a non-session view releases its browsing context and rendering
surface. The shared engine stays available for the next view.

Retained sessions use a separate Servo registry, so a native session and a Servo session never
share an engine object. As with native retained sessions, these contexts live for the process
lifetime. Resource providers are retained while their view or session uses them.

## Current limits

- The portable canvas host reads frames back through CPU memory and encodes/decodes PNGs.
  It currently rasterizes at one pixel per logical point (up to 8192 pixels per dimension).
  This can look softer on high-density screens and adds latency and CPU cost. Compare layouts
  and engine behavior, not native-versus-Servo GPU performance using this presentation path.
  Basic taps, drag scrolling, focus, and hardware key events are forwarded. Hover, selection
  drags, wheel routing, IME, and mobile soft-keyboard/text editing integration remain incomplete.
- This host runs Servo **in the app process**. It does not provide content-process sandboxing or
  crash isolation. Native WKWebView uses WebKit's content processes. Servo's multiprocess mode
  needs a separate executable entry point and packaging work; it is not enabled here.
- Servo views share one engine storage context in a process-lifetime temporary directory.
  Storage does not persist across app launches. Named profiles, private browsing, cookie APIs,
  and complete profile deletion are unsupported. Use the default `WebProfile`; a nondefault
  profile is blocked from loading rather than silently sharing storage. Native profiles stay
  separate from Servo.
- Servo 0.6 has no public stop-loading method. The demo disables Stop for Servo. Force reload
  currently uses Servo's ordinary reload and does not promise a network-cache bypass.
- Audio/video playback, WebGL, WebGPU, WebXR, and system clipboard integration are not enabled
  in this initial build. WebCrypto is enabled. This is not the same feature set as servoshell.
- Basic keyboard input works through key events; macOS marked-text/IME composition,
  accessibility-tree bridging, file pickers, permission prompts, JavaScript dialogs, native
  context menus, and `window.open` handling are not implemented. Command-click links can open
  another demo tab. These limits belong to this host, not necessarily to Servo's engine.
- Transparent backgrounds are not implemented by this host.
- The host does not yet forward AppKit appearance changes to Servo. Pages can report a light
  `prefers-color-scheme` while WKWebView follows the system's dark appearance. Select the same
  theme in the website when comparing layout.
- Desktop bundled pages use file URLs; mobile bundles use intercepted HTTPS origins. Resource providers use an intercepted HTTPS origin scoped to
  their provider ID; URL reports translate back to the piece's provider namespace. Remote sites
  use Servo's network stack and trust store, which can differ from system WebKit.
- The Servo feature currently resolves ICU 2.1 dependencies through SpiderMonkey. Day accepts
  that version family. The manifest also pins the PKCS#1 prerelease used by Servo's release to
  avoid an incompatible newer prerelease. Recheck these constraints when upgrading Servo.
- The macOS contribution opts into Day's experimental selective archive loading only when
  the adapter's `servo` feature is enabled. Native-only builds retain `-force_load`. The opt-in
  affects the complete app archive and uses symbol-retention heuristics; test other native
  dependencies and release/LTO configurations before shipping. This is a workaround for the
  bundled-library link conflict, not proof that overlapping library versions are interchangeable.
  Universal Servo builds are rejected until selective loading has architecture-specific roots
  and corresponding integration coverage. AppKit arm64 Debug and Release have passed the
  mixed-renderer regression; LTO configurations remain untested.

Upstream references: [Servo embedding guide](https://book.servo.org/embedding/overview.html),
[LTS requirements](https://book.servo.org/embedding/lts-release.html), and
[Servo 0.6 API](https://docs.rs/servo/0.6.0/servo/).

## Assessment of the shared API

The shared renderer-selection API keeps navigation, JavaScript, resource providers, tab lifetime,
and demo controls common to both engines. The AppKit walkthrough renders `daybrite.dev` with
both engines in one process and preserves the native tab's JavaScript state while another tab
switches engines.

The separate adapter package keeps engine dependencies and platform hosting code outside the
native piece. Keeping it in the same repository lets changes to the experimental registration
contract, adapter, and comparison demo be reviewed together. A separate Git repository would
not further isolate Cargo resolution; the one-way package dependency already does that.

The shared API does not imply equal capabilities. Servo retains the profile, privacy, reload,
input, and process-isolation limits listed above. Release runtime has been exercised on AppKit;
LTO and universal Servo configurations have not been validated. See the
[release build size comparison](build-sizes.md) for the measured artifacts on other platforms.
