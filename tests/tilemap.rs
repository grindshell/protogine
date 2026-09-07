//! Phase 1: kernel map ownership.
//!
//! Expected coordinates, IDs and extents are stated here from the frozen Phase 0
//! contract rather than read back out of the implementation. No feature gate:
//! this suite is part of what proves the map is usable without a decoder, VM or
//! window.

use protogine::{
    kernel::{Kernel, KernelError, Position},
    tilemap::{
        Axis, GEOMETRY_LIMIT, MAX_CELLS, MAX_DIMENSION, MAX_REGION_CELLS, MAX_SOLID_IDS,
        MAX_TILE_SIZE, TileMap, TileMapError, TileMapInfo,
    },
};

/// A deliberately rectangular map with rectangular tiles, so a transposed index
/// or a swapped axis cannot pass.
const COLUMNS: u32 = 5;
const ROWS: u32 = 3;
const TILE_WIDTH: u32 = 16;
const TILE_HEIGHT: u32 = 24;

/// IDs 1..4 with only 2 and 4 solid, so "non-zero" is not the same as "solid"
/// and ID 0 is not the only empty tile.
const SOLIDS: [bool; 4] = [false, true, false, true];

/// Row-major, five wide and three tall:
///   . # . a .
///   . . B . .
///   b . . . #
/// where `#` is ID 2, `B` is ID 4, and `a`/`b` are the non-solid IDs 1 and 3.
const CELLS: [u16; 15] = [0, 2, 0, 1, 0, 0, 0, 4, 0, 0, 3, 0, 0, 0, 2];

fn info() -> TileMapInfo {
    TileMapInfo {
        columns: COLUMNS,
        rows: ROWS,
        tile_width: TILE_WIDTH,
        tile_height: TILE_HEIGHT,
        origin_x: 0,
        origin_y: 0,
    }
}

fn map() -> TileMap {
    TileMap::new(info(), SOLIDS.to_vec(), CELLS.to_vec()).expect("the fixture map should validate")
}

fn installed() -> Kernel {
    let mut kernel = Kernel::new();
    kernel.set_tilemap(map()).unwrap();
    kernel
}

/// A uniform map of the given shape, for bounds and storage checks.
fn uniform(description: TileMapInfo) -> TileMap {
    TileMap::new(description, vec![true], vec![0; description.cell_count()])
        .expect("a uniform map of a legal shape should validate")
}

#[test]
fn row_major_ids_and_solidity_survive_a_rectangular_grid() {
    let grid = map();
    assert_eq!(grid.info(), info());
    assert_eq!(grid.highest_id(), SOLIDS.len() as u16);

    // Read every cell against the fixture, so a transposed index fails here
    // rather than only at the two asymmetric spot checks below.
    for row in 0..ROWS as i32 {
        for column in 0..COLUMNS as i32 {
            let expected = CELLS[(row as u32 * COLUMNS + column as u32) as usize];
            assert_eq!(grid.tile(column, row).unwrap(), expected, "{column},{row}");
            let solid = expected != 0 && SOLIDS[expected as usize - 1];
            assert_eq!(grid.is_solid(column, row), solid, "{column},{row}");
        }
    }
    // The two cells that separate row-major from column-major on this fixture.
    let row_major = "cells are row-major";
    assert_eq!(grid.tile(1, 0).unwrap(), 2, "{row_major}");
    assert_eq!(grid.tile(0, 1).unwrap(), 0, "{row_major}");

    // A non-zero, non-solid ID is not the same as an empty one.
    assert_eq!(grid.tile(3, 0).unwrap(), 1);
    assert!(
        !grid.is_solid(3, 0),
        "a defined non-solid ID must not block"
    );
    assert!(!grid.solid_definition(1).unwrap());
    assert!(grid.solid_definition(2).unwrap());
    assert!(!grid.solid_definition(0).unwrap(), "ID 0 is never solid");
    assert_eq!(grid.solid_definition(5), Err(TileMapError::TileId));
}

