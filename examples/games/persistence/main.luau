-- The game owns this schema and decides when to read and write it.
local ticks = 0

return {
    init = function(ctx)
        local exists = false
        for _, entry in ctx.fs.list("data", "") do
            if entry.name == "progress.tot" then exists = true end
        end
        if exists then
            local saved = ctx.data.parse(ctx.fs.read("data", "progress.tot"))
            assert(saved.version.text == "1", "unsupported progress version")
            ticks = ctx.data.number(saved.ticks)
            assert(ticks >= 0 and ticks % 1 == 0, "invalid saved tick count")
        end
        ctx.log(string.format("Loaded %d ticks", ticks))
    end,
    update = function(ctx, dt)
        ticks += 1
        local saved = {
            version = ctx.data.integer("1"),
            ticks = ctx.data.integer(string.format("%.0f", ticks)),
            seconds = ticks * dt,
            items = ctx.data.array({}),
        }
        -- Persist during play; shutdown may be skipped on faults or termination.
        ctx.fs.write("progress.tot", ctx.data.format(saved))
        ctx.log(string.format("Stored %d ticks", ticks))
    end,
}
