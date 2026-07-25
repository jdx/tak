#!/usr/bin/env python3
"""Generate the tak logo family. Pure geometry, no dependencies.

Run `python3 assets/logo.py` after editing to regenerate every SVG.
See README.md for how to preview a change, and for what each file is for.
"""
import math
import pathlib

OUT = pathlib.Path(__file__).resolve().parent

# ---------------------------------------------------------------- palette
LIGHT = {"ink": "#12161C", "track": "#CBD3DE", "tick": "#9AA5B5",
         "amber": "#F0A02A", "red": "#E5484D"}
DARK = {"ink": "#F2F5F9", "track": "#333D4B", "tick": "#6C7788",
        "amber": "#F0A02A", "red": "#E5484D"}
# reversed out of an ink field: the track has to lift off the background
SOLID = {"ink": "#F2F5F9", "track": "#3E4959", "tick": "#6C7788",
         "amber": "#F0A02A", "red": "#E5484D"}
FIELD = "#0F131A"


class Theme:
    """Colours are emitted as presentation attributes, with a dark override in
    a <style> block on top. Presentation attributes lose to any CSS rule, so
    the override still wins — but if a host strips the <style> (GitHub has
    historically sanitised SVG), the file degrades to the light palette
    instead of rendering blank."""

    def __init__(self, palette, adaptive=False):
        self.palette, self.adaptive = palette, adaptive

    def paint(self, name, prop="stroke"):
        if not self.adaptive:
            return f'{prop}="{self.palette[name]}"'
        cls = name if prop == "stroke" else f"{name}-f"
        return f'class="{cls}" {prop}="{LIGHT[name]}"'

    @property
    def style(self):
        if not self.adaptive:
            return ""
        rules = " ".join(f".{k}{{stroke:{v}}} .{k}-f{{fill:{v}}}"
                         for k, v in DARK.items() if v != LIGHT[k])
        return ("  <style>\n"
                f"    @media (prefers-color-scheme:dark){{{rules}}}\n"
                "  </style>\n")


ADAPTIVE = Theme(LIGHT, adaptive=True)
REVERSED = Theme(SOLID)


def f(x):
    return f"{x:.2f}".rstrip("0").rstrip(".")


def P(cx, cy, r, deg):
    a = math.radians(deg)
    return cx + r * math.cos(a), cy - r * math.sin(a)


def arc(t, name, cx, cy, r, a0, a1, w, cap="round"):
    """arc from a0 sweeping clockwise (decreasing angle) to a1"""
    x0, y0 = P(cx, cy, r, a0)
    x1, y1 = P(cx, cy, r, a1)
    large = 1 if (a0 - a1) > 180 else 0
    return (f'<path d="M {f(x0)} {f(y0)} A {f(r)} {f(r)} 0 {large} 1 {f(x1)} {f(y1)}"'
            f' {t.paint(name)} fill="none" stroke-width="{f(w)}" stroke-linecap="{cap}"/>')


def ticks(t, name, cx, cy, a0, a1, n, r_in, r_out, w):
    out = []
    for i in range(n):
        a = a0 + (a1 - a0) * i / (n - 1)
        x0, y0 = P(cx, cy, r_in, a)
        x1, y1 = P(cx, cy, r_out, a)
        out.append(f'<line x1="{f(x0)}" y1="{f(y0)}" x2="{f(x1)}" y2="{f(y1)}"'
                   f' {t.paint(name)} stroke-width="{f(w)}" stroke-linecap="round"/>')
    return "\n  ".join(out)


def needle(t, cx, cy, deg, length, base_w):
    """a dart: widest at the hub, tapering to a point. No tail wedge —
    the hub disc is drawn over the base."""
    a = math.radians(deg)
    ux, uy = math.cos(a), -math.sin(a)
    px, py = -uy, ux
    return (f'<path d="M {f(cx + ux * length)} {f(cy + uy * length)}'
            f' L {f(cx + px * base_w / 2)} {f(cy + py * base_w / 2)}'
            f' L {f(cx - px * base_w / 2)} {f(cy - py * base_w / 2)} Z"'
            f' {t.paint("ink", "fill")}/>')


def svg(t, w, h, body, label="tak"):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}"'
            f' width="{w}" height="{h}" role="img" aria-label="{label}">\n'
            f'{t.style}{body}\n</svg>\n')


# ============================================================ the mark
# 240-degree sweep. Amber = swept, grey = headroom, red = the CI gate.
# Geometry is centred on its own ink, not on the circle centre: the dial
# spans y 41.5-213.5 in a 256 box.
CX, CY, R, W = 128, 154, 106, 13
A0, A1 = 210, -30          # sweep limits
NEEDLE, REDLINE = 44, 22   # needle angle, start of the red band


def dial(t, simple=False, tiny=False):
    """Three weights of the same dial, all on roughly the same bounds:

    default  ticks, 13px ring      >=64px
    simple   no ticks, 18px ring   32-48px, where the ticks turn to mush
    tiny     no ticks, 30px ring   16px, where an 18px ring renders under one
                                   device pixel and antialiases to brown mud
    """
    r, w = (96, 30) if tiny else (103.5, 18) if simple else (R, W)
    cy = 154
    b = [arc(t, "track", CX, cy, r, A0, A1, w),
         arc(t, "red", CX, cy, r, REDLINE, A1, w),
         arc(t, "amber", CX, cy, r, A0, NEEDLE, w)]
    if not (simple or tiny):
        b.append(ticks(t, "tick", CX, cy, A0, A1, 7, r - 34, r - 20, 7))
    nl, nw, hub, dot = ((r - 18, 40, 29, 12) if tiny else
                        (r - 22, 30, 22, 8.5) if simple else
                        (r - 30, 22, 17, 6.5))
    b += [needle(t, CX, cy, NEEDLE, nl, nw),
          f'<circle cx="{CX}" cy="{f(cy)}" r="{hub}" {t.paint("ink", "fill")}/>',
          f'<circle cx="{CX}" cy="{f(cy)}" r="{dot}" {t.paint("amber", "fill")}/>']
    return "  " + "\n  ".join(b)


