//! Tile colliders and the axis-separated swept solver.
//!
//! Pure geometry over [`crate::tilemap`] and nothing else: no ECS, scripting,
//! asset or graphics dependency reaches this module, and it holds no state
//! between calls. The kernel owns bodies, budgets and commit order; this module
//! owns the rules frozen as N1-N5 and N7 in
//! [the Phase 0 record](../docs/implementation/TILEMAP_COLLISION_PHASE0.md).

use crate::tilemap::{Axis, TileMap, TileMapInfo};
use std::fmt;

/// Smallest collider extent on either axis, in world pixels.
pub const MIN_COLLIDER_EXTENT: f64 = 1.0 / 256.0;
/// Absolute cap on a collider extent, independent of tile size.
pub const MAX_COLLIDER_PIXELS: f64 = 4_096.0;
/// Tile-relative cap on a collider extent. This is the half of the bound that
/// caps the perpendicular span a sweep enumerates, so a pixel cap alone cannot
/// replace it: a pixel-only rule would admit a body spanning half a map of
/// one-pixel tiles.
pub const MAX_COLLIDER_TILES: f64 = 8.0;
/// Largest magnitude of a collider offset, in world pixels.
pub const MAX_COLLIDER_OFFSET: f64 = 4_096.0;
/// Cells a legal body can span on one axis. Eight tiles of extent straddle at
/// most nine cells, which is what bounds every perpendicular enumeration.
pub const MAX_SPAN_CELLS: i32 = 9;
/// Colliders one session may keep attached.
pub const MAX_LIVE_COLLIDERS: u32 = 1_024;

/// Cell-visit units one fixed-systems pass may charge, with no per-entity reset.
///
/// The frozen limits keep this out of reach. At most [`MAX_LIVE_COLLIDERS`]
/// bodies each enumerate at most [`MAX_SPAN_CELLS`] cells on at most
/// `columns + rows` candidate faces - the two map boundary faces charge nothing,
/// because reaching the boundary index clamps before any cell is inspected - and
/// the map schema caps `columns + rows` at 1,280 for the 1,024 x 256 shape that
/// maximises it. A pass therefore cannot exceed 11,796,480 units. The ceiling is
/// deliberate headroom rather than a live guard, and `tests/collision.rs` pins
/// that arithmetic instead of leaving it a claim.
pub const MAX_FIXED_PASS_WORK: u64 = 16_777_216;
/// Cell-visit units one callback may charge. Phase 3 owns the aggregate across
/// a callback; this bounds any single kernel entry point.
pub const MAX_CALLBACK_WORK: u64 = 1_048_576;

/// Repair steps [`clamp_below`] and [`clamp_above`] may take before an
/// unsatisfied postcondition becomes an invariant violation (N5).
const REPAIR_STEPS: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionError {
    /// A collider offset that is not finite or exceeds [`MAX_COLLIDER_OFFSET`].
    Offset,
    /// A collider extent outside `1/256 ..= min(8 tiles, 4096)` world pixels.
    Extent,
    /// A box that would leave the map or overlap a solid tile.
    Placement,
    /// A deterministic tile-work ceiling was exhausted.
    Work,
    /// A position or displacement that is not finite.
    Nonfinite,
    /// A swept body did not start clear of the face that blocked it.
    ///
    /// The blocking face is the first at or after the leading edge, so
    /// `(start + offset) + size <= face` holds by construction and the repair
    /// loop's condition is already false at `start`. Reaching this means that
    /// premise failed, and retreating to `start` would publish an overlap.
    Embedded,
    /// A clamped position did not settle within [`REPAIR_STEPS`].
    ///
    /// Distinct from [`Self::Embedded`] because it says convergence was slow,
    /// not that the premise failed: `start` is still provably on the free side
    /// here. They are kept apart so that a log can tell "the world is broken"
    /// from "this took longer than the bound allows", which is the whole value
    /// of the first one.
    Unconverged,
}

impl fmt::Display for CollisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Offset => "collider offsets must be finite within 4096 world pixels",
            Self::Extent => "collider extents must be 1/256 to min(8 tiles, 4096) world pixels",
            Self::Placement => "the collider box must lie in the map clear of solid tiles",
            Self::Work => "tile work budget exhausted",
            Self::Nonfinite => "collider positions and displacements must be finite",
            Self::Embedded => "a swept body started inside the face that blocked it",
            Self::Unconverged => "a clamped position did not settle within its repair bound",
        })
    }
}
impl std::error::Error for CollisionError {}

