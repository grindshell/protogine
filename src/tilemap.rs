//! Checked tile-grid types and dense row-major storage.
//!
//! Deliberately free of ECS, scripting, asset and graphics dependencies: this
//! module owns the map half of the geometry contract frozen in
//! [the Phase 0 record](../docs/implementation/TILEMAP_COLLISION_PHASE0.md) and
//! nothing else. Colliders, sweeps and the fixed-system integration are Phase 2's
//! `src/collision.rs`, so nothing here knows what a body is.

use std::fmt;

/// Cells per axis. The product bound is checked before any allocation.
pub const MAX_DIMENSION: u32 = 1_024;
pub const MAX_CELLS: u32 = 262_144;
/// World pixels per tile on each axis.
pub const MAX_TILE_SIZE: u32 = 1_024;
/// Solid definitions, so a map defines at most 1,025 IDs counting empty. u16 is
/// the cell storage width, never the usable ID space.
pub const MAX_SOLID_IDS: usize = 1_024;
/// Every map edge stays within this many world pixels, which is what keeps each
/// face an exactly representable f64 (Phase 0 rule N1).
pub const GEOMETRY_LIMIT: i64 = 16_777_216;
/// Cells one region read may return.
pub const MAX_REGION_CELLS: u32 = 4_096;

/// Corrections [`TileMap::cell_at`] performs before its debug assertion. The
/// frozen limits make one enough; the bound is structural so that an unclipped
/// caller cannot turn the correction into a scan toward its coordinate.
const CELL_CORRECTIONS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    pub fn other(self) -> Self {
        match self {
            Self::X => Self::Y,
            Self::Y => Self::X,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileMapError {
    /// Column or row count outside `1..=MAX_DIMENSION`.
    Dimension,
    /// Tile width or height outside `1..=MAX_TILE_SIZE`.
    TileSize,
    /// More cells than the product bound, or a cell array of the wrong length.
    CellCount,
    /// A map edge outside the geometry domain.
    Geometry,
    /// No solid definitions, or more than `MAX_SOLID_IDS`.
    SolidCount,
    /// A cell ID above the last solid definition.
    TileId,
    /// Tile coordinates outside the installed grid.
    Bounds,
    /// A region that is empty, oversized, or not wholly inside the grid.
    Region,
    /// A buffer the map could not reserve.
    Capacity,
}

impl fmt::Display for TileMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dimension => "map dimensions must be 1..=1024 cells",
            Self::TileSize => "tile dimensions must be 1..=1024 world pixels",
            Self::CellCount => "cell count must match the dimensions and stay within 262144",
            Self::Geometry => "map edges must stay within 16777216 world pixels",
            Self::SolidCount => "a map defines 1..=1024 solid flags",
            Self::TileId => "tile IDs must not exceed the solid definitions",
            Self::Bounds => "tile coordinates are outside the map",
            Self::Region => "region must be positive, bounded, and inside the map",
            Self::Capacity => "could not reserve map storage",
        })
    }
}
impl std::error::Error for TileMapError {}

/// A map's immutable dimensions, tile size and world origin.
///
/// Owned and `Copy`, so a caller can hold one without aliasing kernel storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileMapInfo {
    pub columns: u32,
    pub rows: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

impl TileMapInfo {
    pub fn count(&self, axis: Axis) -> u32 {
        match axis {
            Axis::X => self.columns,
            Axis::Y => self.rows,
        }
    }

    pub fn tile(&self, axis: Axis) -> u32 {
        match axis {
            Axis::X => self.tile_width,
            Axis::Y => self.tile_height,
        }
    }

    pub fn origin(&self, axis: Axis) -> i32 {
        match axis {
            Axis::X => self.origin_x,
            Axis::Y => self.origin_y,
        }
    }

    /// Cells a map with these dimensions holds. Only meaningful once [`Self::check`]
    /// has passed, which is what bounds the product.
    pub fn cell_count(&self) -> usize {
        self.columns as usize * self.rows as usize
    }

