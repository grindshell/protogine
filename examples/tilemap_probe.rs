//! Phase 0 tilemap/collision feasibility probe: prototype geometry and work
//! counting, not the production kernel. Run under `tools/run_tilemap_probe.ps1`
//! for independent process watchdogs.
//!
//! Nothing here is a production API. Phases 1 and 2 own `src/tilemap.rs` and
//! `src/collision.rs`; this file exists to settle the numerical rule and the
//! worst-case budgets before that code is written, and it remains the historical
//! evidence behind `docs/implementation/TILEMAP_COLLISION_PHASE0.md`.
//!
//! Every mode prints `TILEMAP PASS mode=<name>` last. Each `control-*` mode
//! replaces one frozen rule with the mistake it exists to prevent and must fail
//! at a named assertion rather than merely exiting nonzero.

use hecs::Entity;
use protogine::kernel::{ENTITY_LIMIT, FIXED_DT, Position, Velocity};
use std::time::{Duration, Instant};

// Proposed M1 ceilings from TILEMAP_COLLISION_PLAN.md, recounted by this probe.
const MAX_MAP_DIMENSION: i64 = 1_024;
const MAX_MAP_CELLS: i64 = 262_144;
const MAX_COLLIDER_TILES: i64 = 8;
const MAX_COLLIDER_PIXELS: f64 = 4_096.0;
const MIN_COLLIDER_EXTENT: f64 = 1.0 / 256.0;
const MAX_COLLIDER_OFFSET: f64 = 4_096.0;
const GEOMETRY_LIMIT: f64 = 16_777_216.0;
const SOLID_ID_LIMIT: usize = 1_024;
const COLLIDER_LIMIT: usize = 1_024;
const FIXED_PASS_UNITS: u64 = 16_777_216;
const CALLBACK_UNITS: u64 = 1_048_576;
const REGION_CELLS_PER_CALL: u64 = 4_096;
const REGION_CELLS_PER_CALLBACK: u64 = 262_144;

/// Bounded repair iterations for the clamped-position rule.
const REPAIR_STEPS: u32 = 4;
/// Fixed-point unit of the integer oracle: the smallest allowed collider extent.
const SCALE: i64 = 256;

// ---------------------------------------------------------------------------
// Prototype geometry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

impl Axis {
    fn other(self) -> Self {
        match self {
            Self::X => Self::Y,
            Self::Y => Self::X,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn get(self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.x,
            Axis::Y => self.y,
        }
    }

    fn with(self, axis: Axis, value: f64) -> Self {
        match axis {
            Axis::X => Self { x: value, ..self },
            Axis::Y => Self { y: value, ..self },
        }
    }
}

/// An entity's optional axis-aligned collider, in world pixels relative to its
/// position. Its box is `[position + offset, position + offset + size)`.
#[derive(Clone, Copy, Debug)]
struct Body {
    offset: Point,
    size: Point,
}

impl Body {
    fn square(size: f64) -> Self {
        Self {
            offset: Point::new(0.0, 0.0),
            size: Point::new(size, size),
        }
    }

    fn edge_low(self, position: Point, axis: Axis) -> f64 {
        position.get(axis) + self.offset.get(axis)
    }

    fn edge_high(self, position: Point, axis: Axis) -> f64 {
        self.edge_low(position, axis) + self.size.get(axis)
    }
}

/// A finite orthogonal map: immutable dimensions, origin and solid definitions
/// with mutable dense row-major cell IDs.
#[derive(Clone)]
struct Grid {
    columns: i64,
    rows: i64,
    tile_x: i64,
    tile_y: i64,
    origin_x: i64,
    origin_y: i64,
    cells: Vec<u16>,
    /// `solids[i]` defines tile ID `i + 1`; ID 0 is implicitly non-solid.
    solids: Vec<bool>,
}

impl Grid {
    fn empty(columns: i64, rows: i64, tile: i64) -> Self {
        Self {
            columns,
            rows,
            tile_x: tile,
            tile_y: tile,
            origin_x: 0,
            origin_y: 0,
            cells: vec![0; (columns * rows) as usize],
            solids: vec![true],
        }
    }

    /// A readable fixture: `#` is solid tile ID 1, anything else is empty.
    fn sketch(rows: &[&str], tile: i64) -> Self {
        let columns = rows[0].len() as i64;
        let mut cells = Vec::with_capacity(rows.len() * columns as usize);
        for line in rows {
            assert_eq!(line.len() as i64, columns, "ragged sketch row {line:?}");
            cells.extend(line.bytes().map(|byte| u16::from(byte == b'#')));
        }
        Self {
            columns,
            rows: rows.len() as i64,
            tile_x: tile,
            tile_y: tile,
            origin_x: 0,
            origin_y: 0,
            cells,
            solids: vec![true],
        }
    }

    fn with_origin(mut self, origin_x: i64, origin_y: i64) -> Self {
        self.origin_x = origin_x;
        self.origin_y = origin_y;
        self
    }

    fn with_tiles(mut self, tile_x: i64, tile_y: i64) -> Self {
        self.tile_x = tile_x;
        self.tile_y = tile_y;
        self
    }

    fn count(&self, axis: Axis) -> i64 {
        match axis {
            Axis::X => self.columns,
            Axis::Y => self.rows,
        }
    }

    fn tile(&self, axis: Axis) -> i64 {
        match axis {
            Axis::X => self.tile_x,
            Axis::Y => self.tile_y,
        }
    }

    fn origin(&self, axis: Axis) -> i64 {
        match axis {
            Axis::X => self.origin_x,
            Axis::Y => self.origin_y,
        }
    }

    /// The world coordinate of face `index`. Origins and tile sizes are integers
    /// inside the geometry domain, so every face is an exact f64.
    fn face(&self, axis: Axis, index: i64) -> f64 {
        (self.origin(axis) + index * self.tile(axis)) as f64
    }

    fn low(&self, axis: Axis) -> f64 {
        self.face(axis, 0)
    }

    fn high(&self, axis: Axis) -> f64 {
        self.face(axis, self.count(axis))
    }

    /// Outside the installed map is solid (T3).
    fn solid(&self, column: i64, row: i64) -> bool {
        if column < 0 || row < 0 || column >= self.columns || row >= self.rows {
            return true;
        }
        let id = self.cells[(row * self.columns + column) as usize];
        id != 0 && self.solids[id as usize - 1]
    }

    fn solid_on(&self, axis: Axis, along: i64, across: i64) -> bool {
        match axis {
            Axis::X => self.solid(along, across),
            Axis::Y => self.solid(across, along),
        }
    }

    fn set(&mut self, column: i64, row: i64, id: u16) {
        self.cells[(row * self.columns + column) as usize] = id;
    }
}

/// One deliberately broken rule per control. A control flips a flag the frozen
/// code path already reads, so it exercises that path rather than a second copy.
#[derive(Clone, Copy, Default)]
struct Controls {
    endpoint_only: bool,
    corner_only: bool,
    truncate_index: bool,
    y_first: bool,
    naive_clamp: bool,
}

struct Solver<'a> {
    grid: &'a Grid,
    controls: Controls,
    /// Visited cells and body checks: the fixed-pass and callback work unit.
    work: u64,
    repairs: u64,
    repair_steps_max: u32,
    fallbacks: u64,
    index_corrections_max: u32,
    clamp_gap_max: f64,
}

