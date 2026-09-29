# Harmony WebView tests

The standard `ci.yml` matrix runs `demo/dayscript/webview.yaml` on all primary
targets, including `harmony-arkui`. There is no separate Harmony walkthrough.
The shared `daybrite/actions` workflow automatically installs the verified x86_64
ArkWeb runtime, applies the Oniro namespace/EGL fixes, and reboots before launching
the demo. No app-specific runtime setup is required.

The common walkthrough checks bundled JavaScript and CSS, Unicode, the console,
custom links, and reload. Its final screenshot includes a distinctive browser-only
background. The `harmony-rendering` job checks the downloaded Harmony screenshots
for actual browser paint, catching a blank surface even when JavaScript succeeds.

Runtime sources, checksums, compatibility details, and manual setup commands now
live with the [shared action](https://github.com/daybrite/actions/tree/main/.github/actions/setup-harmony-webview).
The runtime is Chromium 114 and the namespace workaround reduces browser-process
isolation; use fresh disposable emulators and bundled test content.

After preparing a local test emulator using that helper:

```sh
cd demo
DAY_OHOS_ARCH=x86_64 day launch -p harmony-arkui \
  --ohos-device 127.0.0.1:55556 --script dayscript/webview.yaml
cd ..
python3 scripts/verify-webview-render.py demo/build/day/screenshots/harmony-arkui
```

The screenshot verifier requires Pillow (`Pillow==11.3.0` in CI). Consumers of
`dayapp.yml@v1` need the shared workflow changes released to that ref before
removing their Harmony WebView exclusions.
