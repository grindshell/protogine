//! Colliders, the swept solver and the collider invariants every kernel
//! mutation must preserve.
//!
//! Geometry is specified independently of the production sweep: fixtures state
//! the expected face arithmetic directly, and the randomized battery compares
//! against an integer oracle written in exact 1/256-pixel units. Runs in the
//! core configuration, with no decoder, VM or graphics context.

use protogine::collision::{
    self, CollisionError, MAX_CALLBACK_WORK, MAX_FIXED_PASS_WORK, MAX_LIVE_COLLIDERS,
    MIN_COLLIDER_EXTENT, TileCollider, WorkBudget,
};
use protogine::kernel::{
    ColliderPlacement, EntityHandle, FIXED_DT, Kernel, KernelError, Position, TileMapHandle,
    Velocity,
};
use protogine::tilemap::{Axis, GEOMETRY_LIMIT, TileMap, TileMapError, TileMapInfo};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Tile ID 1: solid.
const WALL: u16 = 1;
/// Tile ID 2: a non-solid ID above 0, so ID 0 is not the only clear tile.
const FLOOR: u16 = 2;

/// Build a map from ASCII rows. `#` is solid, `.` is ID 0, `o` is the non-solid
/// ID above 0.
fn build(rows: &[&str], tile_width: u32, tile_height: u32, origin: (i32, i32)) -> TileMap {
    let columns = rows[0].len() as u32;
    let mut cells = Vec::new();
    for row in rows {
        assert_eq!(
            row.len() as u32,
            columns,
            "fixture rows must be equal width"
        );
        for glyph in row.chars() {
            cells.push(match glyph {
                '#' => WALL,
                '.' => 0,
                'o' => FLOOR,
                other => panic!("unknown fixture tile {other:?}"),
            });
        }
    }
    TileMap::new(
        TileMapInfo {
            columns,
            rows: rows.len() as u32,
            tile_width,
            tile_height,
            origin_x: origin.0,
            origin_y: origin.1,
        },
        vec![true, false],
        cells,
    )
    .expect("fixture map is legal")
}

/// A 5x5 room of 32-pixel tiles walled on every side, interior world
/// coordinates `[32, 128)` on both axes.
fn room() -> TileMap {
    build(
        &["#####", "#...#", "#...#", "#...#", "#####"],
        32,
        32,
        (0, 0),
    )
}

fn square(size: f64) -> TileCollider {
    TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: size,
        height: size,
    }
}

fn swept(
    map: &TileMap,
    collider: &TileCollider,
    from: (f64, f64),
    travel: (f64, f64),
) -> (f64, f64) {
    let mut work = WorkBudget::new(MAX_FIXED_PASS_WORK);
    collision::solve(map, collider, from, travel, &mut work).expect("fixture bodies are legal")
}

fn placeable(map: &TileMap, collider: &TileCollider, x: f64, y: f64) -> bool {
    let mut work = WorkBudget::new(MAX_FIXED_PASS_WORK);
    collision::check_placement(map, collider, x, y, &mut work).is_ok()
}

/// A placement joining `map` without moving the body — the shape every fixture
/// here wants, since M2-5 folded transfer into attachment and only a transfer
/// carries a position.
fn on(map: &TileMapHandle, collider: TileCollider) -> Option<ColliderPlacement> {
    Some(ColliderPlacement {
        map: map.clone(),
        collider,
        position: None,
    })
}

/// A kernel with one collider attached to one entity, and the map it joined.
///
/// The map handle is returned because from M2 Phase 2 a collider names its map,
/// and `create_tilemap`'s return is the only way to name it.
fn session(
    map: TileMap,
    at: (f64, f64),
    collider: TileCollider,
) -> (Kernel, EntityHandle, TileMapHandle) {
    let mut kernel = Kernel::new();
    let installed = kernel.create_tilemap(map).expect("install");
    let body = kernel
        .spawn(Position { x: at.0, y: at.1 })
        .expect("spawn the body");
    kernel
        .set_tile_collider(&body, on(&installed, collider))
        .expect("the fixture spawn is a legal placement");
    (kernel, body, installed)
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[test]
fn every_approach_direction_stops_flush_against_the_wall() {
    let map = room();
    let body = square(32.0);
    // The interior is [32, 128); a 32-pixel box therefore rests at 32 against
    // the near wall and at 96 against the far one, on either axis.
    for (travel, expected) in [
        ((1000.0, 0.0), (96.0, 64.0)),
        ((-1000.0, 0.0), (32.0, 64.0)),
        ((0.0, 1000.0), (64.0, 96.0)),
        ((0.0, -1000.0), (64.0, 32.0)),
    ] {
        assert_eq!(
            swept(&map, &body, (64.0, 64.0), travel),
            expected,
            "travel {travel:?} must stop flush"
        );
    }
}

#[test]
fn a_touching_wall_blocks_only_movement_into_it() {
    let map = room();
    let body = square(32.0);
    // Flush against the right wall: its face is exactly the box's high edge.
    let flush = (96.0, 64.0);
    assert_eq!(
        collision::check_placement(&map, &body, flush.0, flush.1, &mut WorkBudget::new(64)),
        Ok(()),
        "edge contact is free: a flush box must not overlap the cell beyond it"
    );
    assert_eq!(
        swept(&map, &body, flush, (5.0, 0.0)),
        flush,
        "moving into a touching face yields zero displacement, never a push"
    );
    assert_eq!(
        swept(&map, &body, flush, (-32.0, 0.0)),
        (64.0, 64.0),
        "moving away from a touching face is free"
    );
    assert_eq!(
        swept(&map, &body, flush, (0.0, 32.0)),
        (96.0, 96.0),
        "moving tangent along a touching face is free"
    );
}

#[test]
fn an_interior_solid_cell_cannot_hide_between_clear_corners() {
    // The solid sits in the middle row of the column the body enters, so all
    // four of the body's corners clear it.
    let map = build(
        &["########", "#......#", "#..#...#", "#......#", "########"],
        32,
        32,
        (0, 0),
    );
    let body = TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: 32.0,
        height: 96.0,
    };
    let resolved = swept(&map, &body, (32.0, 32.0), (1000.0, 0.0));
    assert_eq!(
        resolved,
        (64.0, 32.0),
        "the interior solid cell at column 3 must stop the body at face 96"
    );

    // A placement check enumerates its own columns, so it needs its own case:
    // this body's outer columns are clear and only the middle one is not.
    let wide = TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: 96.0,
        height: 32.0,
    };
    assert!(
        !placeable(&map, &wide, 64.0, 64.0),
        "placement must see the interior solid column, not only the outer two"
    );
    assert!(
        placeable(&map, &wide, 128.0, 64.0),
        "the clear span beside it stays free"
    );
}

#[test]
fn offsets_move_the_box_without_moving_the_position() {
    let map = room();
    let body = TileCollider {
        offset_x: -16.0,
        offset_y: -8.0,
        width: 32.0,
        height: 32.0,
    };
    // The high edge is `position + offset + size`, so a -16 offset lets the
    // position itself sit 16 pixels past the face the box stops on.
    assert_eq!(swept(&map, &body, (64.0, 64.0), (1000.0, 0.0)).0, 112.0);
    assert_eq!(swept(&map, &body, (64.0, 64.0), (-1000.0, 0.0)).0, 48.0);
    assert_eq!(swept(&map, &body, (64.0, 64.0), (0.0, 1000.0)).1, 104.0);
    assert_eq!(swept(&map, &body, (64.0, 64.0), (0.0, -1000.0)).1, 40.0);
}

#[test]
fn the_smallest_legal_box_stops_on_the_same_faces() {
    let map = room();
    let body = square(MIN_COLLIDER_EXTENT);
    assert_eq!(
        swept(&map, &body, (64.0, 64.0), (1000.0, 0.0)).0,
        128.0 - MIN_COLLIDER_EXTENT,
        "a 1/256-pixel box rests exactly its own width short of the face"
    );
    assert_eq!(swept(&map, &body, (64.0, 64.0), (-1000.0, 0.0)).0, 32.0);
}

#[test]
fn fractional_coordinates_land_exactly_on_the_face() {
    let map = room();
    let body = square(32.0);
    // A fractional start that overshoots resolves to the exact integer face.
    assert_eq!(swept(&map, &body, (33.5, 64.0), (1000.0, 0.0)).0, 96.0);
    // A fractional travel that does not reach the wall keeps its own
    // destination, bit for bit.
    let start = 33.5;
    let travel = 0.1;
    assert_eq!(
        swept(&map, &body, (start, 64.0), (travel, 0.0)).0,
        start + travel
    );
}

// ---------------------------------------------------------------------------
// Sweeps
// ---------------------------------------------------------------------------

/// A 20x3 corridor whose middle row is open except for one-cell walls.
fn corridor(walls: &[usize]) -> TileMap {
    let mut middle = vec!['.'; 20];
    middle[0] = '#';
    middle[19] = '#';
    for wall in walls {
        middle[*wall] = '#';
    }
    let middle: String = middle.into_iter().collect();
    let edge = "#".repeat(20);
    build(&[&edge, &middle, &edge], 32, 32, (0, 0))
}

