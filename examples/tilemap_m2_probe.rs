//! Phase 0 feasibility probe for M2's simultaneous maps.
//!
//! **Prototype, not production.** M2 is unimplemented, so this holds a
//! `Vec<TileMap>` and indexes it where a kernel would resolve membership from a
//! component. That is deliberate and it bounds what the probe can prove: a
//! `Vec` index has no per-map cost to grow, so the `work` mode's two arms are
//! equal by construction. They are a *baseline pair*, reference values for
//! Phase 2 to assert against, and not evidence of anything on their own. The
//! [contract](../docs/implementation/TILEMAP_COLLISION_M2.md) says so in the
//! same words.
//!
//! What the modes can genuinely get wrong: the storage figure, the absolute
//! work an M2-shaped arrangement charges, whether the content shapes that are
//! claimed to fit do, and the timing. Each mode also asserts **its own
//! configuration was exercised**, not only its result - Phase 3 shipped a
//! stress arrangement that charged exactly zero units and reported success, and
//! a mode measuring "1,024 bodies across 64 maps" can silently measure 1,024
//! bodies on map 0 and print a plausible smaller number.
//!
//! There are no `control-*` modes. M1's Phase 0 had them because it froze
//! numerical rules and each control replaced one with the mistake it prevents.
//! M2 freezes no numerical rule - the geometry is M1's, unchanged - so there is
//! nothing here for a control to remove. The configuration assertions are what
//! replaces them at this phase; the six mutation controls M2 needs belong to
//! Phases 1 to 3, where the code they mutate exists.

use protogine::{
    collision::{self, MAX_FIXED_PASS_WORK, MAX_LIVE_COLLIDERS, TileCollider, WorkBudget},
    tilemap::{MAX_SOLID_IDS, TileMap, TileMapInfo},
};
use std::time::Instant;

/// Proposed M2 budgets, from the contract. Every one of them is what this probe
/// exists to confirm or move.
const MAX_MAPS: usize = 64;
const MAX_AGGREGATE_CELLS: usize = 524_288;
/// The shape where both budget limits bind together: 524,288 / 64 = 8,192
/// cells, and 128 x 64 is a shape achieving that exactly, so 64 copies fill the
/// aggregate to the cell. It is the largest shape whose 64 copies fit at all,
/// which is why the constant-geometry comparison below uses it.
///
/// Unlike the 32-map case, where 16,384 = 128^2 made the balance point a square,
/// 8,192 is not a perfect square. The largest square whose 64 copies fit is
/// 90 x 90, leaving 5,888 cells unused, so the exact shape has to be
/// rectangular. That is no loss: unequal axes exercise the two sweep legs
/// differently.
const BALANCE: (u32, u32) = (128, 64);
/// The largest single map M1's schema admits, used as the staging candidate.
const WIDEST: (u32, u32) = (1_024, 256);
const TILE: u32 = 32;
/// Eight tiles, the maximum collider extent. A tile-aligned body of this size
/// spans exactly eight cells perpendicular to its travel.
const SPAN_TILES: u32 = 8;
const BODY: f64 = SPAN_TILES as f64 * TILE as f64;

/// Allocate the way `src/scripting/tilemap.rs` does, not with `vec![...]`.
///
/// This is the whole reason the storage figure can come back wrong.
/// `vec![0; n]` has capacity exactly `n` by construction, so measuring one
/// would confirm the prediction rather than test it. The production path is
/// `Vec::new`, `try_reserve_exact`, `resize`, and `try_reserve_exact` is
/// documented as not *deliberately* over-allocating without being guaranteed
/// exact - which is a difference only a measurement through this sequence can
/// see.
fn dense<T: Copy>(len: usize, fill: T) -> Vec<T> {
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .expect("the probe's own allocation must succeed");
    out.resize(len, fill);
    out
}

/// A map of empty, non-solid cells with the maximum number of solid
/// definitions, which is the worst case for storage. Every cell is ID 0, so the
/// interior is free and a body sweeps until the boundary clamps it.
fn map_of(columns: u32, rows: u32) -> TileMap {
    let info = TileMapInfo {
        columns,
        rows,
        tile_width: TILE,
        tile_height: TILE,
        origin_x: 0,
        origin_y: 0,
    };
    TileMap::new(
        info,
        dense(MAX_SOLID_IDS, true),
        dense((columns as usize) * (rows as usize), 0u16),
    )
    .expect("a legal map")
}