impl<'a> Solver<'a> {
    fn new(grid: &'a Grid, controls: Controls) -> Self {
        Self {
            grid,
            controls,
            work: 0,
            repairs: 0,
            repair_steps_max: 0,
            fallbacks: 0,
            index_corrections_max: 0,
            clamp_gap_max: 0.0,
        }
    }

    /// The cell containing `world` on `axis`.
    ///
    /// The quotient rounds, and near the domain edge it can round up to an exact
    /// integer a whole cell past the answer, so the raw floor is never trusted:
    /// exact integer faces decide. Callers clip into the map first, which keeps
    /// the correction one cell wide.
    fn cell_index(&mut self, axis: Axis, world: f64) -> i64 {
        let (origin, tile, count) = (
            self.grid.origin(axis),
            self.grid.tile(axis),
            self.grid.count(axis),
        );
        if self.controls.truncate_index {
            // Negative control: truncation toward zero, which folds the first
            // cell left of a negative origin onto the first cell inside it.
            return ((world - origin as f64) / tile as f64) as i64;
        }
        let mut index = ((world - origin as f64) / tile as f64)
            .floor()
            .clamp(-1.0, count as f64) as i64;
        let mut corrections = 0;
        while self.grid.face(axis, index) > world {
            index -= 1;
            corrections += 1;
        }
        while self.grid.face(axis, index + 1) <= world {
            index += 1;
            corrections += 1;
        }
        self.index_corrections_max = self.index_corrections_max.max(corrections);
        index
    }

    /// The half-open cell span `[low, high)` covers. A box whose far edge lands
    /// exactly on a face does not overlap the cell beyond it.
    fn span(&mut self, axis: Axis, low: f64, high: f64) -> (i64, i64) {
        let first = self.cell_index(axis, low);
        let mut last = self.cell_index(axis, high);
        if self.grid.face(axis, last) >= high {
            last -= 1;
        }
        (first, last.max(first))
    }

    /// Every cell of one perpendicular span, so an interior solid tile cannot
    /// hide between clear corners.
    fn solid_across(&mut self, axis: Axis, along: i64, first: i64, last: i64) -> bool {
        // Negative control: keep only the span's endpoints, which is the
        // four-corner test this enumeration replaces.
        let step = if self.controls.corner_only && last > first {
            last - first
        } else {
            1
        };
        let mut across = first;
        while across <= last {
            self.work += 1;
            if self.grid.solid_on(axis, along, across) {
                return true;
            }
            across += step;
        }
        false
    }

    /// T5/T6 placement predicate: positive-area overlap with a solid tile, or
    /// any part of the box outside the installed map. The extent is compared
    /// against the map before any index conversion, so an out-of-map box is
    /// refused rather than converted.
    fn blocked(&mut self, body: Body, position: Point) -> bool {
        for axis in [Axis::X, Axis::Y] {
            if body.edge_low(position, axis) < self.grid.low(axis)
                || body.edge_high(position, axis) > self.grid.high(axis)
            {
                return true;
            }
        }
        let (first, last) = self.span(
            Axis::X,
            body.edge_low(position, Axis::X),
            body.edge_high(position, Axis::X),
        );
        let (row_first, row_last) = self.span(
            Axis::Y,
            body.edge_low(position, Axis::Y),
            body.edge_high(position, Axis::Y),
        );
        let step = if self.controls.corner_only && last > first {
            last - first
        } else {
            1
        };
        let mut column = first;
        while column <= last {
            if self.solid_across(Axis::X, column, row_first, row_last) {
                return true;
            }
            column += step;
        }
        false
    }

    fn order(&self) -> [Axis; 2] {
        // Negative control: T4 fixes X before Y, which decides corner outcomes.
        if self.controls.y_first {
            [Axis::Y, Axis::X]
        } else {
            [Axis::X, Axis::Y]
        }
    }

    /// T4: sweep the first axis fully, then the second from the resolved position.
    fn solve(&mut self, body: Body, position: Point, travel: Point) -> Point {
        let mut moved = position;
        for axis in self.order() {
            if self.controls.endpoint_only {
                // Negative control: the sample's endpoint test, which tunnels
                // through any wall thinner than one tick of travel.
                let candidate = moved.with(axis, moved.get(axis) + travel.get(axis));
                if !self.blocked(body, candidate) {
                    moved = candidate;
                }
            } else {
                moved = moved.with(axis, self.sweep(axis, body, moved, travel.get(axis)));
            }
        }
        moved
    }

    /// One axis of the sweep: clamp at the first solid face or map boundary the
    /// leading edge crosses, enumerating every perpendicular cell on the way.
    fn sweep(&mut self, axis: Axis, body: Body, position: Point, travel: f64) -> f64 {
        let start = position.get(axis);
        if travel == 0.0 {
            return start;
        }
        let offset = body.offset.get(axis);
        let size = body.size.get(axis);
        let across = axis.other();
        let (first, last) = self.span(
            across,
            body.edge_low(position, across),
            body.edge_high(position, across),
        );
        let count = self.grid.count(axis);
        let target = start + travel;
        // The limit is the destination edge the committed position reconstructs,
        // not `lead + travel`, so a rounding difference cannot skip a face.
        let destination = position.with(axis, target);
        if travel > 0.0 {
            let lead = body.edge_high(position, axis);
            let limit = body.edge_high(destination, axis);
            let mut index = self.cell_index(axis, lead);
            if self.grid.face(axis, index) < lead {
                index += 1;
            }
            // Faces are enumerated, never pixels, and the map boundary bounds the
            // count however large a finite velocity is.
            while self.grid.face(axis, index) < limit {
                if index >= count {
                    let boundary = self.grid.high(axis);
                    return self.clamp_below(boundary, offset, size, start, target);
                }
                if self.solid_across(axis, index, first, last) {
                    let face = self.grid.face(axis, index);
                    return self.clamp_below(face, offset, size, start, target);
                }
                index += 1;
            }
            target
        } else {
            let lead = body.edge_low(position, axis);
            let limit = body.edge_low(destination, axis);
            let mut index = self.cell_index(axis, lead);
            while self.grid.face(axis, index) > limit {
                if index <= 0 {
                    let boundary = self.grid.low(axis);
                    return self.clamp_above(boundary, offset, start, target);
                }
                if self.solid_across(axis, index - 1, first, last) {
                    let face = self.grid.face(axis, index);
                    return self.clamp_above(face, offset, start, target);
                }
                index -= 1;
            }
            target
        }
    }