#[test]
fn travel_across_many_tiles_stops_at_a_one_cell_wall() {
    let map = corridor(&[9]);
    let body = square(32.0);
    // Ten tiles of travel toward a single blocking cell whose left face is 288.
    // The destination itself is free, so an endpoint-only test would tunnel
    // straight to 352 rather than stopping.
    let resolved = swept(&map, &body, (32.0, 32.0), (320.0, 0.0));
    assert_eq!(
        resolved,
        (256.0, 32.0),
        "high-speed wall stop: the body must stop at 256, not tunnel past 288"
    );
    assert!(
        placeable(&map, &body, 352.0, 32.0),
        "the destination is free"
    );
}

#[test]
fn the_nearer_of_two_walls_wins() {
    let map = corridor(&[5, 9]);
    let body = square(32.0);
    assert_eq!(
        swept(&map, &body, (32.0, 32.0), (1000.0, 0.0)).0,
        128.0,
        "the wall at column 5 is nearer than the one at column 9"
    );
    // The mirror: starting past both, the nearer one on the way back wins.
    assert_eq!(
        swept(&map, &body, (352.0, 32.0), (-1000.0, 0.0)).0,
        320.0,
        "the wall at column 9 is nearer on the way back"
    );
}

#[test]
fn an_enormous_finite_velocity_stops_at_the_map_boundary() {
    // The middle row runs off the right edge of the map, so nothing but the
    // map's own boundary can stop the body.
    let edge = "#".repeat(20);
    let middle = format!("#{}", ".".repeat(19));
    let map = build(&[&edge, &middle, &edge], 32, 32, (0, 0));
    let body = square(32.0);
    for travel in [1.0e9, 1.0e18, f64::MAX / 4.0] {
        assert_eq!(
            swept(&map, &body, (32.0, 32.0), (travel, 0.0)).0,
            608.0,
            "travel {travel:e} must clamp at the far edge 640, not scan toward it"
        );
    }
}

#[test]
fn the_diagonal_corner_resolves_x_before_y() {
    // One solid diagonal neighbour. X-first yields (32, 0) of movement and
    // Y-first yields (0, 32), so the axis order is observable here (N7).
    let map = build(
        &["#####", "#...#", "#.#.#", "#...#", "#####"],
        32,
        32,
        (0, 0),
    );
    let body = square(32.0);
    assert_eq!(
        swept(&map, &body, (32.0, 32.0), (32.0, 32.0)),
        (64.0, 32.0),
        "x-before-y corner: X resolves fully, then Y is blocked from the resolved X"
    );
}

#[test]
fn a_blocked_axis_still_slides_along_the_other() {
    let map = room();
    let body = square(32.0);
    // Pressed into the right wall while also moving down: X yields nothing, Y
    // travels in full.
    assert_eq!(
        swept(&map, &body, (96.0, 32.0), (32.0, 32.0)),
        (96.0, 64.0),
        "wall sliding: a blocked X must not cancel the Y displacement"
    );
}

#[test]
fn a_body_flush_with_the_map_boundary_neither_moves_nor_visits_a_cell() {
    // Distinct from resting against an interior wall, and reached by a distinct
    // path: `cell_at` saturates to the cell count, the boundary face equals the
    // leading edge so no increment happens, and the loop takes the `index >=
    // count` branch. That branch returns before any cell is inspected, which is
    // also why the two boundary faces contribute nothing to the work bound.
    let map = build(&["....", "....", "....", "...."], 32, 32, (0, 0));
    let offset_body = TileCollider {
        offset_x: -4_096.0,
        offset_y: 0.0,
        // One representable step off a whole number, so the reconstruction is
        // inexact rather than landing on the face by luck.
        width: 32.0_f64.next_up(),
        height: 32.0,
    };
    for (body, start) in [(square(32.0), 32.0), (offset_body, 4_100.0)] {
        assert!(placeable(&map, &body, start, 0.0), "the start is legal");
        let mut reaching = WorkBudget::new(MAX_FIXED_PASS_WORK);
        let flush = collision::solve(&map, &body, (start, 0.0), (1.0e6, 0.0), &mut reaching)
            .expect("reaching the far boundary is legal");
        assert!(
            placeable(&map, &body, flush.0, flush.1),
            "the flush position must itself be a legal placement"
        );
        assert!(
            body.aabb(flush.0, flush.1).high(Axis::X) <= map.high(Axis::X),
            "the flush box must not reconstruct past the boundary"
        );

        let mut pressing = WorkBudget::new(MAX_FIXED_PASS_WORK);
        assert_eq!(
            collision::solve(&map, &body, flush, (64.0, 0.0), &mut pressing),
            Ok(flush),
            "a body already flush with the far boundary must not move"
        );
        assert_eq!(
            pressing.used(),
            0,
            "the boundary face charges no cell visit, which is what keeps it out of the work bound"
        );

        // The mirror at the near edge, entered the same way.
        let mut returning = WorkBudget::new(MAX_FIXED_PASS_WORK);
        let low = collision::solve(&map, &body, flush, (-1.0e6, 0.0), &mut returning)
            .expect("reaching the near boundary is legal");
        assert!(placeable(&map, &body, low.0, low.1));
        assert!(body.aabb(low.0, low.1).low(Axis::X) >= map.low(Axis::X));
        let mut pressing_low = WorkBudget::new(MAX_FIXED_PASS_WORK);
        assert_eq!(
            collision::solve(&map, &body, low, (-64.0, 0.0), &mut pressing_low),
            Ok(low),
            "a body already flush with the near boundary must not move"
        );
        assert_eq!(pressing_low.used(), 0);
    }
}

#[test]
fn a_body_already_past_its_blocking_face_faults_instead_of_retreating() {
    // `solve` does not check that its starting box is legal, so a caller can
    // reach the arm where N5's fallback would be unsound. The kernel's own
    // guards make this unreachable through the entry points, but the rule has
    // to be observable somewhere or it is only an argument.
    //
    // An unwalled map, so the blocking face is the map's own edge and the body
    // can start beyond it.
    let map = build(&["....", "....", "....", "...."], 32, 32, (0, 0));
    let body = square(32.0);
    let mut work = WorkBudget::new(MAX_FIXED_PASS_WORK);
    assert_eq!(
        collision::solve(&map, &body, (128.0, 32.0), (64.0, 0.0), &mut work),
        Err(CollisionError::Embedded),
        "a start beyond the blocking face must fault, never retreat to itself"
    );
    // The mirror, moving the other way off the near edge.
    assert_eq!(
        collision::solve(&map, &body, (-32.0, 32.0), (-64.0, 0.0), &mut work),
        Err(CollisionError::Embedded)
    );
    // A start inside a solid cell but moving out of it is not this case: the
    // body leaves rather than penetrating, which is deliberate and allowed.
    assert_eq!(
        collision::solve(&pillar(), &body, (64.0, 64.0), (32.0, 0.0), &mut work),
        Ok((96.0, 64.0))
    );
}

#[test]
fn public_geometry_entry_points_answer_rather_than_diverging_by_profile() {
    let map = room();
    let body = square(32.0);
    let mut work = WorkBudget::new(MAX_FIXED_PASS_WORK);
    // A non-finite displacement must refuse identically in both profiles, not
    // assert in debug and commit a NaN position in release.
    for travel in [(f64::NAN, 0.0), (0.0, f64::INFINITY)] {
        assert_eq!(
            collision::solve(&map, &body, (64.0, 64.0), travel, &mut work),
            Err(CollisionError::Nonfinite),
            "travel {travel:?} must be refused"
        );
    }
    assert_eq!(
        collision::solve(&map, &body, (f64::NAN, 64.0), (1.0, 0.0), &mut work),
        Err(CollisionError::Nonfinite)
    );

    // `check_placement` is the only thing standing between a non-finite box and
    // the grid: it is public, and it enforces neither `finite` nor
    // `TileCollider::check`, so it must refuse one itself rather than convert it
    // to an index. Saturation and the solid exterior between them refuse a
    // merely out-of-range box, but they accept a NaN edge, whose comparisons
    // are all false.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for body in [
            TileCollider {
                offset_x: bad,
                offset_y: 0.0,
                width: 32.0,
                height: 32.0,
            },
            TileCollider {
                offset_x: 0.0,
                offset_y: 0.0,
                width: bad,
                height: 32.0,
            },
        ] {
            assert!(
                !placeable(&map, &body, 64.0, 64.0),
                "a {bad} edge must be refused, not converted to a cell index"
            );
        }
        assert!(
            !placeable(&map, &square(32.0), bad, 64.0),
            "a {bad} position must be refused"
        );
    }

    // A cell coordinate outside the grid names no cell, and must answer instead
    // of overflowing `index + 1`.
    for (column, row) in [(i32::MAX, 0), (0, i32::MAX), (i32::MIN, 0), (-1, -1)] {
        assert!(
            !collision::overlaps_cell(&map, &body, 64.0, 64.0, column, row),
            "cell {column},{row} is outside the grid, so nothing overlaps it"
        );
    }
    assert!(collision::overlaps_cell(&map, &body, 64.0, 64.0, 2, 2));
}

// ---------------------------------------------------------------------------
// The clamped position at the geometry limit
// ---------------------------------------------------------------------------

