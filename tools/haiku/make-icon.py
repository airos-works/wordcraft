#!/usr/bin/env python3
"""Build WordCraft's Haiku vector icon (HVIF) from the app icon's SVG geometry
(assets/app-icon/wordcraft.svg: a blue tile, a tilted page with ruled lines,
a pencil), so air/OS's Tracker, Deskbar and window tabs show it sharp at any
size.

    python3 tools/haiku/make-icon.py apps/wordcraft-haiku/resources/WordCraft.hvif [preview.png]
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hvif  # noqa: E402

S = 64 / 512  # SVG units to the 64 x 64 icon canvas
K = 0.5523    # cubic handle length for a quarter circle
INK, PAPER, BLUE = hvif.hex_color('0b0b0c'), hvif.hex_color('efe9dc'), hvif.hex_color('3b5bdb')

icon = {'styles': [], 'paths': [], 'shapes': []}


def style(color):
    icon['styles'].append({'color': color})
    return len(icon['styles']) - 1


def path(points, closed=True):
    icon['paths'].append({'closed': closed, 'points': points})
    return len(icon['paths']) - 1


def shape(style_index, paths, transformers=None):
    icon['shapes'].append({'style': style_index, 'paths': paths, 'transformers': transformers or []})


def stroke(width, cap=0):
    # join 2 = round; cap 2 = round
    return [{'type': 'stroke', 'width': width * S, 'join': 2, 'cap': cap, 'miter': 4}]


def rotated(points, degrees=-6, cx=256, cy=256):
    a = math.radians(degrees)
    c, s = math.cos(a), math.sin(a)
    return [((cx + (x - cx) * c - (y - cy) * s) * S, (cy + (x - cx) * s + (y - cy) * c) * S) for x, y in points]


def scaled(points):
    return [(x * S, y * S) for x, y in points]


def rounded_rect(x0, y0, x1, y1, r):
    k = r * K
    return [
        ((x0 + r, y0), (x0 + r - k, y0), (x0 + r, y0)),
        ((x1 - r, y0), (x1 - r, y0), (x1 - r + k, y0)),
        ((x1, y0 + r), (x1, y0 + r - k), (x1, y0 + r)),
        ((x1, y1 - r), (x1, y1 - r), (x1, y1 - r + k)),
        ((x1 - r, y1), (x1 - r + k, y1), (x1 - r, y1)),
        ((x0 + r, y1), (x0 + r, y1), (x0 + r - k, y1)),
        ((x0, y1 - r), (x0, y1 - r + k), (x0, y1 - r)),
        ((x0, y0 + r), (x0, y0 + r), (x0, y0 + r - k)),
    ]


# The tile, inset a little as Haiku icons are.
tile = path(rounded_rect(2, 2, 62, 62, 13))
shape(style(BLUE), [tile])

ink, paper = style(INK), style(PAPER)
page = path(rotated([(150, 92), (322, 92), (384, 154), (384, 420), (150, 420)]))
shape(paper, [page])
shape(ink, [page], stroke(14))
fold = path(rotated([(322, 92), (322, 154), (384, 154)]), closed=False)
shape(ink, [fold], stroke(14))
lines = [path(rotated([(190, y), (x2, y)]), closed=False) for y, x2 in ((200, 344), (246, 344), (292, 344), (338, 290))]
shape(ink, lines, stroke(14, cap=2))

pencil = path(scaled([(300, 410), (418, 292), (446, 320), (328, 438), (292, 446)]))
shape(ink, [pencil])
eraser = path(scaled([(418, 292), (436, 274), (464, 302), (446, 320)]))
shape(paper, [eraser])
shape(ink, [eraser], stroke(8))

if __name__ == '__main__':
    out = sys.argv[1] if len(sys.argv) > 1 else 'WordCraft.hvif'
    data = hvif.encode(icon)
    hvif.decode(data)
    with open(out, 'wb') as f:
        f.write(data)
    print('%s: %d bytes' % (out, len(data)))
    if len(sys.argv) > 2:
        hvif.preview(icon, 256, (245, 245, 240, 255)).save(sys.argv[2])
        print(sys.argv[2])
