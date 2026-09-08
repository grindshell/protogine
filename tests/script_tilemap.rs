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
    input::InputSnapshot,
    kernel::{FIXED_DT, Position},
    runtime::GameRuntime,
    scripting::{ScriptHost, ScriptLimits, ScriptState},
};
use std::{fs, time::Duration};
use tempfile::TempDir;

/// A 6x4 room of 32-pixel tiles with a solid border and a solid column 4, so
/// the free interior is columns 1..3 of rows 1..2 and a 32x32 body at (32, 32)
/// travelling right stops with its leading edge on the face at x = 128.
const ROOM: &str = r#"
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
local function box(size)
    return {offset_x = 0, offset_y = 0, width = size, height = size}
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
                assert(w.tilemap_info() == nil, 'no map is installed yet')
                assert(not pcall(w.tile, 0, 0), 'a read needs an installed map')
                assert(not pcall(w.set_tile, 0, 0, 1))
                w.set_tilemap(room())
                local info = w.tilemap_info()
                assert(info.columns == 6 and info.rows == 4)
                assert(info.tile_width == 32 and info.tile_height == 32)
                assert(info.origin_x == 0 and info.origin_y == 0)
                assert(w.tile(0, 0) == 1 and w.tile(1, 1) == 0)
                assert(w.tile_solid(4, 1) and not w.tile_solid(1, 1))
                -- Outside the installed map is solid (T3), and this is the one
                -- call that accepts indices the grid does not contain.
                assert(w.tile_solid(-1, 1) and w.tile_solid(6, 1))
                assert(not pcall(w.tile, -1, 1), 'tile() requires in-bounds indices')
                local ids = w.tiles_region(0, 1, 6, 1)
                assert(#ids == 6, 'a region returns one ID per cell')
                assert(ids[1] == 1 and ids[2] == 0 and ids[4] == 0 and ids[5] == 1)
                body = w.spawn(32, 32)
                assert(w.tile_collider(body) == nil, 'a fresh entity has no collider')
                w.set_tile_collider(body, box(32))
                local collider = w.tile_collider(body)
                assert(collider.offset_x == 0 and collider.offset_y == 0)
                assert(collider.width == 32 and collider.height == 32)
                w.set_velocity(body, 120, 0)
            end,
            draw = function(ctx)
                -- Draw reads the committed position and the authoritative map.
                assert(ctx.world.tilemap_info().columns == 6)
                assert(ctx.world.position(body).x <= 96)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(positions(&runtime), [Position { x: 32.0, y: 32.0 }]);
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
    assert_eq!(positions(&runtime), [Position { x: 96.0, y: 32.0 }]);
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
                w.set_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(32))
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
        [Position { x: 96.0, y: 64.0 }],
        "one tick must stop at the first solid face on each axis, not at its endpoint"
    );
    // Five more ticks of the same speed: a body already flush with a face is
    // never pushed through it and never nudged back off it either.
    step(&mut runtime, 5);
    assert_eq!(positions(&runtime), [Position { x: 96.0, y: 64.0 }]);
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
                w.set_tilemap(description)
                -- The description was copied, not adopted: nothing a caller
                -- does to it afterwards can reach the installed map.
                description.columns = 99
                description.cells[8] = 1
                description.solids[1] = false
                assert(w.tilemap_info().columns == 6)
                assert(w.tile(1, 1) == 0 and not w.tile_solid(1, 1))
                assert(w.tile_solid(0, 0), 'the copied solid definitions still stand')

                local info = w.tilemap_info()
                info.columns = 99
                assert(w.tilemap_info().columns == 6, 'info is a snapshot')

                kept = w.tiles_region(0, 1, 6, 1)
                kept[2] = 7
                assert(w.tiles_region(0, 1, 6, 1)[2] == 0, 'a region is a snapshot')

                body = w.spawn(32, 32)
                local options = box(32)
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
                ctx.world.set_tile(2, 1, 1)
                assert(kept[3] == 0 and ctx.world.tiles_region(0, 1, 6, 1)[3] == 1)
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert!(runtime.kernel().tile_solid(2, 1).unwrap());
}

