return {
    init = function(ctx)
        ctx.log("ready")
    end,
    update = function(ctx)
        for _, button in {"up", "down", "left", "right", "action", "cancel"} do
            if ctx.input.pressed(button) then
                assert(ctx.input.held(button), "pressed button must be held")
                ctx.log("pressed " .. button)
            end
            if ctx.input.released(button) then
                assert(not ctx.input.held(button), "released button must not be held")
                ctx.log("released " .. button)
            end
        end
    end,
    shutdown = function(ctx)
        ctx.log("shutdown")
    end,
}
