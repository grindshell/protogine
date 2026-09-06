"""Generate Phase 0 PNG fixtures with independently specified RGBA bytes (stdlib)."""

import binascii
import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1] / "target/png-sprite-probe"
ROOT.mkdir(parents=True, exist_ok=True)


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", binascii.crc32(kind + data))


def fixture(name, depth, color, row, rgba=None, extra=b""):
    header = struct.pack(">IIBBBBB", 2, 1, depth, color, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + extra
    png += chunk(b"IDAT", zlib.compress(b"\x00" + row)) + chunk(b"IEND", b"")
    (ROOT / f"{name}.png").write_bytes(png)
    if rgba is not None:
        (ROOT / f"{name}.rgba").write_bytes(bytes(rgba))


fixture("gray", 1, 0, b"\x40", [0, 0, 0, 255, 255, 255, 255, 255])
fixture("gray-alpha", 8, 4, bytes([71, 0, 191, 128]), [71, 71, 71, 0, 191, 191, 191, 128])
fixture("rgb", 8, 2, bytes([7, 13, 19, 41, 43, 47]), [7, 13, 19, 255, 41, 43, 47, 255])
fixture("palette", 1, 3, b"\x40", [7, 13, 19, 0, 41, 43, 47, 128],
        chunk(b"PLTE", bytes([7, 13, 19, 41, 43, 47])) + chunk(b"tRNS", bytes([0, 128])))
fixture("16bit", 16, 0, bytes([0, 1, 255, 255]))
fixture("apng", 8, 6, bytes([255] * 8), extra=chunk(b"acTL", struct.pack(">II", 1, 0)) +
        chunk(b"fcTL", struct.pack(">IIIIIHHBB", 0, 2, 1, 0, 0, 1, 60, 0, 0)))
# High expansion metadata is syntactically framed correctly. The decoder ignores
# text; the ICC payload tests bounded decompression even though it is not a usable
# color profile. No color-management output is expected from either fixture.
compressed = zlib.compress(b"a" * (40 * 1024 * 1024))
for name, kind in [("text", b"zTXt"), ("icc", b"iCCP")]:
    fixture(name, 8, 6, bytes([255] * 8), [255] * 8,
            chunk(kind, b"probe\x00\x00" + compressed))
print("Generated 8 Phase 0 fixtures; compressed metadata expands to 40 MiB.")