/// An axis-aligned box attached to an entity, in world pixels (T3).
///
/// The box is `[position + offset, that + size)` on each axis. Offsets are
/// local to the entity position, which is not reinterpreted as a sprite corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileCollider {
    pub offset_x: f64,
    pub offset_y: f64,
    pub width: f64,
    pub height: f64,
}

impl TileCollider {
    pub fn offset(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.offset_x,
            Axis::Y => self.offset_y,
        }
    }

    pub fn size(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.width,
            Axis::Y => self.height,
        }
    }

    /// Reconstruct this collider's world box at a position.
    ///
    /// **This is the only path from a position to world edges.** N3 permits a
    /// box edge to equal a blocking face and the solver routinely produces
    /// exactly that, so the reconstructed edge has zero margin by design: a
    /// locally reassociated equivalent such as `position + (offset + size)` that
    /// rounds one ulp high turns a legal flush contact into an overlap. Every
    /// predicate below - the sweep, the placement check, teleport validation,
    /// cell-edit validation and install revalidation - goes through here.
    pub fn aabb(&self, x: f64, y: f64) -> Aabb {
        let low_x = x + self.offset_x;
        let low_y = y + self.offset_y;
        Aabb {
            low_x,
            low_y,
            high_x: low_x + self.width,
            high_y: low_y + self.height,
        }
    }

    /// Validate the collider schema against the tile sizes it will be used with.
    ///
    /// The extent cap is tile-relative, so the same collider can be legal on one
    /// map and refused on another. That is why map installation revalidates
    /// every live collider rather than only checking placement.
    pub fn check(&self, info: &TileMapInfo) -> Result<(), CollisionError> {
        for axis in [Axis::X, Axis::Y] {
            let offset = self.offset(axis);
            if !offset.is_finite() || offset.abs() > MAX_COLLIDER_OFFSET {
                return Err(CollisionError::Offset);
            }
            let ceiling =
                (f64::from(info.tile(axis)) * MAX_COLLIDER_TILES).min(MAX_COLLIDER_PIXELS);
            let size = self.size(axis);
            // Phrased so that a NaN extent falls through to the refusal rather
            // than passing an ordered comparison.
            if !(size >= MIN_COLLIDER_EXTENT && size <= ceiling) {
                return Err(CollisionError::Extent);
            }
        }
        Ok(())
    }
}

/// A body's world box. Constructed only by [`TileCollider::aabb`], so the
/// reconstruction expression has exactly one definition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    low_x: f64,
    low_y: f64,
    high_x: f64,
    high_y: f64,
}

impl Aabb {
    pub fn low(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.low_x,
            Axis::Y => self.low_y,
        }
    }

    pub fn high(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.high_x,
            Axis::Y => self.high_y,
        }
    }
}

/// Deterministic cell-visit accounting.
///
/// Work is charged before the refusal is returned, so a request that runs out
/// of budget still consumes what it performed. That is what stops a caller from
/// probing the map for free by repeating a request that always fails.
#[derive(Clone, Copy, Debug)]
pub struct WorkBudget {
    limit: u64,
    used: u64,
}

impl WorkBudget {
    pub fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn charge(&mut self, units: u64) -> Result<(), CollisionError> {
        self.used = self.used.saturating_add(units);
        if self.used > self.limit {
            return Err(CollisionError::Work);
        }
        Ok(())
    }
}

/// The half-open cell span `[low, high)` covers on one axis.
///
/// A box whose far edge lands exactly on a face does not overlap the cell
/// beyond it (N3), which is the whole reason the upper end is pulled back.
fn span(map: &TileMap, axis: Axis, low: f64, high: f64) -> (i32, i32) {
    let first = map.cell_at(axis, low);
    let mut last = map.cell_at(axis, high);
    if map.face(axis, last) >= high {
        last -= 1;
    }
    (first, last.max(first))
}