#[test]
fn the_description_schema_refuses_every_malformed_shape_catchably() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            w.set_tilemap(room())
            local refused = 0
            local function refuse(mutate, what)
                local description = room()
                mutate(description)
                assert(not pcall(w.set_tilemap, description), what)
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

            assert(not pcall(w.set_tilemap), 'a missing description')
            assert(not pcall(w.set_tilemap, 5), 'a description that is not a table')
            assert(refused == 31, 'every case above must have been exercised')

            -- Every one of those was catchable and none of them changed the map.
            local info = w.tilemap_info()
            assert(info.columns == 6 and info.rows == 4 and info.origin_x == 0)
            assert(w.tile(1, 1) == 0 and w.tile(0, 0) == 1)
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    let info = runtime.kernel().tilemap().unwrap().unwrap();
    assert_eq!((info.columns, info.rows), (6, 4));
}

#[test]
fn arguments_and_collider_options_are_refused_without_narrowing() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            w.set_tilemap(room())
            local body = w.spawn(32, 32)

            -- Tile coordinates are exact signed 32-bit integers. A larger
            -- number is a malformed call, never a scan toward the grid.
            for _, index in {'1', true, 1.5, 0/0, math.huge, 2147483648, -2147483649} do
                assert(not pcall(w.tile, index, 1), 'tile column')
                assert(not pcall(w.tile, 1, index), 'tile row')
                assert(not pcall(w.tile_solid, index, 1), 'tile_solid column')
                assert(not pcall(w.set_tile, index, 1, 0), 'set_tile column')
                assert(not pcall(w.tiles_region, index, 1, 1, 1), 'region column')
            end
            assert(not pcall(w.tile, 1), 'a missing row')
            assert(not pcall(w.tile), 'no arguments at all')
            for _, extent in {'1', 1.5, -1, 0/0, 4294967296} do
                assert(not pcall(w.tiles_region, 0, 0, extent, 1), 'region width')
            end
            assert(not pcall(w.tiles_region, 0, 0, 0, 1), 'an empty region')
            assert(not pcall(w.tiles_region, 0, 0, 7, 1), 'a region past the far edge')
            assert(not pcall(w.tiles_region, 0, 0, 4097, 1), 'a region past the per-call cap')
            -- A request larger than the whole per-callback output ceiling is
            -- still an ordinary bounds error. It is charged for what a region
            -- read could have returned, never for what it asked for, so an
            -- out-of-range argument cannot latch the session on its own.
            for _ = 1, 8 do
                assert(not pcall(w.tiles_region, 0, 0, 600, 600), 'a request larger than the ceiling')
            end
            for _, id in {'1', true, 1.5, -1, 65536} do
                assert(not pcall(w.set_tile, 1, 1, id), 'tile ID')
            end
            assert(not pcall(w.set_tile, 1, 1, 2), 'an ID this map does not define')

            -- Collider options carry exactly four numbers.
            local function refuse(mutate, what)
                local options = box(32)
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
            w.set_tile_collider(body, {offset_x = 0.5, offset_y = 0.5, width = 1/256, height = 8})
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
        local body
        local function readonly(ctx, phase)
            local w = ctx.world
            assert(not pcall(w.set_tilemap, room()), phase .. ': set_tilemap must refuse')
            assert(not pcall(w.clear_tilemap), phase .. ': clear_tilemap must refuse')
            assert(not pcall(w.set_tile, 1, 1, 1), phase .. ': set_tile must refuse')
            assert(not pcall(w.set_tile_collider, body, box(32)), phase .. ': attaching must refuse')
            assert(not pcall(w.set_tile_collider, body, nil), phase .. ': detaching must refuse')
            -- Reads stay available in every live callback.
            assert(w.tilemap_info().columns == 6)
            assert(w.tile(1, 1) == 0 and not w.tile_solid(1, 1))
            assert(#w.tiles_region(1, 1, 3, 2) == 6)
            assert(w.tile_collider(body).width == 32)
        end
        return {
            init = function(ctx)
                ctx.world.set_tilemap(room())
                body = ctx.world.spawn(32, 32)
                ctx.world.set_tile_collider(body, box(32))
            end,
            draw = function(ctx) readonly(ctx, 'draw') end,
            shutdown = function(ctx) readonly(ctx, 'shutdown') end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.kernel().tilemap_storage_bytes(), 49);
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
                w.set_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(32))
                stale_world = w
                dead = w.spawn(64, 32)
                w.set_tile_collider(dead, box(32))
                w.despawn(dead)
                assert(not pcall(w.tile_collider, dead), 'a despawned handle')
                assert(not pcall(w.set_tile_collider, dead, box(32)), 'a despawned handle')
                assert(not pcall(w.tile_collider, 5), 'not a handle at all')
                assert(not pcall(w.tile_collider), 'no handle at all')
                assert(not pcall(w.set_tile_collider, box(32), box(32)), 'a table is not a handle')
            end,
            update = function(ctx)
                local w = ctx.world
                -- The previous callback's functions are expired, and the new
                -- ones are scoped exactly as the old ones already were.
                for _, name in {'set_tilemap', 'clear_tilemap', 'tilemap_info', 'tile',
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
                w.set_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(32))
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
                assert(not pcall(w.set_tile, 3, 2, 1), 'a solid edit under the body')
                assert(w.tile(3, 2) == 0, 'the refused edit changed no cell')
                w.set_tile(1, 1, 1)
                assert(w.tile(1, 1) == 1 and w.tile_solid(1, 1))

                -- T6: a replacement that would trap the body refuses.
                local trap = room()
                trap.cells[2 * 6 + 3 + 1] = 1
                assert(not pcall(w.set_tilemap, trap), 'a map that would trap the body')
                assert(w.tile(1, 1) == 1, 'the installed map survived the refusal')

                -- T6: the map cannot be cleared beneath a body, and the M1
                -- transition recipe is detach, clear, reinstall, reattach.
                assert(not pcall(w.clear_tilemap), 'clearing beneath a body')
                w.set_tile_collider(body, nil)
                w.clear_tilemap()
                assert(w.tilemap_info() == nil)
                assert(not pcall(w.set_tile_collider, body, box(32)), 'attaching with no map')
                w.set_tilemap(room())
                w.set_position(body, 32, 32)
                w.set_tile_collider(body, box(32))
                assert(w.tile_collider(body).width == 32)
                assert(w.tile(1, 1) == 0, 'the reinstalled map is the described one')
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    step(&mut runtime, 1);
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.kernel().live_colliders(), 1);
    assert_eq!(positions(&runtime), [Position { x: 32.0, y: 32.0 }]);
}

