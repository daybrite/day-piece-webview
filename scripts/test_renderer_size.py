# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
from types import SimpleNamespace
import zipfile

spec = importlib.util.spec_from_file_location('sizes', Path(__file__).with_name('measure-renderer-size.py'))
sizes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sizes)


class RendererSizes(unittest.TestCase):
    def test_pair_clears_inherited_flavor_and_measures_before_the_next_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / 'demo/build/fixture.exe'
            artifact.parent.mkdir(parents=True)
            (root / 'demo/Cargo.lock').write_text('synthetic lockfile')
            (root / 'demo/Day-servo.toml').write_text("[app]\ntargets = ['windows-winui']\n")
            commands = []

            def build(command, **kwargs):
                self.assertNotIn('DAY_FLAVOR', kwargs['env'])
                commands.append(command)
                artifact.write_bytes(b'x' * (200 if '--flavor' in command else 100))
                kwargs['stdout'].write(f'Built windows-winui → {artifact} (0.1s)\n')
                return SimpleNamespace(returncode=0)

            with mock.patch.object(sizes, 'ROOT', root), \
                    mock.patch.object(sizes.platform, 'platform', return_value='fixture host'), \
                    mock.patch.object(sizes.sys, 'argv', ['sizes', '--target', 'windows-winui']), \
                    mock.patch.dict(os.environ, {'DAY_FLAVOR': 'servo'}, clear=True), \
                    mock.patch.object(sizes.subprocess, 'check_output', return_value='fixture\n'), \
                    mock.patch.object(sizes.subprocess, 'run', side_effect=build), \
                    contextlib.redirect_stdout(io.StringIO()):
                sizes.main()
            report = json.loads((root / 'demo/build/renderer-sizes/windows-winui/release/sizes.json').read_text())
            self.assertNotIn('--flavor', commands[0])
            self.assertEqual(commands[1][-2:], ['--flavor', 'servo'])
            self.assertEqual(report['builds']['native']['artifact_bytes'], 100)
            self.assertEqual(report['builds']['servo']['artifact_bytes'], 200)

    def test_bundle_counts_payload_once_and_keeps_spaces_in_build_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / 'Fixture App.app'
            app.mkdir()
            (app / 'binary').write_bytes(b'fixture')
            if os.name != 'nt':
                (app / 'alias').symlink_to('binary')
            measured = sizes.measure(app)
            self.assertEqual(measured['payload_bytes'], 7)
            log = f'\x1b[1mBuilt\x1b[0m macos-appkit → {app} (1.2s)\n'
            self.assertEqual(sizes.artifact_from_log(log, 'macos-appkit', root), app.resolve())

    def test_package_reports_compressed_and_unpacked_bytes_and_abis(self):
        with tempfile.TemporaryDirectory() as directory:
            package = Path(directory) / 'fixture.apk'
            with zipfile.ZipFile(package, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
                archive.writestr('lib/arm64-v8a/libfixture.so', b'x' * 10000)
                archive.writestr('assets/fixture.txt', b'abc')
            measured = sizes.measure(package)
            self.assertEqual(measured['payload_bytes'], 10003)
            self.assertLess(measured['artifact_bytes'], measured['payload_bytes'])
            self.assertEqual(measured['abis'], ['arm64-v8a'])


if __name__ == '__main__':
    unittest.main()
