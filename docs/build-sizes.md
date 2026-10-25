<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Demo build sizes

The demo's `servo` Cargo feature adds the engine to the app. It is disabled by default.
The `servo` Day flavor enables it; the resulting app contains both renderers. A runtime
renderer preference changes which engine a view uses, not the size of the installed app.

From `demo/`, build either variant:

```sh
day build -t macos-appkit --profile release
day build -t macos-appkit --profile release --flavor servo
```

Use another [supported target](servo.md#platforms) in place of `macos-appkit`.
Use the local checkout CLI when developing Day; see [the demo instructions](../demo/README.md).
Leave `DAY_FLAVOR` unset for a native-only build.

## Measured release builds

Measured on October 12, 2026, using demo revision `43ab638`, Servo 0.6.0, Rust 1.99.0, and
the local Day checkout. Every row uses ARM64 and the `release` profile for both variants.
The Cargo lockfile checksum matched within each pair. One MiB is 1,048,576 bytes.

| Target / artifact | Native only (MiB) | Native + Servo (MiB) | Added (MiB) | Size ratio |
| --- | ---: | ---: | ---: | ---: |
| macOS AppKit `.app` | 38.31 | 200.77 | 162.46 | 5.24× |
| Android Material Components APK | 8.67 | 115.36 | 106.69 | 13.30× |
| Harmony ArkUI HAP | 7.30 | 111.04 | 103.74 | 15.21× |

The Android APKs are unsigned release outputs; the Harmony HAPs use the local development
signature. The macOS numbers sum the `.app` bundle's files, without a DMG wrapper.
Unpacked Android payloads are 10.30 and 117.02 MiB; unpacked Harmony payloads are 7.19 and
110.12 MiB. Most of the added bytes are in the app's native executable or shared library.

The macOS Servo release app also passed all 36 steps of the mixed-renderer regression.
Android and Harmony release results here establish build size, not release runtime validation.
Linux GTK/Qt and Windows XAML/WinUI have not been measured in this local run. Use their CI
size reports when available; these ARM64 results are not estimates for those targets.

The native variant uses the platform's browser installation. The Servo variant ships its
engine alongside the native integration. Compare within each row: packaging and external
runtime dependencies differ between operating systems.

## Measure a pair

From the repository root, with Python 3.11 or newer:

```sh
python3 scripts/measure-renderer-size.py --target macos-appkit --day-bin /path/to/day
```

The default profile is `release`; `--profile debug` measures development builds. For local
framework development, pass an executable wrapper that invokes the checkout's CLI:

```sh
#!/bin/sh
exec cargo run --manifest-path /path/to/day/Cargo.toml -q -p day-cli -- "$@"
```

The script builds native-only first and Servo second with the same target, profile, and Cargo
lockfile. It measures each artifact immediately because mobile build hosts may reuse the same
output path across flavors. It writes `sizes.json`, `sizes.md`, and build logs beneath
`demo/build/renderer-sizes/<target>/<profile>/`. `--out` changes that directory.

Set `DAY_ANDROID_ABI` explicitly for Android, for example `arm64-v8a`. If the staging directory
already contains other ABIs, include those in the requested list so both variants rebuild them.
The script rejects stale ABI contributions and mismatched package ABI sets. Set `DAY_OHOS_ARCH`
for Harmony. Compare the same architecture set; a dual-ABI APK contains two native builds.
Use the same macOS deployment target for both variants; the supplied Servo flavor requires 15.5.

Run the CI workflow manually with **measure-sizes** enabled to measure release builds and
upload `renderer-sizes-<target>` artifacts. Ordinary push and pull-request runs skip these
additional builds. Tables appear in the workflow summary, including when a runtime check
failed after a successful build. Android CI measures ARM64 + x86_64 packages; Harmony measures
ARM64. A platform that cannot build does not have a size result.

## Reading the numbers

- **Artifact bytes** measure the built `.app` bundle, APK/HAP package, or desktop executable.
- **Payload bytes** sum files in the bundle or unpacked archive. For an executable output,
  this is the executable's size; external shared libraries and adjacent resources are excluded.
- Both columns exclude Cargo caches, intermediate objects, separate debug symbols, and system
  browser/toolkit installations. Payload bytes do not measure disk allocation or memory use.
- Packages include compression, alignment, and signing overhead. Compare like package formats
  and profiles. Debug builds can be much larger than release builds.

The native demo still resolves the optional adapter's dependencies into its Cargo lockfile.
That resolution cost does not mean the Servo engine is compiled or shipped without the feature.
