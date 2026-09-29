import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("render", Path(__file__).with_name("verify-webview-render.py"))
render = importlib.util.module_from_spec(spec)
spec.loader.exec_module(render)


class BrowserPaint(unittest.TestCase):
    def test_blank_screenshot_is_rejected_and_browser_marker_accepted(self):
        from PIL import Image

        with tempfile.TemporaryDirectory() as directory:
            screenshot = Path(directory) / "screen.png"
            Image.new("RGB", (100, 100), "white").save(screenshot)
            with self.assertRaisesRegex(ValueError, "Browser paint marker missing"):
                render.verify_render(screenshot)
            Image.new("RGB", (100, 100), (18, 217, 164)).save(screenshot)
            render.verify_render(screenshot)

