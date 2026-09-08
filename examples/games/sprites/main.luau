-- The sprite sample: a tiled room and a two-frame character, both drawn from
-- PNGs this bundle carries. Copy this directory beside the Player as game/,
-- including its assets/ subdirectory, and run the Player. Copying the .luau
-- files alone stops the game: a request for a missing file refuses at once,
-- and this one does not catch it. A file that resolves but cannot decode is
-- different, and is reported through `status` below.
--
-- Arrows move; Backspace returns to the spawn; Space unloads both images so the
-- next tick requests them again and the loading state runs live. Pushing into a
-- crate breaks it, which is the one thing here that changes the room.
--
-- The room's tileset and the character's frames are separate logical images, so
-- each becomes drawable on its own. Nothing here waits for both.
--
-- **The engine owns the grid and the movement.** `room.luau` is still the
-- authored source, but init converts it once into a numeric description and
-- installs it, the character is an entity carrying a tile collider, and the
-- fixed pass resolves every move: there is no solver in this file. Draw reads
-- the committed position and the authoritative tile IDs back out, so the cell
-- this game breaks during update is the same cell the next frame draws and the
-- next tick collides against. Nothing here keeps a second copy of the grid.
local room = require("./room")

local CELL = 16 -- Source cell size in both PNGs.
local SCALE = 2 -- Integer scale, so nearest sampling lands on exact texels.
local SIZE = CELL * SCALE -- Screen pixels per tile and per character frame.
-- Pixels per second, not per tick: the engine integrates at a fixed 60 Hz, and
-- 120 * (1/60) is exactly 2.0 in binary floating point. That exactness is what
-- keeps the positions below whole numbers at every presentation frame rate.
local SPEED = 120
local FRAME_TICKS = 8 -- Ticks per animation frame.
local FRAMES = 2
local BAND = 56 -- Height of the status strip below the room.
local WIDTH, BAND_TOP = room.columns * SIZE, room.rows * SIZE
-- Two equal bars in that strip, with one margin between them and at each end.
local BAR_MARGIN = 24
local BAR_WIDTH = (WIDTH - BAR_MARGIN * 3) // 2
local BAR_HEIGHT = 16
local SPAWN_X, SPAWN_Y = room.spawn.column * SIZE, room.spawn.row * SIZE

-- One entry per shipped PNG. `encoded` is the committed file size and only
-- scales the read segment of the progress bar; a replaced PNG of a different
-- size still loads, its bar just fills at a different rate.
--
-- One job runs at a time, in request order, so asking for the small character
-- first lets it draw within a few ticks while the much larger tileset is still
-- decoding. Nothing here waits for both.
local sheets = {
    {path = "assets/character.png", encoded = 170},
    {path = "assets/tiles.png", encoded = 17497},
}
local hero, tiles = sheets[1], sheets[2]

-- The authored legend, turned into the engine's numeric tile IDs. `palette[id]`
-- is the art the ID stands for: the sheet cell, the tint, and the solidity the
-- installed description was built from. It is immutable lookup data, not a
-- second grid - every cell ID drawn below comes back from the engine, never
-- from here - so breaking a crate changes what is drawn without this file
-- tracking the change at all.
--
-- IDs are the legend's keys in byte order, which makes them the same on every
-- run and every machine: '#' is 1, '.' is 2, 'T' is 3, 'c' is 4 and 'f' is 5.
-- Byte order rather than the default string comparison, which some Lua
-- implementations resolve through the C locale and which could put 'c' before
-- 'T'. The engine's ID 0 means an empty cell and this room never uses it.
local palette = {}
local FLOOR, CRATE

local body
-- The handle the room is installed under. Every map read and edit names it:
-- there is no implicit "the map" any more, which is what lets a game hold more
-- than one at a time.
local map
local facing, steps, ticks = 1, 0, 0
-- Where the body was, and which way this game pushed it, at the previous
-- update. Comparing that against the position the fixed pass committed is how
-- the crate rule below learns that a wall stopped the character.
local last_x, last_y, last_dx, last_dy = SPAWN_X, SPAWN_Y, 0, 0

