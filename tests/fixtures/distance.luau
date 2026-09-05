local distance = require('./distance')
local function compare(ctx, width, height, source, tiles, known)
    local expected = distance.pure(width, height, source, tiles)
    local actual = distance.native(ctx, width, height, source, tiles)
    assert(#expected == #actual)
    for i = 1, #expected do
        assert(actual[i] == expected[i], `mismatch at {width}x{height}:{i}`)
        if known then assert(actual[i] == known[i]) end
    end
end
return {init=function(ctx)
    compare(ctx,3,3,0,{0,1,0, 0,1,0, 0,0,0},{0,-1,6, 1,-1,5, 2,3,4})
    compare(ctx,2,2,0,{0,1,1,0},{0,-1,-1,-1})
    for _, size in {{1,1},{1,16},{16,1},{3,3},{17,11},{64,64},{256,256}} do
        local w,h=size[1],size[2]
        for pattern=1,3 do
            local tiles=table.create(w*h,0)
            for i=1,#tiles do
                tiles[i] = if pattern == 1 then 0 elseif pattern == 2 then 1 else math.random(0,3)//3
            end
            local source = w*h-1; tiles[source+1]=0
            compare(ctx,w,h,source,tiles)
        end
    end
    local function refuse(input, capacity)
        local output=buffer.create(capacity); buffer.fill(output,0,85)
        assert(not pcall(ctx.native.call,'org.protogine.grid','grid.distance',input,output))
        assert(buffer.tostring(output) == string.rep(string.char(85),capacity))
    end
    local input=buffer.create(16)
    buffer.writeu32(input,0,2); buffer.writeu32(input,4,2)
    refuse(input,15)
    refuse(buffer.create(0),16); refuse(buffer.create(11),16)
    for _, length in {15,17} do
        local invalid=buffer.create(length)
        buffer.writeu32(invalid,0,2); buffer.writeu32(invalid,4,2)
        refuse(invalid,16) -- valid 2x2 header with missing/trailing tile data
    end
    for _, pair in {{0,0},{0,257},{0,0xffffffff},{4,0},{4,257},{8,4}} do
        local invalid=buffer.fromstring(buffer.tostring(input))
        buffer.writeu32(invalid,pair[1],pair[2]); refuse(invalid,16)
    end
    buffer.writeu8(input,12,1); refuse(input,16)
    buffer.writeu8(input,12,0); buffer.writeu8(input,15,2); refuse(input,16)
    assert(not pcall(distance.native, {native={plugins={}}}, 1,1,0,{0}))
    compare(ctx,1,1,0,{0},{0}) -- recoverable refusals leave the registry usable
    ctx.log('distance parity and schema refusals passed')
end}
