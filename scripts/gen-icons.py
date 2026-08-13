#!/usr/bin/env python3
"""Generate the Persea Desktop icon set from the design masters.

Produces src-tauri/icons/{32x32.png,128x128.png,128x128@2x.png,icon.png,
icon.ico,icon.icns} and docs/installer-art/{tile-150.png,banner-600x180.png}:
the monogram tile (rounded square, emerald gradient, white P, leaf sprout
above 48px) rendered with PIL from the same geometry as the SVG masters
(wayfinder/v1.2.0/design/logo-tile.svg, favicon.svg, tray.svg in the persea
repo; the masters are gitignored, so this script is the committed source of
truth and the single generator for every size).

Dependencies: python3 + Pillow. Run from the repo root.
"""

import os
import struct

import PIL.Image
import PIL.ImageDraw

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
ICON_DIR = os.path.join(ROOT, "src-tauri", "icons")
ART_DIR = os.path.join(ROOT, "docs", "installer-art")

GRAD_A = (0x05, 0x96, 0x69)  # #059669 top-left
GRAD_B = (0x10, 0xB9, 0x81)  # #10b981 bottom-right
TILE_INSET = 0.0625          # 6.25% inset (8/128)
TILE_RADIUS = 0.21875        # 28/128 corner radius


def _tile_mask(size: int) -> "PIL.Image.Image":
    inset = max(1, int(size * TILE_INSET))
    radius = max(1, int(size * TILE_RADIUS))
    mask = PIL.Image.new("L", (size, size), 0)
    d = PIL.ImageDraw.Draw(mask)
    d.rounded_rectangle(
        (inset, inset, size - inset - 1, size - inset - 1),
        radius=radius,
        fill=255,
    )
    return mask


def _gradient(size: int) -> "PIL.Image.Image":
    small = PIL.Image.new("RGB", (2, 2))
    small.putpixel((0, 0), GRAD_A)
    small.putpixel((1, 0), GRAD_A)
    small.putpixel((0, 1), GRAD_B)
    small.putpixel((1, 1), GRAD_B)
    return small.resize((size, size), PIL.Image.BILINEAR)


def render_tile(size: int, sprout: bool = True) -> "PIL.Image.Image":
    """The monogram tile: gradient rounded square + white P (+ sprout)."""
    tile = _gradient(size)
    mask = _tile_mask(size)
    canvas = PIL.Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.paste(tile, (0, 0), mask)

    # The P (master geometry at 128: stem x52-64 y30-86, bowl x52-93 y30-64,
    # counter x64-82 y44-56). Drawn as wide lines + caps, punched counter.
    s = size / 128.0
    white = (255, 255, 255, 255)
    d = PIL.ImageDraw.Draw(canvas)
    width = max(1, int(12 * s))
    d.line(
        [
            (int(58 * s), int(34 * s)),
            (int(58 * s), int(82 * s)),
            (int(58 * s), int(46 * s)),
            (int(79 * s), int(46 * s)),
        ],
        fill=white,
        width=width,
        joint="curve",
    )
    cap_r = max(1, int(17 * s))
    d.ellipse(
        (
            int(79 * s) - width // 2,
            int(46 * s) - cap_r,
            int(79 * s) + width // 2 + cap_r,
            int(46 * s) + cap_r,
        ),
        fill=white,
    )
    # Counter: punch the gradient back through the P bowl (crop the
    # gradient at the counter rect, alpha-masked to the rounded rect).
    cx0, cy0, cx1, cy1 = (
        int(66 * s),
        int(44 * s),
        int(80 * s),
        int(56 * s),
    )
    grad_crop = tile.crop((cx0, cy0, cx1, cy1)).convert("RGBA")
    cw, ch = cx1 - cx0, cy1 - cy0
    c_mask = PIL.Image.new("L", (cw, ch), 0)
    PIL.ImageDraw.Draw(c_mask).rounded_rectangle(
        (0, 0, cw - 1, ch - 1),
        radius=max(1, int(6 * s)),
        fill=255,
    )
    grad_crop.putalpha(c_mask)
    canvas.paste(grad_crop, (cx0, cy0), grad_crop)

    if sprout and size >= 48:
        # Leaf sprout at bottom right (master: M84 96 c-4-8 -9-14 -15-19 ...).
        pts = [
            (int(84 * s), int(96 * s)),
            (int(69 * s), int(77 * s)),
            (int(84 * s), int(77 * s)),
        ]
        d.polygon(pts, fill=white)

    return canvas