    /// The frozen rule for a face ahead of the leading edge: aim at the exact
    /// free position, keep it inside the requested travel, then repair with the
    /// measured overshoot until the reconstructed box edge is on the free side.
    /// The start position is always free, so the bounded loop has a safe floor.
    fn clamp_below(&mut self, face: f64, offset: f64, size: f64, start: f64, target: f64) -> f64 {
        let ideal = (face - size) - offset;
        // The face only decides the result when the requested travel would have
        // reached it; a shorter request keeps its own destination.
        let engaged = ideal <= target;
        let mut position = ideal.min(target).max(start);
        if self.controls.naive_clamp {
            // Negative control: trust the subtraction without reconstructing.
            return position;
        }
        let mut steps = 0;
        while (position + offset) + size > face {
            if position <= start || steps == REPAIR_STEPS {
                self.fallbacks += 1;
                return start;
            }
            let overshoot = ((position + offset) + size) - face;
            let stepped = position - overshoot;
            // A correction below the position's own resolution cannot move it,
            // so fall back to a representable step and keep making progress.
            position = if stepped < position {
                stepped
            } else {
                position.next_down()
            };
            if position < start {
                position = start;
            }
            steps += 1;
        }
        self.record_clamp(face - ((position + offset) + size), steps, engaged);
        position
    }

    /// The mirror rule for a face behind the leading edge.
    fn clamp_above(&mut self, face: f64, offset: f64, start: f64, target: f64) -> f64 {
        let ideal = face - offset;
        let engaged = ideal >= target;
        let mut position = ideal.max(target).min(start);
        if self.controls.naive_clamp {
            return position;
        }
        let mut steps = 0;
        while position + offset < face {
            if position >= start || steps == REPAIR_STEPS {
                self.fallbacks += 1;
                return start;
            }
            let shortfall = face - (position + offset);
            let stepped = position + shortfall;
            position = if stepped > position {
                stepped
            } else {
                position.next_up()
            };
            if position > start {
                position = start;
            }
            steps += 1;
        }
        self.record_clamp((position + offset) - face, steps, engaged);
        position
    }

    fn record_clamp(&mut self, gap: f64, steps: u32, engaged: bool) {
        if steps > 0 {
            self.repairs += 1;
            self.repair_steps_max = self.repair_steps_max.max(steps);
        }
        // Only a clamp the face decided says anything about rounding; a request
        // that stopped short of the wall leaves an ordinary gameplay gap.
        if engaged && gap > self.clamp_gap_max {
            self.clamp_gap_max = gap;
        }
    }
}

// ---------------------------------------------------------------------------
// Independent integer oracle
// ---------------------------------------------------------------------------

/// The same T4 sweep restated in exact 1/256-pixel integer arithmetic and
/// phrased on box edges rather than position plus offset. It shares only the
/// fixture grid with the solver, so agreement is evidence and not a tautology.
fn oracle_sweep(grid: &Grid, axis: Axis, low: [i64; 2], size: [i64; 2], travel: i64) -> i64 {
    let (along, across) = (axis.index(), axis.other().index());
    if travel == 0 {
        return low[along];
    }
    let origin = grid.origin(axis) * SCALE;
    let tile = grid.tile(axis) * SCALE;
    let count = grid.count(axis);
    let face = |index: i64| origin + index * tile;

    let other_origin = grid.origin(axis.other()) * SCALE;
    let other_tile = grid.tile(axis.other()) * SCALE;
    let first = (low[across] - other_origin).div_euclid(other_tile);
    let mut last = (low[across] + size[across] - other_origin).div_euclid(other_tile);
    if other_origin + last * other_tile >= low[across] + size[across] {
        last -= 1;
    }
    let last = last.max(first);

    if travel > 0 {
        let lead = low[along] + size[along];
        let limit = lead + travel;
        let mut index = (lead - origin).div_euclid(tile);
        if face(index) < lead {
            index += 1;
        }
        while face(index) < limit {
            if index >= count {
                return face(count) - size[along];
            }
            for other in first..=last {
                if grid.solid_on(axis, index, other) {
                    return face(index) - size[along];
                }
            }
            index += 1;
        }
        low[along] + travel
    } else {
        let lead = low[along];
        let limit = lead + travel;
        let mut index = (lead - origin).div_euclid(tile);
        while face(index) > limit {
            if index <= 0 {
                return face(0);
            }
            for other in first..=last {
                if grid.solid_on(axis, index - 1, other) {
                    return face(index);
                }
            }
            index -= 1;
        }
        lead + travel
    }
}

fn oracle_solve(grid: &Grid, low: [i64; 2], size: [i64; 2], travel: [i64; 2]) -> [i64; 2] {
    let mut moved = low;
    for axis in [Axis::X, Axis::Y] {
        moved[axis.index()] = oracle_sweep(grid, axis, moved, size, travel[axis.index()]);
    }
    moved
}

// ---------------------------------------------------------------------------
// Deterministic value generation
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0 = state;
        state
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    fn between(&mut self, low: i64, high: i64) -> i64 {
        low + self.below((high - low + 1) as u64) as i64
    }

    fn real(&mut self, low: f64, high: f64) -> f64 {
        low + (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * (high - low)
    }

    /// Move a value a few representable steps in either direction: the
    /// adjacent-f64 perturbation the clamp fixtures are built from.
    fn adjacent(&mut self, value: f64) -> f64 {
        let steps = self.below(3);
        let up = self.next_u64() & 1 == 0;
        let mut shifted = value;
        for _ in 0..steps {
            shifted = if up {
                shifted.next_up()
            } else {
                shifted.next_down()
            };
        }
        shifted
    }
}

// ---------------------------------------------------------------------------
// Numerical mode
// ---------------------------------------------------------------------------

struct ClampReport {
    cases: u64,
    repairs: u64,
    steps_max: u32,
    fallbacks: u64,
    gap_max: f64,
}

