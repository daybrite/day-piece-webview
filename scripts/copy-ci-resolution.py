#!/usr/bin/env python3
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
"""Preserve CI's Cargo resolution when copying a checkout to a short Windows path."""
import argparse
from pathlib import Path
import shutil

# These are the independent Cargo workspaces used by the renderer comparison.
WORKSPACES = (Path('.'), Path('demo'), Path('servo'))
RESOLUTION_FILES = (Path('.cargo/config.toml'), Path('Cargo.lock'))


def copy_resolution(source: Path, destination: Path) -> list[Path]:
    source, destination = source.resolve(), destination.resolve()
    if source == destination:
        raise ValueError('source and destination must be different checkouts')
    copied = []
    for workspace in WORKSPACES:
        for name in RESOLUTION_FILES:
            relative = workspace / name
            original = source / relative
            if not original.is_file():
                continue
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(original, target)
            copied.append(relative)
    return copied


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    for path in copy_resolution(args.source, args.destination):
        print(f'Preserved {path}')
