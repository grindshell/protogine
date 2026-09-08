#![cfg(feature = "scripting")]

//! The `ctx.world` map and collider bindings, through the real headless runtime.
//!
//! Everything here drives `GameRuntime` rather than calling the kernel, because
//! what Phase 3 adds is a boundary: schema validation, copying, phase gating,
//! the shared attempt budget and the aggregate ceilings. The kernel-side rules
//! those calls reach are already pinned by `tests/tilemap.rs` and
//! `tests/collision.rs`; what these fixtures prove is that a script cannot get
//! past them, cannot retain anything the engine owns, and cannot spend an
//! unbounded amount of engine work to find that out.

use protogine::{
    collision::MAX_CALLBACK_WORK,
    input::InputSnapshot,
    kernel::{FIXED_DT, Position},
    maps::{MAX_AGGREGATE_CELLS, MAX_TILEMAPS},
    runtime::GameRuntime,
    scripting::{ScriptHost, ScriptLimits, ScriptState},
};
use std::{fs, time::Duration};
use tempfile::TempDir;

/// A 6x4 room of 32-pixel tiles with a solid border and a solid column 4, so
/// the free interior is columns 1..3 of rows 1..2 and a 32x32 body at (32, 32)
/// travelling right stops with its leading edge on the face at x = 128.
const ROOM: &str = r#"
-- Module-scope so every fixture below has somewhere to keep the handle its
-- `create_tilemap` returns. M2-6 gives every read and edit an explicit map, so
-- a fixture that installs one and then reads it needs to hold the name.
local map
local function room()
    local cells = {}
    for row = 0, 3 do
        for column = 0, 5 do
            local solid = row == 0 or row == 3 or column == 0 or column >= 4
            cells[row * 6 + column + 1] = (solid and 1 or 0)
        end
    end
    return {
        columns = 6, rows = 4, tile_width = 32, tile_height = 32,
        origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
    }
end
-- The spawn and speed two fixtures share, named here because they depend on
-- each other's numbers. `owned_reads_are_snapshots_and_input_tables_are_never_retained`
-- asserts a body pinned at the spawn by a cell it just made solid, and that
-- figure only means anything because
-- `a_script_installs_a_map_reads_it_back_and_stops_against_its_wall` pins the
-- unedited run against the room's own wall from the same spawn at the same
-- changing either moves both fixtures together and neither can drift into
-- passing while its number stops meaning what its comment says.
local SPAWN_X, SPAWN_Y, SPEED = 32, 32, 120
-- A placement rather than an options table: the map is a field of it now, and
-- omitting it is a refusal rather than an attachment to whatever was installed.
local function box(on, size)
    return {map = on, offset_x = 0, offset_y = 0, width = size, height = size}
end
"#;

fn game(source: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), source).unwrap();
    root
}

/// A bundle whose `main.luau` may call `room()` and `box(size)`.
fn room_game(source: &str) -> TempDir {
    game(&(ROOM.to_string() + source))
}

fn load(root: &TempDir) -> GameRuntime {
    GameRuntime::load(root.path(), ScriptLimits::default()).unwrap()
}

/// A host with room to copy a million description elements inside one callback.
/// The budget fixtures below are about the tile-work ceiling, not the clock.
fn load_patient(root: &TempDir) -> GameRuntime {
    GameRuntime::load(
        root.path(),
        ScriptLimits {
            startup_timeout: Duration::from_secs(120),
            callback_timeout: Duration::from_secs(120),
            ..ScriptLimits::default()
        },
    )
    .unwrap()
}

/// The prelude room's geometry as the Rust assertions read it.
///
/// The Luau side takes the spawn and the speed from `ROOM`'s own constants;
/// these are the same facts on this side of the string boundary, named for the
/// same reason. `WALL_X` in particular was a bare `96.0` in four assertions and
/// the number "96" in five comments across both syntaxes - the shape that put a
/// wrong ordinal past a correction earlier in this file.
const TILE: u32 = 32;
/// The room's first solid interior column, from `ROOM`'s `column >= 4`.
const FIRST_SOLID_COLUMN: u32 = 4;
/// Where a tile-wide body's left edge comes to rest against that column's face:
/// its right edge meets `FIRST_SOLID_COLUMN * TILE`, so its left edge sits one
/// tile back.
const WALL_X: f64 = ((FIRST_SOLID_COLUMN - 1) * TILE) as f64;
const _: () = assert!((FIRST_SOLID_COLUMN - 1) * TILE == 96);
/// Matching `SPAWN_X, SPAWN_Y` in the prelude.
const SPAWN: Position = Position { x: 32.0, y: 32.0 };

fn positions(runtime: &GameRuntime) -> Vec<Position> {
    runtime
        .kernel()
        .snapshot()
        .unwrap()
        .into_iter()
        .map(|entity| entity.position)
        .collect()
}

fn step(runtime: &mut GameRuntime, ticks: u32) {
    for _ in 0..ticks {
        runtime.step(InputSnapshot::default()).unwrap();
    }
}

