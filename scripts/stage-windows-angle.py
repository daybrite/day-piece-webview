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
    """Use one complete ANGLE build from this executable's Cargo profile directory."""
    binary_dir = Path(binary_dir).resolve()
    candidates = sorted(path for path in (binary_dir / 'build').glob('mozangle-*/out')
                        if all((path / name).is_file() for name in DLLS))
    if len(candidates) != 1:
        raise RuntimeError(
            f'Expected one complete mozangle DLL pair under {binary_dir / "build"}, '
            f'found {len(candidates)}. Build the Servo flavor first; if several builds '
            'remain, clean this Cargo target directory and rebuild.')
    source = candidates[0]
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
