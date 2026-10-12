"""Check that the common walkthrough actually painted its browser surface."""
import argparse
from pathlib import Path

def verify_render(path, color=(18, 217, 164)):
    from PIL import Image

    with Image.open(path) as screenshot:
        pixels = screenshot.convert("RGB")
        count = sum(abs(r - color[0]) <= 3 and abs(g - color[1]) <= 3 and abs(b - color[2]) <= 3
                    for r, g, b in pixels.getdata())
        if count < 1000:
            raise ValueError(f"Browser paint marker missing in {path}: only {count} pixels")
        print(f"Verified browser paint: {count} marker pixels in {path}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("screenshots", type=Path)
    parser.add_argument("--pattern", default="webview-render-proof.png")
    parser.add_argument("--color", default="12d9a4", help="Six-digit RGB paint marker")
    args = parser.parse_args()
    if len(args.color) != 6:
        parser.error("--color requires six hexadecimal digits")
    try:
        color = tuple(bytes.fromhex(args.color))
    except ValueError:
        parser.error("--color requires six hexadecimal digits")
    captures = sorted(args.screenshots.rglob(args.pattern))
    if not captures:
        raise SystemExit(f"No browser paint captures found under {args.screenshots}")
    for capture in captures:
        verify_render(capture, color)
