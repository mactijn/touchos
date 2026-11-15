#!/usr/bin/env python3
"""Turn a raw 800x480 RGB565 (little-endian) framebuffer dump into a PNG.

    fb2png.py fb.raw fb.png [width height]

The dump comes from QEMU's `pmemsave` (tools/toon/fbshot) or from OpenOCD's
`dump_image` on the Toon (0xa1400000, 768000 bytes). No dependencies.
"""
import struct
import sys
import zlib


def main():
    src, dst = sys.argv[1], sys.argv[2]
    w, h = (int(sys.argv[3]), int(sys.argv[4])) if len(sys.argv) > 4 else (800, 480)
    raw = open(src, "rb").read()
    if len(raw) < w * h * 2:
        sys.exit(f"{src}: {len(raw)} bytes, need {w * h * 2}")
    rows = bytearray()
    for y in range(h):
        rows.append(0)  # filter: none
        for (p,) in struct.iter_unpack("<H", raw[y * w * 2:(y + 1) * w * 2]):
            r, g, b = p >> 11, (p >> 5) & 0x3F, p & 0x1F
            rows += bytes(((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)))

    def chunk(kind, data):
        c = kind + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")
    open(dst, "wb").write(png)


if __name__ == "__main__":
    main()
