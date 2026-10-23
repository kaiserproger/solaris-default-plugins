#!/usr/bin/env python3
"""Generate the chisel item texture: a 16x16 RGBA PNG (stdlib only).

Writes ``client-src/assets/chisel/textures/item/chisel.png`` next to this
script. Pixel art, drawn programmatically:

- a wooden handle (browns) running from the bottom-left toward the center,
- a steel-grey blade from the center to the top-right tip,
- a dark outline around both, transparent background.

The writer uses zlib + struct only: 8-bit RGBA, no interlace, one IDAT.
"""
from __future__ import annotations

import struct
import zlib
from pathlib import Path

SIZE = 16

# Palette.
TRANSPARENT = (0, 0, 0, 0)
OUTLINE = (28, 24, 22, 255)
HANDLE = (139, 90, 43, 255)
HANDLE_DARK = (101, 63, 28, 255)
HANDLE_LIGHT = (172, 120, 62, 255)
STEEL = (168, 178, 190, 255)
STEEL_DARK = (120, 130, 145, 255)
STEEL_LIGHT = (210, 218, 230, 255)
EDGE = (70, 76, 88, 255)


def draw() -> list[list[tuple[int, int, int, int]]]:
    canvas = [[TRANSPARENT for _ in range(SIZE)] for _ in range(SIZE)]

    def put(x: int, y: int, color: tuple[int, int, int, int]) -> None:
        if 0 <= x < SIZE and 0 <= y < SIZE:
            canvas[y][x] = color

    # Blade: diagonal band from (6,9) up to the (13,2) tip. Each step paints
    # a 2px-wide band: light steel on the upper-left edge, mid steel center,
    # dark steel lower-right edge, outline fringe on both sides.
    for step in range(8):
        x = 6 + step
        y = 9 - step
        put(x - 1, y, OUTLINE)
        put(x, y - 1, OUTLINE)
        put(x, y, STEEL_LIGHT)
        put(x + 1, y, STEEL)
        put(x, y + 1, STEEL)
        put(x + 1, y + 1, STEEL_DARK)
        put(x + 2, y + 1, OUTLINE)
        put(x + 1, y + 2, OUTLINE)
    # Tip highlight.
    put(13, 2, STEEL_LIGHT)
    put(14, 1, OUTLINE)
    # Cutting edge along the lower-right side of the blade.
    for step in range(6):
        put(7 + step, 10 - step, EDGE)

    # Ferrule between blade and handle.
    put(6, 9, HANDLE_DARK)
    put(5, 10, HANDLE_DARK)
    put(6, 10, STEEL_DARK)
    put(5, 9, OUTLINE)

    # Handle: diagonal band from (1,14) to (5,10).
    for step in range(5):
        x = 1 + step
        y = 14 - step
        put(x - 1, y, OUTLINE)
        put(x, y + 1, OUTLINE)
        put(x, y, HANDLE_DARK if step % 2 else HANDLE)
        put(x + 1, y, HANDLE_LIGHT if step % 2 else HANDLE)
        put(x, y - 1, HANDLE_LIGHT)
        put(x + 1, y + 1, OUTLINE)
        put(x - 1, y - 1, OUTLINE)
    # Pommel cap.
    put(1, 14, HANDLE_DARK)
    put(0, 14, OUTLINE)
    put(0, 15, OUTLINE)
    put(1, 15, OUTLINE)
    put(2, 15, OUTLINE)
    return canvas


def png_chunk(kind: bytes, data: bytes) -> bytes:
    chunk = kind + data
    return struct.pack(">I", len(data)) + chunk + struct.pack(">I", zlib.crc32(chunk))


def write_png(path: Path, canvas: list[list[tuple[int, int, int, int]]]) -> None:
    raw = bytearray()
    for row in canvas:
        raw.append(0)  # filter type 0 (None)
        for pixel in row:
            raw.extend(pixel)
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + png_chunk(b"IEND", b"")
    )
    path.write_bytes(png)


def main() -> None:
    root = Path(__file__).resolve().parent
    target = root / "client-src" / "assets" / "chisel" / "textures" / "item" / "chisel.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    write_png(target, draw())
    print(f"wrote {target} ({target.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