#[test]
fn the_aggregate_tile_work_ceiling_latches_outside_pcall() {
    // 128 x 128 cells plus one solid flag is 16,385 units per install, so the
    // 64th crosses 1,048,576 partway through its cells and the 63 before it fit.
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
            for _ = 1, 63 do ctx.world.set_tilemap(description) end
            ctx.log('installed 63')
            -- The ceiling is not catchable: it latches like every other
            -- aggregate budget, so this pcall cannot swallow it.
            pcall(ctx.world.set_tilemap, description)
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
            init = function(ctx) ctx.world.set_tilemap(grid()) end,
            update = function(ctx)
                local w = ctx.world
                -- 4,096 IDs a call, 262,144 a callback: exactly 64 whole reads.
                for _ = 1, 64 do assert(#w.tiles_region(0, 0, 64, 64) == 4096) end
                ctx.log('read 64 regions')
                pcall(w.tiles_region, 0, 0, 1, 1)
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
    // The ceiling is per callback, so the first tick's 64 reads were all served.
    assert_eq!(runtime.completed_ticks(), 0);
}

#[test]
fn a_refused_region_is_charged_for_what_it_could_have_returned() {
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            w.set_tilemap(room())
            -- A request no map could serve is still charged, before either
            -- allocation, so a refusal is bounded rather than free to repeat.
            -- 4,096 is the per-call cap, so 64 of them is the whole ceiling.
            for _ = 1, 64 do
                assert(not pcall(w.tiles_region, 0, 0, 64, 64), 'past the far edge')
            end
            ctx.log('64 refusals')
            pcall(w.tiles_region, 0, 0, 1, 1)
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
            w.set_tilemap(room())
            -- Bodies do not block one another, so all of them fit in one cell.
            for _ = 1, 1024 do
                w.set_tile_collider(w.spawn(32, 32), box(32))
            end
            ctx.log('attached 1024')
            pcall(w.set_tile_collider, w.spawn(32, 32), box(32))
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
            w.set_tilemap(room())
            -- One counter, not one per family: 2,048 entity reads plus 2,048
            -- map reads is the 4,096th attempt, and this call is the 4,097th.
            for _ = 1, 2047 do w.entities() end
            for _ = 1, 2048 do w.tile_solid(0, 0) end
            ctx.log('4096 attempts')
            pcall(w.tile_solid, 0, 0)
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
            init = function(ctx) ctx.world.set_tilemap(room()) end,
            draw = function(ctx)
                local w = ctx.world
                -- A refused call has still been attempted: malformed arguments
                -- and wrong-phase mutations are counted before conversion.
                for _ = 1, 2048 do assert(not pcall(w.tile, 'x', 'y')) end
                for _ = 1, 2047 do assert(not pcall(w.set_tile, 1, 1, 0)) end
                ctx.log('4095 refusals')
                pcall(w.tile, 'x', 'y')
                pcall(w.tile, 'x', 'y')
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
            w.set_tilemap(room())
            local body = w.spawn(32, 32)
            w.set_tile_collider(body, box(32))
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
    assert_eq!(positions(&runtime)[0], Position { x: 32.0, y: 32.0 });
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
                w.set_tilemap(room())
                local body = w.spawn(32, 32)
                w.set_tile_collider(body, box(32))
                w.set_velocity(body, 120, 0)
            end,
            update = function(ctx)
                ticks += 1
                if ticks == 1 then return end
                -- An accepted write, then a failure in the same callback.
                ctx.world.set_tile(2, 1, 1)
                assert(ctx.world.tile(2, 1) == 1, 'the write took effect')
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
                w.set_tilemap(room())
                body = w.spawn(32, 32)
                w.set_tile_collider(body, box(32))
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
            w.set_tilemap(room())
            local first = w.spawn(32, 32)
            w.set_tile_collider(first, box(32))
            w.set_velocity(first, 120, 0)
            local second = w.spawn(32, 64)
            w.set_tile_collider(second, box(32))
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
                w.set_tilemap(room())
                for _, handle in w.entities() do w.set_tile_collider(handle, box(32)) end
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
    // The fast body reaches the wall at 96 after 32 ticks and holds it; the
    // slow one is still travelling at 72 after 40.
    assert_eq!(
        results[0],
        [Position { x: 96.0, y: 32.0 }, Position { x: 72.0, y: 64.0 }]
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
            init = function(ctx) ctx.world.set_tilemap(room()) end,
            update = function(ctx)
                -- 25 units of description plus nothing else, every tick. If the
                -- accounting did not restart, a long session would eventually
                -- refuse an install that fits on its own.
                for _ = 1, 100 do ctx.world.set_tilemap(room()) end
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    // 25 units per install, so a callback's 100 installs cost 2,500 and a
    // thousand ticks would exceed the ceiling if it never reset.
    for tick in 0..500 {
        runtime
            .step(InputSnapshot::default())
            .unwrap_or_else(|error| {
                panic!("tile-work accounting must restart each callback, tick {tick}: {error}")
            });
    }
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.completed_ticks(), 500);
}

#[test]
fn no_map_mutation_returns_a_value_that_could_fail_to_allocate() {
    // `spawn` needs a rollback path because it mutates and then allocates a
    // handle to publish, and a failure between the two would leave an entity
    // nobody can name; `tests/scripting.rs` and the unit harness beside the
    // bindings cover that. None of the calls added here has that shape, and this
    // is what says so: they all mutate and return nothing, so there is no window
    // between publishing a change and allocating something to report it. Adding
    // a return value to one of them would fail here, which is the point - that
    // change would need a rollback path, and nothing else would ask for one.
    let root = room_game(
        r#"
        return {init = function(ctx)
            local w = ctx.world
            local body = w.spawn(32, 32)
            assert(select('#', w.set_tilemap(room())) == 0, 'set_tilemap returns nothing')
            assert(select('#', w.set_tile(2, 1, 1)) == 0, 'set_tile returns nothing')
            assert(select('#', w.set_tile_collider(body, box(32))) == 0,
                'set_tile_collider returns nothing')
            assert(select('#', w.set_tile_collider(body, nil)) == 0,
                'detaching returns nothing')
            assert(select('#', w.clear_tilemap()) == 0, 'clear_tilemap returns nothing')
        end}
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
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
