# Example assets

[`kenney_1-bit-pack_transparent-packed.png`](kenney_1-bit-pack_transparent-packed.png)
is from the [1-Bit Pack](https://kenney.nl/assets/1-bit-pack) by
[Kenney](https://kenney.nl), licensed under CC0. It is 784x352 with 16x16 tiles
in 49 columns and 22 rows.

[`games/loading/assets/sheet.png`](games/loading/assets/sheet.png) is a byte-for-byte
copy of that file, so the sample bundle is self-contained: image paths resolve
against the bundle root, and copying a game must copy its PNGs as well as its
Luau source. Replacing it changes the artwork without rebuilding the engine.
