#!/usr/bin/env python3
"""Draw the menu bar (tray) icons.

The icons are macOS template images: black shapes on a transparent background.
macOS recolours them to match the menu bar, so only the alpha channel matters.

Run from this directory to regenerate the PNGs (standard library only):

    python3 generate.py
"""

import struct
import zlib

SIZE = 36  # pixels; shown at 18 points (Retina @2x)
SAMPLES = 4  # supersampling per axis, for smooth edges


def in_ellipse(x, y, cx, cy, rx, ry):
    return ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1.0


def in_triangle(x, y, a, b, c):
    def side(p, q):
        return (x - q[0]) * (p[1] - q[1]) - (p[0] - q[0]) * (y - q[1])

    d1, d2, d3 = side(a, b), side(b, c), side(c, a)
    has_neg = d1 < 0 or d2 < 0 or d3 < 0
    has_pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (has_neg and has_pos)


def in_rect(x, y, x0, y0, x1, y1):
    return x0 <= x <= x1 and y0 <= y <= y1


def fish(x, y, dx=0.0):
    """A simple salmon silhouette (lax is Swedish for salmon), facing left."""
    x -= dx
    if in_ellipse(x, y, 7.5, 16.5, 1.7, 1.7):  # eye, cut out
        return False
    body = in_ellipse(x, y, 14.0, 18.0, 11.5, 7.5)
    tail = in_triangle(x, y, (22.0, 18.0), (33.5, 9.5), (33.5, 26.5))
    return body or tail


def active(x, y):
    return 1.0 if fish(x, y) else 0.0


def paused(x, y):
    # Two solid pause bars over a faded fish.
    if in_rect(x, y, 23.0, 9.0, 26.5, 27.0) or in_rect(x, y, 30.0, 9.0, 33.5, 27.0):
        return 1.0
    if in_rect(x, y, 21.0, 0.0, 36.0, 36.0):  # keep a gap around the bars
        return 0.0
    return 0.35 if fish(x, y, dx=-1.0) else 0.0


def render(shape):
    rows = []
    for py in range(SIZE):
        row = bytearray([0])  # PNG filter type: none
        for px in range(SIZE):
            total = 0.0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    x = px + (sx + 0.5) / SAMPLES
                    y = py + (sy + 0.5) / SAMPLES
                    total += shape(x, y)
            alpha = round(255 * total / SAMPLES**2)
            row += bytes([0, 0, 0, alpha])
        rows.append(bytes(row))
    return b"".join(rows)


def write_png(path, pixels):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", header))
        f.write(chunk(b"IDAT", zlib.compress(pixels, 9)))
        f.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    write_png("active.png", render(active))
    write_png("paused.png", render(paused))
