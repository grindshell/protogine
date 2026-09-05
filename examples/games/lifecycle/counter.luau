--!native
local counter = { ticks = 0, time = 0 }

function counter.tick(dt: number)
    counter.ticks += 1
    counter.time += dt
end

return counter
