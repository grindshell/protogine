--!strict
export type Tiles = {number}
export type Distances = {number}

local function validate(width: number, height: number, source: number, tiles: Tiles): number
    assert(width >= 1 and width <= 256 and width % 1 == 0, 'invalid width')
    assert(height >= 1 and height <= 256 and height % 1 == 0, 'invalid height')
    local count = width * height
    assert(source >= 0 and source < count and source % 1 == 0, 'invalid source')
    assert(#tiles == count, 'invalid tile count')
    for i = 1, count do assert(tiles[i] == 0 or tiles[i] == 1, 'invalid tile') end
    assert(tiles[source + 1] == 0, 'blocked source')
    return count
end

local function native(ctx: any, width: number, height: number, source: number, tiles: Tiles): Distances
    local count = validate(width, height, source, tiles)
    local plugin = ctx.native.plugins['org.protogine.grid']
    local info = plugin and plugin['grid.distance']
    assert(info and info.schema == 'protogine.grid_distance' and info.schema_version == 1, 'distance schema mismatch')
    local input, output = buffer.create(12 + count), buffer.create(4 * count)
    buffer.writeu32(input, 0, width); buffer.writeu32(input, 4, height); buffer.writeu32(input, 8, source)
    for i = 1, count do buffer.writeu8(input, 11 + i, tiles[i]) end
    assert(ctx.native.call('org.protogine.grid', 'grid.distance', input, output) == 4 * count, 'invalid distance output size')
    local result: Distances = table.create(count, -1)
    for i = 1, count do
        local value = buffer.readu32(output, 4 * (i - 1))
        result[i] = if value == 0xffffffff then -1 else value
    end
    return result
end

local function pure(width: number, height: number, source: number, tiles: Tiles): Distances
    local count = validate(width, height, source, tiles)
    local result: Distances = table.create(count, -1)
    local queue: {number} = table.create(count, 0)
    local head, tail = 1, 1
    source += 1; result[source] = 0; queue[1] = source
    local function visit(at: number, distance: number)
        if tiles[at] == 0 and result[at] == -1 then
            result[at] = distance; tail += 1; queue[tail] = at
        end
    end
    while head <= tail do
        local at = queue[head]; head += 1
        local x, nextDistance = (at - 1) % width, result[at] + 1
        if x > 0 then visit(at - 1, nextDistance) end
        if x + 1 < width then visit(at + 1, nextDistance) end
        if at > width then visit(at - width, nextDistance) end
        if at + width <= count then visit(at + width, nextDistance) end
    end
    return result
end

return {native = native, pure = pure}