    /// Validate dimensions, tile size and the geometry domain.
    ///
    /// Callers building the cell array run this first, so an oversized map is
    /// refused before anything allocates for it.
    pub fn check(&self) -> Result<(), TileMapError> {
        for count in [self.columns, self.rows] {
            if !(1..=MAX_DIMENSION).contains(&count) {
                return Err(TileMapError::Dimension);
            }
        }
        for tile in [self.tile_width, self.tile_height] {
            if !(1..=MAX_TILE_SIZE).contains(&tile) {
                return Err(TileMapError::TileSize);
            }
        }
        if u64::from(self.columns) * u64::from(self.rows) > u64::from(MAX_CELLS) {
            return Err(TileMapError::CellCount);
        }
        for axis in [Axis::X, Axis::Y] {
            let origin = i64::from(self.origin(axis));
            let far = origin + i64::from(self.count(axis)) * i64::from(self.tile(axis));
            // The extent is positive, so bounding the near and far edges bounds
            // every face between them.
            if origin < -GEOMETRY_LIMIT || far > GEOMETRY_LIMIT {
                return Err(TileMapError::Geometry);
            }
        }
        Ok(())
    }
}

/// One finite orthogonal map: immutable dimensions, origin and solid
/// definitions, with mutable dense row-major cell IDs.
pub struct TileMap {
    info: TileMapInfo,
    /// Row-major, exactly `info.cell_count()` entries.
    cells: Vec<u16>,
    /// `solids[i]` defines tile ID `i + 1`; ID 0 is implicitly non-solid.
    solids: Vec<bool>,
}

impl TileMap {
    /// Validate a complete description and take ownership of it.
    ///
    /// Everything is checked before the value exists, so there is no partially
    /// built map to observe and a refusal leaves the caller's previous map
    /// untouched by construction.
    pub fn new(
        info: TileMapInfo,
        solids: Vec<bool>,
        cells: Vec<u16>,
    ) -> Result<Self, TileMapError> {
        info.check()?;
        if solids.is_empty() || solids.len() > MAX_SOLID_IDS {
            return Err(TileMapError::SolidCount);
        }
        if cells.len() != info.cell_count() {
            return Err(TileMapError::CellCount);
        }
        // solids.len() is bounded by 1024, so the cast is exact.
        let highest = solids.len() as u16;
        if cells.iter().any(|id| *id > highest) {
            return Err(TileMapError::TileId);
        }
        Ok(Self {
            info,
            cells,
            solids,
        })
    }

    pub fn info(&self) -> TileMapInfo {
        self.info
    }

    /// The highest tile ID this map defines. ID 0 is always available and empty.
    pub fn highest_id(&self) -> u16 {
        self.solids.len() as u16
    }

    /// Live storage, for the memory accounting the plan requires of stress runs.
    /// Capacity rather than length, so a reserve that overshot is still counted.
    pub fn storage_bytes(&self) -> usize {
        self.cells.capacity() * size_of::<u16>() + self.solids.capacity()
    }

    fn contains(&self, column: i32, row: i32) -> bool {
        column >= 0
            && row >= 0
            && (column as u32) < self.info.columns
            && (row as u32) < self.info.rows
    }

    fn offset(&self, column: i32, row: i32) -> usize {
        debug_assert!(self.contains(column, row));
        row as usize * self.info.columns as usize + column as usize
    }

    /// The ID at in-bounds tile coordinates.
    pub fn tile(&self, column: i32, row: i32) -> Result<u16, TileMapError> {
        if !self.contains(column, row) {
            return Err(TileMapError::Bounds);
        }
        Ok(self.cells[self.offset(column, row)])
    }

    /// Whether a tile blocks. Outside the map is solid, so this accepts any
    /// coordinates rather than refusing them.
    pub fn is_solid(&self, column: i32, row: i32) -> bool {
        if !self.contains(column, row) {
            return true;
        }
        self.id_is_solid(self.cells[self.offset(column, row)])
    }