/// The tile body `i` starts on inside its own map.
///
/// Both coordinates vary, so both sweep legs vary; 21 and 50 are coprime and
/// their lowest common multiple exceeds 1,024, so the pair does not repeat
/// inside a battery. An earlier version held the column fixed, which on the
/// 128x128 shape it then used made the X leg charge an identical 952 units for
/// every body, 63% of the average cost, invariant across the whole battery.
/// The row range is bounded by the shorter
/// axis: a body of eight tiles on a 64-row map must start above row 55 or it
/// charges nothing on Y.
fn start_cell(i: usize) -> (u32, u32) {
    ((i % 21) as u32, (i % 50) as u32)
}

/// Where body `i` starts inside its own map, and how far it asks to travel.
///
/// Identical in both arms of the `work` comparison, which is what leaves the
/// number of distinct map objects as the only difference between them.
fn body(i: usize) -> ((f64, f64), (f64, f64)) {
    let (column, row) = start_cell(i);
    let start = (f64::from((1 + column) * TILE), f64::from((1 + row) * TILE));
    // Far enough to reach the far boundary on both axes from anywhere inside,
    // so every sweep clamps rather than stopping short by arithmetic accident.
    let span = f64::from(BALANCE.0.max(BALANCE.1) * TILE);
    (start, (span, span))
}

/// What `sweep` must charge for body `i`, derived from the map geometry rather
/// than from the solver.
///
/// The leading edge starts on face `1 + column + SPAN_TILES` and enumerates
/// every face up to the last interior one - the boundary face clamps before a
/// cell is inspected and charges nothing - with `SPAN_TILES` perpendicular
/// cells at each. The Y leg does the same from the resolved X, where the body
/// is flush against the right boundary and still spans `SPAN_TILES` columns.
///
/// **This is what actually guards the arrangement, and "cost must vary" is not.**
/// Adding a constant to every body's charge - an M2 pass resolving membership
/// once per body, say - leaves the spread untouched, leaves the two arms equal,
/// and sits far inside the arrangement's own worst case. Before this prediction
/// existed, a per-body overhead of up to 609 units each - 54% of a body's actual
/// average cost of 1,119 - passed every assertion in the `work` mode. Only a
/// predicted total catches an additive term, and no amount of variation ever
/// will.
fn predicted(i: usize) -> u64 {
    let (column, row) = start_cell(i);
    let faces = |side: u32, from: u32| u64::from(side - (1 + from + SPAN_TILES));
    u64::from(SPAN_TILES) * (faces(BALANCE.0, column) + faces(BALANCE.1, row))
}

/// Sweep every body against the map `assign` gives it, sharing one budget.
///
/// Returns each body's charge and how many bodies each map received, because
/// the configuration assertions need both and a total alone cannot express
/// either.
fn sweep(maps: &[TileMap], assign: impl Fn(usize) -> usize) -> (Vec<u64>, Vec<usize>) {
    let collider = TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: BODY,
        height: BODY,
    };
    let mut budget = WorkBudget::new(MAX_FIXED_PASS_WORK);
    let mut charges = Vec::with_capacity(MAX_LIVE_COLLIDERS as usize);
    let mut received = vec![0usize; maps.len()];
    for i in 0..MAX_LIVE_COLLIDERS as usize {
        let index = assign(i);
        let before = budget.used();
        let (start, travel) = body(i);
        collision::solve(&maps[index], &collider, start, travel, &mut budget)
            .expect("every sweep in an empty map must resolve");
        charges.push(budget.used() - before);
        received[index] += 1;
    }
    (charges, received)
}