#[test]
fn a_script_installs_a_map_reads_it_back_and_stops_against_its_wall() {
    let root = room_game(
        r#"
        local body
        return {
            init = function(ctx)
                local w = ctx.world
                -- Nothing to read yet, and nothing to read it *with*: every map
                -- call names a map, so an empty session has no argument to
                -- offer them rather than a map-shaped absence to report.
                assert(not pcall(w.tilemap_info), 'info needs a map handle')
                assert(not pcall(w.tile, nil, 0, 0), 'a read needs a map handle')
                assert(not pcall(w.set_tile, nil, 0, 0, 1))
                map = w.create_tilemap(room())
                local info = w.tilemap_info(map)
                assert(info.columns == 6 and info.rows == 4)
                assert(info.tile_width == 32 and info.tile_height == 32)
                assert(info.origin_x == 0 and info.origin_y == 0)
                assert(w.tile(map, 0, 0) == 1 and w.tile(map, 1, 1) == 0)
                assert(w.tile_solid(map, 4, 1) and not w.tile_solid(map, 1, 1))
                -- Outside the installed map is solid (T3), and this is the one
                -- call that accepts indices the grid does not contain.
                assert(w.tile_solid(map, -1, 1) and w.tile_solid(map, 6, 1))
                assert(not pcall(w.tile, map, -1, 1), 'tile() requires in-bounds indices')
                local ids = w.tiles_region(map, 0, 1, 6, 1)
                assert(#ids == 6, 'a region returns one ID per cell')
                assert(ids[1] == 1 and ids[2] == 0 and ids[4] == 0 and ids[5] == 1)
                body = w.spawn(SPAWN_X, SPAWN_Y)
                assert(w.tile_collider(body) == nil, 'a fresh entity has no collider')
                w.set_tile_collider(body, box(map, 32))
                local collider = w.tile_collider(body)
                assert(collider.offset_x == 0 and collider.offset_y == 0)
                assert(collider.width == 32 and collider.height == 32)
                w.set_velocity(body, SPEED, 0)
            end,
            draw = function(ctx)
                -- Draw reads the committed position and the authoritative map.
                assert(ctx.world.tilemap_info(map).columns == 6)
                assert(ctx.world.position(body).x <= 96)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(positions(&runtime), [SPAWN]);
    assert_eq!(runtime.kernel().live_colliders(), 1);
    // What init charged, exactly: one solid flag and 24 cells copied and
    // validated, plus the one cell the attached box covers. Reads charge no
    // tile work at all. Pinned rather than reported, because the ceiling below
    // is only meaningful if the unit price is.
    assert_eq!(
        runtime.kernel().callback_work(),
        26,
        "a description costs one unit per element and a placement one per covered cell"
    );

    for _ in 0..40 {
        runtime.step(InputSnapshot::default()).unwrap();
        runtime.draw(0.0).unwrap();
    }
    // 120 px/s is exactly 2 px per tick, so 32 ticks reach the wall and the
    // eight after it press against a face that does not move.
    assert_eq!(
        positions(&runtime),
        [Position {
            x: WALL_X,
            y: SPAWN.y
        }]
    );
    assert_eq!(runtime.completed_ticks(), 40);
    assert_eq!(runtime.state(), ScriptState::Running);
}

#[test]
fn one_tick_of_enormous_velocity_stops_at_the_first_face_it_crosses() {
    // `tests/collision.rs` pins this against the kernel; what this adds is that
    // the script path cannot get round it, because a game sets velocity and
    // never a position. One tick at this speed travels 16,666 pixels across a
    // 192-pixel map, so an endpoint-only solver would land far outside the grid
    // rather than one tile in, and a per-pixel one would not terminate usefully.
    let root = room_game(
        r#"
        local body
        return {
            init = function(ctx)
                local w = ctx.world
                map = w.create_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(map, 32))
                w.set_velocity(body, 1000000, 1000000)
            end,
            update = function(ctx)
                -- Velocity is the requested value even when nothing moved, so
                -- holding a direction keeps pressing the wall.
                local v = ctx.world.velocity(body)
                assert(v.x == 1000000 and v.y == 1000000, 'a blocked body keeps its velocity')
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    step(&mut runtime, 1);
    assert_eq!(
        positions(&runtime),
        [Position { x: WALL_X, y: 64.0 }],
        "one tick must stop at the first solid face on each axis, not at its endpoint"
    );
    // Five more ticks of the same speed: a body already flush with a face is
    // never pushed through it and never nudged back off it either.
    step(&mut runtime, 5);
    assert_eq!(positions(&runtime), [Position { x: WALL_X, y: 64.0 }]);
    assert_eq!(runtime.state(), ScriptState::Running);
}

#[test]
fn owned_reads_are_snapshots_and_input_tables_are_never_retained() {
    let root = room_game(
        r#"
        local body, kept
        return {
            init = function(ctx)
                local w = ctx.world
                local description = room()
                map = w.create_tilemap(description)
                -- The description was copied, not adopted: nothing a caller
                -- does to it afterwards can reach the installed map.
                description.columns = 99
                description.cells[8] = 1
                description.solids[1] = false
                assert(w.tilemap_info(map).columns == 6)
                assert(w.tile(map, 1, 1) == 0 and not w.tile_solid(map, 1, 1))
                assert(w.tile_solid(map, 0, 0), 'the copied solid definitions still stand')

                local info = w.tilemap_info(map)
                info.columns = 99
                assert(w.tilemap_info(map).columns == 6, 'info is a snapshot')

                kept = w.tiles_region(map, 0, 1, 6, 1)
                kept[2] = 7
                assert(w.tiles_region(map, 0, 1, 6, 1)[2] == 0, 'a region is a snapshot')

                body = w.spawn(SPAWN_X, SPAWN_Y)
                local options = box(map, 32)
                w.set_tile_collider(body, options)
                options.width = 1
                assert(w.tile_collider(body).width == 32, 'options are copied')
                local collider = w.tile_collider(body)
                collider.width = 1
                assert(w.tile_collider(body).width == 32, 'a collider read is a snapshot')
            end,
            update = function(ctx)
                -- A retained region is a snapshot of the tick it was read in,
                -- never a live alias onto the map. Column 2 is clear of the
                -- body, so this edit is one T6 accepts.
                ctx.world.set_tile(map, 2, 1, 1)
                assert(kept[3] == 0 and ctx.world.tiles_region(map, 0, 1, 6, 1)[3] == 1)
                assert(ctx.world.tile_solid(map, 2, 1), 'the edited cell became solid')
                -- Drive the body at the cell that was just made solid, so the
                -- Rust side below can read the consequence instead of the cell.
                -- SPEED and the spawn are the prelude's, shared with the fixture
                -- that pins the unedited run against the room's wall.
                ctx.world.set_velocity(body, SPEED, 0)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().tilemap_count(), 1);
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 49);

    // **The edit is confirmed through a path that is not the read bindings.**
    // The cell-level check this fixture used to make from Rust is gone - there
    // is no handle on this side any more - and replacing it with a Luau
    // `tile_solid` made the fixture self-referential: a compatible fault in
    // both `set_tile` and the reads would agree with itself and pass. What is
    // independent is the fixed pass, which resolves collisions against the
    // kernel's own view of the map and never through a binding.
    //
    // The body spawned at (32, 32) fills column 1. Cell (2, 1) was just made
    // solid, so its left face at x = 64 is now a wall and the body cannot move
    // at all; without the edit it would run to WALL_X, the face of the room's
    // own solid column 4. Sixteen ticks at 2 px each is enough for either.
    //
    // **What WALL_X rests on, since a check that cannot fail is worth nothing.**
    // `a_script_installs_a_map_reads_it_back_and_stops_against_its_wall` runs
    // the same room, the same body at the same spawn, at the same speed, and
    // pins WALL_X with no edit. So the two values are separated by a sibling
    // fixture rather than by an assertion here about a number nobody produced.
    //
    // That cross-file dependency is real and was unenforced: the room came from
    // the shared prelude by construction, but the spawn and the speed were the
    // same literals written twice, so changing one fixture's would leave this
    // one passing while its number stopped meaning what this comment says.
    // `SPAWN_X`, `SPAWN_Y` and `SPEED` are in the prelude now, so the two move
    // together and neither can drift alone. Found by the review session, which
    // asked whether the coupling was by construction or by coincidence.
    //
    // No mutation control reaches this line, and that is a property of where it
    // sits rather than an omission. Every single-edit fault that breaks the
    // write - a skipped kernel call, a transposed index in one binding - is
    // caught by the Luau assertions three lines above and never gets here. What
    // gets here is a *compatible* fault, read and write wrong the same way,
    // which is by construction more than one edit. That is exactly the class
    // the old Rust-side cell read used to cover and the Luau replacement cannot,
    // and it is why the loss was independence and not coverage.
    //
    // **What makes this unreachable, and what would end it.** Two facts, both
    // outside this file and both plausible to stop being true:
    //
    // 1. `TileMap` has **one** storage. `cells: Vec<u16>` holds the IDs and
    //    `id_is_solid` derives solidity from `solids[id - 1]`, so the read
    //    bindings and the fixed pass consult the same entry and there is no
    //    second copy for one edit to desynchronise. *A precomputed solid bitmap
    //    beside `cells` - the obvious optimisation for a sweep that currently
    //    indirects per cell - ends this: `set_tile` updating one and not the
    //    other is a single edit that lands here while every assertion above
    //    passes.*
    // 2. This fixture holds one map, so a membership-resolution fault has no
    //    other map to resolve wrongly. *A second map here, which Phase 3 or 4
    //    may well add, ends it too.*
    //
    // Either change makes this line reachable by a single edit and voids this
    // note. Both the reasoning and the two expiry conditions are the review
    // session's; without them the paragraph reads as permanent and invites
    // deletion for tidiness.
    step(&mut runtime, 16);
    assert_eq!(
        positions(&runtime),
        [SPAWN],
        "the edit did not reach the map the fixed pass sweeps against"
    );
}

#[test]
fn the_description_schema_refuses_every_malformed_shape_catchably() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            local refused = 0
            local function refuse(mutate, what)
                local description = room()
                mutate(description)
                assert(not pcall(w.create_tilemap, description), what)
                refused += 1
            end

            -- Required fields, unexpected fields, metatables.
            refuse(function(d) d.rows = nil end, 'rows is required')
            refuse(function(d) d.cells = nil end, 'cells is required')
            refuse(function(d) d.solids = nil end, 'solids is required')
            refuse(function(d) d.extra = 1 end, 'an unknown field is refused')
            refuse(function(d) d[1] = 1 end, 'a numeric key is refused')
            refuse(function(d) setmetatable(d, {}) end, 'a metatable is refused')
            refuse(function(d) setmetatable(d.cells, {}) end, 'a cells metatable is refused')
            refuse(function(d) setmetatable(d.solids, {}) end, 'a solids metatable is refused')

            -- No coercions, no truthy substitutes, no fractional indices.
            refuse(function(d) d.columns = '6' end, 'a numeric string is a coercion')
            refuse(function(d) d.columns = true end, 'a boolean is not a count')
            refuse(function(d) d.columns = 6.5 end, 'a fractional dimension is refused')
            refuse(function(d) d.origin_x = 0.5 end, 'a fractional origin is refused')
            refuse(function(d) d.cells[8] = '1' end, 'a numeric string ID is refused')
            refuse(function(d) d.cells[8] = 0.5 end, 'a fractional ID is refused')
            refuse(function(d) d.cells[8] = 0/0 end, 'a NaN ID is refused')
            refuse(function(d) d.solids[1] = 1 end, 'a truthy substitute is not a boolean')

            -- Schema and geometry limits, checked before anything is allocated.
            refuse(function(d) d.columns = 0 end, 'zero columns')
            refuse(function(d) d.rows = 1025 end, 'more rows than the dimension cap')
            refuse(function(d) d.columns = 1024 d.rows = 1024 end, 'the cell product cap')
            refuse(function(d) d.tile_width = 0 end, 'zero tile width')
            refuse(function(d) d.tile_height = 1025 end, 'an oversized tile')
            refuse(function(d) d.origin_x = 16777215 end, 'a far edge past the geometry limit')
            refuse(function(d) d.solids = {} end, 'a map defines at least one ID')
            refuse(function(d) d.cells[8] = 2 end, 'an ID above the solid definitions')

            -- Density: holes, extras and hash keys, none of which `#` can see.
            refuse(function(d) d.cells[8] = nil end, 'a hole in cells')
            refuse(function(d) d.cells[25] = 0 end, 'one cell too many')
            refuse(function(d) d.cells.x = 0 end, 'a hash key in cells')
            refuse(function(d) d.cells[0] = 0 end, 'a zero index in cells')
            refuse(function(d) d.cells[2.5] = 0 end, 'a fractional index in cells')
            refuse(function(d) d.solids[3] = true end, 'a hole in solids')
            refuse(function(d)
                for index = 25, 5000 do d.cells[index] = 0 end
            end, 'a huge array claiming a small map')

            assert(not pcall(w.create_tilemap), 'a missing description')
            assert(not pcall(w.create_tilemap, 5), 'a description that is not a table')
            assert(refused == 31, 'every case above must have been exercised')

            -- Every one of those was catchable and none of them changed the map.
            local info = w.tilemap_info(map)
            assert(info.columns == 6 and info.rows == 4 and info.origin_x == 0)
            assert(w.tile(map, 1, 1) == 0 and w.tile(map, 0, 0) == 1)
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    // Thirty-one refused descriptions cost one map and one map's storage: the
    // one the fixture installed before any of them. The kernel-side reads that
    // used to check the survivor's shape from here now live in the fixture,
    // because the handle that names it does.
    assert_eq!(runtime.kernel().tilemap_count(), 1);
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 49);
}

#[test]
fn arguments_and_collider_options_are_refused_without_narrowing() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            local body = w.spawn(32, 32)

            -- Tile coordinates are exact signed 32-bit integers. A larger
            -- number is a malformed call, never a scan toward the grid.
            for _, index in {'1', true, 1.5, 0/0, math.huge, 2147483648, -2147483649} do
                assert(not pcall(w.tile, map, index, 1), 'tile column')
                assert(not pcall(w.tile, map, 1, index), 'tile row')
                assert(not pcall(w.tile_solid, map, index, 1), 'tile_solid column')
                assert(not pcall(w.set_tile, map, index, 1, 0), 'set_tile column')
                assert(not pcall(w.tiles_region, map, index, 1, 1, 1), 'region column')
            end
            assert(not pcall(w.tile, map, 1), 'a missing row')
            assert(not pcall(w.tile), 'no arguments at all')
            for _, extent in {'1', 1.5, -1, 0/0, 4294967296} do
                assert(not pcall(w.tiles_region, map, 0, 0, extent, 1), 'region width')
            end
            assert(not pcall(w.tiles_region, map, 0, 0, 0, 1), 'an empty region')
            assert(not pcall(w.tiles_region, map, 0, 0, 7, 1), 'a region past the far edge')
            assert(not pcall(w.tiles_region, map, 0, 0, 4097, 1), 'a region past the per-call cap')
            -- A request larger than the whole per-callback output ceiling is
            -- still an ordinary bounds error. It is charged for what a region
            -- read could have returned, never for what it asked for, so an
            -- out-of-range argument cannot latch the session on its own.
            for _ = 1, 8 do
                assert(not pcall(w.tiles_region, map, 0, 0, 600, 600), 'a request larger than the ceiling')
            end
            for _, id in {'1', true, 1.5, -1, 65536} do
                assert(not pcall(w.set_tile, map, 1, 1, id), 'tile ID')
            end
            assert(not pcall(w.set_tile, map, 1, 1, 2), 'an ID this map does not define')

            -- Collider options carry exactly four numbers.
            local function refuse(mutate, what)
                local options = box(map, 32)
                mutate(options)
                assert(not pcall(w.set_tile_collider, body, options), what)
            end
            refuse(function(o) o.width = nil end, 'width is required')
            refuse(function(o) o.extra = 1 end, 'an unknown option')
            refuse(function(o) setmetatable(o, {}) end, 'a metatable')
            refuse(function(o) o.width = '32' end, 'a numeric string')
            refuse(function(o) o.width = true end, 'a boolean extent')
            refuse(function(o) o.width = 0/0 end, 'a NaN extent')
            refuse(function(o) o.width = 0 end, 'a degenerate extent')
            refuse(function(o) o.width = 257 end, 'an extent beyond eight tiles')
            refuse(function(o) o.offset_x = 4097 end, 'an offset beyond the cap')
            refuse(function(o) o.offset_y = math.huge end, 'a non-finite offset')
            assert(not pcall(w.set_tile_collider, body, 5), 'options that are not a table')
            assert(w.tile_collider(body) == nil, 'no refusal attached anything')

            -- A fractional box inside a cell is legal; the schema refuses only
            -- what the frozen limits refuse.
            w.set_tile_collider(body, {map = map, offset_x = 0.5, offset_y = 0.5, width = 1/256, height = 8})
            assert(w.tile_collider(body).width == 1/256)
            w.set_tile_collider(body, nil)
            assert(w.tile_collider(body) == nil, 'nil removes the collider')
            w.set_tile_collider(body)
            assert(w.tile_collider(body) == nil, 'so does an absent argument')
        end}
    "#,
    );
    let mut runtime = load(&root);
    // Every refusal above is an ordinary call error, so `pcall` caught all of
    // them and init returned normally. A malformed argument that latched instead
    // would arrive here rather than at the Luau assertion, because a latch
    // escapes `pcall` at the next VM interrupt and never reaches it.
    runtime.init().unwrap_or_else(|error| {
        panic!("every argument refusal must stay catchable rather than latch: {error}")
    });
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().live_colliders(), 0);
}

