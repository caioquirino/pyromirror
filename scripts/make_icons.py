#!/usr/bin/env python3
"""Regenerates the PyroMirror icon files from the one design used everywhere.

The same shapes are drawn at run time by crates/pyromirror-gui/src/tray.rs (for the tray, in
each sharing state); a test there checks that it matches the PNG written here. Change the
design in both places.

Usage: scripts/make_icons.py   (from the repository root; needs only Python 3)
"""
import struct
import zlib

ORANGE, YELLOW, DARK = (0xFF, 0x7A, 0x2F), (0xFF, 0xD2, 0x7A), (0x12, 0x15, 0x1C)


def rrect(x, y, x0, y0, w, h, r):
    cx = min(max(x, x0 + r), x0 + w - r)
    cy = min(max(y, y0 + r), y0 + h - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def flame(x, y, cy, r, tip):
    """A disc with a pointed top, centred horizontally."""
    dx = x - 128.0
    if dx * dx + (y - cy) ** 2 <= r * r:
        return True
    return tip <= y <= cy and abs(dx) <= r * ((y - tip) / (cy - tip)) ** 0.8


def design(x, y):
    """Colour at (x, y) in a 256x256 space, or None where the icon is transparent.
    This is the "sharing" look: orange monitor, dark screen, flame."""
    c = None
    if rrect(x, y, 100, 186, 56, 26, 4) or rrect(x, y, 64, 210, 128, 26, 13):
        c = ORANGE
    if rrect(x, y, 6, 20, 244, 176, 30):
        c = ORANGE
    if rrect(x, y, 26, 40, 204, 136, 14):
        c = DARK
    if flame(x, y, 122, 44, 50):
        c = ORANGE
    if flame(x, y, 138, 19, 100):
        c = YELLOW
    return c


def render(size, ss=4):
    scale = 256.0 / (size * ss)
    out = bytearray(size * size * 4)
    for py in range(size):
        for px in range(size):
            r = g = b = n = 0
            for sy in range(ss):
                for sx in range(ss):
                    c = design((px * ss + sx + 0.5) * scale, (py * ss + sy + 0.5) * scale)
                    if c:
                        r += c[0]; g += c[1]; b += c[2]; n += 1
            if n:
                i = (py * size + px) * 4
                out[i:i + 4] = bytes((r // n, g // n, b // n, n * 255 // (ss * ss)))
    return bytes(out)


def png(rgba, size):
    rows = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    chunk = lambda t, c: struct.pack(">I", len(c)) + t + c + struct.pack(">I", zlib.crc32(t + c))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


def main():
    sizes = [16, 20, 24, 32, 40, 48, 64, 256]
    images = [png(render(s), s) for s in sizes]

    # ICO with PNG-compressed entries (supported since Windows Vista).
    offset = 6 + 16 * len(sizes)
    entries = b""
    for size, data in zip(sizes, images):
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    ico = struct.pack("<HHH", 0, 1, len(sizes)) + entries + b"".join(images)

    big = images[-1]
    for path, data in [
        ("crates/pyromirror-gui/assets/pyromirror.ico", ico),
        ("crates/pyromirror-gui/assets/icon.png", big),
        ("packaging/linux/pyromirror.png", big),
        ("site/assets/icon.png", big),
    ]:
        with open(path, "wb") as f:
            f.write(data)
        print("wrote", path)


if __name__ == "__main__":
    main()
