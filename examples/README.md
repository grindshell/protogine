# Example assets

[`kenney_1-bit-pack_transparent-packed.png`](kenney_1-bit-pack_transparent-packed.png)
is from the [1-Bit Pack](https://kenney.nl/assets/1-bit-pack) by
[Kenney](https://kenney.nl), licensed under CC0. It is 784x352 with 16x16 tiles
in 49 columns and 22 rows, and holds exactly two colors: fully transparent and
`(249, 250, 251)`. The artwork is monochrome, so a sprite tint is what gives a
tile its stone, foliage or timber color.

The [sprite sample](games/sprites/main.luau) carries its own copies, because
image paths resolve against the bundle root and copying a game must copy its
PNGs as well as its Luau source:

| File | Provenance |
| --- | --- |
| [`games/sprites/assets/tiles.png`](games/sprites/assets/tiles.png) | A byte-for-byte copy of the sheet above |
| [`games/sprites/assets/character.png`](games/sprites/assets/character.png) | Sheet cells (27,1) and (28,1), the 32x16 region at (432,16), re-encoded as 8-bit RGBA |

Both are produced by [`tools/sample_sprites.py`](../tools/sample_sprites.py),
which reads the sheet with the Python standard library alone:

```text
python tools/sample_sprites.py
```

Regeneration rewrites both committed files identically. The tileset remains
1-bit indexed and the character sheet 8-bit RGBA, exercising both PNG color types.

To use different artwork, replace either PNG in a copied bundle and relaunch the
same Player executable; no Rust rebuild is involved. The tileset needs only the
cells the room's legend names, so any image at least 144x96 works, and the
character sheet needs two 16x16 frames side by side. A replacement that cannot
be decoded shows as a failed bar and leaves the rest of the game running; a
missing file refuses the request outright and stops the session.