#[test]
fn only_init_and_update_may_mutate_the_map_or_its_colliders() {
    let root = room_game(
        r#"
        local body, map
        local function readonly(ctx, phase)
            local w = ctx.world
            assert(not pcall(w.set_tilemap, room()), phase .. ': set_tilemap must refuse')
            assert(not pcall(w.clear_tilemap), phase .. ': clear_tilemap must refuse')
            assert(not pcall(w.create_tilemap, room()), phase .. ': create_tilemap must refuse')
            assert(not pcall(w.replace_tilemap, map, room()),
                phase .. ': replace_tilemap must refuse')
            assert(not pcall(w.remove_tilemap, map), phase .. ': remove_tilemap must refuse')
            assert(not pcall(w.set_tile, map, 1, 1, 1), phase .. ': set_tile must refuse')
            assert(not pcall(w.set_tile_collider, body, box(map, 32)), phase .. ': attaching must refuse')
            assert(not pcall(w.set_tile_collider, body, nil), phase .. ': detaching must refuse')
            -- Reads stay available in every live callback.
            assert(w.tilemap_info(map).columns == 6)
            assert(w.tile(map, 1, 1) == 0 and not w.tile_solid(map, 1, 1))
            assert(#w.tiles_region(map, 1, 1, 3, 2) == 6)
            assert(w.tile_collider(body).width == 32)
        end
        return {
            init = function(ctx)
                map = ctx.world.create_tilemap(room())
                body = ctx.world.spawn(32, 32)
                ctx.world.set_tile_collider(body, box(map, 32))
                -- A second, empty map, held only so the read-only phases have a
                -- live handle to be refused with. Refusing an *invalid* handle
                -- would pass this fixture for the wrong reason.
                map = ctx.world.create_tilemap(room())
            end,
            draw = function(ctx) readonly(ctx, 'draw') end,
            shutdown = function(ctx) readonly(ctx, 'shutdown') end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    // Two rooms of 24 cells and one solid flag each: (24 x 2 + 1) x 2. The
    // figure is the aggregate across maps (M2-7), so the second map arriving
    // through `create_tilemap` is what doubles it, and a `create_tilemap` that
    // installed nothing would leave this at 49.
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 98);
    assert_eq!(runtime.kernel().tilemap_count(), 2);
    runtime.draw(0.0).unwrap();
    runtime.shutdown().unwrap();
    assert_eq!(runtime.state(), ScriptState::Stopped);
    // The shutdown callback read the map and its collider; stop then released
    // both rather than only denying access to them.
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 0);
    assert_eq!(runtime.kernel().live_colliders(), 0);
    assert!(runtime.kernel().tilemap().is_err());
}

#[test]
fn collider_calls_refuse_stale_reused_and_expired_handles() {
    // A handle from a different session cannot be produced by a game at all, so
    // that case is pinned by the unit test beside the bindings, which can inject
    // one. What a game can reach is everything below.
    let root = room_game(
        r#"
        local body, stale_world, dead
        return {
            init = function(ctx)
                local w = ctx.world
                map = w.create_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(map, 32))
                stale_world = w
                dead = w.spawn(64, 32)
                w.set_tile_collider(dead, box(map, 32))
                w.despawn(dead)
                assert(not pcall(w.tile_collider, dead), 'a despawned handle')
                assert(not pcall(w.set_tile_collider, dead, box(map, 32)), 'a despawned handle')
                assert(not pcall(w.tile_collider, 5), 'not a handle at all')
                assert(not pcall(w.tile_collider), 'no handle at all')
                assert(not pcall(w.set_tile_collider, box(map, 32), box(map, 32)), 'a table is not a handle')
            end,
            update = function(ctx)
                local w = ctx.world
                -- The previous callback's functions are expired, and the new
                -- ones are scoped exactly as the old ones already were.
                for _, name in {'set_tilemap', 'clear_tilemap', 'create_tilemap',
                                'replace_tilemap', 'remove_tilemap', 'tilemap_info', 'tile',
                                'tile_solid', 'tiles_region', 'set_tile', 'set_tile_collider',
                                'tile_collider'} do
                    assert(not pcall(stale_world[name], 1, 1, 1), name .. ' expired')
                end
                -- The despawned entity's slot comes back with a new generation,
                -- so the old handle names nothing and the new one is a body
                -- with no collider rather than an inherited one.
                local fresh = w.spawn(64, 32)
                assert(fresh ~= dead)
                assert(not pcall(w.tile_collider, dead), 'a reused slot is not the old entity')
                assert(w.tile_collider(fresh) == nil, 'a reused slot inherits no collider')
                assert(w.tile_collider(body).width == 32)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    // Despawning released the capacity the second collider held.
    assert_eq!(runtime.kernel().live_colliders(), 1);
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().live_colliders(), 1);
}

#[test]
fn the_t5_and_t6_guards_reach_lua_unchanged() {
    let root = room_game(
        r#"
        local body
        return {
            init = function(ctx)
                local w = ctx.world
                map = w.create_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(map, 32))
            end,
            update = function(ctx)
                local w = ctx.world
                -- T5: a teleport is not swept, but its destination must be a
                -- legal placement.
                assert(not pcall(w.set_position, body, 128, 32), 'into a wall')
                assert(not pcall(w.set_position, body, -32, 32), 'outside the map')
                assert(w.position(body).x == 32, 'a refused teleport moved nothing')
                w.set_position(body, 96, 64)
                assert(w.position(body).x == 96 and w.position(body).y == 64)

                -- T6: an edit that would trap the body refuses, atomically.
                assert(not pcall(w.set_tile, map, 3, 2, 1), 'a solid edit under the body')
                assert(w.tile(map, 3, 2) == 0, 'the refused edit changed no cell')
                w.set_tile(map, 1, 1, 1)
                assert(w.tile(map, 1, 1) == 1 and w.tile_solid(map, 1, 1))

                -- T6: a replacement that would trap the body refuses. Under
                -- M2-R1 that is a statement about *this* map's members, and
                -- `replace_tilemap` is the call M2-6 renames it to.
                local trap = room()
                trap.cells[2 * 6 + 3 + 1] = 1
                assert(not pcall(w.replace_tilemap, map, trap), 'a map that would trap the body')
                assert(w.tile(map, 1, 1) == 1, 'the installed map survived the refusal')

                -- T6: the map cannot be removed beneath its own member, and the
                -- transition recipe is detach, remove, create, reattach.
                assert(not pcall(w.remove_tilemap, map), 'removing beneath a body')
                w.set_tile_collider(body, nil)
                w.remove_tilemap(map)
                -- The handle outlived the map it named, which is the successor
                -- to M1's "no map is installed": an attachment now refuses
                -- because the map is gone rather than because none was chosen.
                assert(not pcall(w.tilemap_info, map), 'the handle names nothing now')
                assert(not pcall(w.set_tile_collider, body, box(map, 32)),
                    'attaching to a removed map')
                map = w.create_tilemap(room())
                w.set_position(body, 32, 32)
                w.set_tile_collider(body, box(map, 32))
                assert(w.tile_collider(body).width == 32)
                assert(w.tile(map, 1, 1) == 0, 'the new map is the described one')
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().live_colliders(), 1);
    assert_eq!(positions(&runtime), [SPAWN]);
}

#[test]
fn the_aggregate_tile_work_ceiling_latches_outside_pcall() {
    // 128 x 128 cells plus one solid flag is 16,385 units per install, so the
    // 64th crosses 1,048,576 partway through its cells and the 63 before it fit.
    const CELLS_PER_MAP: u32 = 128 * 128;
    const PER_INSTALL: u64 = CELLS_PER_MAP as u64 + 1;
    const _: () = assert!(63 * PER_INSTALL < MAX_CALLBACK_WORK);
    const _: () = assert!(64 * PER_INSTALL > MAX_CALLBACK_WORK);

    // And why the fixture must *replace* rather than create. `MapTable::admit`
    // refuses on `total > budget + replaced`, so 32 of these land exactly on
    // 524,288 and are admitted and the 33rd is the first refusal - less than
    // halfway to the 63 this fixture needs. Creating them would latch on the
    // cell budget with the wrong message, long before the ceiling under test.
    //
    // Stated as assertions because the sentence they replace was **the entire
    // guard** on this arrangement, and it carried a wrong figure: it said the
    // aggregate would latch "at the fifth". The conclusion was right, which is
    // what makes that the durable kind of error - a reader who checks it gets
    // 33, sees the conclusion still stands, and learns the comment is
    // unreliable about numbers; a reader who does not check inherits a wrong
    // model of where the aggregate sits. Neither outcome fails anything.
    // Converting the fixture back to creates *does* fail, loudly and at the
    // message assertion below; what these add is that the reason is checkable
    // rather than asserted, and that the ordinal cannot drift again without the
    // build objecting. Found by the review session.
    const _: () = assert!(MAX_AGGREGATE_CELLS / CELLS_PER_MAP == 32);
    const _: () = assert!(33 * CELLS_PER_MAP > MAX_AGGREGATE_CELLS);
    const _: () = assert!(MAX_AGGREGATE_CELLS / CELLS_PER_MAP < 63);
    let root = game(
        r#"
        local function grid()
            local cells = {}
            for index = 1, 128 * 128 do cells[index] = 0 end
            return {
                columns = 128, rows = 128, tile_width = 8, tile_height = 8,
                origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
            }
        end
        return {init = function(ctx)
            local description = grid()
            -- One map, replaced in place, so the cell count stays at 16,384
            -- and tile work is the only budget that can refuse. The arithmetic
            -- is in the `const _` gates above rather than restated here: this
            -- comment carried its own copy of the ordinal, disagreed with the
            -- Rust one after that was corrected, and was invisible to a diff of
            -- the correction. One fact, two syntaxes, and the second silently
            -- wrong is the failure D1 turns on.
            local map = ctx.world.create_tilemap(description)
            for _ = 1, 62 do ctx.world.replace_tilemap(map, description) end
            ctx.log('installed 63')
            -- The ceiling is not catchable: it latches like every other
            -- aggregate budget, so this pcall cannot swallow it.
            pcall(ctx.world.replace_tilemap, map, description)
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load_patient(&root);
    let error = runtime
        .init()
        .expect_err("the aggregate tile-work ceiling must refuse the 64th install, uncatchably");
    assert_eq!(error.phase, "init");
    assert!(
        error.message.contains("tile work limit exceeded"),
        "the aggregate tile-work ceiling must refuse the 64th install, uncatchably: {}",
        error.message
    );
    assert_eq!(runtime.state(), ScriptState::Faulted);
    let logs = runtime.take_logs();
    assert_eq!(
        logs,
        ["installed 63"],
        "63 installs fit and the 64th did not"
    );
}

#[test]
fn the_region_output_ceiling_latches_outside_pcall() {
    let root = game(
        r#"
        local function grid()
            local cells = {}
            for index = 1, 64 * 64 do cells[index] = 0 end
            return {
                columns = 64, rows = 64, tile_width = 8, tile_height = 8,
                origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
            }
        end
        return {
            init = function(ctx) map = ctx.world.create_tilemap(grid()) end,
            update = function(ctx)
                local w = ctx.world
                -- 4,096 IDs a call, 262,144 a callback: exactly 64 whole reads.
                for _ = 1, 64 do assert(#w.tiles_region(map, 0, 0, 64, 64) == 4096) end
                ctx.log('read 64 regions')
                pcall(w.tiles_region, map, 0, 0, 1, 1)
                ctx.log('unreachable')
            end,
        }
    "#,
    );
    let mut runtime = load_patient(&root);
    runtime.init().unwrap();
    let error = runtime
        .step(InputSnapshot::default())
        .expect_err("the region output ceiling must refuse the 65th read, uncatchably");
    assert!(
        error.message.contains("region output limit exceeded"),
        "the region output ceiling must refuse the 65th read, uncatchably: {}",
        error.message
    );
    assert_eq!(runtime.state(), ScriptState::Faulted);
    // The ceiling is per callback, so the first tick's reads were all served.
    assert_eq!(runtime.completed_ticks(), 0);
}

#[test]
fn a_refused_region_is_charged_for_what_it_could_have_returned() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            -- A request no map could serve is still charged, before either
            -- allocation, so a refusal is bounded rather than free to repeat.
            -- 4,096 is the per-call cap, so 64 of them is the whole ceiling.
            for _ = 1, 64 do
                assert(not pcall(w.tiles_region, map, 0, 0, 64, 64), 'past the far edge')
            end
            ctx.log('64 refusals')
            pcall(w.tiles_region, map, 0, 0, 1, 1)
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load(&root);
    let error = runtime
        .init()
        .expect_err("64 refused requests must exhaust the region output ceiling");
    assert!(
        error.message.contains("region output limit exceeded"),
        "64 refused requests must exhaust the region output ceiling: {}",
        error.message
    );
    assert_eq!(
        runtime.take_logs(),
        ["64 refusals"],
        "64 refused requests must exhaust the region output ceiling"
    );
}

#[test]
fn the_live_collider_limit_latches_outside_pcall() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            -- Bodies do not block one another, so all of them fit in one cell.
            for _ = 1, 1024 do
                w.set_tile_collider(w.spawn(32, 32), box(map, 32))
            end
            ctx.log('attached 1024')
            pcall(w.set_tile_collider, w.spawn(32, 32), box(map, 32))
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load_patient(&root);
    let error = runtime
        .init()
        .expect_err("the collider limit must refuse the 1025th attachment, uncatchably");
    assert!(
        error.message.contains("collider limit exceeded"),
        "the collider limit must refuse the 1025th attachment, uncatchably: {}",
        error.message
    );
    assert_eq!(
        runtime.take_logs(),
        ["attached 1024"],
        "the collider limit must refuse the 1025th attachment, uncatchably"
    );
}

#[test]
fn map_calls_share_the_existing_world_attempt_budget() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            -- One counter, not one per family: 2,048 entity reads plus 2,048
            -- map reads is the 4,096th attempt, and this call is the 4,097th.
            for _ = 1, 2047 do w.entities() end
            for _ = 1, 2048 do w.tile_solid(map, 0, 0) end
            ctx.log('4096 attempts')
            pcall(w.tile_solid, map, 0, 0)
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load(&root);
    let error = runtime
        .init()
        .expect_err("map calls must share the 4096 world attempts, not have their own");
    assert!(
        error.message.contains("world operation limit exceeded"),
        "map calls must share the 4096 world attempts, not have their own: {}",
        error.message
    );
    assert_eq!(
        runtime.take_logs(),
        ["4096 attempts"],
        "map calls must share the 4096 world attempts, not have their own"
    );
}

#[test]
fn malformed_and_wrong_phase_calls_count_against_the_attempt_budget() {
    let root = room_game(
        r#"
        return {
            init = function(ctx) map = ctx.world.create_tilemap(room()) end,
            draw = function(ctx)
                local w = ctx.world
                -- A refused call has still been attempted: malformed arguments
                -- and wrong-phase mutations are counted before conversion.
                for _ = 1, 2048 do assert(not pcall(w.tile, map, 'x', 'y')) end
                for _ = 1, 2047 do assert(not pcall(w.set_tile, map, 1, 1, 0)) end
                ctx.log('4095 refusals')
                pcall(w.tile, map, 'x', 'y')
                pcall(w.tile, map, 'x', 'y')
                ctx.log('unreachable')
            end,
        }
    "#,
    );
    let mut runtime = load_patient(&root);
    runtime.init().unwrap();
    let error = runtime
        .draw(0.0)
        .expect_err("a refused call has still been attempted and must be counted");
    assert!(
        error.message.contains("world operation limit exceeded"),
        "a refused call has still been attempted and must be counted: {}",
        error.message
    );
    assert_eq!(
        runtime.take_logs(),
        ["4095 refusals"],
        "a refused call has still been attempted and must be counted"
    );
}

#[test]
fn a_systems_fault_moves_no_entity_and_stops_the_session() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            local body = w.spawn(32, 32)
            w.set_tile_collider(body, box(map, 32))
            w.set_velocity(body, 120, 0)
            -- A free-flight entity whose next position is not representable.
            -- The pass validates every candidate before committing any, so the
            -- swept body must not have moved either.
            w.set_velocity(w.spawn(1.7976931348623157e308, 0), 1e308, 0)
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(positions(&runtime)[0], SPAWN);
    let error = runtime.step(InputSnapshot::default()).unwrap_err();
    assert_eq!(error.phase, "systems");
    assert!(
        error
            .message
            .contains("position and velocity must remain finite"),
        "{}",
        error.message
    );
    assert_eq!(runtime.state(), ScriptState::Faulted);
    assert_eq!(
        runtime.completed_ticks(),
        0,
        "the failed tick did not count"
    );
    // A fault invalidates the session, so the swept body's untouched position
    // is no longer readable here; that the pass commits nothing is asserted
    // directly against the kernel in `tests/collision.rs`.
    assert!(runtime.kernel().snapshot().is_err());
}