#[test]
fn outside_the_map_is_solid_and_refuses_every_other_read() {
    let grid = map();
    for (column, row) in [
        (-1, 0),
        (0, -1),
        (COLUMNS as i32, 0),
        (0, ROWS as i32),
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX),
    ] {
        assert!(grid.is_solid(column, row), "{column},{row} must be solid");
        assert_eq!(grid.tile(column, row), Err(TileMapError::Bounds));
    }
    // The far corner inside the grid still reads, so the bound is exclusive and
    // not off by one.
    assert!(grid.tile(COLUMNS as i32 - 1, ROWS as i32 - 1).is_ok());
}

#[test]
fn rectangular_tiles_and_negative_origins_convert_both_ways() {
    let mut description = info();
    description.origin_x = -100;
    description.origin_y = -70;
    let grid = uniform(description);

    // Faces are the authored arithmetic, stated independently here.
    assert_eq!(grid.low(Axis::X), -100.0);
    assert_eq!(grid.low(Axis::Y), -70.0);
    assert_eq!(grid.face(Axis::X, 1), -84.0);
    assert_eq!(grid.face(Axis::Y, 1), -46.0);
    assert_eq!(grid.high(Axis::X), -100.0 + (COLUMNS * TILE_WIDTH) as f64);
    assert_eq!(grid.high(Axis::Y), -70.0 + (ROWS * TILE_HEIGHT) as f64);

    // Mathematical floor, not truncation toward zero: the cell left of a
    // negative origin is -1, and truncation would fold it onto 0.
    for (world, expected) in [(-100.0, 0), (-99.5, 0), (-85.0, 0), (-84.0, 1), (-83.9, 1)] {
        assert_eq!(grid.cell_at(Axis::X, world), expected, "x {world}");
    }
    let left_of_origin = "the cell left of a negative origin is -1, not 0";
    assert_eq!(grid.cell_at(Axis::X, -101.0), -1, "{left_of_origin}");
    assert_eq!(grid.cell_at(Axis::X, -116.0), -1, "{left_of_origin}");
    assert_eq!(grid.cell_at(Axis::Y, -71.0), -1, "{left_of_origin}");
    assert_eq!(grid.cell_at(Axis::Y, -70.0), 0);
    assert_eq!(grid.cell_at(Axis::Y, -46.0), 1);

    // The axes use different tile sizes, so mixing them up changes the answer.
    assert_ne!(
        grid.cell_at(Axis::X, -60.0),
        grid.cell_at(Axis::Y, -60.0),
        "a swapped axis must not agree"
    );
}

#[test]
fn cell_lookup_saturates_instead_of_scanning_toward_a_far_coordinate() {
    let grid = uniform(info());
    let columns = COLUMNS as i32;
    let rows = ROWS as i32;
    // One cell outside is exact; beyond that the answer saturates, which is what
    // bounds the correction rather than the caller's discipline.
    let saturates = "a far coordinate must saturate one cell out";
    assert_eq!(grid.cell_at(Axis::X, -1.0), -1, "{saturates}");
    assert_eq!(grid.cell_at(Axis::X, -1.0e300), -1, "{saturates}");
    assert_eq!(grid.cell_at(Axis::X, f64::NEG_INFINITY), -1, "{saturates}");
    assert_eq!(
        grid.cell_at(Axis::X, grid.high(Axis::X)),
        columns,
        "{saturates}"
    );
    assert_eq!(grid.cell_at(Axis::X, 1.0e300), columns, "{saturates}");
    assert_eq!(grid.cell_at(Axis::X, f64::INFINITY), columns, "{saturates}");
    assert_eq!(grid.cell_at(Axis::Y, f64::INFINITY), rows, "{saturates}");
    // A non-finite coordinate cannot reach a validated map, but the entry point
    // still owes a bounded answer rather than an unspecified cast.
    assert!((-1..=columns).contains(&grid.cell_at(Axis::X, f64::NAN)));
}

