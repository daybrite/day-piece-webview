"""Check that the common walkthrough actually painted its browser surface."""
import argparse
from pathlib import Path

def verify_render(path):
    from PIL import Image

    with Image.open(path) as screenshot:
        pixels = screenshot.convert("RGB")
        count = sum(abs(r - 18) <= 3 and abs(g - 217) <= 3 and abs(b - 164) <= 3
                    for r, g, b in pixels.getdata())
        if count < 1000:
            raise ValueError(f"Browser paint marker missing in {path}: only {count} pixels")
        print(f"Verified browser paint: {count} marker pixels in {path}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("screenshots", type=Path)
    args = parser.parse_args()
    captures = sorted(args.screenshots.rglob("webview-render-proof.png"))
    if not captures:
        raise SystemExit(f"No browser paint captures found under {args.screenshots}")
    for capture in captures:
        verify_render(capture)