/// Whether any cell of one perpendicular span is solid.
///
/// `along` indexes `axis`; `first..=last` indexes the other. Every cell is
/// visited, so an interior solid tile cannot hide between clear corners.
fn solid_across(
    map: &TileMap,
    axis: Axis,
    along: i32,
    first: i32,
    last: i32,
    work: &mut WorkBudget,
) -> Result<bool, CollisionError> {
    debug_assert!(
        last - first < MAX_SPAN_CELLS,
        "a legal body spans at most {MAX_SPAN_CELLS} cells, not {}",
        last - first + 1
    );
    for across in first..=last {
        work.charge(1)?;
        let solid = match axis {
            Axis::X => map.is_solid(along, across),
            Axis::Y => map.is_solid(across, along),
        };
        if solid {
            return Ok(true);
        }
    }
    Ok(false)
}

/// T5/T6 placement predicate: refuse a box that leaves the map or overlaps a
/// solid tile with positive area.
///
/// The extent is compared against the map before any index conversion, so a box
/// far outside the grid is refused rather than converted and scanned.
pub fn check_placement(
    map: &TileMap,
    collider: &TileCollider,
    x: f64,
    y: f64,
    work: &mut WorkBudget,
) -> Result<(), CollisionError> {
    let body = collider.aabb(x, y);
    for axis in [Axis::X, Axis::Y] {
        // Phrased so a non-finite edge lands here rather than passing an ordered
        // comparison. This is the only guard between a degenerate box and the
        // grid: the function is public and enforces neither finiteness nor
        // [`TileCollider::check`], so it owes a refusal rather than an index
        // conversion. Saturation and the solid exterior between them already
        // refuse a merely out-of-range box; what they accept, and this does not,
        // is a NaN edge, whose comparisons are all false, and an inverted one,
        // whose high edge sits below its low.
        if !(body.low(axis) >= map.low(axis)
            && body.high(axis) <= map.high(axis)
            && body.low(axis) <= body.high(axis))
        {
            return Err(CollisionError::Placement);
        }
    }
    let (first, last) = span(map, Axis::X, body.low(Axis::X), body.high(Axis::X));
    let (row_first, row_last) = span(map, Axis::Y, body.low(Axis::Y), body.high(Axis::Y));
    for column in first..=last {
        if solid_across(map, Axis::X, column, row_first, row_last, work)? {
            return Err(CollisionError::Placement);
        }
    }
    Ok(())
}

/// Whether a body at a position overlaps one cell with positive area (N3).
///
/// Used by cell-edit validation, which asks about a single named cell rather
/// than scanning the body's whole footprint.
pub fn overlaps_cell(
    map: &TileMap,
    collider: &TileCollider,
    x: f64,
    y: f64,
    column: i32,
    row: i32,
) -> bool {
    let body = collider.aabb(x, y);
    for (axis, index) in [(Axis::X, column), (Axis::Y, row)] {
        // A coordinate outside the grid names no cell, so nothing overlaps it.
        // Answering here also keeps `index + 1` from overflowing at `i32::MAX`,
        // which is what a public entry point owes a caller it has not checked.
        // The solid exterior is `check_placement`'s rule, not this one.
        if index < 0 || index >= map.info().count(axis) as i32 {
            return false;
        }
        if !(body.low(axis) < map.face(axis, index + 1) && body.high(axis) > map.face(axis, index))
        {
            return false;
        }
    }
    true
}

/// T4: sweep X fully, then sweep Y from the resolved X.
///
/// Returns the resolved position. Velocity is the caller's and is never
/// changed, so holding a direction keeps pressing the wall.
pub fn solve(
    map: &TileMap,
    collider: &TileCollider,
    position: (f64, f64),
    travel: (f64, f64),
    work: &mut WorkBudget,
) -> Result<(f64, f64), CollisionError> {
    // N7: the axis order is a contract. A single solid diagonal neighbour
    // resolves differently under the other order, so this is observable and is
    // pinned by fixture rather than left to ties or iteration order.
    let x = sweep(
        map,
        collider,
        Axis::X,
        position.0,
        position.1,
        travel.0,
        work,
    )?;
    let y = sweep(map, collider, Axis::Y, x, position.1, travel.1, work)?;
    Ok((x, y))
}