/// An 8x8 map of 1,024-pixel tiles whose far edges land exactly on +2^24.
fn limit_map() -> TileMap {
    let origin = (GEOMETRY_LIMIT - 8 * 1_024) as i32;
    build(
        &[
            "........", "........", "........", "........", "........", "........", "........",
            "........",
        ],
        1_024,
        1_024,
        (origin, origin),
    )
}

#[test]
fn the_clamped_box_stays_clear_of_the_boundary_face() {
    let map = limit_map();
    let limit = GEOMETRY_LIMIT as f64;
    assert_eq!(map.high(Axis::X), limit);
    // Phase 0's witness: a maximum-extent box whose offset cancels against the
    // face, where the naive subtraction reconstructs past the boundary.
    let body = TileCollider {
        offset_x: -4_096.0,
        offset_y: -4_096.0,
        width: 4_095.666_666_666_666_5,
        height: 4_095.666_666_666_666_5,
    };
    let start = (limit - 4_096.0, limit - 4_096.0);
    assert!(
        placeable(&map, &body, start.0, start.1),
        "the start is legal"
    );

    let naive = (limit - body.width) - body.offset_x;
    assert_eq!(
        (naive + body.offset_x) + body.width,
        16_777_216.000_000_004,
        "the unrepaired subtraction reconstructs past the boundary, so the \
         repair is load-bearing rather than dead code"
    );

    let resolved = swept(&map, &body, start, (1.0e6, 0.0));
    let reconstructed = body.aabb(resolved.0, resolved.1).high(Axis::X);
    assert!(
        reconstructed <= limit,
        "clamped box on the free side: reconstructed {reconstructed} past {limit}"
    );
    assert!(
        limit - reconstructed < MIN_COLLIDER_EXTENT,
        "the repair must cost a rounding step, not a visible gap"
    );
    assert!(
        resolved.0 >= start.0 && resolved.0 <= start.0 + 1.0e6,
        "the clamped position stays inside the requested interval"
    );
}

/// The same far edge with room to the left of it, so a maximum-offset body has
/// somewhere legal to start from.
fn wide_limit_map() -> TileMap {
    let rows: Vec<&str> = vec!["................"; 8];
    build(
        &rows,
        1_024,
        1_024,
        (
            (GEOMETRY_LIMIT - 16 * 1_024) as i32,
            (GEOMETRY_LIMIT - 8 * 1_024) as i32,
        ),
    )
}

#[test]
fn the_clamp_postcondition_holds_over_adjacent_positions_and_extents() {
    let map = wide_limit_map();
    let limit = GEOMETRY_LIMIT as f64;
    let mut repairs = 0;
    let mut cases = 0;
    let mut worst_gap: f64 = 0.0;

    // Walk the representable neighbourhood of the awkward configuration on both
    // the extent and the starting position, which is where N3 recorded that
    // penetration has to be expressed on the edge rather than on the position.
    // The exact witness configuration is the first case, not one step past it.
    let mut width: f64 = 4_095.666_666_666_666_5;
    for _ in 0..64 {
        let mut offset: f64 = -4_096.0;
        for _ in 0..8 {
            let body = TileCollider {
                offset_x: offset,
                offset_y: 0.0,
                width,
                height: 1_024.0,
            };
            let mut start = limit - 8_192.0;
            for _ in 0..64 {
                if !placeable(&map, &body, start, map.low(Axis::Y)) {
                    start = start.next_down();
                    continue;
                }
                cases += 1;
                let resolved = swept(&map, &body, (start, map.low(Axis::Y)), (1.0e6, 0.0));
                let edge = body.aabb(resolved.0, resolved.1).high(Axis::X);
                assert!(
                    edge <= limit,
                    "clamped box on the free side: {edge} past {limit} from start {start}"
                );
                assert!(
                    resolved.0 >= start,
                    "the clamp never moves a body backwards past its start"
                );
                assert!(
                    placeable(&map, &body, resolved.0, map.low(Axis::Y)),
                    "committed position {resolved:?} is not a legal placement"
                );
                worst_gap = worst_gap.max(limit - edge);
                let naive = (limit - width) - offset;
                if (naive + offset) + width > limit {
                    repairs += 1;
                }
                start = start.next_down();
            }
            offset = offset.next_up();
        }
        width = width.next_down();
    }
    assert!(
        cases > 10_000,
        "the battery must actually run: {cases} cases"
    );
    assert!(
        repairs > 0,
        "no case needed repair, so this battery proves nothing about it"
    );
    assert!(
        worst_gap < MIN_COLLIDER_EXTENT,
        "worst residual gap {worst_gap} is larger than the smallest legal box"
    );
}

/// A 16 x 8 map of 1,024-pixel tiles with one solid interior column, placed so
/// that column's near face lands exactly on `face`.
fn interior_wall_map(face: i64, wall: usize) -> TileMap {
    let rows: Vec<String> = (0..8)
        .map(|_| {
            let mut row = vec!['.'; 16];
            row[wall] = '#';
            row.into_iter().collect()
        })
        .collect();
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    build(
        &refs,
        1_024,
        1_024,
        (
            (face - wall as i64 * 1_024) as i32,
            (GEOMETRY_LIMIT - 8 * 1_024) as i32,
        ),
    )
}

#[test]
fn the_clamp_postcondition_holds_against_an_interior_face() {
    // The awkward corner of the interior case, not the common one. The common
    // one is `the_clamp_repair_is_reached_by_ordinary_content`, where about a
    // third of random draws need a repair with no searching at all.
    //
    // Both share the mechanism: the repair bites when the reconstruction lands
    // in a coarser binade than the intermediate `face - size`. What separates
    // them is where that coarseness comes from. Ordinarily it comes from the
    // offset being large next to the face, which is why the rate there climbs
    // with `|offset|` and is exactly zero at offset zero. Here `|offset|` is at
    // most 4,096 against a face near 2^23, a ratio of about 1/2000, so that
    // source is unavailable and the only one left is the face's own alignment.
    // Hence binade-boundary faces and a 2^-31 offset grid: without both, this
    // configuration finds nothing, which says how rare the effect is at this
    // ratio rather than anything about the rule. Walking adjacent *extents*
    // finds nothing here either, for the same reason: the extent is not the
    // sensitive parameter, the offset is.
    let step = 2.0_f64.powi(-31);
    let mut repairs = 0;
    let mut cases = 0;
    let mut worst_gap: f64 = 0.0;

    for (face_value, wall) in [
        (8_388_608_i64, 12_usize), // 2^23
        (4_194_304, 12),           // 2^22
        (12_582_912, 12),          // 3 * 2^22
        (GEOMETRY_LIMIT - 1_024, 15),
    ] {
        let map = interior_wall_map(face_value, wall);
        let face = map.face(Axis::X, wall as i32);
        assert_eq!(face, face_value as f64, "the wall face is where it was put");
        for width_step in 0..4 {
            let width = 4_095.666_666_666_666_5 - f64::from(width_step) * step;
            for offset_step in 0..2_048 {
                let offset = -4_095.0 - f64::from(offset_step) * step;
                let naive = (face - width) - offset;
                if (naive + offset) + width <= face {
                    continue;
                }
                repairs += 1;
                let body = TileCollider {
                    offset_x: offset,
                    offset_y: 0.0,
                    width,
                    height: 1_024.0,
                };
                let mut start = map.low(Axis::X) + 4_098.0;
                for _ in 0..8 {
                    if placeable(&map, &body, start, map.low(Axis::Y)) {
                        cases += 1;
                        let resolved = swept(&map, &body, (start, map.low(Axis::Y)), (1.0e6, 0.0));
                        let edge = body.aabb(resolved.0, resolved.1).high(Axis::X);
                        assert!(
                            edge <= face,
                            "clamped box on the free side: {edge} past interior face {face}"
                        );
                        assert!(
                            placeable(&map, &body, resolved.0, map.low(Axis::Y)),
                            "committed position {resolved:?} is not a legal placement"
                        );
                        worst_gap = worst_gap.max(face - edge);
                    }
                    start = start.next_down();
                }
            }
        }
    }
    assert!(
        repairs > 0,
        "no interior case needed repair, so this battery proves nothing about it"
    );
    assert!(
        cases > 100,
        "the interior battery must actually run: {cases} cases"
    );
    assert!(
        worst_gap < MIN_COLLIDER_EXTENT,
        "worst interior residual gap {worst_gap} is larger than the smallest legal box"
    );
}

/// A value whose fractional part is not a tidy binary fraction, so the
/// arithmetic under test actually rounds.
fn draw(rng: &mut Rng, low: f64, high: f64) -> f64 {
    const SCALE: u64 = 1_000_003;
    low + (rng.next() % SCALE) as f64 * (high - low) / SCALE as f64
}

