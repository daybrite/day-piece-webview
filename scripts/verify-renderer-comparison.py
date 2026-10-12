#!/usr/bin/env python3
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
"""Require complete, equally sized renderer pairs and intact screenshot metadata."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

SCENES = ("bundled", "resources")
ENGINES = ("webview", "servo")


def verify(root, target):
    directory = Path(root) / target
    index = json.loads((directory / "gallery.json").read_text())
    if index.get("target") != target:
        raise ValueError("Screenshot index target does not match the CI target")
    captures = {}
    for entry in index["screenshots"]:
        if not entry["shot"].startswith("renderer-"):
            continue
        key = (entry["shot"], entry["variant"])
        if key in captures:
            raise ValueError(f"Duplicate comparison capture: {key}")
        captures[key] = entry
        if not entry.get("title") or not entry.get("caption"):
            raise ValueError(f"Missing localized publication metadata: {key}")
        path = directory / entry["variant"] / entry["file"]
        data = path.read_bytes()
        if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
            raise ValueError(f"Invalid PNG: {path}")
        dimensions = struct.unpack(">II", data[16:24])
        if min(dimensions) == 0 or dimensions != (entry["width"], entry["height"]):
            raise ValueError(f"Screenshot dimensions disagree with the index: {path}")
        if len(data) != entry["bytes"] or hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise ValueError(f"Screenshot bytes disagree with the index: {path}")
    variants = {variant for _, variant in captures}
    if not variants:
        raise ValueError("No renderer comparison captures")
    for variant in variants:
        for scene in SCENES:
            pair = []
            for engine in ENGINES:
                key = (f"renderer-{scene}-{engine}", variant)
                if key not in captures:
                    raise ValueError(f"Missing comparison capture: {key}")
                pair.append(captures[key])
            fields = ("width", "height", "locale", "theme")
            if any(pair[0].get(field) != pair[1].get(field) for field in fields):
                raise ValueError(f"Comparison conditions differ: {scene}/{variant}")
    print(f"Verified {len(SCENES) * len(variants)} WebView/Servo pairs for {target}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--target", required=True)
    arguments = parser.parse_args()
    verify(arguments.root, arguments.target)