#[test]
fn faces_stay_exact_against_the_geometry_limit() {
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
    let grid = uniform(description);
    let limit = GEOMETRY_LIMIT as f64;
    assert_eq!(grid.high(Axis::X), limit);
    assert_eq!(grid.high(Axis::Y), limit);
    // The face just inside the far edge is exact too, which is what the sweep
    // will compare against in Phase 2.
    assert_eq!(
        grid.face(Axis::X, columns as i32 - 1),
        limit - f64::from(tile)
    );
    assert_eq!(grid.cell_at(Axis::X, limit), columns as i32);
    assert_eq!(grid.cell_at(Axis::X, limit - 1.0), columns as i32 - 1);
    assert_eq!(grid.cell_at(Axis::X, limit.next_down()), columns as i32 - 1);

    // One pixel further out on either axis leaves the domain.
    let mut past = description;
    past.origin_x += 1;
    assert_eq!(past.check(), Err(TileMapError::Geometry));
    let mut below = description;
    below.origin_y = (-GEOMETRY_LIMIT - 1) as i32;
    assert_eq!(below.check(), Err(TileMapError::Geometry));
    let mut edge = description;
    edge.origin_x = -(GEOMETRY_LIMIT as i32);
    edge.origin_y = -(GEOMETRY_LIMIT as i32);
    assert_eq!(edge.check(), Ok(()), "the negative edge is inclusive");
}

#[test]
fn malformed_dimensions_ids_and_arrays_are_refused_by_reason() {
    let valid = info();
    for (description, expected) in [
        (
            TileMapInfo {
                columns: 0,
                ..valid
            },
            TileMapError::Dimension,
        ),
        (
            TileMapInfo {
                rows: MAX_DIMENSION + 1,
                ..valid
            },
            TileMapError::Dimension,
        ),
        (
            TileMapInfo {
                tile_width: 0,
                ..valid
            },
            TileMapError::TileSize,
        ),
        (
            TileMapInfo {
                tile_height: MAX_TILE_SIZE + 1,
                ..valid
            },
            TileMapError::TileSize,
        ),
        (
            // Each dimension is legal on its own; only the product is not.
            TileMapInfo {
                columns: MAX_DIMENSION,
                rows: MAX_DIMENSION,
                ..valid
            },
            TileMapError::CellCount,
        ),
    ] {
        assert_eq!(description.check(), Err(expected), "{description:?}");
        assert_eq!(
            TileMap::new(description, vec![true], Vec::new()).err(),
            Some(expected),
            "construction must refuse for the same reason"
        );
    }

    // The largest legal shape is accepted, so the bounds above are exclusive of
    // the legal values rather than off by one.
    let largest = TileMapInfo {
        columns: MAX_DIMENSION,
        rows: MAX_CELLS / MAX_DIMENSION,
        tile_width: MAX_TILE_SIZE,
        tile_height: MAX_TILE_SIZE,
        ..valid
    };
    assert_eq!(largest.check(), Ok(()));

    // Solid definitions and cell arrays.
    assert_eq!(
        TileMap::new(valid, Vec::new(), CELLS.to_vec()).err(),
        Some(TileMapError::SolidCount)
    );
    assert_eq!(
        TileMap::new(valid, vec![false; MAX_SOLID_IDS + 1], CELLS.to_vec()).err(),
        Some(TileMapError::SolidCount)
    );
    assert!(TileMap::new(valid, vec![false; MAX_SOLID_IDS], CELLS.to_vec()).is_ok());
    for wrong in [CELLS.len() - 1, CELLS.len() + 1, 0] {
        assert_eq!(
            TileMap::new(valid, SOLIDS.to_vec(), vec![0; wrong]).err(),
            Some(TileMapError::CellCount),
            "{wrong} cells"
        );
    }
    // An ID one past the last definition, anywhere in the array.
    for position in [0, CELLS.len() / 2, CELLS.len() - 1] {
        let mut cells = CELLS;
        cells[position] = SOLIDS.len() as u16 + 1;
        assert_eq!(
            TileMap::new(valid, SOLIDS.to_vec(), cells.to_vec()).err(),
            Some(TileMapError::TileId),
            "bad ID at {position}"
        );
    }
    // The highest defined ID is accepted, and so is u16::MAX once defined.
    let mut cells = CELLS;
    cells[0] = SOLIDS.len() as u16;
    assert!(TileMap::new(valid, SOLIDS.to_vec(), cells.to_vec()).is_ok());
    cells[0] = u16::MAX;
    assert_eq!(
        TileMap::new(valid, SOLIDS.to_vec(), cells.to_vec()).err(),
        Some(TileMapError::TileId),
        "u16 is the storage width, not the ID space"
    );
}