#[test]
fn a_failed_callback_runs_no_systems_and_releases_the_map_it_was_holding() {
    let root = room_game(
        r#"
        local ticks = 0
        return {
            init = function(ctx)
                local w = ctx.world
                map = w.create_tilemap(room())
                local body = w.spawn(32, 32)
                w.set_tile_collider(body, box(map, 32))
                w.set_velocity(body, 120, 0)
            end,
            update = function(ctx)
                ticks += 1
                if ticks == 1 then return end
                -- An accepted write, then a failure in the same callback.
                ctx.world.set_tile(map, 2, 1, 1)
                assert(ctx.world.tile(map, 2, 1) == 1, 'the write took effect')
                error('the game gave up')
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    // 24 cells of u16 plus the one solid flag.
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 49);
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(positions(&runtime), [Position { x: 34.0, y: 32.0 }]);

    let error = runtime.step(InputSnapshot::default()).unwrap_err();
    assert_eq!(error.phase, "update");
    assert_eq!(runtime.state(), ScriptState::Faulted);
    // The first tick counted and the failed one did not, which is what says no
    // system pass followed the failed callback.
    assert_eq!(runtime.completed_ticks(), 1);
    // Stop releases map and collider storage rather than only denying access.
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 0);
    assert_eq!(runtime.kernel().live_colliders(), 0);
    assert!(runtime.kernel().tilemap().is_err());
}

