#!/usr/bin/env python3
"""Generate the tak logo family. Pure geometry, no dependencies.

Run `python3 assets/logo.py` after editing to regenerate every SVG.
Preview a change:  pip install resvg-py && python3 -c "..."  (see README).
"""
import math, pathlib

OUT = pathlib.Path(__file__).resolve().parent

# ---------------------------------------------------------------- palette
INK_L, INK_D = "#12161C", "#F2F5F9"
TRACK_L, TRACK_D = "#CBD3DE", "#333D4B"
TICK_L, TICK_D = "#9AA5B5", "#6C7788"
AMBER, RED = "#F0A02A", "#E5484D"

STYLE = f"""  <style>
    .ink{{stroke:{INK_L}}} .ink-f{{fill:{INK_L}}}
    .track{{stroke:{TRACK_L}}} .tick{{stroke:{TICK_L}}} .tick-f{{fill:{TICK_L}}}
    .amber{{stroke:{AMBER}}} .amber-f{{fill:{AMBER}}}
    .red{{stroke:{RED}}} .red-f{{fill:{RED}}}
    @media (prefers-color-scheme:dark){{
      .ink{{stroke:{INK_D}}} .ink-f{{fill:{INK_D}}}
      .track{{stroke:{TRACK_D}}} .tick{{stroke:{TICK_D}}} .tick-f{{fill:{TICK_D}}}
    }}
  </style>
"""


def f(x):
    return f"{x:.2f}".rstrip("0").rstrip(".")


def P(cx, cy, r, deg):
    a = math.radians(deg)
    return cx + r * math.cos(a), cy - r * math.sin(a)


def arc(cx, cy, r, a0, a1, w, cls, cap="round"):
    """arc from a0 sweeping clockwise (decreasing angle) to a1"""
    x0, y0 = P(cx, cy, r, a0)
    x1, y1 = P(cx, cy, r, a1)
    large = 1 if (a0 - a1) > 180 else 0
    return (f'<path d="M {f(x0)} {f(y0)} A {f(r)} {f(r)} 0 {large} 1 {f(x1)} {f(y1)}"'
            f' class="{cls}" fill="none" stroke-width="{f(w)}" stroke-linecap="{cap}"/>')


def ticks(cx, cy, a0, a1, n, r_in, r_out, w, cls):
    out = []
    for i in range(n):
        a = a0 + (a1 - a0) * i / (n - 1)
        x0, y0 = P(cx, cy, r_in, a)
        x1, y1 = P(cx, cy, r_out, a)
        out.append(f'<line x1="{f(x0)}" y1="{f(y0)}" x2="{f(x1)}" y2="{f(y1)}"'
                   f' class="{cls}" stroke-width="{f(w)}" stroke-linecap="round"/>')
    return "\n  ".join(out)


def needle(cx, cy, deg, length, base_w, cls="ink-f"):
    """a dart: widest at the hub, tapering to a point. No tail wedge —
    the hub disc is drawn over the base."""
    a = math.radians(deg)
    ux, uy = math.cos(a), -math.sin(a)
    px, py = -uy, ux
    return (f'<path d="M {f(cx + ux * length)} {f(cy + uy * length)}'
            f' L {f(cx + px * base_w / 2)} {f(cy + py * base_w / 2)}'
            f' L {f(cx - px * base_w / 2)} {f(cy - py * base_w / 2)} Z" class="{cls}"/>')


def svg(w, h, body, style=True):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}"'
            f' width="{w}" height="{h}" role="img" aria-label="tak">\n'
            f'{STYLE if style else ""}{body}\n</svg>\n')


# ============================================================ the mark
# 240-degree sweep. Amber = swept, grey = headroom, red = the CI gate.
# Geometry is centred on its own ink, not on the circle centre: the dial
# spans y 41.5-213.5 in a 256 box.
CX, CY, R, W = 128, 154, 106, 13
A0, A1 = 210, -30          # sweep limits
NEEDLE, REDLINE = 44, 22   # needle angle, start of the red band


def dial(dots=False, cls=None, simple=False, tiny=False):
    """Three weights of the same dial, all on roughly the same bounds:

    default  ticks, 13px ring      >=64px
    simple   no ticks, 18px ring   32-48px, where the ticks turn to mush
    tiny     no ticks, 30px ring   16px, where an 18px ring renders under one
                                   device pixel and antialiases to brown mud
    """
    c = cls or {}
    g = lambda k, d: c.get(k, d)
    r, w = (96, 30) if tiny else (103.5, 18) if simple else (R, W)
    simple = simple or tiny
    cy = 154
    b = [arc(CX, cy, r, A0, A1, w, g("track", "track")),
         arc(CX, cy, r, REDLINE, A1, w, g("red", "red")),
         arc(CX, cy, r, A0, NEEDLE, w, g("amber", "amber"))]
    if not simple:
        if dots:
            b += [f'<circle cx="{f(x)}" cy="{f(y)}" r="4.5" class="{g("tick-f", "tick-f")}"/>'
                  for x, y in (P(CX, cy, r - 27, A0 + (A1 - A0) * i / 8) for i in range(9))]
        else:
            b.append(ticks(CX, cy, A0, A1, 7, r - 34, r - 20, 7, g("tick", "tick")))
    nl, nw, hub, dot = ((r - 18, 40, 29, 12) if tiny else
                        (r - 22, 30, 22, 8.5) if simple else
                        (r - 30, 22, 17, 6.5))
    b += [needle(CX, cy, NEEDLE, nl, nw, g("ink-f", "ink-f")),
          f'<circle cx="{CX}" cy="{f(cy)}" r="{hub}" class="{g("ink-f", "ink-f")}"/>',
          f'<circle cx="{CX}" cy="{f(cy)}" r="{dot}" class="{g("amber-f", "amber-f")}"/>']
    return "  " + "\n  ".join(b)


