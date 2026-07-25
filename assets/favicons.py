#!/usr/bin/env python3
"""Rasterise the favicon set into docs/public/ from the SVGs in assets/.

Unlike logo.py this one needs two libraries, so it is not part of the normal
edit loop — run it when the mark itself changes:

    pip install resvg-py pillow
    python3 assets/favicons.py

Output matches the layout the other CLIs use (mise, hk, aube, fnox):

    docs/public/favicon.svg
    docs/public/favicon.ico            16 + 32 + 48
    docs/public/favicon-16x16.png
    docs/public/favicon-32x32.png
    docs/public/apple-touch-icon.png   180, no transparency, square corners
    docs/public/android-chrome-192x192.png
    docs/public/android-chrome-512x512.png
    docs/public/site.webmanifest
"""
import io
import json
import pathlib
import sys

try:
    import resvg_py
    from PIL import Image
except ImportError:  # pragma: no cover - developer tooling
    sys.exit("need: pip install resvg-py pillow")

ASSETS = pathlib.Path(__file__).resolve().parent
PUBLIC = ASSETS.parent / "docs" / "public"

# Three weights of the same dial. Picking one per output size is the whole
# point of this script: the detailed dial's tick marks turn to mush below
# ~48px, and its ring falls under one device pixel at 16.
DETAILED = ASSETS / "tak-icon-square.svg"
SIMPLE = ASSETS / "tak-icon-square-small.svg"
TINY = ASSETS / "tak-icon-square-tiny.svg"
FIELD = "#0F131A"


def render(src: pathlib.Path, px: int) -> Image.Image:
    png = resvg_py.svg_to_bytes(svg_string=src.read_text(), zoom=px / 256)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def source_for(px: int) -> pathlib.Path:
    if px <= 16:
        return TINY
    return SIMPLE if px < 64 else DETAILED


def main() -> None:
    PUBLIC.mkdir(parents=True, exist_ok=True)

    (PUBLIC / "favicon.svg").write_text(SIMPLE.read_text())

    for px, name in ((16, "favicon-16x16.png"), (32, "favicon-32x32.png"),
                     (180, "apple-touch-icon.png"),
                     (192, "android-chrome-192x192.png"),
                     (512, "android-chrome-512x512.png")):
        render(source_for(px), px).save(PUBLIC / name)
        print(f"{name:30} {px}x{px}")

    # multi-resolution .ico, each frame rendered at its own size rather than
    # downscaled from one bitmap. The base image must be the *largest* — Pillow
    # silently drops any requested size bigger than it.
    ico = [render(source_for(px), px) for px in (48, 32, 16)]
    ico[0].save(PUBLIC / "favicon.ico", format="ICO",
                sizes=[(16, 16), (32, 32), (48, 48)],
                append_images=ico[1:])
    print(f"{'favicon.ico':30} 16+32+48")

    manifest = {
        "name": "tak",
        "short_name": "tak",
        "description": "Benchmark CI on instruction counts, stored in git notes",
        "start_url": "/",
        "icons": [
            {"src": "/favicon.svg", "sizes": "any", "type": "image/svg+xml"},
            {"src": "/android-chrome-192x192.png", "sizes": "192x192",
             "type": "image/png"},
            {"src": "/android-chrome-512x512.png", "sizes": "512x512",
             "type": "image/png"},
        ],
        "theme_color": FIELD,
        "background_color": FIELD,
        "display": "standalone",
    }
    (PUBLIC / "site.webmanifest").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{'site.webmanifest':30} ok")


if __name__ == "__main__":
    main()
