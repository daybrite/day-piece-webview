# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "comparison", Path(__file__).with_name("verify-renderer-comparison.py"))
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)


class RendererComparison(unittest.TestCase):
    def test_untitled_proofs_are_checked_but_do_not_require_publication_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory, index = self.fixture(root)
            proof = dict(index["screenshots"][0], shot="renderer-servo-proof", title=None,
                         caption=None)
            index["screenshots"].append(proof)
            manifest = directory / "gallery.json"
            manifest.write_text(json.dumps(index))
            comparison.verify(root, "macos-appkit")
            proof["sha256"] = "corrupt proof fixture"
            manifest.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "bytes disagree"):
                comparison.verify(root, "macos-appkit")
            index["screenshots"].pop()
            index["screenshots"][0]["title"] = None
            manifest.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "Missing localized publication"):
                comparison.verify(root, "macos-appkit")

    def fixture(self, root):
        directory = root / "macos-appkit"
        (directory / "default").mkdir(parents=True)
        entries = []
        # Synthetic PNG header fixture; this verifier checks capture/index integrity, not pixels.
        data = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 2, 3)
        for scene in comparison.SCENES:
            for engine in comparison.ENGINES:
                name = f"renderer-{scene}-{engine}"
                (directory / "default" / f"{name}.png").write_bytes(data)
                entries.append(dict(shot=name, file=f"{name}.png", variant="default", locale="en",
                                    width=2, height=3, title="fixture", caption="fixture",
                                    bytes=len(data), sha256=hashlib.sha256(data).hexdigest()))
        return directory, dict(target="macos-appkit", screenshots=entries)

    def test_complete_pairs_and_missing_or_corrupt_captures(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory, index = self.fixture(root)
            manifest = directory / "gallery.json"
            manifest.write_text(json.dumps(index))
            comparison.verify(root, "macos-appkit")
            removed = index["screenshots"].pop()
            manifest.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "Missing comparison"):
                comparison.verify(root, "macos-appkit")
            index["screenshots"].append(removed)
            removed["locale"] = "fr"
            manifest.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "conditions differ"):
                comparison.verify(root, "macos-appkit")
            removed["locale"] = "en"
            removed["sha256"] = "invalid fixture checksum"
            manifest.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "bytes disagree"):
                comparison.verify(root, "macos-appkit")
