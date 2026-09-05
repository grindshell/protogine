-- Copy this directory beside the Player as game/. No Rust rebuild is needed.
-- Arrows steer; release to cruise horizontally; Space pauses; Backspace resets.
local palette = require("./palette")
local player, direction = nil, 1
return {
    init = function(ctx)
        player = ctx.world.spawn(64, 128)
        ctx.world.set_velocity(player, 60, 0)
    end,
    update = function(ctx, dt)
        local input, world = ctx.input, ctx.world
        if input.pressed("cancel") then
            world.set_position(player, 64, 128)
            direction = 1
        end
        local p = world.position(player)
        if p.x >= 608 then direction = -1 end
        if p.x <= 32 then direction = 1 end
        local dx = (if input.held("right") then 1 else 0) - (if input.held("left") then 1 else 0)
        local dy = (if input.held("down") then 1 else 0) - (if input.held("up") then 1 else 0)
        if dx == 0 and dy == 0 then dx = direction end
        if input.held("action") then dx, dy = 0, 0 end
        if (p.y <= 32 and dy < 0) or (p.y >= 352 and dy > 0) then dy = 0 end
        if (p.x <= 32 and dx < 0) or (p.x >= 608 and dx > 0) then dx = 0 end
        world.set_velocity(player, dx * 60, dy * 60)
    end,
    draw = function(ctx, alpha)
        ctx.draw.clear(0.055, 0.07, 0.10, 1)
        for y = 0, 11 do
            for x = 0, 19 do
                local shade = if (x + y) % 2 == 0 then 0.12 else 0.14
                ctx.draw.rect(24 + x * 32, 24 + y * 32, 30, 30, shade, shade + 0.025, shade + 0.05, 1)
            end
        end
        local p = ctx.world.position(player)
        ctx.draw.rect(p.x + 3, p.y + 4, 24, 24, 0, 0, 0, 0.4)
        ctx.draw.rect(p.x, p.y, 24, 24, palette.r, palette.g, palette.b, 1)
        ctx.draw.rect(p.x + 6, p.y + 6, 4, 4, 0.95, 0.98, 1, 1)
        ctx.draw.rect(p.x + 15, p.y + 6, 4, 4, 0.95, 0.98, 1, 1)
    end,
}
