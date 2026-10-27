#!/usr/bin/env python3
"""Generate the minimal PNG icon set of the LayerAgent desktop shell.

The icons are written by hand (signature, IHDR, IDAT, IEND) so the project
never depends on a downloaded or third party asset.

Usage: python3 tools/generate_icons.py
"""

from __future__ import annotations

import pathlib
import struct
import zlib

BACKGROUND = (18, 22, 33, 255)
FOREGROUND = (86, 156, 214, 255)
ACCENT = (232, 240, 254, 255)

SIZES = {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512,
}


def chunk(tag: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + tag
        + payload
        + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
    )


def pixels(size: int) -> bytes:
    """A rounded square with a lighter bar, drawn without any dependency."""
    rows = bytearray()
    radius = size // 6
    inner = size // 3
    for y in range(size):
        rows.append(0)  # filter type "none"
        for x in range(size):
            corner = (
                (x < radius and y < radius)
                or (x >= size - radius and y < radius)
                or (x < radius and y >= size - radius)
                or (x >= size - radius and y >= size - radius)
            )
            if corner:
                dx = radius - x if x < radius else x - (size - radius - 1)
                dy = radius - y if y < radius else y - (size - radius - 1)
                if dx * dx + dy * dy > radius * radius:
                    rows.extend((0, 0, 0, 0))
                    continue
            # horizontal bar in the middle of the square
            if inner <= y < size - inner and radius <= x < size - radius:
                rows.extend(ACCENT)
            elif inner <= y < size - inner:
                rows.extend(FOREGROUND)
            else:
                rows.extend(BACKGROUND)
    return bytes(rows)


def png(size: int) -> bytes:
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    body = zlib.compress(pixels(size), 9)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", body) + chunk(b"IEND", b"")


def main() -> None:
    target = pathlib.Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
    target.mkdir(parents=True, exist_ok=True)
    for name, size in SIZES.items():
        path = target / name
        path.write_bytes(png(size))
        print(f"wrote {path.relative_to(target.parent.parent.parent)} ({size}x{size})")


if __name__ == "__main__":
    main()