/// One axis of the sweep: clamp at the first solid face or map boundary the
/// leading edge crosses, enumerating every perpendicular cell on the way (N4).
fn sweep(
    map: &TileMap,
    collider: &TileCollider,
    axis: Axis,
    x: f64,
    y: f64,
    travel: f64,
    work: &mut WorkBudget,
) -> Result<f64, CollisionError> {
    let start = match axis {
        Axis::X => x,
        Axis::Y => y,
    };
    // The kernel validates its candidates first, but this is reachable through
    // a public entry point and owes a bounded answer rather than a debug-only
    // assertion that becomes a silently committed NaN position in release.
    if !(x.is_finite() && y.is_finite() && travel.is_finite()) {
        return Err(CollisionError::Nonfinite);
    }
    if travel == 0.0 {
        // A zero axis displacement performs no sweep and visits no cells.
        return Ok(start);
    }

    let here = collider.aabb(x, y);
    let across = axis.other();
    let (first, last) = span(map, across, here.low(across), here.high(across));
    let offset = collider.offset(axis);
    let size = collider.size(axis);
    let count = map.info().count(axis) as i32;
    let target = start + travel;
    // The limit is the destination edge the committed position reconstructs,
    // not `leading edge + travel`: using the latter lets a rounding difference
    // skip the last face.
    let destination = match axis {
        Axis::X => collider.aabb(target, y),
        Axis::Y => collider.aabb(x, target),
    };

    if travel > 0.0 {
        let lead = here.high(axis);
        let limit = destination.high(axis);
        let mut index = map.cell_at(axis, lead);
        if map.face(axis, index) < lead {
            index += 1;
        }
        // Faces are enumerated, never pixels or endpoints, and reaching the
        // map's own boundary index clamps there because outside is solid. A
        // huge finite velocity therefore costs work proportional to the map.
        while map.face(axis, index) < limit {
            if index >= count {
                return clamp_below(map.high(axis), offset, size, start, target);
            }
            if solid_across(map, axis, index, first, last, work)? {
                return clamp_below(map.face(axis, index), offset, size, start, target);
            }
            index += 1;
        }
        Ok(target)
    } else {
        let lead = here.low(axis);
        let limit = destination.low(axis);
        let mut index = map.cell_at(axis, lead);
        // The mirror: each candidate face is entered from above, so the cell
        // tested is the one below it. A face already touching the leading edge
        // is included, which yields zero displacement rather than a push.
        while map.face(axis, index) > limit {
            if index <= 0 {
                return clamp_above(map.low(axis), offset, start, target);
            }
            if solid_across(map, axis, index - 1, first, last, work)? {
                return clamp_above(map.face(axis, index), offset, start, target);
            }
            index -= 1;
        }
        Ok(target)
    }
}

