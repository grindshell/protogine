-- A complete distance field from one native batch call per fixed update.
local distance = require('./distance')
local width, height = 16, 12
local tiles = table.create(width * height, 0)
for y = 0, height - 1 do
    if y ~= 6 then tiles[y * width + 8] = 1 end
end
local result = {}
return {
    init = function(ctx)
        result = distance.native(ctx, width, height, 0, tiles)
        local expected = distance.pure(width, height, 0, tiles)
        for i, value in result do assert(value == expected[i]) end
        ctx.log('distance field verified')
    end,
    update = function(ctx)
        result = distance.native(ctx, width, height, 0, tiles)
    end,
    draw = function(ctx)
        ctx.draw.clear(0.04, 0.05, 0.08, 1)
        for i, value in result do
            local x, y = (i - 1) % width, math.floor((i - 1) / width)
            local brightness = if value < 0 then 0.08 else 0.9 - math.min(value, 36) / 50
            ctx.draw.rect(12 + x * 22, 12 + y * 22, 20, 20, 0.1, brightness, brightness * 0.8, 1)
        end
    end,
}