#[test]
fn regions_copy_a_bounded_rectangle_and_refuse_everything_else() {
    let grid = map();
    // The whole map, in row-major order.
    assert_eq!(grid.region(0, 0, COLUMNS, ROWS).unwrap(), CELLS);
    // A single cell, and an interior rectangle stated independently.
    assert_eq!(grid.region(1, 0, 1, 1).unwrap(), vec![2]);
    // Rows 1 and 2, columns 0 to 2: spans both of the fixture's lower non-zero
    // cells, which sit in different rows and different columns, so an off-by-one
    // row or a transposed read changes the answer.
    assert_eq!(grid.region(0, 1, 3, 2).unwrap(), vec![0, 0, 4, 3, 0, 0]);
    // Exact far edges.
    assert_eq!(
        grid.region(COLUMNS as i32 - 1, ROWS as i32 - 1, 1, 1)
            .unwrap(),
        vec![CELLS[CELLS.len() - 1]]
    );

    for (column, row, columns, rows) in [
        (0, 0, 0, 1),                    // empty
        (0, 0, 1, 0),                    // empty
        (0, 0, COLUMNS + 1, ROWS),       // one column past the edge
        (0, 0, COLUMNS, ROWS + 1),       // one row past the edge
        (1, 0, COLUMNS, ROWS),           // shifted off the edge
        (-1, 0, 1, 1),                   // negative origin
        (0, -1, 1, 1),                   // negative origin
        (0, 0, MAX_REGION_CELLS + 1, 1), // over the per-call cap
    ] {
        assert_eq!(
            grid.region(column, row, columns, rows).err(),
            Some(TileMapError::Region),
            "{column},{row} {columns}x{rows}"
        );
    }

    // A region at the cap is accepted on a map large enough to hold it, so the
    // cap is a real boundary rather than an unreachable one. No single row can
    // reach 4,096 cells, since a map is at most 1,024 columns wide.
    let rows_at_cap = MAX_REGION_CELLS / MAX_DIMENSION;
    let wide = uniform(TileMapInfo {
        columns: MAX_DIMENSION,
        rows: rows_at_cap * 2,
        ..info()
    });
    assert_eq!(
        wide.region(0, 0, MAX_DIMENSION, rows_at_cap).unwrap().len(),
        MAX_REGION_CELLS as usize
    );
    // One row more is still wholly inside this map, so only the cap refuses it.
    assert_eq!(
        wide.region(0, 0, MAX_DIMENSION, rows_at_cap + 1).err(),
        Some(TileMapError::Region)
    );
}

#[test]
fn returned_reads_are_owned_copies_of_kernel_state() {
    let mut kernel = installed();

    // Mutating a returned region cannot reach the map.
    let mut region = kernel.tiles_region(0, 0, COLUMNS, ROWS).unwrap();
    region[0] = 2;
    region.clear();
    assert_eq!(kernel.tiles_region(0, 0, COLUMNS, ROWS).unwrap(), CELLS);
    assert_eq!(kernel.tile(0, 0).unwrap(), 0);

    // A retained info snapshot does not track a later replacement.
    let before = kernel.tilemap().unwrap().unwrap();
    let mut replacement = info();
    replacement.origin_x = 64;
    kernel.set_tilemap(uniform(replacement)).unwrap();
    assert_eq!(before.origin_x, 0, "the snapshot must not follow the map");
    assert_eq!(kernel.tilemap().unwrap().unwrap().origin_x, 64);
}