#[test]
fn the_clamp_repair_is_reached_by_ordinary_content() {
    // The sensitive parameter is the offset's magnitude, not the face's
    // position. When `|offset|` is large next to the face, `(face - size) -
    // offset` lands in a binade far coarser than the face, so reconstructing
    // loses bits that are significant at the face's own scale. That is ordinary
    // cancellation, and a 32-pixel grid with a sprite-anchor offset reaches it:
    // no geometry limit and no power-of-two face are involved.
    let map = build(
        // Full height, so the wall is what stops the body whatever row it is
        // in and the assertion names the face that actually blocked.
        &[".................#.."; 15],
        32,
        32,
        (0, 0),
    );
    let face = map.face(Axis::X, 17);
    assert_eq!(face, 544.0);

    let mut rng = Rng(0x51a7_3c92_ee14_b60d);
    let mut penetrating = 0;
    let mut cases = 0;
    let mut worst_gap: f64 = 0.0;

    for _ in 0..20_000 {
        let offset = -draw(&mut rng, 256.0, 4_096.0);
        let body = TileCollider {
            offset_x: offset,
            offset_y: 0.0,
            width: draw(&mut rng, 8.0, 64.0),
            height: draw(&mut rng, 8.0, 64.0),
        };
        if body.check(&map.info()).is_err() {
            continue;
        }
        let start = draw(&mut rng, -offset, face - offset - body.width);
        let y = draw(&mut rng, 32.0, 320.0);
        if !placeable(&map, &body, start, y) {
            continue;
        }
        cases += 1;
        let naive = (face - body.width) - offset;
        if (naive + offset) + body.width > face {
            penetrating += 1;
        }
        let resolved = swept(&map, &body, (start, y), (1.0e4, 0.0));
        let edge = body.aabb(resolved.0, resolved.1).high(Axis::X);
        assert!(
            edge <= face,
            "clamped box on the free side: {edge} past ordinary face {face} \
             with offset {offset} width {} from start {start}",
            body.width
        );
        assert!(
            placeable(&map, &body, resolved.0, resolved.1),
            "committed position {resolved:?} is not a legal placement"
        );
        worst_gap = worst_gap.max(face - edge);
    }

    assert!(
        cases > 10_000,
        "the ordinary battery must run: {cases} cases"
    );
    // Recorded as a rate rather than a bare "some": the repair is routine here,
    // not a corner. A threshold well under the observed rate, so this fails when
    // the effect disappears rather than when it merely moves.
    assert!(
        penetrating * 100 > cases,
        "only {penetrating} of {cases} ordinary cases needed repair; the \
         unrepaired rule is supposed to be reached routinely at these offsets"
    );
    assert!(
        worst_gap < MIN_COLLIDER_EXTENT,
        "worst ordinary residual gap {worst_gap}"
    );
}

#[test]
fn the_clamp_postcondition_holds_moving_toward_the_low_boundary() {
    // The mirror needs its own witness, and it is not simply the reflection of
    // the other one. `face - offset` and `position + offset` are exact inverses
    // in magnitude, so at a face of exactly -2^24 the second rounding undoes the
    // first and no repair is ever needed. The reachable case is a face one
    // binade below its difference: an integer face just inside 2^24, where
    // `face - offset` rounds away from zero by more than the sum's own ulp.
    let rows: Vec<&str> = vec!["................"; 8];
    let origin = -(GEOMETRY_LIMIT - 1) as i32;
    let map = build(&rows, 1_024, 1_024, (origin, origin));
    let limit = f64::from(origin);
    assert_eq!(map.low(Axis::X), limit);
    let step = f64::from(2.0_f32).powi(-31);
    let mut repairs = 0;
    let mut cases = 0;

    for width_step in 0..4 {
        let width = 4_095.666_666_666_666_5 - f64::from(width_step) * step;
        for offset_step in 0..1_024 {
            let offset = 4_095.0 + f64::from(offset_step) * step;
            let body = TileCollider {
                offset_x: offset,
                offset_y: 0.0,
                width,
                height: 1_024.0,
            };
            let naive = limit - offset;
            if naive + offset >= limit {
                continue;
            }
            repairs += 1;
            let mut start = limit + 4_096.0;
            for _ in 0..8 {
                if !placeable(&map, &body, start, map.low(Axis::Y)) {
                    start = start.next_up();
                    continue;
                }
                cases += 1;
                let resolved = swept(&map, &body, (start, map.low(Axis::Y)), (-1.0e6, 0.0));
                let edge = body.aabb(resolved.0, resolved.1).low(Axis::X);
                assert!(
                    edge >= limit,
                    "clamped box on the free side: {edge} below {limit} from start {start}"
                );
                assert!(
                    resolved.0 <= start,
                    "the clamp never moves a body forwards past its start"
                );
                assert!(
                    placeable(&map, &body, resolved.0, map.low(Axis::Y)),
                    "committed position {resolved:?} is not a legal placement"
                );
                start = start.next_up();
            }
        }
    }
    assert!(
        repairs > 0,
        "no offset needed the mirror repair, so this battery proves nothing about it"
    );
    assert!(cases > 100, "the mirror battery must run: {cases} cases");
}

// ---------------------------------------------------------------------------
// An independent integer oracle, at the geometry limit
// ---------------------------------------------------------------------------

/// Fixed-point unit of the oracle: the smallest legal collider extent.
const SCALE: i64 = 256;

fn div_floor(a: i64, b: i64) -> i64 {
    let quotient = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        quotient - 1
    } else {
        quotient
    }
}

fn div_ceil(a: i64, b: i64) -> i64 {
    -div_floor(-a, b)
}

fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::X => 0,
        Axis::Y => 1,
    }
}

/// The same sweep restated in exact 1/256-pixel integers.
///
/// Phrased on box edges throughout, never on position plus offset, and it
/// shares nothing with the solver but the fixture grid. Every generated value
/// is a multiple of 1/256 inside the geometry domain, so both this and the f64
/// solver are exact on it: what it pins is *which face blocks*, across the whole
/// domain including its edge. How a clamped position rounds is a separate
/// question, settled above against N5's postcondition rather than an oracle
/// value.
struct Oracle {
    count: [i64; 2],
    tile: [i64; 2],
    origin: [i64; 2],
    solid: Vec<bool>,
}

impl Oracle {
    fn face(&self, axis: Axis, index: i64) -> i64 {
        let a = axis_index(axis);
        (self.origin[a] + index * self.tile[a]) * SCALE
    }

    fn solid_at(&self, column: i64, row: i64) -> bool {
        if column < 0 || row < 0 || column >= self.count[0] || row >= self.count[1] {
            return true;
        }
        self.solid[(row * self.count[0] + column) as usize]
    }

    fn span(&self, axis: Axis, low: i64, high: i64) -> (i64, i64) {
        let a = axis_index(axis);
        let step = self.tile[a] * SCALE;
        let base = self.origin[a] * SCALE;
        let first = div_floor(low - base, step);
        let last = div_ceil(high - base, step) - 1;
        (first, last.max(first))
    }

    fn blocked_across(&self, axis: Axis, along: i64, first: i64, last: i64) -> bool {
        (first..=last).any(|across| match axis {
            Axis::X => self.solid_at(along, across),
            Axis::Y => self.solid_at(across, along),
        })
    }