#[test]
fn zero_tick_frames_only_draw_and_catch_up_ticks_each_move_once() {
    let root = room_game(
        r#"
        local body, draws = nil, 0
        return {
            init = function(ctx)
                local w = ctx.world
                map = w.create_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(map, 32))
                w.set_velocity(body, 120, 0)
            end,
            draw = function(ctx)
                draws += 1
                ctx.log(string.format('%d %g', draws, ctx.world.position(body).x))
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();

    let report = runtime.frame(0.0, InputSnapshot::default()).unwrap();
    assert_eq!(report.ticks, 0);
    assert_eq!(
        runtime.take_logs(),
        ["1 32"],
        "a zero-tick frame only draws"
    );

    let report = runtime
        .frame(5.0 * FIXED_DT, InputSnapshot::default())
        .unwrap();
    assert_eq!((report.ticks, report.dropped_ticks), (5, 0));
    assert_eq!(runtime.take_logs(), ["2 42"], "five ticks move five times");
    assert_eq!(positions(&runtime), [Position { x: 42.0, y: 32.0 }]);
}

#[test]
fn replay_is_independent_of_spawn_order_and_of_the_installing_callback() {
    // Two bundles describing the same room and the same two bodies, differing
    // only in the order they are created and when the map is installed.
    let sources = [
        r#"
        return {init = function(ctx)
            local w = ctx.world
            map = w.create_tilemap(room())
            local first = w.spawn(32, 32)
            w.set_tile_collider(first, box(map, 32))
            w.set_velocity(first, 120, 0)
            local second = w.spawn(32, 64)
            w.set_tile_collider(second, box(map, 32))
            w.set_velocity(second, 60, 0)
        end}
    "#,
        r#"
        local pending = true
        return {
            init = function(ctx)
                local w = ctx.world
                local second = w.spawn(32, 64)
                local first = w.spawn(32, 32)
                w.despawn(second)
                second = w.spawn(32, 64)
                w.set_velocity(second, 60, 0)
                w.set_velocity(first, 120, 0)
                ctx.log('deferred')
            end,
            update = function(ctx)
                if not pending then return end
                pending = false
                local w = ctx.world
                map = w.create_tilemap(room())
                for _, handle in w.entities() do w.set_tile_collider(handle, box(map, 32)) end
            end,
        }
    "#,
    ];
    let mut results = Vec::new();
    for source in sources {
        let root = room_game(source);
        let mut runtime = load(&root);
        runtime.init().unwrap();
        step(&mut runtime, 40);
        let mut settled = positions(&runtime);
        settled.sort_by(|a, b| a.y.total_cmp(&b.y));
        results.push(settled);
    }
    // The fast body reaches WALL_X after 32 ticks and holds it; the
    // slow one is still travelling at 72 after 40.
    assert_eq!(
        results[0],
        [
            Position {
                x: WALL_X,
                y: SPAWN.y
            },
            Position { x: 72.0, y: 64.0 }
        ]
    );
    // The second bundle spawns in the other order, recycles a slot, and installs
    // the map a callback later, and lands on the same two positions.
    assert_eq!(results[1], results[0]);
}

#[test]
fn tile_work_accounting_starts_over_in_every_callback() {
    let root = room_game(
        r#"
        return {
            init = function(ctx) map = ctx.world.create_tilemap(room()) end,
            update = function(ctx)
                -- One description copied per iteration and nothing else, every
                -- tick. If the accounting did not restart, a long session would
                -- eventually refuse an install that fits on its own.
                -- Replacement rather than creation, so the map count and the
                -- cell aggregate stay where they started and only the
                -- per-callback work moves. The unit price is gated on the Rust
                -- side rather than restated here.
                for _ = 1, 100 do ctx.world.replace_tilemap(map, room()) end
            end,
        }
    "#,
    );
    // **The fixture's premise, gated rather than stated.** It claims a
    // non-resetting accounting *would* have refused, and that is only true
    // while the total exceeds the ceiling. Nothing asserted it: the test checks
    // `Running` after the last tick, so a smaller room silently drops the total
    // under 1,048,576 and the fixture passes having demonstrated nothing.
    // `room()` at 5x3 instead of 6x4 is enough to do it - 800,000 against
    // 1,048,576 - and the margin here is only 19%. Found by the review session,
    // sweeping for restated figures and landing on a live vacuity instead.
    const ROOM_UNITS: u64 = 6 * 4 + 1;
    const INSTALLS_PER_TICK: u64 = 100;
    const TICKS: u64 = 500;
    const _: () = assert!(TICKS * INSTALLS_PER_TICK * ROOM_UNITS > MAX_CALLBACK_WORK);

    let mut runtime = load(&root);
    runtime.init().unwrap();
    runtime.step(InputSnapshot::default()).unwrap();
    // The gate above is arithmetic about numbers written here; this ties them
    // to the room and the loop the script actually runs. Either the prelude's
    // room shrinking or the Luau loop count changing breaks it, which is what
    // the `const _` alone cannot see - it would go on asserting a premise about
    // a fixture that no longer matches it.
    assert_eq!(
        runtime.kernel().callback_work(),
        INSTALLS_PER_TICK * ROOM_UNITS,
        "one tick must charge exactly its installs, or the premise above is \
         arithmetic about a different room"
    );
    for tick in 1..TICKS {
        runtime
            .step(InputSnapshot::default())
            .unwrap_or_else(|error| {
                panic!("tile-work accounting must restart each callback, tick {tick}: {error}")
            });
    }
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.completed_ticks(), TICKS);
}