#[test]
fn single_cell_edits_apply_immediately_and_refuse_bad_input() {
    let mut kernel = installed();
    assert!(!kernel.tile_solid(0, 0).unwrap());
    kernel.set_tile(0, 0, 2).unwrap();
    assert_eq!(kernel.tile(0, 0).unwrap(), 2);
    assert!(kernel.tile_solid(0, 0).unwrap());
    // The edit is visible to a bulk read in the same session, not only to the
    // single-cell one.
    assert_eq!(kernel.tiles_region(0, 0, 1, 1).unwrap(), vec![2]);

    // Clearing back to empty, and to a defined non-solid ID.
    kernel.set_tile(0, 0, 0).unwrap();
    assert!(!kernel.tile_solid(0, 0).unwrap());
    kernel.set_tile(0, 0, 1).unwrap();
    assert_eq!(kernel.tile(0, 0).unwrap(), 1);
    assert!(!kernel.tile_solid(0, 0).unwrap());

    // Refusals leave the cell alone.
    for (column, row, id, expected) in [
        (-1, 0, 0, TileMapError::Bounds),
        (COLUMNS as i32, 0, 0, TileMapError::Bounds),
        (0, ROWS as i32, 0, TileMapError::Bounds),
        (0, 0, SOLIDS.len() as u16 + 1, TileMapError::TileId),
        (0, 0, u16::MAX, TileMapError::TileId),
    ] {
        assert_eq!(
            kernel.set_tile(column, row, id),
            Err(KernelError::TileMap(expected)),
            "{column},{row} id {id}"
        );
    }
    assert_eq!(
        kernel.tile(0, 0).unwrap(),
        1,
        "a refused edit changed a cell"
    );
}

#[test]
fn a_refused_replacement_leaves_the_installed_map_whole() {
    let mut kernel = installed();
    kernel.set_tile(2, 1, 2).unwrap();
    let before = kernel.tiles_region(0, 0, COLUMNS, ROWS).unwrap();
    let info_before = kernel.tilemap().unwrap().unwrap();
    let bytes_before = kernel.tilemap_storage_bytes();

    // Every way a candidate can fail to validate. None of them can even produce
    // a TileMap, so none of them reaches the kernel.
    let mut oversized = info();
    oversized.columns = MAX_DIMENSION;
    oversized.rows = MAX_DIMENSION;
    for (description, solids, cells) in [
        (info(), Vec::new(), CELLS.to_vec()),
        (info(), SOLIDS.to_vec(), vec![0; CELLS.len() + 1]),
        (info(), SOLIDS.to_vec(), vec![9; CELLS.len()]),
        (oversized, SOLIDS.to_vec(), Vec::new()),
    ] {
        assert!(TileMap::new(description, solids, cells).is_err());
        // The map is unchanged and still completely readable after each refusal.
        assert_eq!(kernel.tiles_region(0, 0, COLUMNS, ROWS).unwrap(), before);
        assert_eq!(kernel.tilemap().unwrap().unwrap(), info_before);
        assert_eq!(kernel.tilemap_storage_bytes(), bytes_before);
    }

    // A valid replacement does take effect, so the checks above are not passing
    // because replacement is broken.
    let mut replacement = info();
    replacement.columns = COLUMNS + 1;
    kernel.set_tilemap(uniform(replacement)).unwrap();
    assert_eq!(kernel.tilemap().unwrap().unwrap().columns, COLUMNS + 1);
    assert_eq!(kernel.tile(COLUMNS as i32, 0).unwrap(), 0);
}

