//! The map registry, exercised from outside the crate.
//!
//! Phase 1's identity fixtures are unit tests in `src/kernel.rs` because they
//! read `TileMapHandle::slot`, which is `#[cfg(test)] pub(crate)`: M2-1 makes
//! slot *reuse* a contract clause a fixture has to prove it forced, and does not
//! make the slot *number* something a game observes. That is the right home for
//! them, and it leaves this file the job they cannot do.
//!
//! **Without it, every entry point below could be `pub(crate)` and the whole
//! suite would still pass.** M2-6 makes this the game-facing surface and Phase 3
//! binds it to Luau, so "reachable from outside at all" is a property worth one
//! file rather than an inference from the unit tests. The gap was the review
//! session's finding, on a phase whose clean `tests/` diff had made it invisible.

use protogine::kernel::{Kernel, KernelError, TileMapHandle};
use protogine::maps::{MAX_AGGREGATE_CELLS, MAX_TILEMAPS};
use protogine::tilemap::{MAX_CELLS, TileMap, TileMapInfo};

fn grid(columns: u32, rows: u32) -> TileMap {
    TileMap::new(
        TileMapInfo {
            columns,
            rows,
            tile_width: 32,
            tile_height: 32,
            origin_x: 0,
            origin_y: 0,
        },
        vec![true],
        vec![0; (columns * rows) as usize],
    )
    .expect("test map is within the frozen limits")
}

#[test]
fn a_map_is_created_read_replaced_and_removed_through_its_handle() {
    let mut kernel = Kernel::new();
    // Named rather than inferred, and imported rather than reached through the
    // return type. A `pub fn` returning a non-public type is a warn-level lint
    // and not an error, so with every binding inferred the *type* could still
    // have been `pub(crate)` while this file compiled - which is the half of
    // the header's claim the seven functions do not cover.
    let handle: TileMapHandle = kernel.create_tilemap(grid(4, 4)).unwrap();
    assert_eq!(kernel.tilemap_info(&handle).unwrap().columns, 4);

    // A replacement keeps the handle valid, which is the whole of what
    // separates it from remove-then-create (M2-4).
    kernel.replace_tilemap(&handle, grid(8, 8)).unwrap();
    // Compared as a `Result` rather than unwrapped: a handle invalidated by the
    // replacement would panic at the `unwrap` and never reach this message, so
    // the named assertion the control has to hit would be unreachable.
    assert_eq!(
        kernel.tilemap_info(&handle).map(|info| info.columns),
        Ok(8),
        "a replaced map must still answer to the handle that named it"
    );

    kernel.remove_tilemap(&handle).unwrap();
    for refusal in [
        kernel.tilemap_info(&handle).err(),
        kernel.replace_tilemap(&handle, grid(4, 4)).err(),
        kernel.remove_tilemap(&handle).err(),
    ] {
        assert_eq!(
            refusal,
            Some(KernelError::InvalidTileMap),
            "every call taking a handle must refuse a removed one"
        );
    }
    assert_eq!(kernel.tilemap_count(), 0);
}

#[test]
fn each_accounting_accessor_reports_the_quantity_it_names() {
    // One large map, chosen so all four accessors differ. Three of them
    // delegate to adjacent methods on the same table, so a copy-paste between
    // them is the realistic mistake and only distinct values catch it - each
    // assertion below fails if its accessor returns any of the other three.
    let mut kernel = Kernel::new();
    let handle = kernel.create_tilemap(grid(512, 512)).unwrap();
    assert_eq!(kernel.tilemap_count(), 1, "live maps, not cells or bytes");
    assert_eq!(
        kernel.tilemap_cells(),
        MAX_CELLS,
        "cells across live maps, not the map count"
    );
    assert_eq!(
        kernel.tilemap_storage_bytes(),
        MAX_CELLS as usize * 2 + 1,
        "cells at two bytes each plus one solid flag, exactly as M1 reported it"
    );
    // A range rather than a value: the slot layout is a property of the target,
    // not of the contract. What matters is that it is neither of the two figures
    // above nor the count, and that it stays negligible beside them.
    assert!(
        (2..1_000).contains(&kernel.tilemap_table_bytes()),
        "the table's overhead is its slots, not its maps: {}",
        kernel.tilemap_table_bytes()
    );

    // Removal returns all four to zero rather than leaving the accounting to
    // drift from the table.
    kernel.remove_tilemap(&handle).unwrap();
    assert_eq!(kernel.tilemap_count(), 0);
    assert_eq!(kernel.tilemap_cells(), 0);
    assert_eq!(kernel.tilemap_storage_bytes(), 0);

    // And `stop` releases the table itself, not only the maps in it.
    kernel.create_tilemap(grid(4, 4)).unwrap();
    kernel.stop();
    assert_eq!(kernel.tilemap_table_bytes(), 0);
    assert_eq!(kernel.tilemap_storage_bytes(), 0);
}

#[test]
fn both_aggregate_budgets_refuse_by_name_from_outside() {
    let mut kernel = Kernel::new();
    for _ in 0..MAX_TILEMAPS {
        kernel.create_tilemap(grid(1, 1)).unwrap();
    }
    assert_eq!(
        kernel.create_tilemap(grid(1, 1)),
        Err(KernelError::TileMapLimit),
        "the sixty-fifth map must refuse"
    );
    assert_eq!(kernel.tilemap_count(), MAX_TILEMAPS);

    // The cell budget, reached with room to spare on the count: two maps of the
    // single-map maximum fill the aggregate exactly, since it is twice M1's.
    let mut kernel = Kernel::new();
    kernel.create_tilemap(grid(512, 512)).unwrap();
    kernel.create_tilemap(grid(512, 512)).unwrap();
    assert_eq!(kernel.tilemap_cells(), MAX_AGGREGATE_CELLS);
    assert_eq!(
        kernel.create_tilemap(grid(1, 1)),
        Err(KernelError::AggregateCellLimit),
        "one cell past a full aggregate must refuse"
    );
    assert_eq!(kernel.tilemap_count(), 2, "a refusal installed nothing");
    assert_eq!(kernel.tilemap_cells(), MAX_AGGREGATE_CELLS);
}
