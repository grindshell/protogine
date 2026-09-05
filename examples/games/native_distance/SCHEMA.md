# Grid distance example

Plugin `org.protogine.grid`, function `grid.distance`, schema
`protogine.grid_distance` version 1. All integers use little-endian bytes;
the C implementation reads/writes bytes without packed casts.

Input: u32 width, u32 height, u32 source, then width*height tile bytes.
Dimensions are 1..256, source is a zero-based row-major index, and each tile
is 0 (walkable) or 1 (blocked). The source must be walkable. Reject trailing or
missing bytes. Output capacity must be at least 4*width*height bytes.

Output: one u32 per cell in row-major order, giving shortest four-neighbor
distance from source. Blocked/unreachable cells are `0xffffffff`; source is 0.
No diagonal moves or row wrapping. Success writes exactly 4*width*height bytes.
Invalid inputs return INVALID_ARGUMENT; insufficient capacity returns
BUFFER_TOO_SMALL. Neither publishes output; calls never retry automatically.

`distance.luau` supplies `native(ctx, width, height, source, tiles)` and
`pure(width, height, source, tiles)`. Both accept tile arrays and return distance
arrays using -1 for blocked/unreachable. The native wrapper validates the schema,
packs input, invokes once, and decodes results. Scripts can retain these arrays;
the callback-scoped native function itself expires after return.