/// Adjacent-f64 clamp fixtures across the whole geometry domain.
///
/// Faces are exact integers by construction, so only the collider extent, its
/// offset and the start position are perturbed. Every case asserts the three
/// properties the frozen rule promises: the reconstructed box lands on the free
/// side of the face, the result stays inside the requested travel, and the gap
/// it leaves is far below the smallest collider extent the schema admits.
fn clamp_battery(controls: Controls, rounds: u64) -> ClampReport {
    let grid = Grid::empty(1, 1, 1);
    let mut solver = Solver::new(&grid, controls);
    let mut rng = Rng(0x005e_ed0f_c1a3_9d11);
    let faces = [
        0.0,
        1.0,
        -1.0,
        32.0,
        1_024.0,
        -1_024.0,
        65_536.0,
        8_388_608.0,
        GEOMETRY_LIMIT,
        -GEOMETRY_LIMIT,
        GEOMETRY_LIMIT - 4_096.0,
        16_777_215.0,
        -16_777_215.0,
    ];
    let mut checked = 0;
    for round in 0..rounds {
        let face = if round % 4 == 0 {
            faces[(round as usize / 4) % faces.len()]
        } else {
            // Any integer face a legal map can place.
            rng.between(-16_777_216, 16_777_216) as f64
        };
        let extent = rng.real(MIN_COLLIDER_EXTENT, MAX_COLLIDER_PIXELS);
        let size = rng.adjacent(extent);
        let placed = rng.real(-MAX_COLLIDER_OFFSET, MAX_COLLIDER_OFFSET);
        let offset = rng.adjacent(placed);
        let ideal = (face - size) - offset;
        if !ideal.is_finite() {
            continue;
        }
        // A legal start is any free position behind the face; a zero gap puts
        // the hardest cases directly against it.
        let gap = if round % 3 == 0 {
            0.0
        } else {
            rng.real(0.0, 4_096.0)
        };
        let start = rng.adjacent(ideal - gap);
        if (start + offset) + size > face {
            continue;
        }
        let reach = rng.real(0.0, 8_192.0).max(gap);
        let target = start + reach;
        let clamped = solver.clamp_below(face, offset, size, start, target);
        checked += 1;

        let reconstructed = (clamped + offset) + size;
        assert!(
            reconstructed <= face,
            "clamped box on the free side: face={face:?} size={size:?} offset={offset:?} \
             start={start:?} clamped={clamped:?} edge={reconstructed:?}"
        );
        assert!(
            clamped >= start && clamped <= target,
            "clamp stays within the requested travel: start={start:?} target={target:?} \
             clamped={clamped:?}"
        );
        // Only a clamp the face actually decided says anything about rounding.
        if ideal <= target {
            assert!(
                face - reconstructed <= MIN_COLLIDER_EXTENT / 2.0,
                "clamp gap stays far below one collider unit: face={face:?} size={size:?} \
                 offset={offset:?} gap={:?}",
                face - reconstructed
            );
        }

        // The mirror rule, driven from the same numbers.
        let behind = rng.real(0.0, 4_096.0);
        let start_above = rng.adjacent((face - offset) + behind);
        if start_above + offset < face {
            continue;
        }
        let target_above = start_above - rng.real(0.0, 8_192.0).max(behind);
        let clamped = solver.clamp_above(face, offset, start_above, target_above);
        assert!(
            clamped + offset >= face,
            "clamped box on the free side: face={face:?} offset={offset:?} \
             start={start_above:?} clamped={clamped:?} edge={:?}",
            clamped + offset
        );
        assert!(
            clamped <= start_above && clamped >= target_above,
            "clamp stays within the requested travel: start={start_above:?} \
             target={target_above:?} clamped={clamped:?}"
        );
        if face - offset >= target_above {
            assert!(
                (clamped + offset) - face <= MIN_COLLIDER_EXTENT / 2.0,
                "clamp gap stays far below one collider unit: face={face:?} \
                 offset={offset:?} gap={:?}",
                (clamped + offset) - face
            );
        }
    }
    ClampReport {
        cases: checked,
        repairs: solver.repairs,
        steps_max: solver.repair_steps_max,
        fallbacks: solver.fallbacks,
        gap_max: solver.clamp_gap_max,
    }
}

fn free_flight() {
    // The sample's migrated speed. `120 * FIXED_DT` must be exactly 2.0 for the
    // sample's existing exact-equality assertions to survive the migration.
    let step = 120.0 * FIXED_DT;
    assert!(
        step == 2.0,
        "fixed-step product: 120 * FIXED_DT is {step:?} (0x{:016x}), not 2.0",
        step.to_bits()
    );
    println!(
        "fixed_dt=0x{:016x} step_120=0x{:016x}",
        FIXED_DT.to_bits(),
        step.to_bits()
    );

    // Thirty ticks right then thirty down, the sample's replay expectation.
    let (mut x, mut y) = (448.0_f64, 256.0_f64);
    for _ in 0..30 {
        x += step;
    }
    for _ in 0..30 {
        y += step;
    }
    assert!(
        (x, y) == (508.0, 316.0),
        "free-flight accumulation: reached ({x:?}, {y:?}), not (508.0, 316.0)"
    );
    // Sixty whole ticks are exactly one second of travel, so a 30/60/144 FPS
    // replay compares equal positions rather than nearby ones.
    let mut travelled = 0.0_f64;
    for _ in 0..60 {
        travelled += step;
    }
    assert!(
        travelled == 120.0,
        "free-flight accumulation: one second of travel is {travelled:?}"
    );

    // The exactness belongs to 120, not to the integration: a nearby speed
    // accumulates a different value than one multiplication produces.
    let inexact = 130.0 * FIXED_DT;
    let mut summed = 0.0_f64;
    for _ in 0..30 {
        summed += inexact;
    }
    assert!(
        summed != 30.0 * inexact,
        "the exactness check must discriminate: 130 px/s also accumulated exactly"
    );
    println!(
        "step_130=0x{:016x} drift_30={:e}",
        inexact.to_bits(),
        summed - 30.0 * inexact
    );
}

fn negative_origin(controls: Controls) {
    // A 4x4 map of 32-pixel tiles whose origin is left of and above zero.
    let grid = Grid::sketch(&["....", "....", "....", "...."], 32).with_origin(-100, -70);
    let mut solver = Solver::new(&grid, controls);
    for (world, expected) in [
        (-100.0, 0),
        (-99.0, 0),
        (-69.0, 0),
        (-68.0, 1),
        (-101.0, -1),
        (-132.0, -1),
        (-133.0, -2),
        (28.0, 4),
    ] {
        let index = solver.cell_index(Axis::X, world);
        assert!(
            index == expected,
            "negative origin cell index: world {world:?} is column {index}, not {expected}"
        );
    }
    for (world, expected) in [(-70.0, 0), (-71.0, -1), (-39.0, 0), (-38.0, 1)] {
        let index = solver.cell_index(Axis::Y, world);
        assert!(
            index == expected,
            "negative origin cell index: world {world:?} is row {index}, not {expected}"
        );
    }
    // A box that starts left of the origin is outside the map, and outside is
    // solid, so it is refused before any index conversion happens.
    assert!(
        solver.blocked(Body::square(32.0), Point::new(-101.0, -70.0)),
        "outside the map is solid: a box left of the origin must be refused"
    );
    assert!(
        !solver.blocked(Body::square(32.0), Point::new(-100.0, -70.0)),
        "outside the map is solid: the first cell inside the origin is free"
    );
}

