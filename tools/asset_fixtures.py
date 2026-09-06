"""Generate the committed PNG asset fixtures and their expected RGBA8 bytes.

Python stdlib only. Every PNG is framed here from an explicit specification, and
every `.rgba` file lists the pixels the engine must produce, so the tests compare
against an independent statement of the contract rather than against whatever a
first passing implementation happened to emit. Regenerate with:

    python tools/asset_fixtures.py
"""

import binascii
import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/assets"
ROOT.mkdir(parents=True, exist_ok=True)
SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunk(kind, data):
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", binascii.crc32(kind + data))
    )


def header(width, height, depth, color, interlace=0):
    return chunk(
        b"IHDR", struct.pack(">IIBBBBB", width, height, depth, color, 0, 0, interlace)
    )


def write(name, png, rgba=None):
    (ROOT / f"{name}.png").write_bytes(png)
    if rgba is not None:
        (ROOT / f"{name}.rgba").write_bytes(bytes(rgba))
    return png


def simple(name, width, height, depth, color, rows, rgba, extra=b"", interlace=0):
    """One PNG whose scanlines all use filter 0, plus its expected RGBA8 output."""
    raw = b"".join(b"\x00" + row for row in rows)
    png = (
        SIGNATURE
        + header(width, height, depth, color, interlace)
        + extra
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )
    return write(name, png, rgba)


# Grayscale, 1 bit per sample. Two pixels: black then white, expanded to RGBA8.
simple("gray", 2, 1, 1, 0, [b"\x40"], [0, 0, 0, 255, 255, 255, 255, 255])

# Grayscale with alpha, 8 bit. Opaque dark gray, then half-transparent light gray.
simple(
    "gray_alpha",
    2,
    1,
    8,
    4,
    [bytes([71, 255, 191, 128])],
    [71, 71, 71, 255, 191, 191, 191, 128],
)

# Truecolor without alpha. Opaque output alpha is supplied by the engine.
simple(
    "rgb",
    3,
    1,
    8,
    2,
    [bytes([7, 13, 19, 41, 43, 47, 250, 2, 100])],
    [7, 13, 19, 255, 41, 43, 47, 255, 250, 2, 100, 255],
)

# Truecolor with alpha, deliberately asymmetric between rows and columns so a
# transposed or vertically flipped implementation cannot match.
simple(
    "rgba",
    2,
    3,
    8,
    6,
    [
        bytes([255, 0, 0, 255, 0, 255, 0, 128]),
        bytes([0, 0, 255, 0, 255, 255, 0, 255]),
        bytes([9, 8, 7, 6, 255, 255, 255, 255]),
    ],
    [
        255, 0, 0, 255, 0, 255, 0, 128,
        0, 0, 255, 0, 255, 255, 0, 255,
        9, 8, 7, 6, 255, 255, 255, 255,
    ],
)

# Indexed color at 1 bit per pixel with palette transparency.
simple(
    "palette",
    2,
    1,
    1,
    3,
    [b"\x40"],
    [7, 13, 19, 0, 41, 43, 47, 128],
    extra=chunk(b"PLTE", bytes([7, 13, 19, 41, 43, 47]))
    + chunk(b"tRNS", bytes([0, 128])),
)


def adam7(pixels, width, height):
    """Interlaced scanlines, built from the Adam7 pass geometry directly."""
    raw = b""
    for x0, y0, dx, dy in [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ]:
        if x0 >= width or y0 >= height:
            continue
        for y in range(y0, height, dy):
            raw += b"\x00"
            for x in range(x0, width, dx):
                raw += bytes(pixels[y * width + x])
    return raw


# 9x9 keeps every Adam7 pass non-empty and leaves partial final passes. Each
# pixel encodes its own coordinates, so any misplacement is visible.
INTERLACED_W, INTERLACED_H = 9, 9
interlaced_pixels = [
    (x * 25 % 256, y * 25 % 256, (x * 9 + y) % 256, 255 - (x + y) * 5)
    for y in range(INTERLACED_H)
    for x in range(INTERLACED_W)
]
interlaced_raw = adam7(interlaced_pixels, INTERLACED_W, INTERLACED_H)
write(
    "interlaced",
    SIGNATURE
    + header(INTERLACED_W, INTERLACED_H, 8, 6, interlace=1)
    + chunk(b"IDAT", zlib.compress(interlaced_raw))
    + chunk(b"IEND", b""),
    [component for pixel in interlaced_pixels for component in pixel],
)

# --- refusals -------------------------------------------------------------

# 16-bit samples are rejected rather than silently narrowed.
simple("sixteen_bit", 2, 1, 16, 0, [bytes([0, 1, 255, 255])], None)

# A single animation frame must not be chosen silently.
write(
    "apng",
    SIGNATURE
    + header(2, 1, 8, 6)
    + chunk(b"acTL", struct.pack(">II", 1, 0))
    + chunk(b"fcTL", struct.pack(">IIIIIHHBB", 0, 2, 1, 0, 0, 1, 60, 0, 0))
    + chunk(b"IDAT", zlib.compress(b"\x00" + bytes([255] * 8)))
    + chunk(b"IEND", b""),
)

# A header far above the dimension bound with almost no payload behind it.
write(
    "huge_dimensions",
    SIGNATURE
    + header(60000, 60000, 8, 6)
    + chunk(b"IDAT", zlib.compress(b"\x00" + bytes([1, 2, 3, 4])))
    + chunk(b"IEND", b""),
)

# Valid framing, truncated compressed pixel data.
full = (
    SIGNATURE
    + header(8, 8, 8, 6)
    + chunk(b"IDAT", zlib.compress(b"".join(b"\x00" + bytes([9] * 32) for _ in range(8))))
    + chunk(b"IEND", b"")
)
write("truncated", full[: len(full) - 30])

# Correct signature and plausible header, but a header CRC that does not match.
# Corrupting a dimension byte instead would exercise the dimension bound rather
# than malformed framing, which the huge_dimensions fixture already covers.
corrupt = bytearray(SIGNATURE + header(4, 4, 8, 6) + chunk(b"IEND", b""))
corrupt[31] ^= 0xFF
write("corrupt", bytes(corrupt))

# Not a PNG at all, despite the extension the request policy requires.
write("wrong_format", b"\xff\xd8\xff\xe0JFIF not a portable network graphic")

print(f"Wrote asset fixtures to {ROOT}")
