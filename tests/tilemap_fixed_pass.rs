//! What the fixed pass charges once bodies resolve a map each.
//!
//! **The equality is necessary and not sufficient, and that is why both checks
//! are here.** An equality between two arms can only see a term that *differs*
//! between them. A cost added uniformly per body - the pass resolving
//! membership once per body, which is a completely plausible implementation -
//! raises both arms equally, leaves them equal, and passes. So does every
//! weaker assertion: the spread is unchanged, the totals sit far inside the
//! arrangement's own worst case, and no amount of varying the battery helps,
//! because variation cannot expose an additive constant. Only a prediction
//! does. Before one existed, a per-body overhead of up to 609 units each - 54%
//! of a body's average cost - passed every other assertion in Phase 0's probe.
//!
//! Neither check is "still fits". Both arms charge 1,145,600 against a
//! 16,777,216 ceiling, so "still fits" is satisfied with 93% of the ceiling
//! unused and goes on being satisfied by a fourteenfold per-map term. An
//! accidental unit per map per body adds 65,536 and lands at 1,211,136, which
//! is not close to anything: "still fits" passes, the equality fails.
//!
//! Do not delete the prediction because the equality beside it passes.

use protogine::collision::{MAX_FIXED_PASS_WORK, MAX_LIVE_COLLIDERS, TileCollider};
use protogine::kernel::{ColliderPlacement, FIXED_DT, Kernel, Position, TileMapHandle, Velocity};
use protogine::maps::{MAX_AGGREGATE_CELLS, MAX_TILEMAPS};
use protogine::tilemap::{MAX_SOLID_IDS, TileMap, TileMapInfo};

/// The shape whose 64 copies fill the aggregate to the cell: 524,288 / 64 is
/// 8,192, and 128x64 achieves it exactly. Holding geometry constant between the
/// arms is what leaves distribution as the only difference - "spread across
/// many maps" under a fixed aggregate otherwise means *smaller* maps, and a
/// smaller map ends the sweep's walk sooner and charges less.
const BALANCE: (u32, u32) = (128, 64);
const TILE: u32 = 32;
/// Eight tiles, the maximum collider extent. A tile-aligned body of this size
/// spans exactly eight cells perpendicular to its travel.
const SPAN_TILES: u32 = 8;
const BODY: f64 = SPAN_TILES as f64 * TILE as f64;

fn balanced() -> TileMap {
    TileMap::new(
        TileMapInfo {
            columns: BALANCE.0,
            rows: BALANCE.1,
            tile_width: TILE,
            tile_height: TILE,
            origin_x: 0,
            origin_y: 0,
        },
        vec![true; MAX_SOLID_IDS],
        vec![0; (BALANCE.0 * BALANCE.1) as usize],
    )
    .expect("the balance shape is within the frozen limits")
}

/// Coprime moduli against each other and against the body count, so 1,024
/// bodies take 1,024 distinct starting cells rather than repeating a few.
fn start_cell(i: usize) -> (u32, u32) {
    ((i % 21) as u32, (i % 50) as u32)
}

fn start_of(i: usize) -> Position {
    let (column, row) = start_cell(i);
    Position {
        x: f64::from((1 + column) * TILE),
        y: f64::from((1 + row) * TILE),
    }
}

/// What the sweep must charge for body `i`, derived from the map geometry
/// rather than from the solver.
///
/// The leading edge starts on face `1 + column + SPAN_TILES` and enumerates
/// every face up to the last interior one - the boundary face clamps before a
/// cell is inspected and charges nothing - with `SPAN_TILES` perpendicular
/// cells at each. The Y leg does the same from the resolved X, where the body
/// is flush against the far boundary and still spans `SPAN_TILES` columns.
fn predicted(i: usize) -> u64 {
    let (column, row) = start_cell(i);
    let faces = |side: u32, from: u32| u64::from(side - (1 + from + SPAN_TILES));
    u64::from(SPAN_TILES) * (faces(BALANCE.0, column) + faces(BALANCE.1, row))
}

fn body(size: f64) -> TileCollider {
    TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: size,
        height: size,
    }
}

fn joining(map: &TileMapHandle) -> Option<ColliderPlacement> {
    Some(ColliderPlacement {
        map: map.clone(),
        collider: body(BODY),
        position: None,
    })
}

/// Far enough to reach the far boundary on both axes from anywhere inside, so
/// every sweep clamps rather than stopping short by arithmetic accident. The
/// exact figure does not matter once it overshoots: the clamp is at the
/// boundary, so a larger velocity adds no faces and no charge.
const VELOCITY: Velocity = Velocity {
    x: 300_000.0,
    y: 300_000.0,
};

