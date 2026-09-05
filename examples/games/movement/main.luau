-- Position is in world pixels; the kernel integrates velocity after update.
local player

return {
    init = function(ctx)
        player = ctx.world.spawn(16, 16)
    end,
    update = function(ctx, dt)
        local input = ctx.input
        local x = (if input.held('right') then 1 else 0) - (if input.held('left') then 1 else 0)
        local y = (if input.held('down') then 1 else 0) - (if input.held('up') then 1 else 0)
        ctx.world.set_velocity(player, x * 60, y * 60)
    end,
}
