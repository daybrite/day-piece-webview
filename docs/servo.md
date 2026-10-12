<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Experimental Servo renderer

The separate [`day-piece-webview-servo`](../servo/) package adds Servo 0.6 to **macos-appkit**.
Its `servo` Cargo feature is disabled by default; the native piece has no dependency on it.
WKWebView remains available in the same build. Each piece selects its renderer independently;
Servo and WebView pages can coexist in tabs or separate windows of one app.

## Enable it

```toml
[dependencies]
day-piece-webview = { git = "https://github.com/daybrite/day-piece-webview.git" }
day-piece-webview-servo = { git = "https://github.com/daybrite/day-piece-webview.git", optional = true, default-features = false }

[features]
servo = ["dep:day-piece-webview-servo", "day-piece-webview-servo/servo"]
```

Day enables the adapter's `appkit` feature for `macos-appkit`. For direct Cargo checks, enable
`day-piece-webview-servo/appkit` as well. Register on the UI thread before constructing views,
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

## Host and lifecycle

One lazily initialized Servo engine serves all Servo views on the AppKit main thread. Each
view has a Surfman/WebRender rendering context attached to its NSView. Day's existing piece
renderer registration supplies the native view; there is no Servo dependency in day-core.

`day_piece_webview_servo::register()` installs a static AppKit host descriptor through
`day_piece_webview::renderer::appkit`. Re-registering the same descriptor is harmless; replacing
a registered host or overriding the native renderer returns an error. Registration is permanent
for the process and does not initialize the engine. Availability and storage capabilities come
from that descriptor. The AppKit piece dispatches creation by renderer and routes updates and
release to the host that owns the view. The adapter uses shared helpers for events, evaluation
replies, and resource IO rather than copying the piece's private wire format. This adapter API
is experimental and currently has only an AppKit host.

Servo wakes AppKit's main dispatch queue when it has work. Wakes are coalesced. Painting follows
Servo's frame-ready notifications; the adapter does not subscribe to Day's frame clock or
install an idle polling timer. AppKit forwards resizing, backing scale, focus, mouse, scroll,
and keyboard events. Closing a non-session view releases its browsing context and rendering
surface. The shared engine stays available for the next view.

Retained sessions use a separate Servo registry, so a native session and a Servo session never
share an engine object. As with native retained sessions, these contexts live for the process
lifetime. Resource providers are retained while their view or session uses them.

## Current limits

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
- Bundled pages use file URLs. Resource providers use an intercepted HTTPS origin scoped to
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
  and corresponding integration coverage. The tested configuration is arm64 Debug.

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
input, and process-isolation limits listed above. Release/LTO and universal Servo configurations
have not been validated.