#[test]
fn only_create_tilemap_returns_a_value_that_could_fail_to_allocate() {
    // `spawn` needs a rollback path because it mutates and then allocates a
    // handle to publish, and a failure between the two would leave an entity
    // nobody can name; `tests/scripting.rs` and the unit harness beside the
    // bindings cover that. This is the fixture that says which map calls have
    // that shape, and until M2-6 split `set_tilemap` the answer was none.
    //
    // `create_tilemap` is now the one that does, and it carries the same
    // rollback for the same reason: a failure between the insert and the
    // userdata would leave a map holding a slot and its cells against both
    // M2-7 budgets with no handle able to remove it. The others still mutate
    // and return nothing, so there is no window between publishing a change and
    // allocating something to report it - and adding a return value to one of
    // them fails here, which is the point. That is how this fixture named
    // `create_tilemap`'s obligation before `create_tilemap` was written.
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local body = w.spawn(32, 32)
            assert(select('#', w.set_tilemap(room())) == 0, 'set_tilemap returns nothing')
            assert(select('#', w.clear_tilemap()) == 0, 'clear_tilemap returns nothing')
            map = w.create_tilemap(room())
            assert(select('#', w.set_tile(map, 2, 1, 1)) == 0, 'set_tile returns nothing')
            assert(select('#', w.set_tile_collider(body, box(map, 32))) == 0,
                'set_tile_collider returns nothing')
            assert(select('#', w.set_tile_collider(body, nil)) == 0,
                'detaching returns nothing')
            assert(select('#', w.replace_tilemap(map, room())) == 0,
                'replace_tilemap returns nothing')
            -- Exactly one, not "at least one": a second return value would be a
            -- second thing to allocate after the mutation and a second thing to
            -- roll back.
            assert(select('#', w.create_tilemap(room())) == 1,
                'create_tilemap returns exactly its handle')
            assert(select('#', w.remove_tilemap(map)) == 0, 'remove_tilemap returns nothing')
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    // The implicit map was installed and cleared, `map` was created, replaced
    // and removed, and the arity probe's map is still live because the script
    // dropped the only handle that could remove it. One map, and the count is
    // what shows a discarded handle is a leaked map rather than a freed one -
    // which is the whole reason the rollback above exists.
    assert_eq!(runtime.kernel().tilemap_count(), 1);
}

