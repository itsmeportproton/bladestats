#!/usr/bin/env python3
"""Draws bladestats.ico.

The icon is generated rather than drawn by hand so that it can be changed by editing numbers
instead of by opening an image editor, and so that anyone reading this repository can see what
it is made of. Standard library only: no image library is involved, the PNGs are written here.

The mark is the overlay's own: a row of bars at different heights, the per-core load strip that
is the most recognisable thing bladestats draws. At sixteen pixels a wordmark would be a smear
and a glyph would belong to some other program, whereas four bars still read as four bars.

    python assets/icon/make-icon.py
"""

import struct
import zlib
from pathlib import Path

OUT = Path(__file__).with_name("bladestats.ico")

# Sizes Windows asks for. The small ones matter most: the taskbar and the notification area
# never show anything larger.
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]

# Near-black with a hint of blue, the same ground the overlay panel is drawn on.
BACKGROUND = (0x11, 0x14, 0x18, 0xFF)
# The load colour, and one step brighter for the bar that is running hottest.
BAR = (0xE0, 0x50, 0x40, 0xFF)
BAR_HOT = (0xFF, 0x6B, 0x5B, 0xFF)

# Heights as a fraction of the drawable area, in the shape of a processor under uneven load.
# The third is the tall one, which puts the visual weight just right of centre.
BARS = [0.45, 0.70, 1.00, 0.60]

# Supersampling factor. Only the rounded corners need it — the bars are snapped to whole
# pixels, because an anti-aliased two-pixel bar is a grey smudge.
SS = 4


def rounded_coverage(size, radius):
    """Coverage of a rounded square, one value per pixel, anti-aliased by supersampling."""
    coverage = [[0.0] * size for _ in range(size)]
    for y in range(size):
        for x in range(size):
            hits = 0
            for sy in range(SS):
                for sx in range(SS):
                    px = x + (sx + 0.5) / SS
                    py = y + (sy + 0.5) / SS
                    # Distance into the nearest corner's circle, or inside the straight part.
                    cx = min(max(px, radius), size - radius)
                    cy = min(max(py, radius), size - radius)
                    if (px - cx) ** 2 + (py - cy) ** 2 <= radius**2:
                        hits += 1
            coverage[y][x] = hits / (SS * SS)
    return coverage


def draw(size):
    """One icon, as straight RGBA rows."""
    radius = max(2.0, size * 0.22)
    coverage = rounded_coverage(size, radius)

    pixels = [[(0, 0, 0, 0)] * size for _ in range(size)]
    for y in range(size):
        for x in range(size):
            a = coverage[y][x]
            if a > 0:
                r, g, b, _ = BACKGROUND
                pixels[y][x] = (r, g, b, round(a * 255))

    # The bars, snapped to whole pixels so they stay crisp at every size.
    inset = max(2, round(size * 0.20))
    area = size - inset * 2
    gap = max(1, round(size * 0.055))
    width = max(1, (area - gap * (len(BARS) - 1)) // len(BARS))
    # Whatever the division left over is given back, so the row stays centred.
    used = width * len(BARS) + gap * (len(BARS) - 1)
    left = inset + (area - used) // 2
    floor = size - inset

    tallest = max(BARS)
    for i, height in enumerate(BARS):
        bar_h = max(1, round(area * height))
        top = floor - bar_h
        colour = BAR_HOT if height == tallest else BAR
        x0 = left + i * (width + gap)
        for y in range(max(0, top), min(size, floor)):
            for x in range(x0, min(size, x0 + width)):
                # Painted over the background, which is already opaque wherever a bar can be.
                pixels[y][x] = colour

    rows = bytearray()
    for row in pixels:
        rows.append(0)  # PNG filter: none
        for r, g, b, a in row:
            rows += bytes((r, g, b, a))
    return bytes(rows)


def png(size, raw):
    """A minimal RGBA PNG. Windows has accepted PNG inside ICO since Vista."""

    def chunk(kind, payload):
        body = kind + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    images = [png(size, draw(size)) for size in SIZES]

    # ICONDIR, then one ICONDIRENTRY per image, then the images themselves.
    offset = 6 + 16 * len(images)
    directory = struct.pack("<HHH", 0, 1, len(images))
    for size, image in zip(SIZES, images):
        # 256 is written as zero: the field is one byte and 256 does not fit in it.
        dimension = 0 if size == 256 else size
        directory += struct.pack(
            "<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(image), offset
        )
        offset += len(image)

    OUT.write_bytes(directory + b"".join(images))
    print(f"{OUT}  {len(OUT.read_bytes())} bytes, {len(images)} sizes")


if __name__ == "__main__":
    main()