/// N5 for a face ahead of the leading edge: aim, bound, repair, or fault.
///
/// The candidate is verified by reconstructing the box edge exactly as the
/// committed position will, then repaired by the measured overshoot. Correcting
/// by the overshoot rather than by ulps of the position matters: when a large
/// offset cancels against a large face the needed correction is billions of
/// position ulps wide, while a correction in the box's own magnitude converges
/// at once. The `next_down` branch exists only because a correction smaller than
/// the position's own resolution cannot move it.
fn clamp_below(
    face: f64,
    offset: f64,
    size: f64,
    start: f64,
    target: f64,
) -> Result<f64, CollisionError> {
    let ideal = (face - size) - offset;
    let mut position = ideal.min(target).max(start);
    let mut steps = 0;
    while (position + offset) + size > face {
        if position <= start {
            return Err(CollisionError::Embedded);
        }
        if steps == REPAIR_STEPS {
            return Err(CollisionError::Unconverged);
        }
        let overshoot = ((position + offset) + size) - face;
        let stepped = position - overshoot;
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
    Ok(position)
}

/// N5 for a face behind the leading edge.
fn clamp_above(face: f64, offset: f64, start: f64, target: f64) -> Result<f64, CollisionError> {
    let ideal = face - offset;
    let mut position = ideal.max(target).min(start);
    let mut steps = 0;
    while position + offset < face {
        if position >= start {
            return Err(CollisionError::Embedded);
        }
        if steps == REPAIR_STEPS {
            return Err(CollisionError::Unconverged);
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
    Ok(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilemap::{MAX_CELLS, MAX_DIMENSION, MAX_TILE_SIZE, TileMapInfo};

    fn info(tile_width: u32, tile_height: u32) -> TileMapInfo {
        TileMapInfo {
            columns: 4,
            rows: 4,
            tile_width,
            tile_height,
            origin_x: 0,
            origin_y: 0,
        }
    }

    fn collider(width: f64, height: f64) -> TileCollider {
        TileCollider {
            offset_x: 0.0,
            offset_y: 0.0,
            width,
            height,
        }
    }

    #[test]
    fn the_extent_cap_is_tile_relative_on_each_axis_separately() {
        // Eight one-pixel tiles cap the extent at 8 pixels even though the
        // absolute pixel cap is 4096, which is the coupling that bounds the
        // perpendicular span.
        assert_eq!(collider(8.0, 8.0).check(&info(1, 1)), Ok(()));
        assert_eq!(
            collider(8.000000000000002, 8.0).check(&info(1, 1)),
            Err(CollisionError::Extent),
            "one ulp past eight one-pixel tiles must be refused"
        );
        // Non-square tiles cap the axes independently.
        assert_eq!(collider(8.0, 64.0).check(&info(1, 8)), Ok(()));
        assert_eq!(
            collider(64.0, 8.0).check(&info(1, 8)),
            Err(CollisionError::Extent),
            "the X cap must follow tile_width, not tile_height"
        );
        // The absolute cap wins once eight tiles would exceed it.
        assert_eq!(collider(4096.0, 4096.0).check(&info(1024, 1024)), Ok(()));
        assert_eq!(
            collider(4097.0, 4096.0).check(&info(1024, 1024)),
            Err(CollisionError::Extent),
            "eight 1024-pixel tiles must not lift the 4096-pixel cap"
        );
    }

    #[test]
    fn degenerate_and_unrepresentable_boxes_are_refused() {
        for size in [0.0, -1.0, f64::NAN, f64::INFINITY, 1.0 / 512.0] {
            assert_eq!(
                collider(size, 1.0).check(&info(32, 32)),
                Err(CollisionError::Extent),
                "extent {size:?} must be refused"
            );
        }
        assert_eq!(
            collider(MIN_COLLIDER_EXTENT, 1.0).check(&info(32, 32)),
            Ok(())
        );
        for offset in [
            f64::NAN,
            f64::INFINITY,
            4096.000000000001,
            -4096.000000000001,
        ] {
            let body = TileCollider {
                offset_x: offset,
                offset_y: 0.0,
                width: 1.0,
                height: 1.0,
            };
            assert_eq!(
                body.check(&info(32, 32)),
                Err(CollisionError::Offset),
                "offset {offset:?} must be refused"
            );
        }
    }

    #[test]
    fn the_fixed_pass_ceiling_cannot_be_reached_under_the_frozen_limits() {
        // The shape that maximises `columns + rows` inside the cell product.
        let mut widest = 0;
        let mut columns = 1;
        while columns <= MAX_DIMENSION {
            let rows = (MAX_CELLS / columns).min(MAX_DIMENSION);
            widest = widest.max(columns + rows);
            columns += 1;
        }
        assert_eq!(
            widest,
            MAX_DIMENSION + MAX_CELLS / MAX_DIMENSION,
            "1024 x 256"
        );

        // Each body enumerates at most nine cells on at most one face per cell
        // on both axes. The two map boundary faces are not among them: reaching
        // the boundary index clamps before `solid_across` is called, so a body
        // pressed against the far edge charges nothing at all.
        let per_body = u64::from(widest) * MAX_SPAN_CELLS as u64;
        let worst = per_body * u64::from(MAX_LIVE_COLLIDERS);
        assert_eq!(worst, 11_796_480);
        assert!(
            worst < MAX_FIXED_PASS_WORK,
            "the ceiling is headroom: {worst} of {MAX_FIXED_PASS_WORK}"
        );
        // Stated so a later limit change that closes the gap is noticed here.
        assert_eq!(MAX_TILE_SIZE, 1_024);
    }

    #[test]
    fn work_is_charged_before_the_refusal_is_returned() {
        let mut budget = WorkBudget::new(3);
        assert_eq!(budget.charge(2), Ok(()));
        assert_eq!(budget.charge(2), Err(CollisionError::Work));
        assert_eq!(
            budget.used(),
            4,
            "a refused request must still consume the work it performed"
        );
    }
}