def mark(dots=False, simple=False, tiny=False):
    return svg(256, 256, dial(dots, simple=simple, tiny=tiny))


FIELD = "#0F131A"
# the dial reversed out of an ink field — no @media block, these are dark always
SOLID = {"ink-f": f'" fill="{INK_D}', "track": '" stroke="#3E4959',
         "tick": f'" stroke="{TICK_D}', "tick-f": f'" fill="{TICK_D}',
         "amber": f'" stroke="{AMBER}', "amber-f": f'" fill="{AMBER}',
         "red": f'" stroke="{RED}'}


def tile(simple=False, tiny=False, radius=58, scale=0.82):
    """app icon / avatar — rounded ink field"""
    body = (f'  <rect width="256" height="256" rx="{radius}" fill="{FIELD}"/>\n'
            f'  <g transform="translate(128,128) scale({scale}) translate(-128,-128)">\n'
            f'{dial(cls=SOLID, simple=simple, tiny=tiny)}\n  </g>')
    return svg(256, 256, body, style=False)


def square(simple=False, tiny=False):
    """favicon / touch-icon source. Full bleed, square corners: iOS and Android
    apply their own mask, and pre-rounded corners come out double-rounded."""
    return tile(simple=simple, tiny=tiny, radius=0, scale=0.94 if tiny else 0.88)


# ============================================================ wordmark
# Geometric single-storey letterforms. The bowl of the "a" is the dial:
# a slice of its rim is the redline, and a needle points at it.
SW, BASE, XTOP, ASC = 14, 150, 78, 44
ACX, ACY, AR = 110, 114, 36
TX, KX = 26, 188
INKA = f'fill="none" stroke-width="{SW}" stroke-linecap="round" stroke-linejoin="round"'


def letters(live=True):
    b = [  # t
        f'<path d="M {TX} {ASC} L {TX} {BASE - 16} Q {TX} {BASE} {TX + 16} {BASE}"'
        f' class="ink" {INKA}/>',
        f'<line x1="{TX - 20}" y1="{XTOP}" x2="{TX + 26}" y2="{XTOP}" class="ink" {INKA}/>']

    # bowl in two butt-capped arcs so a slice of the rim reads as the redline,
    # placed clear of the right stem so the two never merge into a blob
    if live:
        b.append(arc(ACX, ACY, AR, 34, -282, SW, "ink", cap="butt"))
        b.append(arc(ACX, ACY, AR, 78, 34, SW, "red", cap="butt"))
    else:
        b.append(f'<circle cx="{ACX}" cy="{ACY}" r="{AR}" class="ink" {INKA}/>')
    b.append(f'<line x1="{ACX + AR}" y1="{XTOP}" x2="{ACX + AR}" y2="{BASE}"'
             f' class="ink" {INKA}/>')
    if live:
        b.append(needle(ACX, ACY, 56, AR - 12, 11))
        b.append(f'<circle cx="{ACX}" cy="{ACY}" r="6.5" class="ink-f"/>')

    b += [  # k — arms terminate on the stem's centreline so the round cap
            # lands flush with the stem instead of nubbing out the side
        f'<line x1="{KX}" y1="{ASC}" x2="{KX}" y2="{BASE}" class="ink" {INKA}/>',
        f'<line x1="{KX + 46}" y1="{XTOP}" x2="{KX}" y2="{BASE - 30}" class="ink" {INKA}/>',
        f'<line x1="{KX + 17}" y1="{BASE - 47}" x2="{KX + 48}" y2="{BASE}" class="ink" {INKA}/>']
    return "\n  ".join(b)


WM_W, WM_H = KX + 48 + 7 + 8, 194   # right edge + cap + margin


def wordmark(live=True):
    return svg(WM_W, WM_H, f'  <g transform="translate(8,0)">{letters(live)}</g>')


# the mark's own ink bounds inside its 256 box
M_L, M_T, M_W, M_H = 15.5, 41.5, 225.0, 172.0


def lockup():
    """mark + plain wordmark. The wordmark's "a" drops its dial here — two
    gauges side by side just read as a repeat."""
    s = (BASE - ASC) / M_H                    # mark spans ascender to baseline
    dx, dy = -M_L * s, ASC - M_T * s
    gap = 24
    x = M_W * s + gap + 7                     # +7 clears the t crossbar's cap
    body = (f'  <g transform="translate({f(dx)},{f(dy)}) scale({f(s)})">\n'
            f'{dial()}\n  </g>\n'
            f'  <g transform="translate({f(x)},0)">{letters(False)}</g>')
    return svg(int(x + KX + 48 + 7), WM_H, body)


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
         "tak-lockup.svg": lockup()}
for name, content in files.items():
    (OUT / name).write_text(content)
    print(f"{name:26} {len(content):5}b")
