-- The Phase 2 asset driver: request a PNG during update, keep playing while it
-- loads, then draw it. Copy this directory beside the Player as game/.
-- Arrows move the marker; Backspace resets it; Space evicts and reloads.
--
-- The sprite commands this publishes are owned data. Drawing them needs the
-- shared renderer from Phase 3, so the Player still shows only the rectangles.
local sheet, size = nil, nil
local x, y, ticks = 160, 120, 0

-- Dimensions are frozen once an image is ready, and the 256-call asset budget
-- is per callback, so read them once instead of querying inside a draw loop.
local function ready(ctx)
    if not sheet then return false end
    local status = ctx.assets.status(sheet)
    if status.state ~= "ready" then return false end
    if not size then
        size = ctx.assets.size(sheet)
        ctx.log(`ready at tick {ticks}: {size.width}x{size.height}`)
    end
    return true
end

return {
    update = function(ctx)
        ticks += 1
        local input = ctx.input
        -- Requesting during update, not init: the callback returns immediately
        -- with a pending handle and the game keeps running.
        if not sheet and ticks >= 2 then
            sheet = ctx.assets.request_png("assets/sheet.png")
            ctx.log("requested at tick " .. ticks)
        end
        if sheet and input.pressed("action") then
            ctx.assets.unload(sheet)
            sheet, size = nil, nil
            ctx.log("evicted at tick " .. ticks)
        end
        if input.pressed("cancel") then x, y = 160, 120 end
        if input.held("left") then x -= 2 end
        if input.held("right") then x += 2 end
        if input.held("up") then y -= 2 end
        if input.held("down") then y += 2 end
    end,
    draw = function(ctx)
        ctx.draw.clear(0.055, 0.07, 0.10, 1)
        if ready(ctx) then
            -- One 16x16 tile of the sheet, doubled, plus the whole sheet
            -- scaled into the window behind it.
            ctx.draw.sprite(sheet, 8, 8, {width = 320, height = 144, tint = {r = 1, g = 1, b = 1, a = 0.35}})
            ctx.draw.sprite(sheet, x, y, {
                source = {x = 16 * ((ticks // 30) % 2), y = 0, width = 16, height = 16},
                width = 32,
                height = 32,
            })
        else
            -- A responsive loading state while the staged job runs.
            local status = if sheet then ctx.assets.status(sheet) else nil
            local progress = if status then math.min(status.bytes_read / 17497, 1) else 0
            ctx.draw.rect(8, 8, 304, 12, 0.15, 0.17, 0.2, 1)
            ctx.draw.rect(8, 8, 304 * progress, 12, 0.3, 0.75, 0.6, 1)
            ctx.draw.rect(x, y, 32, 32, 0.85, 0.85, 0.9, 1)
        end
    end,
}