/// Scan every legal tile size against the face-adjacent coordinates a map pushed
/// to the geometry limit can produce, and report how often the raw quotient
/// disagrees with the exact integer answer the correction enforces.
///
/// This is what decides whether the correction is load-bearing today or only
/// insurance against a future limit change, so the count is reported rather than
/// asserted away.
fn quotient_scan(controls: Controls) -> (u64, u64) {
    let mut checked = 0;
    let mut disagreements = 0;
    for tile in 1..=MAX_MAP_DIMENSION {
        // The widest extent a legal map can reach on one axis is 2^20 pixels.
        let columns = MAX_MAP_DIMENSION.min((1 << 20) / tile).max(1);
        for edge in [GEOMETRY_LIMIT as i64, -(GEOMETRY_LIMIT as i64)] {
            let origin = if edge > 0 {
                edge - columns * tile
            } else {
                edge
            };
            let grid = Grid::empty(columns, 1, 1)
                .with_tiles(tile, 1)
                .with_origin(origin, 0);
            let mut solver = Solver::new(&grid, controls);
            for index in [0, 1, columns / 2, columns - 1, columns] {
                let face = grid.face(Axis::X, index);
                // A face and its two representable neighbours: the only places a
                // rounded quotient can land on the wrong side of a cell edge.
                for (world, expected) in [
                    (face.next_down(), index - 1),
                    (face, index),
                    (face.next_up(), index),
                ] {
                    if world < grid.low(Axis::X) || world > grid.high(Axis::X) {
                        continue;
                    }
                    checked += 1;
                    let raw = ((world - origin as f64) / tile as f64).floor();
                    if raw != expected as f64 {
                        disagreements += 1;
                    }
                    let got = solver.cell_index(Axis::X, world);
                    assert!(
                        got == expected,
                        "cell index matches its exact face: world {world:?} on tile {tile} \
                         origin {origin} is {got}, not {expected}"
                    );
                }
            }
        }
    }
    (checked, disagreements)
}

fn interior_cell(controls: Controls) {
    // Three tiles square with only the middle cell solid: every corner is clear.
    let grid = Grid::sketch(&["...", ".#.", "..."], 32);
    let mut solver = Solver::new(&grid, controls);
    assert!(
        solver.blocked(Body::square(96.0), Point::new(0.0, 0.0)),
        "interior solid cell: a 3x3 box over a solid centre must be blocked"
    );

    // The same hole inside a sweep's perpendicular span: only the middle row of
    // the blocking column is solid, so endpoint rows alone would pass through.
    let grid = Grid::sketch(&["....", "...#", "....", "....", "...."], 32);
    let mut solver = Solver::new(&grid, controls);
    let moved = solver.solve(
        Body::square(96.0),
        Point::new(0.0, 0.0),
        Point::new(32.0, 0.0),
    );
    assert!(
        moved.x == 0.0,
        "interior solid cell: the sweep passed a solid row and reached x={:?}",
        moved.x
    );
}

fn high_speed_wall(controls: Controls) {
    // Twenty columns with one solid cell in the middle row. A single tick of
    // travel crosses that wall and lands on a clear cell beyond it.
    let one_wall = [
        "....................",
        "..........#.........",
        "....................",
    ];
    let grid = Grid::sketch(&one_wall, 32);
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(32.0);
    let start = Point::new(0.0, 32.0);
    let moved = solver.solve(body, start, Point::new(384.0, 0.0));
    assert!(
        moved.x == 288.0,
        "high-speed wall stop: twelve tiles of travel ended at x={:?}, not 288.0",
        moved.x
    );

    // The nearest of two walls wins, whatever the requested travel.
    let two_walls = [
        "....................",
        "......#...#.........",
        "....................",
    ];
    let grid = Grid::sketch(&two_walls, 32);
    let mut solver = Solver::new(&grid, controls);
    let moved = solver.solve(body, start, Point::new(512.0, 0.0));
    assert!(
        moved.x == 160.0,
        "nearest wall wins: stopped at x={:?}, not 160.0",
        moved.x
    );
}

fn boundary_stop(controls: Controls) {
    // A finite but enormous velocity must stop at the map's own edge, having
    // enumerated faces rather than pixels.
    let grid = Grid::empty(MAX_MAP_DIMENSION, MAX_MAP_CELLS / MAX_MAP_DIMENSION, 1);
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(8.0);
    let start = Point::new(0.0, 0.0);
    let mut units = 0;
    for speed in [1.0e9, 1.0e300, f64::MAX] {
        let travel = speed * FIXED_DT;
        assert!(travel.is_finite());
        solver.work = 0;
        let moved = solver.solve(body, start, Point::new(travel, travel));
        assert!(
            moved == Point::new(1_016.0, 248.0),
            "boundary stop: {speed:e} px/s reached {moved:?}, not (1016.0, 248.0)"
        );
        // 1,016 crossed columns and 248 crossed rows over eight-cell spans.
        assert!(
            solver.work <= 16_384,
            "boundary stop: {speed:e} px/s visited {} cells",
            solver.work
        );
        units = solver.work;
    }
    println!("boundary_stop_units={units}");
}

fn corner_order(controls: Controls) {
    // One solid diagonal neighbour. Resolving X first slides along that tile's
    // top face; resolving Y first slides along its left face.
    let grid = Grid::sketch(&["....", ".#..", "....", "...."], 32);
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(32.0);
    let moved = solver.solve(body, Point::new(0.0, 0.0), Point::new(32.0, 32.0));
    assert!(
        moved == Point::new(32.0, 0.0),
        "x-before-y corner: resolved to {moved:?}, not (32.0, 0.0)"
    );

    // Wall sliding: blocked on X, still free on Y, with velocity unchanged.
    let grid = Grid::sketch(&["..#.", "..#.", "..#.", "..#."], 32);
    let mut solver = Solver::new(&grid, controls);
    let moved = solver.solve(body, Point::new(32.0, 0.0), Point::new(32.0, 32.0));
    assert!(
        moved == Point::new(32.0, 32.0),
        "wall sliding: resolved to {moved:?}, not (32.0, 32.0)"
    );
}

fn edge_contact(controls: Controls) {
    let grid = Grid::sketch(&["..#.", "..#.", "....", "...."], 32);
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(32.0);
    // Flush against the wall's left face: contact is not overlap.
    let touching = Point::new(32.0, 0.0);
    assert!(
        !solver.blocked(body, touching),
        "edge contact is free: a flush box must not overlap"
    );
    assert!(
        solver.solve(body, touching, Point::new(2.0, 0.0)).x == 32.0,
        "edge contact is free: moving into a touching face must not advance"
    );
    assert!(
        solver.solve(body, touching, Point::new(-2.0, 0.0)).x == 30.0,
        "edge contact is free: moving away from a touching face must advance"
    );
    // One representable step of the reconstructed edge decides the half-open
    // comparison either way. The step has to be taken on the edge, not on the
    // position: at this magnitude one step of the position is half an ulp of the
    // sum, so nudging the position alone rounds back onto the face.
    let face = 64.0_f64;
    for (size, expected) in [
        (face.next_down() - 32.0, false),
        (face.next_up() - 32.0, true),
    ] {
        let probe = Body {
            offset: Point::new(0.0, 0.0),
            size: Point::new(size, 32.0),
        };
        let edge = probe.edge_high(touching, Axis::X);
        assert!(
            solver.blocked(probe, touching) == expected,
            "half-open overlap: an edge at {edge:?} against face {face:?} must \
             {} the box",
            if expected { "block" } else { "free" }
        );
    }

    // A row boundary shared with a solid tile is tangent, not overlapping.
    let grid = Grid::sketch(&["....", "..#.", "....", "...."], 32);
    let mut solver = Solver::new(&grid, controls);
    assert!(
        solver
            .solve(body, Point::new(0.0, 0.0), Point::new(96.0, 0.0))
            .x
            == 96.0,
        "tangent row excluded: a box above a solid tile must slide past it"
    );
}

