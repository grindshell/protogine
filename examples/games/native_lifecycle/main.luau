-- This milestone proves native startup/cleanup. Batch calls arrive in Phase 5.
return {
    init = function(ctx)
        ctx.log("Luau initialized after native plugins")
    end,
    draw = function(ctx)
        ctx.draw.clear(0.08, 0.10, 0.14, 1)
        ctx.draw.rect(40, 40, 96, 96, 0.3, 0.8, 0.5, 1)
    end,
    shutdown = function(ctx)
        ctx.log("Luau stopped before native plugins")
    end,
}