/// Check every body against the geometry and return the total.
///
/// Reports the first disagreement rather than the two vectors: `assert_eq!` on
/// 1,024-element slices prints both in full, which is unreadable at exactly the
/// moment someone needs to know which body is wrong and by how much.
fn check_predicted(charges: &[u64], arm: &str) -> u64 {
    let mut total = 0;
    for (i, measured) in charges.iter().enumerate() {
        let want = predicted(i);
        assert_eq!(
            *measured, want,
            "{arm}: every body must charge exactly what its geometry predicts, and body {i} did not"
        );
        total += measured;
    }
    total
}

/// The worst case M1's ceiling arithmetic gives for this arrangement:
/// `(columns + rows) * 9 * bodies`. Measured cost must be positive and must not
/// exceed it - bounded on both sides, because `> 0` alone held for a Phase 2
/// battery that did no work.
fn ceiling_for((columns, rows): (u32, u32)) -> u64 {
    u64::from(columns + rows) * 9 * u64::from(MAX_LIVE_COLLIDERS)
}

fn storage() {
    let maps: Vec<TileMap> = (0..MAX_MAPS)
        .map(|_| map_of(BALANCE.0, BALANCE.1))
        .collect();

    assert_eq!(maps.len(), MAX_MAPS, "the maximum map count must be built");
    let cells: usize = maps.iter().map(|map| map.info().cell_count()).sum();
    assert_eq!(
        cells, MAX_AGGREGATE_CELLS,
        "the maps must fill the aggregate exactly, not approximately"
    );

    let live: usize = maps.iter().map(TileMap::storage_bytes).sum();
    let candidate = map_of(WIDEST.0, WIDEST.1);
    assert_eq!(
        candidate.info().cell_count(),
        262_144,
        "the staging candidate must be the largest map the schema admits"
    );
    let peak = live + candidate.storage_bytes();

    let predicted = MAX_AGGREGATE_CELLS * 2
        + MAX_MAPS * MAX_SOLID_IDS
        + candidate.info().cell_count() * 2
        + MAX_SOLID_IDS;
    println!("aggregate cells   {cells}");
    println!("live storage      {live} bytes");
    println!("peak with staging {peak} bytes");
    println!("contract predicts {predicted} bytes");
    assert_eq!(
        peak, predicted,
        "storage must match the contract's arithmetic through the production allocation path"
    );
    println!("TILEMAP-M2 PASS mode=storage");
}