/// One fixed pass with every collider spread over `maps` identical maps.
fn pass_over(map_count: usize) -> u64 {
    let mut kernel = Kernel::new();
    let handles: Vec<TileMapHandle> = (0..map_count)
        .map(|_| kernel.create_tilemap(balanced()).expect("admitted"))
        .collect();
    let per_map = MAX_LIVE_COLLIDERS as usize / map_count;
    for i in 0..MAX_LIVE_COLLIDERS as usize {
        let entity = kernel.spawn(start_of(i)).unwrap();
        kernel
            .set_tile_collider(&entity, joining(&handles[i / per_map]))
            .expect("every start is a legal placement");
        kernel.set_velocity(&entity, VELOCITY).unwrap();
    }
    assert_eq!(kernel.live_colliders(), MAX_LIVE_COLLIDERS);
    assert_eq!(kernel.tilemap_count(), map_count as u32);
    kernel.fixed_update().expect("the pass completes");
    kernel.fixed_pass_work()
}

#[test]
fn every_body_charges_exactly_what_its_geometry_predicts() {
    // Measured one body at a time, because the kernel reports a pass total and
    // a total cannot show that the *distribution* is right. A uniform per-body
    // term would leave any total-level assertion satisfied.
    let mut kernel = Kernel::new();
    let map = kernel.create_tilemap(balanced()).unwrap();
    let mut total = 0u64;
    for i in 0..MAX_LIVE_COLLIDERS as usize {
        let entity = kernel.spawn(start_of(i)).unwrap();
        kernel
            .set_tile_collider(&entity, joining(&map))
            .expect("every start is a legal placement");
        kernel.set_velocity(&entity, VELOCITY).unwrap();
        kernel.fixed_update().unwrap();
        assert_eq!(
            kernel.fixed_pass_work(),
            predicted(i),
            "every body must charge exactly what its geometry predicts, body {i}"
        );
        total += kernel.fixed_pass_work();
        kernel.despawn(&entity).unwrap();
    }
    // Pinned to a literal, which cannot follow `start_cell` anywhere. The
    // prediction reads the same `start_cell` the battery does, so it would
    // predict a degenerate arrangement correctly and agree; this cannot.
    assert_eq!(
        total, 1_145_600,
        "the arrangement's total is pinned; if this moved deliberately, update it and say so"
    );
}

#[test]
fn the_same_bodies_charge_the_same_across_one_map_and_sixty_four() {
    // **The equality and nothing else, deliberately.** An absolute figure here
    // would make this test catch defects the equality cannot see, and hide the
    // fact that it cannot see them - which is the whole claim. The first draft
    // pinned the total alongside, and the control that was supposed to
    // demonstrate the blindness failed instead of passing. The absolute figures
    // are the next test's.
    assert_eq!(
        pass_over(1),
        pass_over(MAX_TILEMAPS as usize),
        "distribution across maps must not change what the pass charges"
    );
}

#[test]
fn both_arms_charge_what_the_geometry_predicts() {
    for (map_count, arm) in [
        (1usize, "one 128x64 map"),
        (MAX_TILEMAPS as usize, "sixty-four identical 128x64 maps"),
    ] {
        assert_eq!(
            pass_over(map_count),
            1_145_600,
            "{arm} must charge what the geometry predicts"
        );
    }
}

/// Why "still fits" is not the check, stated where it cannot drift: both arms
/// charge 1,145,600 against a 16,777,216 ceiling, so the weaker phrasing stays
/// satisfied by a **fourteenfold** per-map term. Both sides are constants, so
/// this is settled at compile time rather than by a test that could be deleted.
const _: () = assert!(1_145_600 * 14 < MAX_FIXED_PASS_WORK);

#[test]
fn sixty_four_balance_maps_fill_the_aggregate_exactly() {
    // The arm above is only the maximum-stress form of the comparison if the
    // maps really do fill the budget, and an arm one map short would still
    // produce a plausible equality.
    let mut kernel = Kernel::new();
    for _ in 0..MAX_TILEMAPS {
        kernel.create_tilemap(balanced()).unwrap();
    }
    assert_eq!(
        kernel.tilemap_cells(),
        MAX_AGGREGATE_CELLS,
        "the comparison arm must fill the aggregate to the cell, not approximately"
    );
    assert_eq!(kernel.tilemap_count(), MAX_TILEMAPS);
}

#[test]
fn the_battery_takes_a_thousand_distinct_starting_cells() {
    // The prediction shares `start_cell` with the battery, so it cannot see the
    // arrangement collapsing underneath both of them. Asserted rather than left
    // as a claim about coprimality in a comment.
    let mut seen: Vec<(u32, u32)> = (0..MAX_LIVE_COLLIDERS as usize).map(start_cell).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        MAX_LIVE_COLLIDERS as usize,
        "every body must start in a distinct cell"
    );
    // And the two legs are exercised separately: a fixed column would make the
    // X leg charge an identical amount for every body.
    assert!(
        seen.iter()
            .map(|(column, _)| column)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
    );
    assert!(
        seen.iter()
            .map(|(_, row)| row)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
    );
}

/// The travel a velocity produces in one tick, for the record: `FIXED_DT` is
/// not exact in binary, so this documents that the arrangement does not depend
/// on it being so.
#[test]
fn the_sweep_clamps_rather_than_depending_on_exact_travel() {
    assert!(
        VELOCITY.x * FIXED_DT > f64::from(BALANCE.0 * TILE),
        "one tick must overshoot the whole map on both axes"
    );
    assert!(VELOCITY.y * FIXED_DT > f64::from(BALANCE.1 * TILE));
}