#[test]
fn map_operations_require_an_installed_map() {
    let mut kernel = Kernel::new();
    assert_eq!(kernel.tilemap().unwrap(), None);
    assert_eq!(kernel.tilemap_storage_bytes(), 0);
    assert_eq!(kernel.tile(0, 0), Err(KernelError::NoTileMap));
    assert_eq!(kernel.tile_solid(0, 0), Err(KernelError::NoTileMap));
    assert_eq!(kernel.tiles_region(0, 0, 1, 1), Err(KernelError::NoTileMap));
    assert_eq!(kernel.set_tile(0, 0, 0), Err(KernelError::NoTileMap));
    assert_eq!(kernel.tile_face(Axis::X, 0), Err(KernelError::NoTileMap));
    assert_eq!(kernel.tile_at(Axis::X, 0.0), Err(KernelError::NoTileMap));
    // Clearing an absent map succeeds, and entities are unaffected either way.
    kernel.clear_tilemap().unwrap();
    let entity = kernel.spawn(Position { x: 1.0, y: 2.0 }).unwrap();
    kernel.set_tilemap(map()).unwrap();
    assert_eq!(
        kernel.position(&entity).unwrap(),
        Position { x: 1.0, y: 2.0 }
    );
    kernel.clear_tilemap().unwrap();
    assert_eq!(kernel.tilemap().unwrap(), None);
    assert_eq!(kernel.tile(0, 0), Err(KernelError::NoTileMap));
    assert!(kernel.position(&entity).is_ok(), "clearing kept the entity");
}

#[test]
fn stopping_releases_map_storage_and_refuses_every_map_call() {
    let largest = TileMapInfo {
        columns: MAX_DIMENSION,
        rows: MAX_CELLS / MAX_DIMENSION,
        ..info()
    };
    let mut kernel = Kernel::new();
    kernel.set_tilemap(uniform(largest)).unwrap();
    // Half a megabyte of cells plus one solid flag.
    assert_eq!(
        kernel.tilemap_storage_bytes(),
        MAX_CELLS as usize * 2 + 1,
        "the largest map should account for its cells"
    );

    kernel.stop();
    assert_eq!(
        kernel.tilemap_storage_bytes(),
        0,
        "stop must release map storage, not merely deny access to it"
    );
    assert_eq!(kernel.tilemap(), Err(KernelError::Inactive));
    assert_eq!(kernel.tile(0, 0), Err(KernelError::Inactive));
    assert_eq!(kernel.tile_solid(0, 0), Err(KernelError::Inactive));
    assert_eq!(kernel.tiles_region(0, 0, 1, 1), Err(KernelError::Inactive));
    assert_eq!(kernel.set_tile(0, 0, 0), Err(KernelError::Inactive));
    assert_eq!(kernel.set_tilemap(map()), Err(KernelError::Inactive));
    assert_eq!(kernel.clear_tilemap(), Err(KernelError::Inactive));
    assert_eq!(kernel.tile_face(Axis::X, 0), Err(KernelError::Inactive));
    assert_eq!(kernel.tile_at(Axis::X, 0.0), Err(KernelError::Inactive));
}

#[test]
fn a_map_changes_nothing_about_entities_without_colliders() {
    // Phase 1 installs no colliders, so integration must be exactly what it was.
    let mut bare = Kernel::new();
    let mut mapped = Kernel::new();
    mapped.set_tilemap(map()).unwrap();
    for kernel in [&mut bare, &mut mapped] {
        let entity = kernel.spawn(Position { x: 8.0, y: 8.0 }).unwrap();
        kernel
            .set_velocity(&entity, protogine::kernel::Velocity { x: 60.0, y: -60.0 })
            .unwrap();
    }
    for _ in 0..10 {
        bare.fixed_update().unwrap();
        mapped.fixed_update().unwrap();
    }
    assert_eq!(
        bare.snapshot().unwrap()[0].position,
        mapped.snapshot().unwrap()[0].position
    );
    // Including straight through a solid tile and out of the map entirely: only
    // an entity given a collider in Phase 2 is constrained.
    let position = mapped.snapshot().unwrap()[0].position;
    assert_eq!(position, Position { x: 18.0, y: -2.0 });
    assert!(mapped.tile_solid(1, 0).unwrap(), "it passed through a wall");
}

#[test]
fn kernel_errors_describe_their_map_cause() {
    let kernel = Kernel::new();
    assert_eq!(
        kernel.tile(0, 0).unwrap_err().to_string(),
        "no tile map is installed"
    );
    assert_eq!(
        KernelError::TileMap(TileMapError::Bounds).to_string(),
        TileMapError::Bounds.to_string(),
        "a map cause should not be wrapped in a second sentence"
    );
    assert!(!TileMapError::Region.to_string().is_empty());
}