    fn id_is_solid(&self, id: u16) -> bool {
        id != 0 && self.solids[id as usize - 1]
    }

    /// Whether an ID this map defines is solid, without naming a cell.
    pub fn solid_definition(&self, id: u16) -> Result<bool, TileMapError> {
        if id > self.highest_id() {
            return Err(TileMapError::TileId);
        }
        Ok(self.id_is_solid(id))
    }

    /// Copy a wholly in-bounds rectangle of IDs, row-major.
    ///
    /// The returned vector is owned: mutating it cannot reach the map.
    pub fn region(
        &self,
        column: i32,
        row: i32,
        columns: u32,
        rows: u32,
    ) -> Result<Vec<u16>, TileMapError> {
        if columns == 0 || rows == 0 {
            return Err(TileMapError::Region);
        }
        let requested = u64::from(columns) * u64::from(rows);
        if requested > u64::from(MAX_REGION_CELLS) {
            return Err(TileMapError::Region);
        }
        if column < 0 || row < 0 {
            return Err(TileMapError::Region);
        }
        let far_column = i64::from(column) + i64::from(columns);
        let far_row = i64::from(row) + i64::from(rows);
        if far_column > i64::from(self.info.columns) || far_row > i64::from(self.info.rows) {
            return Err(TileMapError::Region);
        }
        // Charged and reserved before copying, so a refusal costs no partial output.
        let mut out = Vec::new();
        out.try_reserve_exact(requested as usize)
            .map_err(|_| TileMapError::Capacity)?;
        for line in 0..rows as i32 {
            let start = self.offset(column, row + line);
            out.extend_from_slice(&self.cells[start..start + columns as usize]);
        }
        Ok(out)
    }

    /// Change one cell. Refuses out-of-bounds coordinates and undefined IDs.
    pub fn set_tile(&mut self, column: i32, row: i32, id: u16) -> Result<(), TileMapError> {
        if !self.contains(column, row) {
            return Err(TileMapError::Bounds);
        }
        if id > self.highest_id() {
            return Err(TileMapError::TileId);
        }
        let offset = self.offset(column, row);
        self.cells[offset] = id;
        Ok(())
    }

    /// The world coordinate of the face at `index` on `axis`.
    ///
    /// Origins and tile sizes are integers inside the geometry domain, so this
    /// is exact for every index a legal map and its one-cell saturation produce.
    pub fn face(&self, axis: Axis, index: i32) -> f64 {
        let origin = i64::from(self.info.origin(axis));
        let tile = i64::from(self.info.tile(axis));
        (origin + i64::from(index) * tile) as f64
    }

    /// The map's near edge on `axis`.
    pub fn low(&self, axis: Axis) -> f64 {
        self.face(axis, 0)
    }

    /// The map's far edge on `axis`.
    pub fn high(&self, axis: Axis) -> f64 {
        // The count fits i32 because MAX_DIMENSION does.
        self.face(axis, self.info.count(axis) as i32)
    }