fn work() {
    // Both arms use identical geometry. "Spread across many maps" under a fixed
    // aggregate necessarily means smaller maps, so comparing a 1024x256 map
    // against the maximum map count would vary geometry and distribution
    // together and measure a ratio of about 0.15 rather than a per-map term.
    let single = vec![map_of(BALANCE.0, BALANCE.1)];
    let many: Vec<TileMap> = (0..MAX_MAPS)
        .map(|_| map_of(BALANCE.0, BALANCE.1))
        .collect();
    let per_map = MAX_LIVE_COLLIDERS as usize / MAX_MAPS;

    let (one_charges, one_received) = sweep(&single, |_| 0);
    let (many_charges, many_received) = sweep(&many, |i| i / per_map);

    // Configuration assertions. Each of these is a way the arrangement could be
    // wrong while the totals stayed plausible.
    assert_eq!(
        many_received.len(),
        MAX_MAPS,
        "the spread arm must hold the maximum map count"
    );
    assert!(
        many_received.iter().all(|count| *count == per_map),
        "every map must carry its share of bodies: {many_received:?}"
    );
    assert_eq!(
        many_received.iter().filter(|count| **count > 0).count(),
        MAX_MAPS,
        "every map must be swept, not just reachable"
    );
    assert_eq!(one_received, vec![MAX_LIVE_COLLIDERS as usize]);
    assert!(
        one_charges.iter().all(|charge| *charge > 0),
        "every body must charge something; a battery of free sweeps proves nothing"
    );
    assert!(many_charges.iter().all(|charge| *charge > 0));
    // This catches a degenerate battery - every body given the same start, so
    // the arrangement is one measurement repeated - which the prediction below
    // cannot, because it would predict the degenerate figure correctly. It does
    // *not* catch a per-body term, and an earlier comment here claimed it did.
    assert!(
        one_charges.iter().any(|charge| *charge != one_charges[0]),
        "per-body cost must vary, or the battery is one measurement repeated"
    );

    // The check that actually bounds the arrangement, in both arms. Derived
    // from the geometry, not from the solver, so it fails for a per-body term
    // of any shape - including the additive one every other assertion here
    // admits - as well as for a per-map one.
    let predicted_total = check_predicted(&one_charges, "one map");
    assert_eq!(check_predicted(&many_charges, "spread"), predicted_total);

    // `predicted` reads the same `start_cell` the battery does, so it cannot
    // see the arrangement changing underneath both of them. Two checks close
    // that, neither of them a second copy of `start_cell`.
    //
    // Order matters here and it took a breakage to see why. Both checks catch
    // a lost column, but the total moves whenever the spread does, so with the
    // literal first the distinct-pair check never fires at all and its
    // diagnosis is permanently pre-empted. The specific one goes first.
    //
    // First, the coprimality argument in `start_cell`'s comment, asserted
    // rather than left as a number-theoretic claim nothing checks: lcm(21, 50)
    // is 1,050, so no two of the 1,024 bodies share a start.
    let distinct: std::collections::HashSet<(u32, u32)> =
        (0..MAX_LIVE_COLLIDERS as usize).map(start_cell).collect();
    assert_eq!(
        distinct.len(),
        MAX_LIVE_COLLIDERS as usize,
        "every body must start somewhere different, which is what the moduli are chosen for"
    );
    // Then the total, pinned to a literal because a literal cannot follow
    // `start_cell` anywhere. The same convention as
    // `the_fixed_pass_ceiling_cannot_be_reached_under_the_frozen_limits`
    // pinning 11,796,480 rather than recomputing it: a deliberate change to the
    // arrangement updates the number, and the diff records that a measurement
    // moved.
    assert_eq!(
        predicted_total, 1_145_600,
        "the arrangement's total is pinned; if this moved deliberately, update it and say so"
    );

    let one: u64 = one_charges.iter().sum();
    let spread: u64 = many_charges.iter().sum();
    let ceiling = ceiling_for(BALANCE);
    println!("one 128x64 map        {one} units");
    println!("64 identical 128x64   {spread} units");
    println!("geometry predicts     {predicted_total} units");
    println!("arrangement ceiling   {ceiling} units");
    println!("fixed-pass ceiling    {MAX_FIXED_PASS_WORK} units");

    assert!(
        one > 0 && one <= ceiling,
        "measured cost must be positive and within the arrangement's own worst case: {one} of {ceiling}"
    );
    assert!(spread > 0 && spread <= ceiling);
    // The baseline pair. Equal here by construction - a `Vec` index has no
    // per-map cost - so this is a reference value and not evidence. Phase 2
    // asserts it against a kernel that can grow one.
    assert_eq!(
        one_charges, many_charges,
        "identical geometry and identical bodies must charge identically per body"
    );
    assert_eq!(one, spread);
    println!("baseline pair equal; Phase 2 owns the assertion");
    println!("TILEMAP-M2 PASS mode=work");
}

/// One content arrangement, and what the contract says should happen to it.
struct Shape {
    name: &'static str,
    /// `(count, columns, rows)` groups.
    groups: &'static [(usize, u32, u32)],
    fits: bool,
}

