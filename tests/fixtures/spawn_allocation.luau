-- Fill the VM heap, then keep each successfully returned handle alive until a
-- spawn fails. Prebuilt diagnostics can still be logged at the allocation cap.
local fillers = table.create(10000)
local handles = table.create(1024)
local counts = table.create(1024)
for i = 1,1024 do counts[i] = tostring(i) end

return {
    init = function(ctx)
        local i = 1
        for _, size in {65536, 4096} do
            while true do
                local ok, value = pcall(buffer.create, size)
                if not ok then break end
                fillers[i] = value
                i += 1
            end
        end
        local successes = 0
        for j = 1,1024 do
            local ok, result = pcall(ctx.world.spawn, 0, 0)
            if not ok then
                ctx.log('spawn failed')
                ctx.log(tostring(result))
                break
            end
            handles[j] = result
            successes += 1
        end
        fillers = nil
        ctx.log(if successes > 0 then counts[successes] else '0')
    end,
}
