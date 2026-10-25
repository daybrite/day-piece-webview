#!/usr/bin/env python3
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
"""Build the same demo target with and without Servo and compare delivered artifacts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\[[0-9;]*m")
MIB = 1024 * 1024


def measure(path):
    """Logical bytes, excluding symlink duplicates and external/system dependencies."""
    path = Path(path)
    if path.is_dir():
        files = [(str(p.relative_to(path)), p.stat().st_size)
                 for p in path.rglob('*') if p.is_file() and not p.is_symlink()]
        size = sum(size for _, size in files)
        kind, payload, abis = 'app bundle', size, []
    elif path.suffix in ('.apk', '.hap', '.msix', '.appx'):
        size = path.stat().st_size
        with zipfile.ZipFile(path) as archive:
            files = [(entry.filename, entry.file_size) for entry in archive.infolist()
                     if not entry.is_dir()]
        payload = sum(size for _, size in files)
        abis = sorted({name.split('/')[1] for name, _ in files
                       if name.startswith(('lib/', 'libs/')) and name.endswith('.so')})
        kind = f'{path.suffix[1:].upper()} package'
    elif path.is_file():
        size = payload = path.stat().st_size
        files, abis, kind = [(path.name, size)], [], 'executable'
    else:
        raise ValueError(f'Missing build artifact: {path}')
    return dict(artifact=str(path), kind=kind, artifact_bytes=size, payload_bytes=payload,
                abis=abis, largest_files=[dict(path=name, bytes=size) for name, size
                                         in sorted(files, key=lambda item: item[1], reverse=True)[:5]])


def artifact_from_log(log, target, cwd):
    pattern = re.compile(rf'\bBuilt\s+{re.escape(target)}\s+→\s+(.+?)(?:\s+\([\d.]+s\))?$')
    for line in reversed(ANSI.sub('', log).splitlines()):
        match = pattern.search(line.strip())
        if match:
            path = Path(match[1])
            path = path if path.is_absolute() else cwd / path
            if path.exists():
                return path.resolve()
    raise ValueError(f'Day did not report an existing artifact for {target}')


def markdown(report):
    native, servo = report['builds']['native'], report['builds']['servo']
    lines = [f"# Demo renderer sizes: {report['target']}", '',
             f"Profile: `{report['profile']}`. Artifact: {native['kind']}."]
    if native['abis']:
        lines.extend(['', f"Package ABIs: {', '.join(native['abis'])}."])
    lines.extend(['', '| Build | Artifact MiB | Payload MiB |', '| --- | ---: | ---: |'])
    for label, row in [('Native only', native), ('Native + Servo', servo)]:
        lines.append(f"| {label} | {row['artifact_bytes'] / MIB:.2f} | {row['payload_bytes'] / MIB:.2f} |")
    for key, label in [('artifact_bytes', 'Artifact'), ('payload_bytes', 'Payload')]:
        delta = servo[key] - native[key]
        lines.extend(['', f'{label} increase: {delta / MIB:.2f} MiB ({delta / native[key] * 100:.1f}%).'])
    lines.extend(['', 'Payload is the sum of files in the app bundle or unpacked package. For desktop',
                  'executable outputs, it measures the executable only. System browsers/toolkits,',
                  'external shared libraries, build caches, and separate debug symbols are excluded.',
                  'Package bytes include compression, alignment, and signing overhead; payload bytes',
                  'are not a measurement of filesystem allocation or memory use.', ''])
    return '\n'.join(lines)


def executable(value):
    if sys.platform == 'win32' and value.startswith('/') and shutil.which('cygpath'):
        return subprocess.check_output(['cygpath', '-w', value], text=True).strip()
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    targets = tomllib.loads((ROOT / 'demo/Day-servo.toml').read_text(encoding='utf-8'))['app']['targets']
    parser.add_argument('--target', required=True, choices=targets)
    parser.add_argument('--profile', choices=['debug', 'release'], default='release')
    parser.add_argument('--day-bin', default=os.environ.get('DAY_BIN', 'day'),
                        help='Day executable; for local development use a wrapper around the checkout CLI')
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    if args.target == 'android-material':
        requested = set(filter(None, re.split(r'[,\s]+', os.environ.get('DAY_ANDROID_ABI', ''))))
        if not requested:
            raise ValueError('Set DAY_ANDROID_ABI explicitly for a reproducible Android comparison')
        staged = ROOT / 'demo/build/day/jniLibs'
        existing = {p.name for p in staged.iterdir() if p.is_dir()} if staged.exists() else set()
        if existing - requested:
            raise ValueError(f'Staged ABIs {sorted(existing - requested)} would be stale; include them in DAY_ANDROID_ABI')
    out = (args.out or ROOT / 'demo/build/renderer-sizes' / args.target / args.profile).resolve()
    out.mkdir(parents=True, exist_ok=True)
    report = dict(target=args.target, profile=args.profile, host=platform.platform(),
                  rustc=subprocess.check_output(['rustc', '--version'], text=True).strip(),
                  revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                  environment={key: value for key, value in os.environ.items()
                               if key.startswith('CARGO_PROFILE_') or key in
                               ['DAY_ANDROID_ABI', 'DAY_OHOS_ARCH', 'MACOSX_DEPLOYMENT_TARGET']},
                  builds={})
    build_env = os.environ.copy()
    # An inherited CI flavor must not turn the native baseline into another Servo build.
    build_env.pop('DAY_FLAVOR', None)
    for mode in ['native', 'servo']:
        command = [executable(args.day_bin), 'build', '-t', args.target, '--profile', args.profile]
        if mode == 'servo':
            command += ['--flavor', 'servo']
        print(f'Building {mode}: {command}', flush=True)
        with (out / f'{mode}.log').open('w', encoding='utf-8') as log:
            result = subprocess.run(command, cwd=ROOT / 'demo', env=build_env,
                                    stdout=log, stderr=subprocess.STDOUT)
        if result.returncode:
            raise RuntimeError(f'{mode} build failed; see {out / (mode + ".log")}')
        artifact = artifact_from_log((out / f'{mode}.log').read_text(encoding='utf-8', errors='replace'),
                                     args.target, ROOT / 'demo')
        # Measure immediately: generated mobile hosts can reuse one package path across flavors.
        row = measure(artifact)
        if args.target == 'android-material' and set(row['abis']) != requested:
            raise ValueError(f"APK ABIs {row['abis']} differ from DAY_ANDROID_ABI")
        lock = ROOT / 'demo/Cargo.lock'
        row['lock_sha256'] = hashlib.sha256(lock.read_bytes()).hexdigest()
        report['builds'][mode] = row
        (out / 'sizes.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(f"{mode}: {row['artifact_bytes'] / MIB:.2f} MiB artifact; {row['payload_bytes'] / MIB:.2f} MiB payload", flush=True)
    native, servo = report['builds']['native'], report['builds']['servo']
    for key in ['kind', 'abis', 'lock_sha256']:
        if native[key] != servo[key]:
            raise ValueError(f'Builds are not comparable: {key} differs; see {out / "sizes.json"}')
    rendered = markdown(report)
    (out / 'sizes.md').write_text(rendered, encoding='utf-8')
    if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(summary, 'a', encoding='utf-8') as stream:
            stream.write(rendered)
    print(rendered)


if __name__ == '__main__':
    main()
