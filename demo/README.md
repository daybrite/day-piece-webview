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
day launch -p android-mdc --script dayscript/webview.yaml
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