-- Progress for one bar. The last tenth is GPU residency, which is tracked
-- separately from CPU readiness: a headless host never reports it, so treat
-- "unavailable" as nothing left to wait for.
local function progress(sheet)
    local status = sheet.status
    if status == nil then
        return 0
    end
    if status.state == "failed" or status.state == "unloaded" then
        return 1
    end
    if sheet.ready then
        return if status.gpu == "pending" then 0.9 else 1
    end
    local stage = status.stage
    if stage == "read" then
        return 0.1 + 0.4 * math.min(status.bytes_read / sheet.encoded, 1)
    elseif stage == "header" or stage == "allocate" then
        return 0.55
    elseif stage == "decode" then
        return 0.7
    elseif stage == "complete" then
        return 0.85
    end
    return 0.05
end

local function bar(ctx, index, sheet)
    local left = BAR_MARGIN + (index - 1) * (BAR_WIDTH + BAR_MARGIN)
    local top = BAND_TOP + (BAND - BAR_HEIGHT) // 2
    ctx.draw.rect(left, top, BAR_WIDTH, BAR_HEIGHT, 0.14, 0.16, 0.21, 1)
    local r, g, b = 0.95, 0.72, 0.25
    if sheet.status ~= nil and sheet.status.state == "failed" then
        r, g, b = 0.88, 0.32, 0.32
    elseif sheet.ready then
        r, g, b = 0.30, 0.78, 0.55
    end
    ctx.draw.rect(left, top, BAR_WIDTH * progress(sheet), BAR_HEIGHT, r, g, b, 1)
end

-- The cell one whole tile ahead of the character's centre. A tile away from any
-- face, so the sub-pixel gap the engine leaves when it clamps a body against a
-- wall cannot select the neighbouring cell instead.
local function ahead(px, py, dx, dy)
    return (px + SIZE / 2 + dx * SIZE) // SIZE, (py + SIZE / 2 + dy * SIZE) // SIZE
end