fn maximum_footprint(controls: Controls) {
    // The largest legal map pushed against the positive geometry limit, with the
    // largest legal body: 1,024 x 256 tiles of 1,024 pixels and a 4,096 pixel
    // square, which is min(8 tiles, 4,096 pixels) at this tile size.
    let columns = MAX_MAP_DIMENSION;
    let rows = MAX_MAP_CELLS / MAX_MAP_DIMENSION;
    let tile = 1_024;
    let grid = Grid::empty(columns, rows, tile).with_origin(
        GEOMETRY_LIMIT as i64 - columns * tile,
        GEOMETRY_LIMIT as i64 - rows * tile,
    );
    assert!(grid.high(Axis::X) == GEOMETRY_LIMIT && grid.high(Axis::Y) == GEOMETRY_LIMIT);
    let size = MAX_COLLIDER_PIXELS.min((MAX_COLLIDER_TILES * tile) as f64);
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(size);
    let start = Point::new(grid.low(Axis::X), grid.low(Axis::Y));
    assert!(!solver.blocked(body, start));

    let moved = solver.solve(body, start, Point::new(1.0e12, 1.0e12));
    let expected = Point::new(GEOMETRY_LIMIT - size, GEOMETRY_LIMIT - size);
    assert!(
        moved == expected,
        "maximum footprint: swept to {moved:?}, not {expected:?}"
    );
    assert!(
        !solver.blocked(body, moved),
        "maximum footprint: the clamped box must be free"
    );

    // The same corner with offsets and extents that no longer subtract exactly.
    let mut checked = 0;
    for offset in [0.5, -0.5, MAX_COLLIDER_OFFSET, -MAX_COLLIDER_OFFSET] {
        // A third of a pixel carries mantissa bits the domain-edge subtraction
        // must drop, so these extents exercise the repair rather than luck.
        for extent in [
            size,
            size.next_down(),
            MAX_COLLIDER_PIXELS - 1.0 / 3.0,
            1.0 / 256.0,
        ] {
            let body = Body {
                offset: Point::new(offset, offset),
                size: Point::new(extent, extent),
            };
            let start = Point::new(
                grid.low(Axis::X) - offset + 1.0,
                grid.low(Axis::Y) - offset + 1.0,
            );
            if solver.blocked(body, start) {
                continue;
            }
            let moved = solver.solve(body, start, Point::new(1.0e12, 1.0e12));
            checked += 1;
            for axis in [Axis::X, Axis::Y] {
                let edge = body.edge_high(moved, axis);
                assert!(
                    edge <= GEOMETRY_LIMIT,
                    "clamped box on the free side: offset={offset:?} extent={extent:?} \
                     edge={edge:?} is past the boundary"
                );
                assert!(
                    GEOMETRY_LIMIT - edge <= MIN_COLLIDER_EXTENT / 2.0,
                    "clamp gap stays far below one collider unit: offset={offset:?} \
                     extent={extent:?} gap={:?}",
                    GEOMETRY_LIMIT - edge
                );
            }
        }
    }
    println!(
        "max_footprint_cases={checked} repairs={} steps_max={} fallbacks={} \
         index_corrections_max={}",
        solver.repairs, solver.repair_steps_max, solver.fallbacks, solver.index_corrections_max
    );
    assert!(
        solver.fallbacks == 0,
        "clamp repair never falls back: {} maximum-footprint cases exhausted it",
        solver.fallbacks
    );
}

/// The shipped sprite room as an engine map, so the migration's expectations are
/// captured against the frozen geometry rather than restated from the sample.
///
/// The legend becomes tile IDs 1..5 with ID 0 unused, which also proves that a
/// non-solid ID other than 0 works and that `solids` is indexed by ID.
fn sample_room() -> Grid {
    const ROWS: [&str; 17] = [
        "##############################",
        "#............................#",
        "#..T.....############........#",
        "#.T.T....#..........#...T.T..#",
        "#..T.....#...####...#....T...#",
        "#........#...####...#........#",
        "#....c...#...####...#....c...#",
        "#............####............#",
        "#..fffff.............fffff...#",
        "#........#..........#........#",
        "#....c...#..........#....c...#",
        "#........#..........#........#",
        "#..T.T...#..........#...T....#",
        "#...T....#..........#..T.T...#",
        "#........############........#",
        "#............................#",
        "##############################",
    ];
    const LEGEND: [(u8, bool); 5] = [
        (b'.', false),
        (b'#', true),
        (b'T', true),
        (b'c', true),
        (b'f', true),
    ];
    let columns = ROWS[0].len() as i64;
    let mut cells = Vec::with_capacity(ROWS.len() * columns as usize);
    for line in ROWS {
        assert_eq!(
            line.len() as i64,
            columns,
            "room row {line:?} is not 30 wide"
        );
        for byte in line.bytes() {
            let id = LEGEND
                .iter()
                .position(|(symbol, _)| *symbol == byte)
                .unwrap_or_else(|| panic!("room uses unknown tile {:?}", byte as char));
            cells.push(id as u16 + 1);
        }
    }
    Grid {
        columns,
        rows: ROWS.len() as i64,
        tile_x: 32,
        tile_y: 32,
        origin_x: 0,
        origin_y: 0,
        cells,
        solids: LEGEND.iter().map(|(_, solid)| *solid).collect(),
    }
}

/// The sample expectations `tests/sprites_sample.rs` and the live probe assert
/// today, reproduced through the frozen sweep at the migrated speed.
fn sample_expectations(controls: Controls) {
    let grid = sample_room();
    let mut solver = Solver::new(&grid, controls);
    let body = Body::square(32.0);
    let spawn = Point::new(448.0, 256.0);
    let step = 120.0 * FIXED_DT;
    assert!(
        !solver.blocked(body, spawn),
        "sample spawn is free: the collider must attach at (448, 256)"
    );

    // Holding Right walks into the fence at column 21, whose left face is 672.
    // The old four-corner `size - 1` test and this half-open box agree there
    // only because 640 + 32 lands exactly on that face.
    let mut position = spawn;
    for _ in 0..120 {
        position = solver.solve(body, position, Point::new(step, 0.0));
    }
    assert!(
        position == Point::new(640.0, 256.0),
        "sample fence stop: holding Right settled at {position:?}, not (640.0, 256.0)"
    );
    assert!(
        grid.solid(21, 8) && !grid.solid(20, 8),
        "sample fence stop: the fence must be column 21 of row 8"
    );

    // Thirty ticks Right then thirty Down, the replay fixture's expectation.
    let mut position = spawn;
    for _ in 0..30 {
        position = solver.solve(body, position, Point::new(step, 0.0));
    }
    for _ in 0..30 {
        position = solver.solve(body, position, Point::new(0.0, step));
    }
    assert!(
        position == Point::new(508.0, 316.0),
        "sample replay position: reached {position:?}, not (508.0, 316.0)"
    );

    // Every position the migrated sample can occupy stays a whole pixel, which
    // is what lets the live probe keep matching integer coordinates.
    assert!(
        position.x.fract() == 0.0 && position.y.fract() == 0.0,
        "sample positions stay integral: {position:?}"
    );
    println!(
        "sample_fence_x=640 sample_replay=(508,316) sample_ids={}",
        grid.solids.len()
    );
}

