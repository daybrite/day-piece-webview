#!/usr/bin/env python3
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
"""Resolve a fresh native-only consumer, including disabled optional dependencies."""
import json
from pathlib import Path
import re
import subprocess
import tempfile


def main():
    root = Path(__file__).resolve().parents[1]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root,
    ))
    package = next(p for p in metadata["packages"] if p["name"] == "day-piece-webview")
    forwarded = [f'day-piece-webview/{feature}' for feature in package["features"]]
    with tempfile.TemporaryDirectory(prefix="day-webview-native-") as directory:
        consumer = Path(directory)
        (consumer / "src").mkdir()
        (consumer / "src/lib.rs").write_text("// Synthetic dependency-resolution fixture.\n")
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "native-consumer-fixture"\nversion = "0.0.0"\n'
            'edition = "2024"\n[workspace]\n[dependencies]\n'
            f'day-piece-webview = {{ path = {json.dumps(str(root))} }}\n'
            # Lock resolution includes every root feature, even though none is enabled here.
            # Forward the piece's features to cover all toolkits and catch optional adapters.
            f'[features]\nall-piece-features = {json.dumps(forwarded)}\n'
        )
        # Running from the checkout also honors the developer's local Day patch configuration.
        subprocess.run(
            ["cargo", "generate-lockfile", "--manifest-path", str(consumer / "Cargo.toml")],
            cwd=root, check=True,
        )
        names = set(re.findall(r'^\[\[package\]\]\nname = "([^"]+)"$',
                               (consumer / "Cargo.lock").read_text(), re.MULTILINE))
        forbidden = sorted(name for name in names if
                           name in {"day-piece-webview-servo", "servo"} or
                           name.startswith(("servo-", "mozjs", "webrender", "surfman")))
        # servo_arc is a standalone utility also used by native HTML/CSS dependencies.
        if forbidden:
            raise SystemExit(f"Native consumer resolved experimental engine packages: {forbidden}")
        print(f"Native consumer: {len(names)} resolved package names; no Servo adapter or engine graph.")


if __name__ == "__main__":
    main()
