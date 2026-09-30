<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Web View Demo

This app exercises `day-piece-webview` through a path dependency on the parent repository.
It contains a bundled website, a JavaScript console, app-link handling, and a dynamic resource
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

There are 37 applicable steps and five captures per target. Screenshots are stored beneath
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
