--!native
local counter = require("./counter")

return {
    init = function(ctx)
        ctx.log("Protogine script started")
    end,
    update = function(ctx, dt)
        counter.tick(dt)
        ctx.log(string.format("Tick %d: %.6f seconds", counter.ticks, counter.time))
    end,
    draw = function(ctx, alpha)
    end,
    shutdown = function(ctx)
        ctx.log("Protogine script stopped")
    end,
}