#[test]
fn a_script_holds_two_map_handles_across_callbacks_and_removes_them_one_at_a_time() {
    let root = room_game(
        r#"
        local a, b
        return {
            init = function(ctx)
                local w = ctx.world
                a = w.create_tilemap(room())
                b = w.create_tilemap(room())
                -- Two installs of the same description are two maps. Identity
                -- is the handle's, never the content's, which is the property
                -- M2-1 needs and the one a shape-based comparison would lose.
                --
                -- Both comparisons here hold under raw identity, so neither
                -- witnesses the `__eq` metamethod: every wrapper a script can
                -- hold today comes from its own `create_tilemap`, and no read
                -- returns a map yet.
                -- `two_wrappers_for_one_map_compare_equal_while_staying_distinct_values`
                -- beside the bindings is what covers the metamethod until
                -- `tile_collider` returns the map and makes it reachable here.
                assert(a ~= b, 'two maps are two handles')
                assert(a == a, 'a handle equals itself')
            end,
            update = function(ctx)
                local w = ctx.world
                -- The handles outlived the callback that made them, unlike the
                -- functions that made them, which expire with their scope.
                -- A successful call is the evidence they still resolve: a stale
                -- handle refuses, so nothing below could pass on a dead one.
                w.replace_tilemap(a, room())
                w.remove_tilemap(b)
                assert(not pcall(w.remove_tilemap, b), 'a removed map cannot be removed again')
                assert(not pcall(w.replace_tilemap, b, room()), 'a removed map cannot be replaced')
                -- Removing one map left the other alone, which is the map-local
                -- lifecycle M2-4 asks for, reached from Lua.
                w.remove_tilemap(a)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.kernel().tilemap_count(), 2);
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 98);
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().tilemap_count(), 0);
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 0);
}

#[test]
fn a_script_reads_and_edits_the_map_it_names_and_no_other() {
    // The binding half of the same claim `tests/tilemap.rs` makes of the
    // kernel: that the handle a script passes is the one that reaches
    // `named_map`, rather than a map the binding chose. A binding that ignored
    // its first argument and resolved the last-installed map passes every other
    // fixture in this file, because every other fixture has one map.
    //
    // **The mutation control for that rule lives on the kernel fixture, not
    // here.** Every map in this one arrives through `create_tilemap`, so there
    // is no `current` for a wrongly-implicit resolution to find and the defect
    // refuses instead of answering wrongly - which fails this fixture at a Lua
    // error rather than at a named assertion.
    // `every_read_and_edit_answers_for_the_map_it_names_and_no_other` installs
    // one of its two maps as `current` precisely so the defect succeeds there.
    // What this fixture adds is reachability: that a script can hold two maps
    // at once and be answered about each, which no in-crate test covers.
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local plain = w.create_tilemap(room())
            local filled = room()
            for index = 1, 24 do filled.cells[index] = 1 end
            local solid = w.create_tilemap(filled)
            assert(plain ~= solid, 'two maps')
            -- Cell (1,1) is open on one room and solid on the other, at the
            -- same world coordinates, so a wrong resolution is a wrong answer
            -- and never a refusal.
            assert(w.tile(plain, 1, 1) == 0 and w.tile(solid, 1, 1) == 1)
            assert(not w.tile_solid(plain, 1, 1) and w.tile_solid(solid, 1, 1))
            assert(w.tiles_region(plain, 1, 1, 2, 1)[1] == 0)
            assert(w.tiles_region(solid, 1, 1, 2, 1)[1] == 1)
            assert(w.tilemap_info(plain).columns == w.tilemap_info(solid).columns)

            w.set_tile(plain, 1, 1, 1)
            assert(w.tile(plain, 1, 1) == 1, 'the edit landed')
            w.set_tile(solid, 1, 1, 0)
            assert(w.tile(solid, 1, 1) == 0, 'and so did the other')
            assert(w.tile(plain, 1, 1) == 1, 'without disturbing the first')

            -- A body on one map is unaffected by the other being removed.
            local body = w.spawn(32, 32)
            w.set_tile_collider(body, box(solid, 32))
            assert(not pcall(w.remove_tilemap, solid), 'its own member refuses')
            w.remove_tilemap(plain)
            assert(w.tile(solid, 1, 1) == 0, 'the surviving map still reads')
            assert(not pcall(w.tile, plain, 1, 1), 'and the removed one does not')
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().tilemap_count(), 1);
    assert_eq!(runtime.kernel().live_colliders(), 1);
}

#[test]
fn a_map_handle_never_names_the_map_that_took_its_slot() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local first = w.create_tilemap(room())
            w.remove_tilemap(first)
            local second = w.create_tilemap(room())
            -- Whether the second map landed in the first's slot is the
            -- allocator's business and is not observable from Lua; what is
            -- observable is that the old handle names nothing either way.
            -- `map_table_reuses_a_freed_slot_under_a_new_generation` in
            -- `src/maps.rs` is the fixture that *forces* the reuse and asserts
            -- the slot, which no script-level test can do - so this one is the
            -- reachability half rather than a weaker copy of it.
            assert(first ~= second, 'a reused slot is not the old map')
            assert(not pcall(w.remove_tilemap, first), 'the old handle names nothing')
            assert(not pcall(w.replace_tilemap, first, room()), 'nor can it be replaced')
            w.remove_tilemap(second)
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().tilemap_count(), 0);
}

#[test]
fn the_map_lifecycle_calls_refuse_everything_that_is_not_their_argument() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local body = w.spawn(32, 32)
            local map = w.create_tilemap(room())
            -- A wrong *kind* of handle is a type refusal at the boundary, not a
            -- stale-map refusal from the kernel, which could not tell the two
            -- apart and would have to answer InvalidTileMap to both.
            assert(not pcall(w.remove_tilemap, body), 'an entity is not a map')
            assert(not pcall(w.despawn, map), 'and a map is not an entity')
            assert(not pcall(w.remove_tilemap, 5), 'a number is not a map')
            assert(not pcall(w.remove_tilemap, room()), 'a description is not a map')
            assert(not pcall(w.remove_tilemap), 'no handle at all')
            assert(not pcall(w.replace_tilemap, body, room()), 'an entity is not a map')
            assert(not pcall(w.replace_tilemap, map), 'a replacement needs a description')
            assert(not pcall(w.replace_tilemap, map, map), 'a handle is not a description')
            assert(not pcall(w.create_tilemap), 'a create needs a description')
            assert(not pcall(w.create_tilemap, map), 'a handle is not a description')
            -- Every refusal above left the map alone, and this is what proves
            -- it: a map that had been removed or replaced by one of them could
            -- not be removed here.
            w.remove_tilemap(map)
        end}
    "#,
    );
    let mut runtime = load(&root);
    // Nothing above latched: a wrong argument is an ordinary catchable error.
    runtime.init().unwrap_or_else(|error| {
        panic!("every argument refusal must stay catchable rather than latch: {error}")
    });
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().tilemap_count(), 0);
}