def render_tray(size: int = 24, dot: bool = False) -> "PIL.Image.Image":
    """Monochrome tray icon: black rounded tile + white P (+ state dot)."""
    inset = max(1, int(size * 0.06))
    radius = max(1, int(size * 0.23))
    canvas = PIL.Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = PIL.ImageDraw.Draw(canvas)
    d.rounded_rectangle(
        (inset, inset, size - inset - 1, size - inset - 1),
        radius=radius,
        fill=(0, 0, 0, 255),
    )
    s = size / 24.0
    width = max(1, int(3 * s))
    stem = (int(9 * s), int(6 * s), int(9 * s), int(16 * s))
    bowl = (int(9 * s), int(8 * s), int(13 * s), int(8 * s))
    d.line(
        [
            (int(9 * s), int(6 * s)),
            (int(9 * s), int(16 * s)),
            (int(9 * s), int(8 * s)),
            (int(13 * s), int(8 * s)),
        ],
        fill=(255, 255, 255, 255),
        width=width,
        joint="curve",
    )
    d.ellipse(
        (
            int(13 * s) - width // 2,
            int(8 * s) - max(1, int(4 * s)),
            int(13 * s) + width // 2 + max(1, int(4 * s)),
            int(8 * s) + max(1, int(4 * s)),
        ),
        fill=(255, 255, 255, 255),
    )
    if dot:
        d.ellipse(
            (
                int(17 * s),
                int(17 * s),
                int(17 * s) + max(2, int(4 * s)),
                int(17 * s) + max(2, int(4 * s)),
            ),
            fill=(255, 255, 255, 255),
        )
    return canvas


def make_ico(png: bytes, size: int) -> bytes:
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


def png_bytes(img: "PIL.Image.Image") -> bytes:
    buf = PIL.Image.new("RGBA", img.size)
    buf.paste(img)
    out = __import__("io").BytesIO()
    buf.save(out, format="PNG")
    return out.getvalue()


def main():
    png32 = png_bytes(render_tile(32, sprout=False))
    png128 = png_bytes(render_tile(128))
    png256 = png_bytes(render_tile(256))
    png512 = png_bytes(render_tile(512))

    os.makedirs(ICON_DIR, exist_ok=True)
    os.makedirs(ART_DIR, exist_ok=True)
    files = {
        "32x32.png": png32,
        "128x128.png": png128,
        "128x128@2x.png": png256,
        "icon.png": png512,
        "icon.ico": make_ico(png256, 256),
        "icon.icns": make_icns([(b"ic07", png128), (b"ic08", png256), (b"ic09", png512)]),
    }
    for name, data in files.items():
        with open(os.path.join(ICON_DIR, name), "wb") as f:
            f.write(data)
        print(f"wrote src-tauri/icons/{name} ({len(data)} bytes)")

    # Installer art.
    tile = render_tile(150)
    tile.save(os.path.join(ART_DIR, "tile-150.png"))
    banner = PIL.Image.new("RGBA", (600, 180), (0, 0, 0, 0))
    band = _gradient(600).crop((0, 0, 600, 180))
    banner.paste(band, (0, 0))
    banner.paste(tile, (225, 15), tile)
    banner.save(os.path.join(ART_DIR, "banner-600x180.png"))
    print("wrote docs/installer-art/{tile-150.png,banner-600x180.png}")


if __name__ == "__main__":
    main()
