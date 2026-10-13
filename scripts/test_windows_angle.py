# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
import importlib.util
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
            with self.assertRaisesRegex(RuntimeError, 'found 0'):
                angle.stage(profile)
            for index, name in enumerate(angle.DLLS):
                out = profile / 'build' / f'mozangle-{index}' / 'out'
                out.mkdir(parents=True)
                (out / name).write_bytes(b'incomplete')
            with self.assertRaisesRegex(RuntimeError, 'found 0'):
                angle.stage(profile)
            self.assertEqual((profile / 'libEGL.dll').read_bytes(), b'existing')

    def test_ambiguous_builds_fail_instead_of_selecting_an_arbitrary_version(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            self.make_pair(profile, 'mozangle-first')
            self.make_pair(profile, 'mozangle-second')
            with self.assertRaisesRegex(RuntimeError, 'found 2'):
                angle.stage(profile)
            self.assertFalse((profile / 'libEGL.dll').exists())


if __name__ == '__main__':
    unittest.main()