fn shapes() {
    const SHAPES: &[Shape] = &[
        Shape {
            name: "sixteen 128x128 rooms",
            groups: &[(16, 128, 128)],
            fits: true,
        },
        Shape {
            name: "overworld 256x256 plus eight 64x64 rooms",
            groups: &[(1, 256, 256), (8, 64, 64)],
            fits: true,
        },
        Shape {
            name: "the balance point: sixty-four 128x64",
            groups: &[(64, 128, 64)],
            fits: true,
        },
        // The arrangement that decided the map count. At 32 maps this was
        // refused with half the cell budget unused; the owner raised the count
        // to 64 on that evidence, so it now fits with room on both limits.
        Shape {
            name: "sixty-four 64x64 rooms",
            groups: &[(64, 64, 64)],
            fits: true,
        },
        // A count still binds, and this is what it now refuses: many small
        // rooms whose storage is trivial. Without any count, 524,288 maps of
        // one cell would fit the cell budget while costing a slot, a
        // generation and three Vec headers each.
        Shape {
            name: "one hundred 32x32 rooms",
            groups: &[(100, 32, 32)],
            fits: false,
        },
        Shape {
            name: "four full-size 1024x256 maps",
            groups: &[(4, 1_024, 256)],
            fits: false,
        },
    ];

    let mut checked = 0;
    for shape in SHAPES {
        let count: usize = shape.groups.iter().map(|(n, _, _)| n).sum();
        let cells: usize = shape
            .groups
            .iter()
            .map(|(n, c, r)| n * (*c as usize) * (*r as usize))
            .sum();
        let over_count = count > MAX_MAPS;
        let over_cells = cells > MAX_AGGREGATE_CELLS;
        let binds = match (over_count, over_cells) {
            (true, true) => "both",
            (true, false) => "map count",
            (false, true) => "cell budget",
            (false, false) => "-",
        };
        println!(
            "{:<42} {:>2} maps {:>7} cells  fits={:<5} binds={}",
            shape.name,
            count,
            cells,
            !over_count && !over_cells,
            binds
        );
        assert_eq!(
            !over_count && !over_cells,
            shape.fits,
            "{} does not behave as the contract claims",
            shape.name
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        SHAPES.len(),
        "every shape must have been evaluated"
    );

    // The cross-over the contract's open question turns on: below this many
    // cells per map the count binds first, above it the cell budget does.
    let per_map = MAX_AGGREGATE_CELLS / MAX_MAPS;
    assert_eq!(
        per_map,
        (BALANCE.0 as usize) * (BALANCE.1 as usize),
        "the comparison shape must be the one that fills the aggregate exactly"
    );
    // 8,192 is not a perfect square, so unlike the 32-map case the balance
    // point is not a square shape. Recorded rather than rounded past, because
    // 90x90 is what a reader reaching for a square will try.
    let square = 90usize;
    assert!(
        MAX_MAPS * square * square <= MAX_AGGREGATE_CELLS
            && MAX_MAPS * (square + 1) * (square + 1) > MAX_AGGREGATE_CELLS,
        "90x90 must be the largest square whose {MAX_MAPS} copies fit"
    );
    println!(
        "cross-over at {per_map} cells per map ({}x{} exactly; largest square {square}x{square}, {} cells spare)",
        BALANCE.0,
        BALANCE.1,
        MAX_AGGREGATE_CELLS - MAX_MAPS * square * square
    );
    println!("TILEMAP-M2 PASS mode=shapes");
}

fn timing() {
    const REPEATS: usize = 15;
    let maps: Vec<TileMap> = (0..MAX_MAPS)
        .map(|_| map_of(BALANCE.0, BALANCE.1))
        .collect();
    let per_map = MAX_LIVE_COLLIDERS as usize / MAX_MAPS;

    let mut samples = Vec::with_capacity(REPEATS);
    let mut total = 0u64;
    for _ in 0..REPEATS {
        let start = Instant::now();
        let (charges, received) = sweep(&maps, |i| i / per_map);
        samples.push(start.elapsed());
        total = charges.iter().sum();
        assert_eq!(
            received.iter().filter(|count| **count > 0).count(),
            MAX_MAPS,
            "the timed pass must sweep every map"
        );
    }
    assert!(total > 0, "the timed pass must have charged something");
    samples.sort_unstable();
    let at = |fraction: f64| samples[((samples.len() - 1) as f64 * fraction) as usize];
    println!("charged {total} units over {REPEATS} passes");
    println!(
        "p50 {:?}  p95 {:?}  max {:?}",
        at(0.5),
        at(0.95),
        samples[samples.len() - 1]
    );
    println!("tick period is 16.667 ms; this is one pass, not a frame");
    println!("TILEMAP-M2 PASS mode=timing");
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("storage") => storage(),
        Some("work") => work(),
        Some("shapes") => shapes(),
        Some("timing") => timing(),
        other => {
            eprintln!("unknown mode {other:?}; expected storage, work, shapes or timing");
            std::process::exit(2);
        }
    }
}
