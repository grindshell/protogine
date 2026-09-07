-- The room: one small tile array and the sheet cells its characters name.
--
-- `cell` is a column and row in assets/tiles.png, not a pixel offset; the game
-- turns it into a source rectangle. `tint` multiplies the sheet's white 1-bit
-- artwork, which is how a single monochrome sheet supplies stone, foliage and
-- timber. `solid` blocks movement.
--
-- Every row must be `columns` characters wide and every character must appear
-- in the legend; the game asserts both during init rather than drawing a hole.
return {
    columns = 30,
    rows = 17,
    -- Tile coordinates, converted to pixels by the game's scale.
    spawn = {column = 14, row = 8},
    legend = {
        ["."] = {cell = {2, 0}, tint = {0.17, 0.20, 0.27}, solid = false},
        ["#"] = {cell = {8, 5}, tint = {0.40, 0.43, 0.50}, solid = true},
        ["T"] = {cell = {2, 1}, tint = {0.24, 0.60, 0.38}, solid = true},
        ["c"] = {cell = {6, 1}, tint = {0.34, 0.66, 0.40}, solid = true},
        ["f"] = {cell = {2, 3}, tint = {0.56, 0.44, 0.28}, solid = true},
    },
    tiles = {
        "##############################",
        "#............................#",
        "#..T.....############........#",
        "#.T.T....#..........#...T.T..#",
        "#..T.....#...####...#....T...#",
        "#........#...####...#........#",
        "#....c...#...####...#....c...#",
        "#............####............#",
        "#..fffff.............fffff...#",
        "#........#..........#........#",
        "#....c...#..........#....c...#",
        "#........#..........#........#",
        "#..T.T...#..........#...T....#",
        "#...T....#..........#..T.T...#",
        "#........############........#",
        "#............................#",
        "##############################",
    },
}
