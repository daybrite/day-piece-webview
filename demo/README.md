<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Web View Demo

This app exercises `day-piece-webview` through a path dependency on the parent repository.
It presents a browser toolbar, URL bar, native history, loading status and privacy settings.
Use the Code button to open testing tools with a JavaScript console, app-link handling and a dynamic resource
view. The resource view loads relative scripts, CSS imports, an image, a nested document, and
CSS from a bundled asset directory.

## Run

From this directory:

```sh
day doctor
day launch -p macos-appkit --script dayscript/webview.yaml
day launch -p ios-uikit --script dayscript/webview.yaml
day launch -p android-material --script dayscript/webview.yaml
day launch -p web-dom --script dayscript/webview.yaml
```

The [walkthrough](dayscript/webview.yaml) checks the actual page, not just support labels.
Same-origin JavaScript evaluation runs on web-dom too. The common CI matrix runs it on all
eight primary targets, including Windows XAML and HarmonyOS. Test content is local; on web-dom
it is served by the development server and scoped resource worker, without external catalog
or website dependencies.

The walkthrough checks URL entry, invalid addresses, native Back/Forward history, Home and
the existing JavaScript/resource checks. History checks apply only to native renderers. Screenshots are stored beneath
`build/day/screenshots/<target>/`. See [testing and CI](../docs/testing.md) for exact coverage,
known failures, and missing cases. [src/lib.rs](src/lib.rs) shows the application API in use.

## Build against a local Day checkout

```sh
day patch --local ../../day
day patch --check
```

The patch configuration is written to gitignored `.cargo/config.toml`. Remove that file to
return to Git dependencies. This repository does not commit Cargo.lock; unpatched CI resolves
current framework sources. Applications consuming the piece should keep their own lockfile
for reproducible dependency selection.

## Harmony SDK layout

The installed API 18 SDK components must be under `<sdk-root>/18/{ets,js,native,toolchains}`
for hvigor's SDK manager. Set `OHOS_BASE_SDK_HOME` to that root and `NODE_PATH` to the
installed hvigor modules, then run `day build -p harmony-arkui`. A temporary root with
symlinks to existing SDK components is sufficient; no emulator is needed for the build.

## Browser commands

Go and Browser menus expose Back (primary+[), Forward (primary+]), Reload (primary+R),
Reload from Server (shift+primary+R), Stop (Escape) and Open Location (primary+L).
Primary is Command on Apple platforms and Control elsewhere. Alt+Home opens the bundled
home page; primary+comma opens Browser Settings. Unavailable commands are disabled.
Share opens the OS chooser. Platforms without a native chooser explicitly offer Copy Link.
The Tabs menu switches, moves and duplicates tabs. Its Show Tab Controls item (also a switch
in the testing drawer) reveals a row of tab buttons above each page that the
[tab walkthroughs](dayscript/tabs.yaml) tap; it is hidden by default.


## Generated native projects

Use a Day CLI with automatic native project generation; Apple builds require **Xcode 27
or newer**. Xcode project documents, Android Gradle files, and HarmonyOS hvigor files are
created under `build/` by `day prepare`, `day build`, and `day launch`. Open the selected
project with `day open -t <target>`; edits to generated files are disposable. Apple Swift
sources, plists and `.xcconfig` files, and any Android resource overlays, remain app-owned
under `platform/`. For additional native configuration, use
`day project add-target <target> --persist-native-project` to create a persistent override.

## Experimental Servo comparison

Servo is controlled by the demo's **`servo` Cargo feature**, which is disabled by default.
The `servo` Day flavor enables that feature and supplies the platform build settings.
From this directory, build either version with the same target and profile:

```sh
# Toolkit-native WebView only.
day build -t macos-appkit --profile release

# Native WebView and Servo in the same app.
day build -t macos-appkit --profile release --flavor servo
```

Replace `build` with `launch` to run either version. Selecting WebView in a Servo-enabled app
does not remove Servo from its binary; omit `--flavor servo` when building to exclude the engine.
Leave the `DAY_FLAVOR` environment variable unset for a native-only build.
For local Day development, use the checkout CLI as described in
[Servo setup](../docs/servo.md#run-the-demo), rather than an installed CLI.

`day launch -t macos-appkit --flavor servo` enables the optional `day-piece-webview-servo`
adapter alongside WKWebView. Substitute another target listed in
[Servo platforms](../docs/servo.md#platforms) to compare its native webview with Servo.
The demo registers the adapter before creating views. Settings chooses
the default for new tabs; each tab's renderer button reloads its current URL with the other
engine. Open the same URL in two tabs, switch one to Servo, and drag a native tab out to compare
separate windows. The ordinary build remains native-only. See
[Servo setup and limitations](../docs/servo.md), including its additional build tools.

The demo declares the adapter as an optional dependency, so Cargo resolves its engine graph
even in a native demo build. Apps depending only on `day-piece-webview` avoid that graph.

CI runs the native walkthrough across the platform matrix and separate mixed-renderer jobs
for all implemented Servo hosts. The website gallery groups matched WebView and Servo captures
by target, showing bundled and provider pages side-by-side. Run `experiments/renderer-comparison.yaml` with `--flavor servo`
to reproduce them locally. See [CI and gallery details](../docs/testing.md#renderer-comparison-in-ci-and-on-the-website).

For measured native-versus-Servo sizes and the repeatable measurement command, see
[demo build sizes](../docs/build-sizes.md).
