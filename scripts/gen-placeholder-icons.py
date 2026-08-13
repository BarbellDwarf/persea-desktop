#!/usr/bin/env python3
"""Generate placeholder app icons for Persea Desktop (D01).

Produces src-tauri/icons/{32x32.png,128x128.png,128x128@2x.png,icon.png,
icon.icns,icon.ico}: a green disc on a dark green square. Placeholders
only; the real artwork is D18.

Dependencies: python3 stdlib only.
"""

import os
import struct
import zlib

ICON_DIR = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "icons")
BG = (0x14, 0x53, 0x2D)
FG = (0x4A, 0xDE, 0x80)
DISC = 0.30


def png_chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def make_png(size):
    rows = bytearray()
    center = size / 2.0
    radius = size * DISC
    for y in range(size):
        rows.append(0)
        for x in range(size):
            dx = x + 0.5 - center
            dy = y + 0.5 - center
            px = FG if dx * dx + dy * dy <= radius * radius else BG
            # tauri requires RGBA (color type 6): full alpha channel.
            rows.extend(px + (255,))
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(bytes(rows), 9))
        + png_chunk(b"IEND", b"")
    )


def make_ico(png, size):
    if size >= 256:
        size = 0
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", size, size, 0, 0, 1, 32, len(png), 22)
    return header + entry + png


def make_icns(chunks):
    body = b""
    for tag, png in chunks:
        body += struct.pack(">4sI", tag, 8 + len(png)) + png
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main():
    png32 = make_png(32)
    png128 = make_png(128)
    png256 = make_png(256)
    png512 = make_png(512)

    os.makedirs(ICON_DIR, exist_ok=True)
    files = {
        "32x32.png": png32,
        "128x128.png": png128,
        "128x128@2x.png": png256,
        "icon.png": png512,
        "icon.ico": make_ico(png256, 256),
        "icon.icns": make_icns(
            [(b"ic07", png128), (b"ic08", png256), (b"ic09", png512)]
        ),
    }
    for name, data in files.items():
        with open(os.path.join(ICON_DIR, name), "wb") as f:
            f.write(data)
        print(f"wrote src-tauri/icons/{name} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
