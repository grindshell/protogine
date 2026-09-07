"""Generate the sprite sample's committed art from the Kenney 1-Bit Pack sheet.

Python stdlib only. `examples/games/sprites/assets/tiles.png` is a byte-for-byte
copy of `examples/kenney_1-bit-pack_transparent-packed.png`, so a shipped bundle
carries its own art; `character.png` is the two-frame walk cycle cropped out of
that same sheet and re-encoded as 8-bit RGBA, which also gives the sample two
different PNG color types to load. Both outputs are derived here rather than
recorded from a first passing run, so the committed bytes can be reproduced and
the crop's provenance is a statement rather than a memory. Regenerate with:

    python tools/sample_sprites.py

The Kenney 1-Bit Pack is CC0; see examples/README.md for attribution.
"""

import binascii
import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
SHEET = ROOT / "examples/kenney_1-bit-pack_transparent-packed.png"
ASSETS = ROOT / "examples/games/sprites/assets"
SIGNATURE = b"\x89PNG\r\n\x1a\n"

# The walk cycle: sheet cells (27,1) and (28,1), one helmeted figure in two
# poses. Cells are 16x16, so this is the 32x16 region at (432,16).
FRAME_COLUMN, FRAME_ROW, FRAME_COUNT = 27, 1, 2
CELL = 16


def chunks(data):
    """Yield (kind, body) for every chunk, ignoring the CRC the writer sets."""
    if data[:8] != SIGNATURE:
        raise SystemExit(f"{SHEET} is not a PNG")
    offset = 8
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset : offset + 4])
        yield data[offset + 4 : offset + 8], data[offset + 8 : offset + 8 + length]
        offset += 12 + length


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def decode(data):
    """Return (width, height, RGBA8 bytes) for a non-interlaced PNG.

    Only the color types this sheet uses are handled; anything else is a hard
    error rather than a silent approximation.
    """
    palette, transparency, compressed = b"", b"", b""
    width = height = depth = color = None
    for kind, body in chunks(data):
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if interlace:
                raise SystemExit("interlaced source PNGs are not supported here")
            if (depth, color) != (1, 3):
                raise SystemExit(f"unexpected source format: depth {depth}, color {color}")
        elif kind == b"PLTE":
            palette = body
        elif kind == b"tRNS":
            transparency = body
        elif kind == b"IDAT":
            compressed += body
    raw = zlib.decompress(compressed)

    stride = (width * depth + 7) // 8
    rows, previous, offset = [], bytearray(stride), 0
    for _ in range(height):
        kind, offset = raw[offset], offset + 1
        row = bytearray(raw[offset : offset + stride])
        offset += stride
        # Filters operate on bytes; at one bit per pixel the left neighbor is
        # the previous byte.
        for i in range(stride):
            left = row[i - 1] if i else 0
            up = previous[i]
            corner = previous[i - 1] if i else 0
            if kind == 1:
                row[i] = (row[i] + left) & 0xFF
            elif kind == 2:
                row[i] = (row[i] + up) & 0xFF
            elif kind == 3:
                row[i] = (row[i] + (left + up) // 2) & 0xFF
            elif kind == 4:
                row[i] = (row[i] + paeth(left, up, corner)) & 0xFF
            elif kind != 0:
                raise SystemExit(f"unknown filter type {kind}")
        rows.append(row)
        previous = row

    rgba = bytearray(width * height * 4)
    for y, row in enumerate(rows):
        for x in range(width):
            index = (row[x // 8] >> (7 - x % 8)) & 1
            at = (y * width + x) * 4
            rgba[at : at + 3] = palette[index * 3 : index * 3 + 3]
            rgba[at + 3] = transparency[index] if index < len(transparency) else 255
    return width, height, rgba


def chunk(kind, body):
    return (
        struct.pack(">I", len(body))
        + kind
        + body
        + struct.pack(">I", binascii.crc32(kind + body))
    )


def encode(width, height, rgba):
    """8-bit RGBA, every scanline filter 0, so the bytes stay reproducible."""
    raw = b"".join(
        b"\x00" + bytes(rgba[y * width * 4 : (y + 1) * width * 4]) for y in range(height)
    )
    return (
        SIGNATURE
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


source = SHEET.read_bytes()
width, height, rgba = decode(source)
ASSETS.mkdir(parents=True, exist_ok=True)
(ASSETS / "tiles.png").write_bytes(source)

crop_width, crop_height = CELL * FRAME_COUNT, CELL
crop = bytearray(crop_width * crop_height * 4)
for y in range(crop_height):
    for x in range(crop_width):
        at = ((FRAME_ROW * CELL + y) * width + FRAME_COLUMN * CELL + x) * 4
        to = (y * crop_width + x) * 4
        crop[to : to + 4] = rgba[at : at + 4]
character = encode(crop_width, crop_height, crop)
(ASSETS / "character.png").write_bytes(character)

opaque = sum(1 for i in range(crop_width * crop_height) if crop[i * 4 + 3])
print(f"tiles.png     {width}x{height}, {len(source)} bytes (copied verbatim)")
print(f"character.png {crop_width}x{crop_height}, {len(character)} bytes")
print(f"              cells ({FRAME_COLUMN},{FRAME_ROW}) and ({FRAME_COLUMN + 1},{FRAME_ROW})")
print(f"              {opaque} opaque texels of {crop_width * crop_height}")
