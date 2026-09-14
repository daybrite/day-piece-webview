<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Web View Demo

The demo and on-device test app for [`day-piece-webview`](..). One page lists the support the piece
reports for this platform, runs JavaScript through a console, and shows a small site bundled under
`resource/assets/site/`. The site carries a link addressed to the app, and the page reports it when
the link policy hands it back. The app depends on the piece by path
(`day-piece-webview = { path = ".." }`), so a change to the piece and a change here land in one
pull request and one CI run.

## Run it

```sh
day doctor                                               # the toolchains for the targets below
day launch -p macos-appkit --script dayscript/webview.yaml
day launch -p ios-uikit --script dayscript/webview.yaml
day launch -p android-mdc --script dayscript/webview.yaml
day launch -p web-dom --script dayscript/webview.yaml
```

The script is the test. It checks the support rows, confirms the site's script and stylesheet
loaded, runs `document.title` through the console, clicks the site's app link, and captures two
screenshots under `build/day/screenshots/<target>/`. CI runs the same script on macOS, the iOS
Simulator, the Android emulator, and a headless browser
([../.github/workflows/ci.yml](../.github/workflows/ci.yml)). The site loads from the app bundle,
so none of those checks depend on a network. On web-dom the page cannot run JavaScript in the
frame, so the script checks the support rows and the view there.

## Build against a local day

No `Cargo.lock` is committed: the first build resolves day at the tip of `main`, and `cargo
update` moves it there again. To build against a checkout of day instead:

```sh
day patch --local ../../day             # writes .cargo/config.toml, gitignored
day patch --check                       # every day crate now resolves from the checkout
```

Delete `.cargo/config.toml` to go back to the git dependency. The lock is gitignored, so a
patched build cannot leave the checkout's paths behind for anyone else.
