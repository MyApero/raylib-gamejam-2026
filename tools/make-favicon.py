#!/usr/bin/env python3
"""Generate client/web/favicon.ico: a gold hexagon on the title screen's navy.

Pure stdlib (no PIL on this box): PNGs are written by hand and embedded in a
PNG-format ICO, which every browser in use understands.
"""
import math
import struct
import zlib

GOLD = (255, 220, 92)    # ui.rs title accent
ORANGE = (232, 138, 36)  # outline: darker than GOLD so the silhouette still
                         # reads against a light browser tab
SS = 8                   # supersampling factor, for antialiased edges
SIZES = [16, 32, 48, 64, 128, 256]
# Outline thickness as a fraction of the hexagon radius. Kept proportional so
# the 16px icon keeps a visible edge instead of losing it to rounding.
OUTLINE = 0.17


def hexagon(cx, cy, r):
    """Pointy-top hexagon vertices — matches the game's tile orientation."""
    return [
        (cx + r * math.cos(math.radians(90 + 60 * i)),
         cy + r * math.sin(math.radians(90 + 60 * i)))
        for i in range(6)
    ]


def inside(poly, x, y):
    hit = False
    n = len(poly)
    for i in range(n):
        x0, y0 = poly[i]
        x1, y1 = poly[(i + 1) % n]
        if (y0 > y) != (y1 > y):
            xint = (x1 - x0) * (y - y0) / (y1 - y0) + x0
            if x < xint:
                hit = not hit
    return hit


def render(size):
    hi = size * SS
    # Fills more of the canvas than it did on the navy plate — with a
    # transparent background there is no frame to sit inside, and browser
    # tabs render the icon small.
    outer = hexagon(hi / 2, hi / 2, hi * 0.49)
    inner = hexagon(hi / 2, hi / 2, hi * 0.49 * (1.0 - OUTLINE))
    # Accumulate supersamples per output pixel, then average.
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0
            for sy in range(SS):
                for sx in range(SS):
                    x = px * SS + sx + 0.5
                    y = py * SS + sy + 0.5
                    if inside(inner, x, y):
                        cr, cg, cb, ca = GOLD + (255,)
                    elif inside(outer, x, y):
                        cr, cg, cb, ca = ORANGE + (255,)
                    else:
                        cr = cg = cb = ca = 0
                    # Weight colour by alpha so transparent samples don't
                    # drag the edge pixels toward black.
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            n = SS * SS
            if a:
                row += bytes((round(r / a), round(g / a), round(b / a), round(a / n)))
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\0" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


images = [(size, png(size, render(size))) for size in SIZES]

header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries, blobs = b"", b""
for size, data in images:
    entries += struct.pack(
        "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset
    )
    blobs += data
    offset += len(data)

with open("client/web/favicon.ico", "wb") as fh:
    fh.write(header + entries + blobs)
print("favicon.ico:", len(header + entries + blobs), "bytes,", len(images), "sizes")
