-- The sprite sample: a tiled room and a two-frame character, both drawn from
-- PNGs this bundle carries. Copy this directory beside the Player as game/,
-- including its assets/ subdirectory, and run the Player. Copying the .luau
-- files alone stops the game: a request for a missing file refuses at once,
-- and this one does not catch it. A file that resolves but cannot decode is
-- different, and is reported through `status` below.
--
-- Arrows move; Backspace returns to the spawn; Space unloads both images so the
-- next tick requests them again and the loading state runs live.
--
-- The room's tileset and the character's frames are separate logical images, so
-- each becomes drawable on its own. Nothing here waits for both.
local room = require("./room")

local CELL = 16 -- Source cell size in both PNGs.
local SCALE = 2 -- Integer scale, so nearest sampling lands on exact texels.
local SIZE = CELL * SCALE -- Screen pixels per tile and per character frame.
local SPEED = 2 -- Pixels per tick.
local FRAME_TICKS = 8 -- Ticks per animation frame.
local FRAMES = 2
local BAND = 56 -- Height of the status strip below the room.
local WIDTH, BAND_TOP = room.columns * SIZE, room.rows * SIZE
-- Two equal bars in that strip, with one margin between them and at each end.
local BAR_MARGIN = 24
local BAR_WIDTH = (WIDTH - BAR_MARGIN * 3) // 2
local BAR_HEIGHT = 16

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

local x, y = room.spawn.column * SIZE, room.spawn.row * SIZE
local facing, steps, ticks = 1, 0, 0

local function entry(column, row)
    if column < 0 or row < 0 or column >= room.columns or row >= room.rows then
        return nil
    end
    return room.legend[string.sub(room.tiles[row + 1], column + 1, column + 1)]
end

local function solid(px, py)
    local tile = entry(px // SIZE, py // SIZE)
    return tile == nil or tile.solid
end

-- The character occupies exactly one tile, so its four corners decide the move.
-- Positions stay whole pixels, which is what lets a capture assert the sprite's
-- exact texels.
local function blocked(left, top)
    local right, bottom = left + SIZE - 1, top + SIZE - 1
    return solid(left, top) or solid(right, top) or solid(left, bottom) or solid(right, bottom)
end

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
        assert(not blocked(x, y), "the spawn tile is solid")
        ctx.log(`room {room.columns}x{room.rows} tiles at {SIZE} pixels`)
    end,

    update = function(ctx)
        ticks += 1
        local input = ctx.input

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
            x, y = room.spawn.column * SIZE, room.spawn.row * SIZE
            facing, steps = 1, 0
        end

        local dx = (if input.held("right") then 1 else 0) - (if input.held("left") then 1 else 0)
        local dy = (if input.held("down") then 1 else 0) - (if input.held("up") then 1 else 0)
        if dx ~= 0 then
            facing = dx
            if not blocked(x + dx * SPEED, y) then
                x += dx * SPEED
            end
        end
        if dy ~= 0 and not blocked(x, y + dy * SPEED) then
            y += dy * SPEED
        end
        -- Animation advances here, in update, so which frame a draw shows
        -- depends on completed ticks and not on the presentation frame rate.
        steps = if dx ~= 0 or dy ~= 0 then steps + 1 else 0
    end,

    draw = function(ctx)
        ctx.draw.clear(0.04, 0.05, 0.08, 1)
        if tiles.ready then
            for row = 0, room.rows - 1 do
                local line = room.tiles[row + 1]
                for column = 0, room.columns - 1 do
                    local tile = room.legend[string.sub(line, column + 1, column + 1)]
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
            -- playable and the walls are where they will be.
            ctx.draw.rect(0, 0, WIDTH, BAND_TOP, 0.10, 0.12, 0.16, 1)
            for row = 0, room.rows - 1 do
                for column = 0, room.columns - 1 do
                    if entry(column, row).solid then
                        ctx.draw.rect(column * SIZE, row * SIZE, SIZE, SIZE, 0.20, 0.22, 0.28, 1)
                    end
                end
            end
        end

        if hero.ready then
            ctx.draw.sprite(hero.handle, x, y, {
                source = {x = (steps // FRAME_TICKS) % FRAMES * CELL, y = 0, width = CELL, height = CELL},
                width = SIZE,
                height = SIZE,
                flip_x = facing < 0,
                tint = {r = 0.98, g = 0.86, b = 0.42, a = 1},
            })
        else
            ctx.draw.rect(x, y, SIZE, SIZE, 0.98, 0.86, 0.42, 1)
        end

        ctx.draw.rect(0, BAND_TOP, WIDTH, BAND, 0.07, 0.08, 0.11, 1)
        for index, sheet in ipairs(sheets) do
            bar(ctx, index, sheet)
        end
    end,
}
