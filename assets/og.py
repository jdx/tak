#!/usr/bin/env python3
"""Generate docs/public/og.png, the 1200x630 share card.

The card is assembled from the logo family (run logo.py first if the mark
changed), so the dial and wordmark cannot drift from the real ones. Text is
set in Inter, which is not vendored; point --fonts at a directory holding
Inter-Bold.ttf and Inter-Regular.ttf (the rsms/inter release zip has them
under extras/ttf/). Needs `pip install resvg-py`.

    python3 assets/og.py --fonts /path/to/inter/extras/ttf
"""
import argparse
import pathlib
import re

import resvg_py

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE.parent / "docs" / "public" / "og.png"

W, H = 1200, 630
INK, MUTED = "#12161C", "#5B6573"

# the mark's ink bounds inside its 256 box, as in logo.py
M_L, M_T, M_W, M_H = 15.5, 41.5, 225.0, 172.0


def inner(name):
    """the drawing inside an SVG, without the wrapper or the dark-mode <style>:
    resvg does not evaluate @media, and a share card is light regardless"""
    text = (HERE / name).read_text()
    text = re.sub(r"<style>.*?</style>", "", text, flags=re.S)
    return re.search(r"<svg[^>]*>(.*)</svg>", text, flags=re.S).group(1)


def card():
    # the dial is the subject, so it gets the right half and is centred on its
    # own ink rather than on its 256 box
    s = 1.75
    cx, cy = 900, 315
    dx, dy = cx - (M_L + M_W / 2) * s, cy - (M_T + M_H / 2) * s
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">
  <rect width="{W}" height="{H}" fill="#FFFFFF"/>
  <g transform="translate({dx:.1f},{dy:.1f}) scale({s})">{inner("tak-mark.svg")}</g>
  <g transform="translate(72,64) scale(0.82)">{inner("tak-wordmark.svg")}</g>
  <g font-family="Inter" fill="{INK}">
    <text x="80" y="300" font-size="68" font-weight="700" letter-spacing="-1.5">A tachometer</text>
    <text x="80" y="376" font-size="68" font-weight="700" letter-spacing="-1.5">for code.</text>
    <g font-size="30" font-weight="400" fill="{MUTED}">
      <text x="80" y="446">Track CLI performance by counting</text>
      <text x="80" y="486">instructions, not seconds.</text>
    </g>
    <text x="80" y="566" font-size="22" font-weight="400" fill="{MUTED}">tak.jdx.dev  ·  pre-v1</text>
  </g>
</svg>
"""


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--fonts", required=True, help="directory with Inter-Bold.ttf and Inter-Regular.ttf")
    args = ap.parse_args()
    fonts = pathlib.Path(args.fonts)
    for face in ("Inter-Bold.ttf", "Inter-Regular.ttf"):
        if not (fonts / face).is_file():
            raise SystemExit(f"{fonts / face} not found")
    png = resvg_py.svg_to_bytes(
        svg_string=card(),
        skip_system_fonts=True,
        font_files=[str(fonts / "Inter-Bold.ttf"), str(fonts / "Inter-Regular.ttf")],
        font_family="Inter",
    )
    OUT.write_bytes(bytes(png))
    print(f"{OUT.relative_to(HERE.parent)} {len(png)}b")