def mark(simple=False, tiny=False):
    return svg(ADAPTIVE, 256, 256, dial(ADAPTIVE, simple, tiny))


def tile(simple=False, tiny=False, radius=58, scale=0.82):
    """app icon / avatar — rounded ink field. Fixed colours, no media query:
    a self-contained dark tile reads correctly on a light or dark page, which
    is what makes it safe to drop straight into a README."""
    body = (f'  <rect width="256" height="256" rx="{radius}" fill="{FIELD}"/>\n'
            f'  <g transform="translate(128,128) scale({scale}) translate(-128,-128)">\n'
            f'{dial(REVERSED, simple, tiny)}\n  </g>')
    return svg(REVERSED, 256, 256, body)


def square(simple=False, tiny=False):
    """favicon / touch-icon source. Full bleed, square corners: iOS and Android
    apply their own mask, and pre-rounded corners come out double-rounded."""
    return tile(simple, tiny, radius=0, scale=0.94 if tiny else 0.88)


# ============================================================ wordmark
# Geometric single-storey letterforms. The bowl of the "a" is the dial:
# a slice of its rim is the redline, and a needle points at it.
SW, BASE, XTOP, ASC = 14, 150, 78, 44
ACX, ACY, AR = 110, 114, 36
TX, KX = 26, 188
CAPS = f'fill="none" stroke-width="{SW}" stroke-linecap="round" stroke-linejoin="round"'


def letters(t, live=True):
    ink = f'{t.paint("ink")} {CAPS}'
    b = [  # t
        f'<path d="M {TX} {ASC} L {TX} {BASE - 16} Q {TX} {BASE} {TX + 16} {BASE}" {ink}/>',
        f'<line x1="{TX - 20}" y1="{XTOP}" x2="{TX + 26}" y2="{XTOP}" {ink}/>']

    # bowl in two butt-capped arcs so a slice of the rim reads as the redline,
    # placed clear of the right stem so the two never merge into a blob
    if live:
        b.append(arc(t, "ink", ACX, ACY, AR, 34, -282, SW, cap="butt"))
        b.append(arc(t, "red", ACX, ACY, AR, 78, 34, SW, cap="butt"))
    else:
        b.append(f'<circle cx="{ACX}" cy="{ACY}" r="{AR}" {ink}/>')
    b.append(f'<line x1="{ACX + AR}" y1="{XTOP}" x2="{ACX + AR}" y2="{BASE}" {ink}/>')
    if live:
        b.append(needle(t, ACX, ACY, 56, AR - 12, 11))
        b.append(f'<circle cx="{ACX}" cy="{ACY}" r="6.5" {t.paint("ink", "fill")}/>')

    b += [  # k — arms terminate on the stem's centreline so the round cap
            # lands flush with the stem instead of nubbing out the side
        f'<line x1="{KX}" y1="{ASC}" x2="{KX}" y2="{BASE}" {ink}/>',
        f'<line x1="{KX + 46}" y1="{XTOP}" x2="{KX}" y2="{BASE - 30}" {ink}/>',
        f'<line x1="{KX + 17}" y1="{BASE - 47}" x2="{KX + 48}" y2="{BASE}" {ink}/>']
    return "\n  ".join(b)


WM_W, WM_H = KX + 48 + 7 + 8, 194   # right edge + cap + margin


def wordmark(live=True, t=ADAPTIVE):
    return svg(t, WM_W, WM_H, f'  <g transform="translate(8,0)">{letters(t, live)}</g>')


# the mark's own ink bounds inside its 256 box
M_L, M_T, M_W, M_H = 15.5, 41.5, 225.0, 172.0


def lockup(t=ADAPTIVE):
    """mark + plain wordmark. The wordmark's "a" drops its dial here — two
    gauges side by side just read as a repeat."""
    s = (BASE - ASC) / M_H                    # mark spans ascender to baseline
    dx, dy = -M_L * s, ASC - M_T * s
    gap = 24
    x = M_W * s + gap + 7                     # +7 clears the t crossbar's cap
    body = (f'  <g transform="translate({f(dx)},{f(dy)}) scale({f(s)})">\n'
            f'{dial(t)}\n  </g>\n'
            f'  <g transform="translate({f(x)},0)">{letters(t, False)}</g>')
    return svg(t, int(x + KX + 48 + 7), WM_H, body)


# ============================================================ write
files = {"tak-mark.svg": mark(),
         "tak-mark-small.svg": mark(simple=True),
         "tak-icon-tile.svg": tile(),
         "tak-icon-tile-small.svg": tile(simple=True),
         "tak-icon-square.svg": square(),
         "tak-icon-square-small.svg": square(simple=True),
         "tak-icon-square-tiny.svg": square(tiny=True),
         "tak-wordmark.svg": wordmark(True),
         "tak-wordmark-plain.svg": wordmark(False),
         "tak-lockup.svg": lockup(),
         # fixed-palette lockups for <picture>, where the host picks the file
         "tak-lockup-light.svg": lockup(Theme(LIGHT)),
         "tak-lockup-dark.svg": lockup(Theme(DARK))}
for name, content in files.items():
    (OUT / name).write_text(content)
    print(f"{name:28} {len(content):5}b")