#[test]
fn creating_and_replacing_a_map_refuse_exactly_the_same_descriptions() {
    // The two installers share one conversion, so a description cannot be
    // accepted by one and refused by the other - a difference no contract
    // states and the reason M2-6 renames rather than duplicates. Giving either
    // its own copy of the schema passes every other fixture in this file.
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local map = w.create_tilemap(room())
            local hole = room()
            hole.cells[1] = nil
            local unknown = room()
            unknown.nonsense = 1
            local short = room()
            short.rows = 3
            for _, bad in {hole, unknown, short} do
                assert(not pcall(w.create_tilemap, bad), 'create must refuse it')
                assert(not pcall(w.replace_tilemap, map, bad), 'and replace must refuse it too')
            end
            w.remove_tilemap(map)
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    // Six refusals cost neither a slot nor a byte, so the aggregate is exactly
    // what M2-7 requires of a refused admission.
    assert_eq!(runtime.kernel().tilemap_count(), 0);
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 0);
}

#[test]
fn the_map_count_latches_and_names_itself_when_a_candidate_is_over_both_budgets() {
    // M2-8 puts these two ceilings on the latching side, and this is the first
    // fixture that can reach either: a script had no way to create a map until
    // `create_tilemap`, so no script could hit the map count or the aggregate.
    //
    // **The three dates are the point, and they run in an order worth stating.**
    // `3d7b5cd` (Phase 1) added `TileMapLimit` and `AggregateCellLimit` to a
    // `kernel_result` that still ended in `_ => None`, so both were classified
    // as ordinary catchable errors from the moment they existed. `7c66540`
    // (Phase 2) made the match exhaustive and corrected them, on no evidence
    // but exhaustiveness - there was no failing test to find, because nothing
    // could reach either arm. This fixture, one phase later, is the first thing
    // that could have caught the defect, and by the time it existed the defect
    // was already gone. The fix arrived before the bug could fire, which is the
    // whole argument for a match with no catch-all and is legible only with the
    // phases the right way round. Verified against the commits rather than
    // recollection, after I twice wrote it with them the wrong way round; the
    // review session resolved it with `git log -S`.
    //
    // 64 maps of 128x64 is M2-7's balance point: 8,192 cells each fills the
    // aggregate to the cell at exactly the map count, so the 65th candidate is
    // over *both* budgets at once and the contract requires it to name the
    // count. A table that weighed cells first would refuse this with the same
    // latch and the wrong message, which is why the assertion is on the message
    // rather than only on the fault.
    //
    // "Over both" is the whole fixture and it is arithmetic, so it is gated
    // rather than stated: if either budget moved, this fixture would quietly
    // become a test of whichever one still binds first.
    const CELLS_PER_MAP: u32 = 128 * 64;
    const _: () = assert!(MAX_TILEMAPS * CELLS_PER_MAP == MAX_AGGREGATE_CELLS);
    let root = game(
        r#"
        local function room()
            local cells = {}
            for index = 1, 128 * 64 do cells[index] = 0 end
            return {
                columns = 128, rows = 64, tile_width = 8, tile_height = 8,
                origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
            }
        end
        local held = {}
        return {init = function(ctx)
            local description = room()
            for index = 1, 64 do held[index] = ctx.world.create_tilemap(description) end
            ctx.log('created 64')
            -- Latching, so this pcall cannot swallow it and cannot let the
            -- script discover the ceiling one refusal at a time.
            pcall(ctx.world.create_tilemap, description)
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load_patient(&root);
    let error = runtime
        .init()
        .expect_err("the map count must refuse the 65th map, uncatchably");
    assert_eq!(error.phase, "init");
    assert!(
        error.message.contains("map limit exceeded"),
        "the 65th map must name the count rather than the cells: {}",
        error.message
    );
    assert_eq!(runtime.state(), ScriptState::Faulted);
    assert_eq!(runtime.take_logs(), ["created 64"]);
    // Zero, not 64: the fault stopped the session and `stop` releases every
    // map, so this is Phase 1's release path reached through a real fault
    // rather than a direct call - which is the only thing about the table this
    // fixture can still see. That a *refused* admission leaves the count,
    // storage and free list unchanged is
    // `maps::tests::a_refused_admission_leaves_the_count_storage_and_free_list_unchanged`,
    // which can observe the table before anything stops it; asserting it here
    // as well would be a weaker copy that passes for the wrong reason.
    assert_eq!(runtime.kernel().tilemap_count(), 0);
    assert_eq!(runtime.kernel().tilemap_cells(), 0);
}

#[test]
fn the_aggregate_cell_budget_latches_with_the_map_count_nowhere_near_full() {
    // The other side of the pair: two maps is nowhere near the count, so
    // nothing but the cell budget can refuse the third, and the message has to
    // say so. Together with the fixture above this pins both directions - a
    // table checking only one budget passes exactly one of them.
    //
    // Both halves of "nowhere near the count, exactly on the cells" are gated,
    // for the same reason as the fixture above: they are what make the expected
    // message the only possible one.
    const FULL_MAP: u32 = 512 * 512;
    const _: () = assert!(2 * FULL_MAP == MAX_AGGREGATE_CELLS);
    const _: () = assert!(3 < MAX_TILEMAPS);
    let root = game(
        r#"
        local function full()
            local cells = {}
            for index = 1, 512 * 512 do cells[index] = 0 end
            return {
                columns = 512, rows = 512, tile_width = 8, tile_height = 8,
                origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
            }
        end
        local function one_cell()
            return {
                columns = 1, rows = 1, tile_width = 8, tile_height = 8,
                origin_x = 0, origin_y = 0, solids = {true}, cells = {0},
            }
        end
        local held = {}
        return {init = function(ctx)
            local description = full()
            held[1] = ctx.world.create_tilemap(description)
            held[2] = ctx.world.create_tilemap(description)
            ctx.log('created 2')
            -- The two maps above are the whole aggregate, so a single further
            -- cell is over it. The figures are in the `const _` gate beside the
            -- fixture rather than restated here: a number inside this string is
            -- a second copy that a diff of the Rust comment cannot show.
            pcall(ctx.world.create_tilemap, one_cell())
            ctx.log('unreachable')
        end}
    "#,
    );
    let mut runtime = load_patient(&root);
    let error = runtime
        .init()
        .expect_err("the aggregate cell budget must refuse the third map, uncatchably");
    assert_eq!(error.phase, "init");
    assert!(
        error.message.contains("aggregate cell limit exceeded"),
        "a candidate under the count and over the cells must name the cells: {}",
        error.message
    );
    assert_eq!(runtime.state(), ScriptState::Faulted);
    assert_eq!(runtime.take_logs(), ["created 2"]);
    // Released by the fault's `stop`, exactly as above. Half a megabyte of
    // cells is the largest release either fixture reaches.
    assert_eq!(runtime.kernel().tilemap_count(), 0);
    assert_eq!(runtime.kernel().tilemap_cells(), 0);
}

#[test]
fn a_standalone_script_host_still_has_no_world() {
    let root = game(
        r#"
        return {init = function(ctx)
            assert(ctx.world == nil, 'a host without a kernel exposes no world')
            assert(ctx.input == nil)
            assert(ctx.data ~= nil and ctx.fs ~= nil and ctx.assets ~= nil)
        end}
    "#,
    );
    let mut host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
    host.init().unwrap();
    assert_eq!(host.state(), ScriptState::Running);
}
