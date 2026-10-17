#!/usr/bin/env python3
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
"""Stage mozangle's runtime DLLs beside a Windows Servo executable."""
import argparse
import ctypes
from pathlib import Path
import shutil
import sys


DLLS = ('libEGL.dll', 'libGLESv2.dll')


def stage(binary_dir):
    """Use the newest complete ANGLE build from this executable's Cargo profile directory.

    CI resolves a fresh Cargo.lock on every run and restores the profile directory from
    Swatinem/rust-cache, which prunes stale build directories by package name only. So once a
    day's resolution changes anything in mozangle's dependency closure, Cargo can compile a
    second build/mozangle-<hash>/ beside the cached one. Prefer the most recently produced
    complete pair for these forward builds. Clean the profile directory before switching
    back to an older dependency resolution: timestamps do not identify Cargo's active build.
    """
    binary_dir = Path(binary_dir).resolve()
    candidates = [path for path in sorted((binary_dir / 'build').glob('mozangle-*/out'))
                  if all((path / name).is_file() for name in DLLS)]
    if not candidates:
        raise RuntimeError(
            f'No complete mozangle DLL pair under {binary_dir / "build"}. Build the Servo '
            'flavor first.')
    candidates.sort(key=lambda path: max((path / name).stat().st_mtime_ns for name in DLLS))
    source = candidates.pop()
    for stale in candidates:
        print(f'Ignoring older ANGLE build: {stale.parent.name}', file=sys.stderr)
    for name in DLLS:
        shutil.copy2(source / name, binary_dir / name)
    return binary_dir


def check_runtime(binary_dir):
    """Exercise the Windows loader and the entry points Surfman needs to initialize."""
    # Resolve dependencies from the staged directory and the system, without relying on
    # Cargo's build-time PATH or another application's ANGLE installation.
    flags = 0x00000100 | 0x00000800  # DLL_LOAD_DIR | SYSTEM32
    gles = ctypes.WinDLL(str(binary_dir / 'libGLESv2.dll'), winmode=flags)
    egl = ctypes.WinDLL(str(binary_dir / 'libEGL.dll'), winmode=flags)
    gles.glGetString
    egl.eglInitialize
    get_proc = egl.eglGetProcAddress
    get_proc.argtypes = [ctypes.c_char_p]
    get_proc.restype = ctypes.c_void_p
    for name in (b'eglCreateDeviceANGLE', b'eglQueryDeviceAttribEXT',
                 b'eglQueryDisplayAttribEXT', b'eglQuerySurfacePointerANGLE',
                 b'eglCreateImageKHR', b'eglDestroyImageKHR', b'glEGLImageTargetTexture2DOES'):
        if not get_proc(name):
            raise RuntimeError(f'Staged ANGLE does not expose {name.decode()}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary_dir', type=Path,
                        help='Cargo profile directory containing the application .exe and build/')
    args = parser.parse_args()
    if sys.platform != 'win32':
        parser.error('Run this helper on Windows with Python matching the application architecture')
    binary_dir = stage(args.binary_dir)
    check_runtime(binary_dir)
    print(f'Staged and loaded ANGLE runtime: {binary_dir}')


if __name__ == '__main__':
    main()