    /// The cell containing `world` on `axis`, saturated to one cell outside the
    /// grid.
    ///
    /// The quotient rounds, so the raw floor is only a guess and exact integer
    /// faces decide (Phase 0 rule N2). Saturation is what bounds the work: a
    /// caller that has not clipped into the map receives `-1` or the cell count
    /// rather than a scan toward its coordinate, which the plan forbids.
    pub fn cell_at(&self, axis: Axis, world: f64) -> i32 {
        let count = self.info.count(axis) as i32;
        let origin = f64::from(self.info.origin(axis));
        let tile = f64::from(self.info.tile(axis));
        let guess = ((world - origin) / tile).floor();
        // A validated map cannot produce a non-finite coordinate, but a public
        // entry point still owes a bounded answer rather than a saturating cast
        // whose input nobody checked.
        let mut index = if guess.is_nan() {
            0
        } else {
            guess.clamp(-1.0, f64::from(count)) as i32
        };
        for _ in 0..CELL_CORRECTIONS {
            if index > -1 && self.face(axis, index) > world {
                index -= 1;
            } else if index < count && self.face(axis, index + 1) <= world {
                index += 1;
            } else {
                break;
            }
        }
        debug_assert!(
            (index <= -1 || self.face(axis, index) <= world)
                && (index >= count || self.face(axis, index + 1) > world)
                || !world.is_finite(),
            "cell_at did not converge for {world:?} on {axis:?}"
        );
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(columns: u32, rows: u32) -> TileMapInfo {
        TileMapInfo {
            columns,
            rows,
            tile_width: 32,
            tile_height: 32,
            origin_x: 0,
            origin_y: 0,
        }
    }

    fn map(columns: u32, rows: u32) -> TileMap {
        TileMap::new(
            info(columns, rows),
            vec![true],
            vec![0; (columns * rows) as usize],
        )
        .unwrap()
    }

    #[test]
    fn cell_index_saturates_one_cell_outside_the_grid() {
        // A negative origin so the first cell inside it has a negative
        // coordinate, which is where truncation toward zero would fold.
        let mut description = info(4, 4);
        description.origin_x = -100;
        description.origin_y = -70;
        let grid = TileMap::new(description, vec![true], vec![0; 16]).unwrap();

        for (world, expected) in [(-100.0, 0), (-99.0, 0), (-69.0, 0), (-68.0, 1), (28.0, 4)] {
            assert_eq!(grid.cell_at(Axis::X, world), expected, "world {world}");
        }
        // One cell out is exact; anything further saturates rather than scanning.
        assert_eq!(grid.cell_at(Axis::X, -101.0), -1);
        assert_eq!(grid.cell_at(Axis::X, -132.0), -1);
        assert_eq!(grid.cell_at(Axis::X, -133.0), -1, "saturated low");
        assert_eq!(grid.cell_at(Axis::X, -1.0e9), -1, "saturated low");
        assert_eq!(grid.cell_at(Axis::X, 1.0e9), 4, "saturated high");
        assert_eq!(grid.cell_at(Axis::Y, -70.0), 0);
        assert_eq!(grid.cell_at(Axis::Y, -71.0), -1);
        assert_eq!(grid.cell_at(Axis::Y, f64::NEG_INFINITY), -1);
        assert_eq!(grid.cell_at(Axis::X, f64::NAN), 0, "NaN stays bounded");
    }

    #[test]
    fn faces_are_exact_at_the_geometry_limit() {
        let columns = MAX_DIMENSION;
        let rows = MAX_CELLS / MAX_DIMENSION;
        let tile = MAX_TILE_SIZE;
        let description = TileMapInfo {
            columns,
            rows,
            tile_width: tile,
            tile_height: tile,
            origin_x: (GEOMETRY_LIMIT - i64::from(columns) * i64::from(tile)) as i32,
            origin_y: (GEOMETRY_LIMIT - i64::from(rows) * i64::from(tile)) as i32,
        };
        let grid =
            TileMap::new(description, vec![true], vec![0; description.cell_count()]).unwrap();
        let limit = GEOMETRY_LIMIT as f64;
        assert_eq!(grid.high(Axis::X), limit);
        assert_eq!(grid.high(Axis::Y), limit);
        assert_eq!(grid.cell_at(Axis::X, limit), columns as i32);
        assert_eq!(grid.cell_at(Axis::X, limit - 1.0), columns as i32 - 1);
        assert_eq!(grid.cell_at(Axis::X, grid.low(Axis::X)), 0);
    }

    #[test]
    fn storage_is_released_with_the_map_and_counted_while_it_lives() {
        let grid = map(MAX_DIMENSION, MAX_CELLS / MAX_DIMENSION);
        assert_eq!(grid.storage_bytes(), MAX_CELLS as usize * 2 + 1);
        assert_eq!(map(1, 1).storage_bytes(), 3);
    }
}