fn oracle_agreement(controls: Controls, rounds: u64) -> (u64, u32) {
    let mut rng = Rng(0x00c0_ffee_1234_5678);
    let mut compared = 0;
    let mut corrections = 0;
    for _ in 0..rounds {
        let columns = rng.between(2, 12);
        let rows = rng.between(2, 9);
        let tile_x = rng.between(1, 8);
        let tile_y = rng.between(1, 8);
        let mut grid = Grid::empty(columns, rows, 1)
            .with_tiles(tile_x, tile_y)
            .with_origin(rng.between(-64, 64), rng.between(-64, 64));
        // Sparse maps leave room for large bodies; dense ones exercise the
        // sweep's early stops. Both are generated.
        let density = rng.between(3, 16) as u64;
        for column in 0..columns {
            for row in 0..rows {
                grid.set(column, row, u16::from(rng.below(density) == 0));
            }
        }

        // Every quantity is a whole number of 1/256 pixels at a magnitude f64
        // represents exactly, so a disagreement is a logic difference and never
        // a rounding difference.
        let mut size = [0i64; 2];
        let mut low = [0i64; 2];
        let mut offset = [0i64; 2];
        let mut travel = [0i64; 2];
        for axis in [Axis::X, Axis::Y] {
            let slot = axis.index();
            let tile = grid.tile(axis);
            let extent = grid.count(axis) * tile * SCALE;
            let cap = (SCALE * tile * MAX_COLLIDER_TILES).min(extent);
            size[slot] = rng.between(1, cap);
            low[slot] = grid.origin(axis) * SCALE + rng.between(0, extent - size[slot]);
            offset[slot] = rng.between(-16 * SCALE, 16 * SCALE);
            travel[slot] = rng.between(-extent - 2 * SCALE, extent + 2 * SCALE);
        }
        let unit = SCALE as f64;
        let body = Body {
            offset: Point::new(offset[0] as f64 / unit, offset[1] as f64 / unit),
            size: Point::new(size[0] as f64 / unit, size[1] as f64 / unit),
        };
        // The position carries the remainder, so the offset is genuinely applied
        // and removed on every conversion the solver performs.
        let position = Point::new(
            (low[0] - offset[0]) as f64 / unit,
            (low[1] - offset[1]) as f64 / unit,
        );
        let mut solver = Solver::new(&grid, controls);
        if solver.blocked(body, position) {
            continue;
        }
        let moved = solver.solve(
            body,
            position,
            Point::new(travel[0] as f64 / unit, travel[1] as f64 / unit),
        );
        let expected = oracle_solve(&grid, low, size, travel);
        for axis in [Axis::X, Axis::Y] {
            let edge = body.edge_low(moved, axis) * unit;
            assert!(
                edge == expected[axis.index()] as f64,
                "oracle agreement: {axis:?} solver edge {edge:?} vs oracle {} \
                 (grid {columns}x{rows} tiles {tile_x}x{tile_y} origin {},{} low {low:?} \
                 size {size:?} offset {offset:?} travel {travel:?})",
                expected[axis.index()],
                grid.origin_x,
                grid.origin_y
            );
        }
        compared += 1;
        corrections = corrections.max(solver.index_corrections_max);
    }
    (compared, corrections)
}

fn numeric(controls: Controls) {
    free_flight();
    negative_origin(controls);
    let (scanned, disagreements) = quotient_scan(controls);
    println!("quotient_scan_cases={scanned} raw_floor_disagreements={disagreements}");
    interior_cell(controls);
    high_speed_wall(controls);
    boundary_stop(controls);
    corner_order(controls);
    edge_contact(controls);
    maximum_footprint(controls);
    sample_expectations(controls);

    let (compared, corrections) = oracle_agreement(controls, 50_000);
    println!("oracle_cases={compared} index_corrections_max={corrections}");
    assert!(compared > 10_000, "too few oracle cases were placeable");

    let report = clamp_battery(controls, 1_000_000);
    println!(
        "clamp_cases={} repaired={} steps_max={} fallbacks={} gap_max={:e}",
        report.cases, report.repairs, report.steps_max, report.fallbacks, report.gap_max
    );
    assert!(
        report.fallbacks == 0,
        "clamp repair never falls back: {} cases exhausted {REPAIR_STEPS} steps",
        report.fallbacks
    );
    assert!(
        report.repairs > 0,
        "the repair path must be exercised rather than dead code"
    );
    assert!(report.steps_max <= REPAIR_STEPS);
}

// ---------------------------------------------------------------------------
// Work and storage mode
// ---------------------------------------------------------------------------

struct Load {
    label: &'static str,
    grid: Grid,
    bodies: Vec<(Body, Point, Point)>,
}

/// The mandated long-sweep stress: the largest map that maximises columns plus
/// rows, the collider limit at maximum footprint, and a velocity that leaves the
/// map on both axes. Bodies do not block each other, so sharing the single most
/// expensive lane is a legal arrangement and the honest worst case.
fn long_sweep() -> Load {
    let columns = MAX_MAP_DIMENSION;
    let rows = MAX_MAP_CELLS / MAX_MAP_DIMENSION;
    let grid = Grid::empty(columns, rows, 1);
    let extent = MAX_COLLIDER_TILES as f64;
    let bodies = (0..COLLIDER_LIMIT)
        .map(|_| {
            (
                Body::square(extent),
                // Half a tile of misalignment makes the X sweep enumerate the
                // widest perpendicular span the extent allows.
                Point::new(0.0, 0.5),
                Point::new(1.0e9 * FIXED_DT, 1.0e9 * FIXED_DT),
            )
        })
        .collect();
    Load {
        label: "long-sweep",
        grid,
        bodies,
    }
}

/// The mandated sparse arrangement: the same population moving less than one
/// tile per tick over a map whose solid cells sit away from the bodies.
fn sparse_motion() -> Load {
    let columns = MAX_MAP_DIMENSION;
    let rows = MAX_MAP_CELLS / MAX_MAP_DIMENSION;
    let mut grid = Grid::empty(columns, rows, 8);
    for index in 0..COLLIDER_LIMIT as i64 {
        grid.set(900 + (index * 7) % 100, (index * 13) % rows, 1);
    }
    let bodies = (0..COLLIDER_LIMIT as i64)
        .map(|index| {
            (
                Body::square(16.0),
                Point::new(
                    ((index * 3) % 800 * 8) as f64,
                    ((index * 5) % 250 * 8) as f64,
                ),
                Point::new(0.5, -0.5),
            )
        })
        .collect();
    Load {
        label: "sparse-short-motion",
        grid,
        bodies,
    }
}