return {
    init = function(ctx)
        for row, line in ipairs(room.tiles) do
            assert(#line == room.columns, `room row {row} is not {room.columns} tiles wide`)
            for column = 1, room.columns do
                local key = string.sub(line, column, column)
                assert(room.legend[key] ~= nil, `room row {row} uses unknown tile '{key}'`)
            end
        end
        assert(#room.tiles == room.rows, "room height does not match its rows")

        local keys = {}
        for key in pairs(room.legend) do
            table.insert(keys, key)
        end
        table.sort(keys, function(a, b)
            return string.byte(a) < string.byte(b)
        end)
        local ids, solids = {}, {}
        for id, key in ipairs(keys) do
            ids[key], palette[id] = id, room.legend[key]
            -- `solids` is dense and one-based: element i defines tile ID i.
            solids[id] = room.legend[key].solid
        end
        FLOOR, CRATE = ids["."], ids["c"]

        -- Row-major, one ID per cell, in the engine's zero-based tile
        -- coordinates. Luau arrays are one-based, hence the trailing + 1.
        local cells = {}
        for row = 0, room.rows - 1 do
            local line = room.tiles[row + 1]
            for column = 0, room.columns - 1 do
                cells[row * room.columns + column + 1] = ids[string.sub(line, column + 1, column + 1)]
            end
        end
        map = ctx.world.create_tilemap({
            columns = room.columns,
            rows = room.rows,
            tile_width = SIZE,
            tile_height = SIZE,
            origin_x = 0,
            origin_y = 0,
            solids = solids,
            cells = cells,
        })

        assert(
            not palette[cells[room.spawn.row * room.columns + room.spawn.column + 1]].solid,
            "the spawn tile is solid"
        )
        body = ctx.world.spawn(SPAWN_X, SPAWN_Y)
        -- One tile, anchored at the sprite's own top left, so the drawn frame
        -- and the collision box are the same rectangle.
        ctx.world.set_tile_collider(body, {map = map, offset_x = 0, offset_y = 0, width = SIZE, height = SIZE})
        ctx.log(`room {room.columns}x{room.rows} tiles at {SIZE} pixels`)
    end,

    update = function(ctx)
        local input, world = ctx.input, ctx.world
        -- The fixed pass runs after this callback returns, so this is where the
        -- body finished the previous tick, not where it will finish this one.
        -- Draw is a tick ahead of it, because it reads after the pass has run.
        local p = world.position(body)
        ticks += 1

        for _, sheet in ipairs(sheets) do
            if sheet.handle == nil then
                -- Requesting during update, not init: the call returns at once
                -- with a pending handle and this callback keeps running while
                -- the staged job reads, decodes and uploads. The same branch
                -- picks the images up again after Space unloads them.
                sheet.handle = ctx.assets.request_png(sheet.path)
                ctx.log(`requested {sheet.path} at tick {ticks}`)
            end
            if not sheet.settled then
                local status = ctx.assets.status(sheet.handle)
                sheet.status = status
                if status.state == "ready" and not sheet.ready then
                    -- Dimensions are frozen once an image is ready and the
                    -- asset call budget is per callback, so read them once here
                    -- rather than querying inside the draw loop below.
                    sheet.size = ctx.assets.size(sheet.handle)
                    sheet.ready = true
                    ctx.log(`ready {sheet.path} {sheet.size.width}x{sheet.size.height} at tick {ticks}`)
                end
                -- Stop polling once there is nothing left to learn: an upload
                -- that finished, a job that failed, or CPU readiness on a
                -- headless host, where `gpu` stays "unavailable" forever.
                sheet.settled = status.state == "failed"
                    or (sheet.ready and status.gpu ~= "pending")
            end
        end

        if input.pressed("action") then
            for _, sheet in ipairs(sheets) do
                if sheet.handle ~= nil then
                    ctx.assets.unload(sheet.handle)
                end
                sheet.handle, sheet.status, sheet.size = nil, nil, nil
                sheet.ready, sheet.settled = false, false
            end
            ctx.log(`unloaded both images at tick {ticks}`)
        end
        if input.pressed("cancel") then
            -- A teleport the engine may refuse, unlike the assignment this used
            -- to be. It cannot refuse here, in three steps rather than one: the
            -- engine checks every cell the box covers, and this box covers
            -- exactly the spawn cell because the spawn is tile-aligned and the
            -- collider is one tile at zero offset; init proves that cell is not
            -- solid; and the only edit below replaces a solid ID with the
            -- floor, so no cell this game touches can ever become solid. Move
            -- the spawn off the tile grid and the middle step stops being
            -- enough. A game that can make its own spawn solid, or that spawns
            -- straddling cells, has to handle the refusal instead.
            world.set_position(body, SPAWN_X, SPAWN_Y)
            p = {x = SPAWN_X, y = SPAWN_Y}
            facing, steps = 1, 0
        end

        local dx = (if input.held("right") then 1 else 0) - (if input.held("left") then 1 else 0)
        local dy = (if input.held("down") then 1 else 0) - (if input.held("up") then 1 else 0)

        -- The body was pushed along an axis and did not move, so a solid tile
        -- stopped it. Both coordinates come out of the same solver, which
        -- returns the identical clamped position for every tick a body presses
        -- the same face, so this compares exactly rather than within a
        -- tolerance. X wins a tie, matching the order the sweep resolves in.
        local sx, sy = 0, 0
        if last_dx ~= 0 and p.x == last_x then
            sx = last_dx
        elseif last_dy ~= 0 and p.y == last_y then
            sy = last_dy
        end
        if sx ~= 0 or sy ~= 0 then
            local column, row = ahead(p.x, p.y, sx, sy)
            -- The room's border is solid, so the character can never stand in
            -- it and this cell is always inside the grid; the bounds test is
            -- here because `tile` refuses an outside index rather than
            -- answering, and a sample should not rely on an argument it has not
            -- checked.
            if column >= 0 and column < room.columns and row >= 0 and row < room.rows then
                if world.tile(map, column, row) == CRATE then
                    -- Fallible and unhandled, for the same kind of reason as
                    -- the teleport above: an edit is refused if it would trap a
                    -- body, and this one can only turn a solid cell into the
                    -- floor. The coordinates were bounds-checked here and FLOOR
                    -- is an ID the installed description defines.
                    world.set_tile(map, column, row, FLOOR)
                    ctx.log(`broke the crate at {column},{row} on tick {ticks}`)
                end
            end
        end

        if dx ~= 0 then
            facing = dx
        end
        -- Velocity, not a position: the fixed pass sweeps it against the map
        -- after this callback returns and clamps it at the first solid face.
        -- Holding a direction into a wall keeps pressing, which is what the
        -- animation below and the crate rule above both read.
        world.set_velocity(body, dx * SPEED, dy * SPEED)
        -- Animation advances here, in update, so which frame a draw shows
        -- depends on completed ticks and not on the presentation frame rate.
        steps = if dx ~= 0 or dy ~= 0 then steps + 1 else 0
        last_x, last_y, last_dx, last_dy = p.x, p.y, dx, dy
    end,

    draw = function(ctx)
        ctx.draw.clear(0.04, 0.05, 0.08, 1)
        -- The authoritative grid, read back once per frame. This room is 510
        -- cells, comfortably inside the 4,096 a single region call may return;
        -- a larger map would have to ask for the visible chunk instead.
        local cells = ctx.world.tiles_region(map, 0, 0, room.columns, room.rows)
        if tiles.ready then
            for row = 0, room.rows - 1 do
                for column = 0, room.columns - 1 do
                    local tile = palette[cells[row * room.columns + column + 1]]
                    local tint = tile.tint
                    ctx.draw.sprite(tiles.handle, column * SIZE, row * SIZE, {
                        source = {
                            x = tile.cell[1] * CELL,
                            y = tile.cell[2] * CELL,
                            width = CELL,
                            height = CELL,
                        },
                        width = SIZE,
                        height = SIZE,
                        tint = {r = tint[1], g = tint[2], b = tint[3], a = 1},
                    })
                end
            end
        else
            -- The room keeps its shape before the sheet arrives, so the game is
            -- playable and the walls are where they will be. Solidity comes
            -- from the palette rather than one `tile_solid` call per cell: the
            -- IDs above are already the engine's answer.
            ctx.draw.rect(0, 0, WIDTH, BAND_TOP, 0.10, 0.12, 0.16, 1)
            for row = 0, room.rows - 1 do
                for column = 0, room.columns - 1 do
                    if palette[cells[row * room.columns + column + 1]].solid then
                        ctx.draw.rect(column * SIZE, row * SIZE, SIZE, SIZE, 0.20, 0.22, 0.28, 1)
                    end
                end
            end
        end

        local p = ctx.world.position(body)
        if hero.ready then
            ctx.draw.sprite(hero.handle, p.x, p.y, {
                source = {x = (steps // FRAME_TICKS) % FRAMES * CELL, y = 0, width = CELL, height = CELL},
                width = SIZE,
                height = SIZE,
                flip_x = facing < 0,
                tint = {r = 0.98, g = 0.86, b = 0.42, a = 1},
            })
        else
            ctx.draw.rect(p.x, p.y, SIZE, SIZE, 0.98, 0.86, 0.42, 1)
        end

        ctx.draw.rect(0, BAND_TOP, WIDTH, BAND, 0.07, 0.08, 0.11, 1)
        for index, sheet in ipairs(sheets) do
            bar(ctx, index, sheet)
        end
    end,
}
