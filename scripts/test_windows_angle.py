# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location('angle', Path(__file__).with_name('stage-windows-angle.py'))
angle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(angle)


class WindowsAngle(unittest.TestCase):
    def make_pair(self, profile, build='mozangle-current', suffix=b''):
        out = profile / 'build' / build / 'out'
        out.mkdir(parents=True)
        for name in angle.DLLS:
            (out / name).write_bytes(name.encode() + suffix)
        return out

    def test_stages_a_matched_pair_beside_the_executable_and_refreshes_it(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory) / 'path with spaces' / 'debug'
            out = self.make_pair(profile)
            (profile / 'app.exe').write_bytes(b'app')
            (profile / 'libEGL.dll').write_bytes(b'stale')
            # Incomplete old builds and unrelated profiles must not supply a DLL.
            stale = profile / 'build/mozangle-incomplete/out'
            stale.mkdir(parents=True)
            (stale / 'libEGL.dll').write_bytes(b'wrong')
            self.make_pair(profile.parent / 'release', suffix=b'release')
            for suffix in (b'first build', b'rebuilt'):
                for name in angle.DLLS:
                    (out / name).write_bytes(name.encode() + suffix)
                self.assertEqual(angle.stage(profile), profile.resolve())
                for name in angle.DLLS:
                    self.assertEqual((profile / name).read_bytes(), name.encode() + suffix)
                self.assertEqual((profile / 'app.exe').read_bytes(), b'app')

    def test_missing_or_split_pairs_fail_without_overwriting_staged_files(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            (profile / 'libEGL.dll').write_bytes(b'existing')
            with self.assertRaisesRegex(RuntimeError, 'No complete mozangle DLL pair'):
                angle.stage(profile)
            for index, name in enumerate(angle.DLLS):
                out = profile / 'build' / f'mozangle-{index}' / 'out'
                out.mkdir(parents=True)
                (out / name).write_bytes(b'incomplete')
            with self.assertRaisesRegex(RuntimeError, 'No complete mozangle DLL pair'):
                angle.stage(profile)
            self.assertEqual((profile / 'libEGL.dll').read_bytes(), b'existing')

    def test_newest_complete_pair_wins_over_cached_builds(self):
        # Cargo's hashes and directory iteration order do not imply build order.
        for current_name, cached_name in (('mozangle-aaa', 'mozangle-zzz'),
                                          ('mozangle-zzz', 'mozangle-aaa')):
            with self.subTest(current=current_name), tempfile.TemporaryDirectory() as directory:
                profile = Path(directory)
                current = self.make_pair(profile, current_name, suffix=b'current')
                cached = self.make_pair(profile, cached_name, suffix=b'cached')
                incomplete = profile / 'build/mozangle-incomplete/out'
                incomplete.mkdir(parents=True)
                (incomplete / 'libEGL.dll').write_bytes(b'incomplete')
                for name in angle.DLLS:
                    os.utime(current / name, (2000, 2000))
                    os.utime(cached / name, (1000, 1000))
                    (profile / name).write_bytes(b'previously staged')
                os.utime(incomplete / 'libEGL.dll', (3000, 3000))
                # Cache extraction can touch directories without rebuilding their DLLs.
                os.utime(cached, (4000, 4000))

                self.assertEqual(angle.stage(profile), profile.resolve())

                for name in angle.DLLS:
                    self.assertEqual((profile / name).read_bytes(), name.encode() + b'current')
                    self.assertEqual((cached / name).read_bytes(), name.encode() + b'cached')

    def test_selection_keeps_both_dlls_from_the_same_build(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            current = self.make_pair(profile, 'mozangle-current', suffix=b'current')
            cached = self.make_pair(profile, 'mozangle-cached', suffix=b'cached')
            # A build can reuse one DLL and relink the other. Do not pick each DLL
            # independently, which would mix outputs from different builds.
            os.utime(current / 'libEGL.dll', (1000, 1000))
            os.utime(current / 'libGLESv2.dll', (3000, 3000))
            for name in angle.DLLS:
                os.utime(cached / name, (2000, 2000))

            angle.stage(profile)

            for name in angle.DLLS:
                self.assertEqual((profile / name).read_bytes(), name.encode() + b'current')


if __name__ == '__main__':
    unittest.main()