/// One whole fixed pass: swept colliders plus free-flight integration for the
/// rest of the entity population, collected into one candidate buffer.
fn fixed_pass(load: &Load, plain: &mut [(Position, Velocity)], candidates: &mut Vec<Point>) -> u64 {
    let mut solver = Solver::new(&load.grid, Controls::default());
    candidates.clear();
    for (body, position, travel) in &load.bodies {
        candidates.push(solver.solve(*body, *position, *travel));
    }
    for (position, velocity) in plain.iter_mut() {
        candidates.push(Point::new(
            position.x + velocity.x * FIXED_DT,
            position.y + velocity.y * FIXED_DT,
        ));
    }
    solver.work
}

fn percentiles(mut samples: Vec<Duration>) -> (Duration, Duration, Duration) {
    samples.sort_unstable();
    let last = samples.len() - 1;
    let pick = |quantile: f64| samples[(last as f64 * quantile).round() as usize];
    (pick(0.5), pick(0.95), samples[last])
}

/// Search the legal tile and extent combinations for the most expensive single
/// body on the widest map, which is what the fixed-pass ceiling must cover.
fn worst_body_cost() -> (u64, i64, i64) {
    let mut worst = (0u64, 0i64, 0i64);
    for tile in [1i64, 2, 4, 8, 32, 512, 1_024] {
        for extent_tiles in 1..=MAX_COLLIDER_TILES {
            let extent = (extent_tiles * tile) as f64;
            if extent > MAX_COLLIDER_PIXELS {
                continue;
            }
            let grid = Grid::empty(MAX_MAP_DIMENSION, MAX_MAP_CELLS / MAX_MAP_DIMENSION, tile);
            let mut solver = Solver::new(&grid, Controls::default());
            let body = Body::square(extent);
            let start = Point::new(0.0, 0.5);
            if solver.blocked(body, start) {
                continue;
            }
            // Count the sweep alone; the placement check above is charged to the
            // callback that attached the body, not to the fixed pass.
            solver.work = 0;
            solver.solve(body, start, Point::new(1.0e9, 1.0e9));
            if solver.work > worst.0 {
                worst = (solver.work, tile, extent_tiles);
            }
        }
    }
    worst
}

fn work() {
    let cell_bytes = MAX_MAP_CELLS as usize * size_of::<u16>();
    let candidate_bytes = size_of::<(Entity, Position)>();
    println!("entity_bytes={}", size_of::<Entity>());
    println!("candidate_bytes={candidate_bytes}");
    println!(
        "candidate_buffer_bytes={}",
        candidate_bytes * ENTITY_LIMIT as usize
    );
    println!("map_cell_bytes={cell_bytes}");
    println!("map_solid_bytes={SOLID_ID_LIMIT}");
    println!(
        "replacement_peak_bytes={}",
        cell_bytes * 2 + SOLID_ID_LIMIT * 2
    );
    assert!(
        cell_bytes == 512 * 1_024,
        "map storage: {cell_bytes} bytes, not the 512 KiB the plan reserves"
    );

    let (worst_units, worst_tile, worst_extent) = worst_body_cost();
    let analytic = 9 * (MAX_MAP_DIMENSION + MAX_MAP_CELLS / MAX_MAP_DIMENSION) as u64;
    println!(
        "worst_body_units={worst_units} tile={worst_tile} extent_tiles={worst_extent} \
         analytic_ceiling={analytic}"
    );
    assert!(
        worst_units <= analytic,
        "the measured worst body cost {worst_units} exceeds the plan's {analytic}"
    );

    let mut plain: Vec<(Position, Velocity)> = (0..ENTITY_LIMIT as usize - COLLIDER_LIMIT)
        .map(|index| {
            (
                Position {
                    x: index as f64,
                    y: -(index as f64),
                },
                Velocity { x: 60.0, y: -60.0 },
            )
        })
        .collect();
    let mut candidates = Vec::with_capacity(ENTITY_LIMIT as usize);

    for load in [long_sweep(), sparse_motion()] {
        let mut guard = Solver::new(&load.grid, Controls::default());
        for (body, position, _) in &load.bodies {
            assert!(
                !guard.blocked(*body, *position),
                "{}: a stress body starts overlapping, so its sweep is undefined",
                load.label
            );
        }
        let mut samples = Vec::new();
        let mut units = 0;
        for _ in 0..15 {
            let started = Instant::now();
            units = fixed_pass(&load, &mut plain, &mut candidates);
            samples.push(started.elapsed());
            assert_eq!(candidates.len(), ENTITY_LIMIT as usize);
        }
        let (p50, p95, max) = percentiles(samples);
        println!(
            "load={} colliders={} entities={} units={units} p50_us={} p95_us={} max_us={}",
            load.label,
            load.bodies.len(),
            ENTITY_LIMIT,
            p50.as_micros(),
            p95.as_micros(),
            max.as_micros()
        );
        assert!(
            units <= FIXED_PASS_UNITS,
            "fixed-pass ceiling: {} needed {units} units, over {FIXED_PASS_UNITS}",
            load.label
        );
    }

    // Callback work: the largest single mutation is a full map replacement that
    // copies and validates every element and rechecks every live collider.
    let install = MAX_MAP_CELLS as u64 + SOLID_ID_LIMIT as u64 + COLLIDER_LIMIT as u64 * 9 * 9;
    println!("install_units={install}");
    assert!(
        install <= CALLBACK_UNITS,
        "callback ceiling: installing the largest map needs {install} units"
    );
    println!(
        "region_calls_per_callback={} region_units={REGION_CELLS_PER_CALLBACK}",
        REGION_CELLS_PER_CALLBACK / REGION_CELLS_PER_CALL
    );
    let edits = (CALLBACK_UNITS - install) / (1 + COLLIDER_LIMIT as u64);
    println!("solid_edits_after_install={edits}");
    assert!(edits > 0, "a full install must leave room for cell edits");
}

// ---------------------------------------------------------------------------

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "numeric".into());
    let controls = match mode.as_str() {
        "numeric" | "work" => Controls::default(),
        "control-endpoint" => Controls {
            endpoint_only: true,
            ..Controls::default()
        },
        "control-corners" => Controls {
            corner_only: true,
            ..Controls::default()
        },
        "control-truncate" => Controls {
            truncate_index: true,
            ..Controls::default()
        },
        "control-yfirst" => Controls {
            y_first: true,
            ..Controls::default()
        },
        "control-naive-clamp" => Controls {
            naive_clamp: true,
            ..Controls::default()
        },
        other => panic!("unknown mode {other:?}"),
    };
    if mode == "work" {
        work();
    } else {
        numeric(controls);
    }
    println!("TILEMAP PASS mode={mode}");
}