    /// The face that stops travel on `axis`, or `None` when it is all free.
    fn stop(&self, axis: Axis, low: [i64; 2], high: [i64; 2], travel: i64) -> Option<i64> {
        let a = axis_index(axis);
        let other = axis.other();
        let (first, last) = self.span(other, low[axis_index(other)], high[axis_index(other)]);
        let step = self.tile[a] * SCALE;
        let base = self.origin[a] * SCALE;
        let count = self.count[a];
        if travel > 0 {
            let limit = high[a] + travel;
            let mut index = div_ceil(high[a] - base, step);
            loop {
                if self.face(axis, index) >= limit {
                    return None;
                }
                if index >= count {
                    return Some(self.face(axis, count));
                }
                if self.blocked_across(axis, index, first, last) {
                    return Some(self.face(axis, index));
                }
                index += 1;
            }
        } else if travel < 0 {
            let limit = low[a] + travel;
            let mut index = div_floor(low[a] - base, step);
            loop {
                if self.face(axis, index) <= limit {
                    return None;
                }
                if index <= 0 {
                    return Some(self.face(axis, 0));
                }
                if self.blocked_across(axis, index - 1, first, last) {
                    return Some(self.face(axis, index));
                }
                index -= 1;
            }
        } else {
            None
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn range(&mut self, low: i64, high: i64) -> i64 {
        low + (self.next() % ((high - low + 1) as u64)) as i64
    }

    fn pick<T: Copy>(&mut self, options: &[T]) -> T {
        options[(self.next() % options.len() as u64) as usize]
    }
}

fn units(value: i64) -> f64 {
    value as f64 / SCALE as f64
}

#[test]
fn the_sweep_agrees_with_an_integer_oracle_across_the_geometry_domain() {
    let mut rng = Rng(0x2f6e_2b1a_9c04_d7e3);
    let mut checked = 0;
    let mut blocked = 0;
    let mut at_limit = 0;

    for _ in 0..24_000 {
        let tile = [
            rng.pick(&[1_i64, 2, 3, 5, 8, 64, 512, 1_024]),
            rng.pick(&[1_i64, 2, 3, 7, 8, 64, 512, 1_024]),
        ];
        let count = [rng.range(2, 6), rng.range(2, 6)];
        // Sit the map hard against one geometry limit, the other, or near zero.
        let mut origin = [0_i64; 2];
        let placement = rng.range(0, 2);
        for a in 0..2 {
            let extent = count[a] * tile[a];
            origin[a] = match placement {
                0 => GEOMETRY_LIMIT - extent,
                1 => -GEOMETRY_LIMIT,
                _ => rng.range(-64, 64),
            };
        }
        if placement < 2 {
            at_limit += 1;
        }

        let cells = (count[0] * count[1]) as usize;
        let solid: Vec<bool> = (0..cells).map(|_| rng.range(0, 3) == 0).collect();
        let oracle = Oracle {
            count,
            tile,
            origin,
            solid: solid.clone(),
        };
        let map = TileMap::new(
            TileMapInfo {
                columns: count[0] as u32,
                rows: count[1] as u32,
                tile_width: tile[0] as u32,
                tile_height: tile[1] as u32,
                origin_x: origin[0] as i32,
                origin_y: origin[1] as i32,
            },
            vec![true, false],
            solid
                .iter()
                .map(|s| if *s { WALL } else { FLOOR })
                .collect(),
        )
        .expect("generated maps stay inside the schema");

        // Legal collider: offsets and extents in whole 1/256 units, extents
        // capped at eight tiles and 4,096 pixels.
        let mut size_units = [0_i64; 2];
        let mut offset_units = [0_i64; 2];
        let mut fits = true;
        for a in 0..2 {
            let ceiling = (8 * tile[a] * SCALE).min(4_096 * SCALE);
            let extent = count[a] * tile[a] * SCALE;
            if extent < 1 {
                fits = false;
                break;
            }
            size_units[a] = rng.range(1, ceiling.min(extent));
            offset_units[a] = rng.range(-4_096 * SCALE, 4_096 * SCALE);
        }
        if !fits {
            continue;
        }
        let collider = TileCollider {
            offset_x: units(offset_units[0]),
            offset_y: units(offset_units[1]),
            width: units(size_units[0]),
            height: units(size_units[1]),
        };
        assert_eq!(collider.check(&map.info()), Ok(()), "generated collider");

        // A legal starting placement, or skip this grid.
        let mut start_units = [0_i64; 2];
        let mut placed = false;
        for _ in 0..24 {
            for a in 0..2 {
                let low = origin[a] * SCALE - offset_units[a];
                let high =
                    (origin[a] + count[a] * tile[a]) * SCALE - offset_units[a] - size_units[a];
                start_units[a] = rng.range(low, high.max(low));
            }
            let start = (units(start_units[0]), units(start_units[1]));
            if placeable(&map, &collider, start.0, start.1) {
                placed = true;
                break;
            }
        }
        if !placed {
            continue;
        }

        let reach = |a: usize| count[a] * tile[a] * SCALE * 2;
        let travel_units = [
            rng.range(-reach(0), reach(0)),
            rng.range(-reach(1), reach(1)),
        ];
        let travel = (units(travel_units[0]), units(travel_units[1]));
        let start = (units(start_units[0]), units(start_units[1]));

        // The oracle resolves X fully, then Y from the resolved X, in edges.
        let mut low = [
            start_units[0] + offset_units[0],
            start_units[1] + offset_units[1],
        ];
        let mut high = [low[0] + size_units[0], low[1] + size_units[1]];
        let mut expected = [0.0_f64; 2];
        for (index, axis) in [Axis::X, Axis::Y].into_iter().enumerate() {
            let stop = oracle.stop(axis, low, high, travel_units[index]);
            expected[index] = match stop {
                Some(face) if travel_units[index] > 0 => {
                    blocked += 1;
                    low[index] = face - size_units[index];
                    high[index] = face;
                    (units(face) - collider.size(axis)) - collider.offset(axis)
                }
                Some(face) => {
                    blocked += 1;
                    low[index] = face;
                    high[index] = face + size_units[index];
                    units(face) - collider.offset(axis)
                }
                None => {
                    low[index] += travel_units[index];
                    high[index] += travel_units[index];
                    units(start_units[index] + travel_units[index])
                }
            };
        }

        let resolved = swept(&map, &collider, start, travel);
        assert_eq!(
            (resolved.0, resolved.1),
            (expected[0], expected[1]),
            "oracle disagreement: map {count:?} tiles {tile:?} origin {origin:?} \
             collider {collider:?} start {start:?} travel {travel:?}"
        );
        // The premise every guard downstream reasons from: what the solver
        // commits is what the placement check accepts. `set_tile`'s solidity
        // short-circuit, `replace_tilemap`'s revalidation and N5's own fallback
        // argument are all unsound without it, so it is asserted rather than
        // argued.
        assert!(
            placeable(&map, &collider, resolved.0, resolved.1),
            "committed position {resolved:?} is not a legal placement: map \
             {count:?} tiles {tile:?} origin {origin:?} collider {collider:?} \
             start {start:?} travel {travel:?}"
        );
        checked += 1;
    }

    assert!(checked > 8_000, "only {checked} cases reached the oracle");
    assert!(blocked > 4_000, "only {blocked} axes were actually blocked");
    assert!(
        at_limit > 8_000,
        "only {at_limit} grids sat at a geometry limit"
    );
}

// ---------------------------------------------------------------------------
// Placement guards on every mutation (T5, T6)
// ---------------------------------------------------------------------------

/// The corner room, whose only interior wall covers world `[64, 96)` on both
/// axes.
fn pillar() -> TileMap {
    build(
        &["#####", "#...#", "#.#.#", "#...#", "#####"],
        32,
        32,
        (0, 0),
    )
}

#[test]
fn a_teleport_crosses_a_wall_but_never_lands_in_one() {
    let (mut kernel, body, _installed) = session(pillar(), (32.0, 32.0), square(32.0));
    // Across the pillar to the far corner: T5 teleports rather than sweeping.
    assert_eq!(
        kernel.set_position(&body, Position { x: 96.0, y: 96.0 }),
        Ok(())
    );
    assert_eq!(
        kernel.position(&body),
        Ok(Position { x: 96.0, y: 96.0 }),
        "a teleport across a wall to a free cell must succeed"
    );
    for refused in [(64.0, 64.0), (0.0, 0.0), (-100.0, -100.0), (200.0, 64.0)] {
        assert_eq!(
            kernel.set_position(
                &body,
                Position {
                    x: refused.0,
                    y: refused.1
                }
            ),
            Err(KernelError::Collision(CollisionError::Placement)),
            "teleporting to {refused:?} must be refused"
        );
        assert_eq!(
            kernel.position(&body),
            Ok(Position { x: 96.0, y: 96.0 }),
            "a refused teleport leaves the position exactly as it was"
        );
    }
    // An entity with no collider keeps the unrestricted teleport.
    let free = kernel.spawn(Position::default()).unwrap();
    assert_eq!(
        kernel.set_position(
            &free,
            Position {
                x: -1.0e9,
                y: 1.0e9
            }
        ),
        Ok(())
    );
}

#[test]
fn attaching_and_resizing_check_every_covered_cell() {
    let mut kernel = Kernel::new();
    let body = kernel.spawn(Position { x: 64.0, y: 64.0 }).unwrap();
    // M2 retires `NoTileMap` at this entry point rather than relaxing it: a
    // collider names its map by handle, so "no map installed" stopped being
    // representable here. The successor is a handle that no longer names a live
    // map, which is the same refusal one layer up.
    let retired = kernel.create_tilemap(pillar()).unwrap();
    kernel.remove_tilemap(&retired).unwrap();
    assert_eq!(
        kernel.set_tile_collider(&body, on(&retired, square(32.0))),
        Err(KernelError::InvalidTileMap),
        "attaching requires a live map"
    );
    let installed = kernel.create_tilemap(pillar()).unwrap();
    // (64, 64) is exactly the pillar cell.
    assert_eq!(
        kernel.set_tile_collider(&body, on(&installed, square(32.0))),
        Err(KernelError::Collision(CollisionError::Placement)),
        "attaching must check every cell the box covers"
    );
    assert_eq!(kernel.tile_collider(&body), Ok(None));
    assert_eq!(kernel.live_colliders(), 0);

    kernel
        .set_position(&body, Position { x: 32.0, y: 32.0 })
        .unwrap();
    let small = square(32.0);
    assert_eq!(
        kernel.set_tile_collider(&body, on(&installed, small)),
        Ok(())
    );
    assert_eq!(
        kernel.tile_collider(&body),
        Ok(Some((installed.clone(), small))),
        "the read names the map the body joined"
    );
    assert_eq!(kernel.live_colliders(), 1);

    // Growing it over the pillar refuses and leaves the previous collider.
    assert_eq!(
        kernel.set_tile_collider(&body, on(&installed, square(64.0))),
        Err(KernelError::Collision(CollisionError::Placement))
    );
    assert_eq!(
        kernel.tile_collider(&body),
        Ok(Some((installed.clone(), small)))
    );
    // Beyond eight tiles on a 32-pixel grid.
    assert_eq!(
        kernel.set_tile_collider(&body, on(&installed, square(256.000_000_000_000_1))),
        Err(KernelError::Collision(CollisionError::Extent)),
        "attaching must refuse an extent beyond eight tiles"
    );
    assert_eq!(
        kernel.tile_collider(&body),
        Ok(Some((installed.clone(), small)))
    );

    // Detaching always succeeds and releases capacity.
    assert_eq!(kernel.set_tile_collider(&body, None), Ok(()));
    assert_eq!(kernel.tile_collider(&body), Ok(None));
    assert_eq!(kernel.live_colliders(), 0);
    assert_eq!(kernel.set_tile_collider(&body, None), Ok(()), "idempotent");
}

#[test]
fn a_map_replacement_that_would_trap_a_body_refuses_without_changing_the_map() {
    let (mut kernel, body, installed) = session(pillar(), (32.0, 32.0), square(32.0));
    // The same room with the body's own cell filled in.
    let trapping = build(
        &["#####", "##..#", "#.#.#", "#...#", "#####"],
        32,
        32,
        (0, 0),
    );
    assert_eq!(
        kernel.replace_tilemap(&installed, trapping),
        Err(KernelError::Collision(CollisionError::Placement)),
        "installation must revalidate every live collider before the swap"
    );
    assert_eq!(
        kernel.tile(&installed, 1, 1),
        Ok(0),
        "a refused install leaves the installed map whole"
    );

    // A map whose tiles are small enough to make the attached extent illegal is
    // refused for the schema rather than the placement, because the extent cap
    // is tile-relative.
    let fine_grid = build(
        &["........", "........", "........", "........"],
        2,
        2,
        (0, 0),
    );
    assert_eq!(
        kernel.replace_tilemap(&installed, fine_grid),
        Err(KernelError::Collision(CollisionError::Extent))
    );
    assert_eq!(kernel.tile_face(&installed, Axis::X, 1), Ok(32.0));

    // A legal replacement swaps in and the body keeps its position.
    let opened = build(
        &["#####", "#...#", "#...#", "#...#", "#####"],
        32,
        32,
        (0, 0),
    );
    assert!(kernel.replace_tilemap(&installed, opened).is_ok());
    assert_eq!(kernel.tile_solid(&installed, 2, 2), Ok(false));
    assert_eq!(kernel.position(&body), Ok(Position { x: 32.0, y: 32.0 }));
}

#[test]
fn a_solid_edit_under_a_body_refuses_and_leaves_the_cell() {
    let (mut kernel, body, installed) = session(pillar(), (32.0, 32.0), square(32.0));
    assert_eq!(
        kernel.set_tile(&installed, 1, 1, WALL),
        Err(KernelError::Collision(CollisionError::Placement)),
        "an edit that would trap the body must refuse"
    );
    assert_eq!(
        kernel.tile(&installed, 1, 1),
        Ok(0),
        "the cell keeps its old ID"
    );

    // The same edit one cell over is free, and so is a non-solid edit beneath
    // the body: it cannot introduce overlap.
    assert_eq!(kernel.set_tile(&installed, 3, 3, WALL), Ok(()));
    assert_eq!(kernel.set_tile(&installed, 1, 1, FLOOR), Ok(()));
    assert_eq!(kernel.tile(&installed, 1, 1), Ok(FLOOR));

    // Clearing the pillar under nobody, then refilling it, both succeed. The
    // body's high edge is exactly the pillar's near face, and edge contact is
    // not overlap.
    assert_eq!(kernel.set_tile(&installed, 2, 2, 0), Ok(()));
    assert_eq!(
        kernel.set_tile(&installed, 2, 2, WALL),
        Ok(()),
        "a body flush against the cell's face does not overlap it"
    );
    // Edge contact is not overlap: the body's high edge is exactly 64.
    assert_eq!(kernel.position(&body), Ok(Position { x: 32.0, y: 32.0 }));
}

#[test]
fn the_map_can_be_removed_only_once_every_collider_is_detached() {
    let (mut kernel, body, installed) = session(room(), (32.0, 32.0), square(32.0));
    assert_eq!(
        kernel.remove_tilemap(&installed),
        Err(KernelError::CollidersAttached),
        "T6 refuses to remove a map beneath its own member"
    );
    assert_eq!(kernel.tilemap_count(), 1);

    // The transition recipe: detach, remove, create, reposition, reattach.
    // Removing twice is no longer "clearing an absent map succeeds" - the
    // second call names a handle whose map is gone, which is a refusal.
    assert_eq!(kernel.set_tile_collider(&body, None), Ok(()));
    assert_eq!(kernel.remove_tilemap(&installed), Ok(()));
    assert_eq!(
        kernel.remove_tilemap(&installed),
        Err(KernelError::InvalidTileMap),
        "a removed map cannot be removed again"
    );
    assert_eq!(kernel.tilemap_count(), 0);
    let reinstalled = kernel.create_tilemap(corridor(&[9])).unwrap();
    kernel
        .set_position(&body, Position { x: 64.0, y: 32.0 })
        .unwrap();
    assert_eq!(
        kernel.set_tile_collider(&body, on(&reinstalled, square(32.0))),
        Ok(())
    );
    assert_eq!(kernel.live_colliders(), 1);
}

#[test]
fn the_collider_limit_is_enforced_and_released() {
    let mut kernel = Kernel::new();
    let installed = kernel.create_tilemap(room()).unwrap();
    // Bodies never block one another, so they can all share a cell.
    let mut handles = Vec::new();
    for _ in 0..MAX_LIVE_COLLIDERS {
        let handle = kernel.spawn(Position { x: 32.0, y: 32.0 }).unwrap();
        kernel
            .set_tile_collider(&handle, on(&installed, square(32.0)))
            .unwrap();
        handles.push(handle);
    }
    assert_eq!(kernel.live_colliders(), MAX_LIVE_COLLIDERS);
    let extra = kernel.spawn(Position { x: 32.0, y: 32.0 }).unwrap();
    assert_eq!(
        kernel.set_tile_collider(&extra, on(&installed, square(32.0))),
        Err(KernelError::ColliderLimit),
        "the live collider limit must refuse the next attachment"
    );
    // Replacing an existing collider at the limit is not a new attachment.
    assert_eq!(
        kernel.set_tile_collider(&handles[0], on(&installed, square(16.0))),
        Ok(())
    );
    // Despawn releases the capacity the detach path also releases.
    kernel.despawn(&handles[0]).unwrap();
    assert_eq!(
        kernel.live_colliders(),
        MAX_LIVE_COLLIDERS - 1,
        "despawn must release collider capacity"
    );
    assert_eq!(
        kernel.set_tile_collider(&extra, on(&installed, square(32.0))),
        Ok(())
    );
}

#[test]
fn collider_calls_refuse_foreign_stale_and_reused_handles() {
    let (mut kernel, body, installed) = session(room(), (32.0, 32.0), square(32.0));

    // Another session's handle is not this session's, even at the same slot.
    let mut other = Kernel::new();
    other.create_tilemap(room()).unwrap();
    let foreign = other.spawn(Position { x: 32.0, y: 32.0 }).unwrap();
    assert_eq!(
        kernel.tile_collider(&foreign),
        Err(KernelError::InvalidHandle)
    );
    assert_eq!(
        kernel.set_tile_collider(&foreign, on(&installed, square(32.0))),
        Err(KernelError::InvalidHandle)
    );
    assert_eq!(
        kernel.set_tile_collider(&foreign, None),
        Err(KernelError::InvalidHandle),
        "detaching must validate the handle before deciding there is nothing to do"
    );

    kernel.despawn(&body).unwrap();
    assert_eq!(kernel.live_colliders(), 0);
    assert_eq!(kernel.tile_collider(&body), Err(KernelError::InvalidHandle));

    // The despawned slot is now free for reuse; the old handle stays dead.
    let reused = kernel.spawn(Position { x: 32.0, y: 32.0 }).unwrap();
    assert_eq!(
        kernel.tile_collider(&body),
        Err(KernelError::InvalidHandle),
        "a reused slot must not revive the handle that held it"
    );
    assert_eq!(kernel.tile_collider(&reused), Ok(None));
}

// ---------------------------------------------------------------------------
// Fixed systems
// ---------------------------------------------------------------------------

#[test]
fn entities_without_colliders_integrate_exactly_as_before() {
    let mut kernel = Kernel::new();
    let free = kernel.spawn(Position { x: 64.0, y: 64.0 }).unwrap();
    kernel
        .set_velocity(&free, Velocity { x: 1200.0, y: 0.0 })
        .unwrap();
    // A map with a wall directly in the path changes nothing without a collider.
    kernel.create_tilemap(corridor(&[9])).unwrap();
    assert_eq!(kernel.live_colliders(), 0);
    for _ in 0..30 {
        kernel.fixed_update().unwrap();
    }
    let mut expected = 64.0_f64;
    for _ in 0..30 {
        expected += 1200.0 * FIXED_DT;
    }
    assert_eq!(kernel.position(&free).unwrap().x, expected);
    assert_eq!(
        kernel.collision_scratch_bytes(),
        0,
        "a session with no collider must not reserve sweep scratch"
    );
}

#[test]
fn a_body_stops_at_the_wall_while_its_velocity_is_preserved() {
    let (mut kernel, body, _installed) = session(corridor(&[9]), (32.0, 32.0), square(32.0));
    kernel
        .set_velocity(&body, Velocity { x: 1200.0, y: 0.0 })
        .unwrap();
    for _ in 0..40 {
        kernel.fixed_update().unwrap();
    }
    assert_eq!(
        kernel.position(&body),
        Ok(Position { x: 256.0, y: 32.0 }),
        "the body must settle flush against the wall at 288"
    );
    assert_eq!(
        kernel.velocity(&body),
        Ok(Velocity { x: 1200.0, y: 0.0 }),
        "velocity keeps its requested value so holding a direction keeps pressing"
    );
    assert!(kernel.collision_scratch_bytes() > 0);
}

#[test]
fn a_late_refusal_moves_no_entity() {
    let (mut kernel, body, _installed) = session(corridor(&[9]), (32.0, 32.0), square(32.0));
    kernel
        .set_velocity(&body, Velocity { x: 1200.0, y: 0.0 })
        .unwrap();
    // Two entities with no collider, in one archetype and in spawn order: the
    // first integrates cleanly, the second overflows to a non-finite candidate.
    let early = kernel.spawn(Position { x: 1.0, y: 2.0 }).unwrap();
    kernel
        .set_velocity(&early, Velocity { x: 60.0, y: 0.0 })
        .unwrap();
    let late = kernel
        .spawn(Position {
            x: f64::MAX,
            y: 0.0,
        })
        .unwrap();
    kernel
        .set_velocity(
            &late,
            Velocity {
                x: f64::MAX,
                y: 0.0,
            },
        )
        .unwrap();

    assert_eq!(kernel.fixed_update(), Err(KernelError::Nonfinite));
    assert_eq!(
        kernel.position(&early),
        Ok(Position { x: 1.0, y: 2.0 }),
        "an entity validated before the refusal must not have been committed"
    );
    assert_eq!(
        kernel.position(&body),
        Ok(Position { x: 32.0, y: 32.0 }),
        "a swept body must not be committed when a later candidate refuses"
    );
    assert_eq!(kernel.position(&late).unwrap().x, f64::MAX);

    // Removing the offender lets the same tick complete.
    kernel.despawn(&late).unwrap();
    assert_eq!(kernel.fixed_update(), Ok(()));
    assert_eq!(kernel.position(&body).unwrap().x, 52.0);
}

#[test]
fn replay_is_independent_of_insertion_order() {
    // The same three bodies and two free entities, spawned in opposite orders.
    let starts = [
        ((32.0, 32.0), (1200.0, 0.0)),
        ((160.0, 32.0), (-600.0, 300.0)),
        ((320.0, 32.0), (900.0, -60.0)),
    ];
    let mut runs = Vec::new();
    for reversed in [false, true] {
        let mut kernel = Kernel::new();
        let installed = kernel.create_tilemap(corridor(&[9, 14])).unwrap();
        let mut handles = Vec::new();
        let order: Vec<usize> = if reversed {
            (0..starts.len()).rev().collect()
        } else {
            (0..starts.len()).collect()
        };
        for index in order {
            let (at, velocity) = starts[index];
            let handle = kernel.spawn(Position { x: at.0, y: at.1 }).unwrap();
            kernel
                .set_tile_collider(&handle, on(&installed, square(32.0)))
                .unwrap();
            kernel
                .set_velocity(
                    &handle,
                    Velocity {
                        x: velocity.0,
                        y: velocity.1,
                    },
                )
                .unwrap();
            // A free entity between each body, so the archetypes interleave.
            kernel.spawn(Position::default()).unwrap();
            handles.push((index, handle));
        }
        for _ in 0..25 {
            kernel.fixed_update().unwrap();
        }
        let mut positions = vec![Position::default(); starts.len()];
        for (index, handle) in handles {
            positions[index] = kernel.position(&handle).unwrap();
        }
        runs.push(positions);
    }
    assert_eq!(
        runs[0], runs[1],
        "fixed-input replay must not depend on ECS insertion order"
    );
    assert_ne!(runs[0][0], Position { x: 32.0, y: 32.0 }, "bodies did move");
}

#[test]
fn stopping_releases_the_sweep_scratch() {
    let (mut kernel, body, _installed) = session(room(), (32.0, 32.0), square(32.0));
    kernel
        .set_velocity(&body, Velocity { x: 120.0, y: 0.0 })
        .unwrap();
    kernel.fixed_update().unwrap();
    assert!(kernel.collision_scratch_bytes() > 0);
    kernel.stop();
    assert_eq!(kernel.collision_scratch_bytes(), 0);
    assert_eq!(kernel.tilemap_storage_bytes(), 0);
    assert_eq!(
        kernel.live_colliders(),
        0,
        "all three accounting accessors must agree that a stopped session holds nothing"
    );
    assert_eq!(kernel.tile_collider(&body), Err(KernelError::Inactive));
    assert_eq!(
        kernel.set_tile_collider(&body, None),
        Err(KernelError::Inactive)
    );
}

// ---------------------------------------------------------------------------
// Work accounting
// ---------------------------------------------------------------------------

#[test]
fn tile_work_is_charged_per_visited_cell_and_survives_a_refusal() {
    let (mut kernel, body, installed) = session(room(), (32.0, 32.0), square(32.0));
    kernel.begin_callback();

    // A cell edit that leaves the map non-solid charges the cell alone.
    kernel.set_tile(&installed, 3, 1, FLOOR).unwrap();
    assert_eq!(
        kernel.callback_work(),
        1,
        "an edit that cannot introduce overlap must skip the collider scan"
    );

    // Nor can replacing one solid ID with another, which is why the scan is
    // conditioned on the transition rather than on the new ID alone.
    kernel.begin_callback();
    kernel.set_tile(&installed, 0, 0, WALL).unwrap();
    assert_eq!(
        kernel.callback_work(),
        1,
        "a solid cell that stays solid must skip the collider scan"
    );

    // Making a cell solid charges the cell plus one check per live collider,
    // whether or not the edit is accepted.
    kernel.begin_callback();
    kernel.set_tile(&installed, 3, 1, WALL).unwrap();
    assert_eq!(kernel.callback_work(), 2);
    kernel.begin_callback();
    assert!(kernel.set_tile(&installed, 1, 1, WALL).is_err());
    assert_eq!(
        kernel.callback_work(),
        2,
        "a refused edit is charged for the scan it performed"
    );

    // A placement check charges one unit per covered cell.
    kernel.begin_callback();
    kernel
        .set_position(&body, Position { x: 33.0, y: 33.0 })
        .unwrap();
    assert_eq!(
        kernel.callback_work(),
        4,
        "a 32-pixel box offset by one pixel covers four 32-pixel cells"
    );
}

/// One assertion per entry point that charges tile work, because the shared
/// helper being right does not establish that all four reach for it.
#[test]
fn every_entry_point_is_budgeted_against_what_the_callback_has_left() {
    let (mut kernel, body, installed) = session(room(), (32.0, 32.0), square(32.0));
    let before = kernel.tile(&installed, 3, 1).unwrap();
    kernel.begin_callback();
    // One unit short of the ceiling: the edit below costs two, so it must be
    // refused part-way rather than allowed one whole call's worth of overrun.
    kernel.charge_callback_work(MAX_CALLBACK_WORK - 1).unwrap();
    assert_eq!(
        kernel.set_tile(&installed, 3, 1, WALL),
        Err(KernelError::Collision(CollisionError::Work)),
        "set_tile must be budgeted against what the callback has left, not the whole ceiling"
    );
    assert_eq!(
        kernel.tile(&installed, 3, 1),
        Ok(before),
        "the refused edit changed nothing"
    );
    assert!(
        kernel.callback_work() >= MAX_CALLBACK_WORK,
        "the work performed is charged before the refusal"
    );

    // The other three, each with nothing left at all. A placement check charges
    // its first cell before it can look at anything, so one unit is enough to
    // separate "budgeted against the remainder" from "budgeted against the
    // ceiling"; what differs between them is not the size of the overrun but
    // whether there is one.
    let exhausted = |kernel: &mut Kernel| {
        kernel.begin_callback();
        kernel.charge_callback_work(MAX_CALLBACK_WORK).unwrap();
    };
    let work = Err(KernelError::Collision(CollisionError::Work));

    exhausted(&mut kernel);
    assert_eq!(
        kernel.set_position(&body, Position { x: 33.0, y: 33.0 }),
        work,
        "set_position must be budgeted against what the callback has left"
    );
    assert_eq!(
        kernel.position(&body),
        Ok(Position { x: 32.0, y: 32.0 }),
        "the refused teleport moved nothing"
    );

    exhausted(&mut kernel);
    assert_eq!(
        kernel.set_tile_collider(&body, on(&installed, square(16.0))),
        work,
        "set_tile_collider must be budgeted against what the callback has left"
    );
    assert_eq!(
        kernel.tile_collider(&body),
        Ok(Some((installed.clone(), square(32.0)))),
        "the refused attachment left the previous collider"
    );

    // Installation charges one placement check per live collider, so its
    // budgeting is only reachable with a body attached - which is also the only
    // case where the overrun is material, at up to 81 units per collider.
    exhausted(&mut kernel);
    assert_eq!(
        kernel.replace_tilemap(&installed, room()),
        work,
        "replace_tilemap must be budgeted against what the callback has left"
    );
    assert_eq!(
        kernel.tile(&installed, 3, 1),
        Ok(before),
        "the refused installation left the map alone"
    );
}

/// One assertion per entry point for the anti-probing half of the same rule.
///
/// `replace_tilemap`'s is the one with teeth. A replacement that 1,023 colliders
/// pass and the 1,024th fails walks about 82,000 cells; uncharged it would cost
/// two units, and the attempt budget allows 4,096 calls per callback, so a
/// script could spend hundreds of millions of cell visits against a
/// 1,048,576-unit ceiling.
#[test]
fn every_entry_point_charges_the_work_it_performed_before_refusing() {
    let (mut kernel, body, installed) = session(room(), (32.0, 32.0), square(32.0));

    kernel.begin_callback();
    assert!(
        kernel
            .set_position(&body, Position { x: 0.0, y: 0.0 })
            .is_err()
    );
    assert!(
        kernel.callback_work() > 0,
        "a refused teleport is charged for the cells it checked"
    );

    kernel.begin_callback();
    // Legal as an extent - eight 32-pixel tiles is 256 - but from (32, 32) the
    // box reaches 160 and overlaps the room's far wall, so it is refused after
    // the scan rather than before it.
    assert!(
        kernel
            .set_tile_collider(&body, on(&installed, square(128.0)))
            .is_err()
    );
    assert!(
        kernel.callback_work() > 0,
        "a refused attachment is charged for the cells it checked"
    );

    // A map whose interior is solid under the body, so revalidation walks the
    // collider and then refuses.
    kernel.begin_callback();
    assert!(
        kernel
            .replace_tilemap(
                &installed,
                build(
                    &["#####", "#####", "#####", "#####", "#####"],
                    32,
                    32,
                    (0, 0)
                )
            )
            .is_err()
    );
    assert!(
        kernel.callback_work() > 0,
        "a refused installation is charged for the colliders it revalidated"
    );

    // And a charge that crosses the ceiling counts, so a caller cannot probe for
    // free by repeating a request that always refuses.
    kernel.begin_callback();
    kernel.charge_callback_work(MAX_CALLBACK_WORK).unwrap();
    assert_eq!(
        kernel.charge_callback_work(7),
        Err(KernelError::Collision(CollisionError::Work))
    );
    assert_eq!(
        kernel.callback_work(),
        MAX_CALLBACK_WORK + 7,
        "a refused charge is still charged"
    );
}

#[test]
fn the_solver_refuses_rather_than_overrunning_its_budget() {
    let map = corridor(&[9]);
    let body = square(32.0);
    let mut tight = WorkBudget::new(3);
    assert_eq!(
        collision::solve(&map, &body, (32.0, 32.0), (320.0, 0.0), &mut tight),
        Err(CollisionError::Work),
        "a sweep past its budget must refuse rather than keep visiting cells"
    );
    assert!(
        tight.used() > 3,
        "the work performed is charged before the refusal"
    );

    // The same sweep inside its real budget reports the exact cells it visited:
    // seven clear columns then the wall, one row each.
    let mut counted = WorkBudget::new(MAX_FIXED_PASS_WORK);
    collision::solve(&map, &body, (32.0, 32.0), (320.0, 0.0), &mut counted).unwrap();
    assert_eq!(counted.used(), 8);
}

// ---------------------------------------------------------------------------
// Max-load stress
// ---------------------------------------------------------------------------

/// The mandated stress shape: the largest map that maximises `columns + rows`
/// inside the cell product, with one-pixel tiles so a maximum body is eight
/// pixels and every column is a candidate face.
fn stress_map() -> TileMap {
    TileMap::new(
        TileMapInfo {
            columns: 1_024,
            rows: 256,
            tile_width: 1,
            tile_height: 1,
            origin_x: 0,
            origin_y: 0,
        },
        vec![true],
        vec![0; 262_144],
    )
    .expect("the stress map is the schema maximum")
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

/// The plan requires this configuration as evidence, so it must complete inside
/// the fixed-pass ceiling rather than fault. It is ignored by default because a
/// debug build of eleven million cell visits is not a unit-test cost; run it
/// through `tools/run_collision_stress.ps1`, which builds release and applies an
/// independent child watchdog.
#[test]
#[ignore = "max-load stress: run through tools/run_collision_stress.ps1"]
fn max_load_stress_completes_inside_the_fixed_pass_ceiling() {
    // The sparse velocity is chosen so a tick's travel is 1.5 one-pixel tiles:
    // short motion that still crosses a face on both axes. A slower body would
    // charge zero units, which would make the arrangement a vacuous measurement
    // rather than a cheap one.
    for (label, base, spread, velocity, expected_units) in [
        // The long sweep starts on an integral X so its leading edge lands
        // exactly on a tile face. N4 includes a face already touching the
        // leading edge when moving into it, so that alignment enumerates one
        // more face than a fractional start and is the costlier of the two. It
        // is also the alignment the Phase 0 probe measured, which is what makes
        // the two counts comparable.
        (
            "long-sweep",
            (0.0, 0.25),
            0.0,
            (122_880.0, 30_720.0),
            11_386_880,
        ),
        (
            "sparse-short-motion",
            (0.25, 0.25),
            1.0,
            (90.0, 90.0),
            18_432,
        ),
    ] {
        let mut kernel = Kernel::new();
        let installed = kernel.create_tilemap(stress_map()).unwrap();
        let body = square(8.0);
        let mut handles = Vec::new();
        for index in 0..MAX_LIVE_COLLIDERS {
            // Bodies never block one another, so the long-sweep arrangement
            // stacks them all at the origin and each sweeps the whole map.
            let at = Position {
                x: base.0 + spread * f64::from(index % 128) * 7.0,
                y: base.1 + spread * f64::from(index / 128) * 7.0,
            };
            let handle = kernel.spawn(at).unwrap();
            kernel
                .set_tile_collider(&handle, on(&installed, body))
                .unwrap();
            kernel
                .set_velocity(
                    &handle,
                    Velocity {
                        x: velocity.0,
                        y: velocity.1,
                    },
                )
                .unwrap();
            handles.push((handle, at));
        }
        // The rest of the entity limit, in free flight and stationary, so the
        // pass carries the full population it is specified against.
        for _ in 0..(16_384 - MAX_LIVE_COLLIDERS) {
            kernel.spawn(Position::default()).unwrap();
        }
        assert_eq!(kernel.live_colliders(), MAX_LIVE_COLLIDERS);

        let mut timings = Vec::new();
        let mut units = 0;
        for _ in 0..15 {
            // Reset outside the timed region: after one long sweep every body
            // is already flush against the boundary and the next tick is free.
            for (handle, at) in &handles {
                kernel.set_position(handle, *at).unwrap();
            }
            let started = std::time::Instant::now();
            kernel
                .fixed_update()
                .expect("the mandated stress arrangement must complete, not fault");
            timings.push(started.elapsed().as_secs_f64() * 1_000.0);
            units = kernel.fixed_pass_work();
        }
        timings.sort_by(f64::total_cmp);

        let storage = kernel.tilemap_storage_bytes() + kernel.collision_scratch_bytes();
        println!(
            "{label}: {units} units ({:.1}% of {MAX_FIXED_PASS_WORK}), \
             p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms, storage {storage} bytes",
            100.0 * units as f64 / MAX_FIXED_PASS_WORK as f64,
            percentile(&timings, 0.50),
            percentile(&timings, 0.95),
            timings[timings.len() - 1],
        );
        assert!(
            units < MAX_FIXED_PASS_WORK,
            "{label} charged {units} units against a {MAX_FIXED_PASS_WORK} ceiling"
        );
        assert!(
            units > 0,
            "{label} visited no cells at all, so it measures nothing"
        );
        // The long sweep's figure is the Phase 0 probe's, to the unit. Pinning
        // it makes the count a checked constant rather than a reported one: the
        // prototype and the production solver enumerate the same faces on the
        // same configuration, and a change to either shows up here.
        assert_eq!(
            units, expected_units,
            "{label} must charge exactly the units its configuration implies"
        );
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn every_new_refusal_has_a_distinct_message() {
    let messages = [
        KernelError::Collision(CollisionError::Offset).to_string(),
        KernelError::Collision(CollisionError::Extent).to_string(),
        KernelError::Collision(CollisionError::Placement).to_string(),
        KernelError::Collision(CollisionError::Work).to_string(),
        KernelError::Collision(CollisionError::Nonfinite).to_string(),
        KernelError::Collision(CollisionError::Embedded).to_string(),
        KernelError::Collision(CollisionError::Unconverged).to_string(),
        KernelError::CollidersAttached.to_string(),
        KernelError::ColliderLimit.to_string(),
        KernelError::Capacity.to_string(),
        KernelError::TileMap(TileMapError::Bounds).to_string(),
    ];
    for (index, message) in messages.iter().enumerate() {
        assert!(!message.is_empty());
        assert!(
            !messages[..index].contains(message),
            "duplicate error message {message:?}"
        );
    }
    assert_eq!(
        KernelError::Collision(CollisionError::Placement).to_string(),
        "the collider box must lie in the map clear of solid tiles"
    );
}
