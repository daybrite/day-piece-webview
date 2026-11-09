# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    'ci_resolution', Path(__file__).with_name('copy-ci-resolution.py'))
resolution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resolution)


class CiResolutionTests(unittest.TestCase):
    def test_preserves_git_pins_and_independent_workspace_locks(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / 'long checkout', Path(directory) / 'wv'
            config = b'[patch.crates-io]\nday-core = { git = "https://example.invalid/day", rev = "fixture-revision" }\n'
            files = {'.cargo/config.toml': config, 'Cargo.lock': b'root lock\n',
                     'demo/Cargo.lock': b'demo lock\n', 'servo/Cargo.lock': b'servo lock\n',
                     'demo/.cargo/config.toml': b'[build]\njobs = 2\n'}
            for name, data in files.items():
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            (source / '.cargo/credentials.toml').write_text('fixture credentials')
            (source / 'build').mkdir()
            (source / 'build/artifact').write_text('fixture output')
            copied = resolution.copy_resolution(source, destination)
            self.assertEqual({path.as_posix() for path in copied}, set(files))
            for name, data in files.items():
                self.assertEqual((destination / name).read_bytes(), data)
            self.assertFalse((destination / '.cargo/credentials.toml').exists())
            self.assertFalse((destination / 'build').exists())

    def test_published_resolution_does_not_create_an_override(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / 'source', Path(directory) / 'dest'
            source.mkdir()
            (source / 'Cargo.lock').write_text('published fixture lock')
            resolution.copy_resolution(source, destination)
            self.assertEqual((destination / 'Cargo.lock').read_text(), 'published fixture lock')
            self.assertFalse((destination / '.cargo').exists())

    def test_rejects_copying_over_the_source(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'different checkouts'):
                resolution.copy_resolution(Path(directory), Path(directory))


if __name__ == '__main__':
    unittest.main()
