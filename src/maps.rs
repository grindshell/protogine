//! The registry of live maps: a slot table with generations and a free list.
//!
//! Deliberately free of ECS, session and scripting dependencies, exactly as
//! [`crate::tilemap`] is. This module owns *which maps exist* and the two
//! aggregate budgets over them; `crate::tilemap` owns what one map is, and
//! `crate::kernel` owns who is allowed to name one.
//!
//! A [`TileMapId`] therefore carries no session marker. An identity is only
//! meaningful inside the table that issued it, so the session check belongs at
//! the public boundary where the handle lives, and duplicating it here would
//! give two places the power to disagree about which kernel a map belongs to.

use crate::tilemap::TileMap;
use std::fmt;

/// Maps one session may hold live at once (M2-7).
///
/// Small enough that a linear scan over the table is never a cost worth
/// optimising, which is what lets every operation here stay obvious.
pub const MAX_TILEMAPS: u32 = 64;

/// Cells summed across every live map (M2-7): 1 MiB of cell storage, twice
/// what a single map may hold under [`crate::tilemap::MAX_CELLS`].
pub const MAX_AGGREGATE_CELLS: u32 = 524_288;

/// A live map's internal identity: a slot and the generation that slot carried
/// when the map was installed.
///
/// `Copy` and sessionless, so the kernel can store one in a field or a
/// component without cloning an `Rc` per body.
/// Crate-visible on purpose. Two kernels that each hold one map both name it
/// `{ slot: 0, generation: 0 }`, and `==` would call those the same map. M2-1
/// puts the session marker on the public *handle* precisely so that comparison
/// is unavailable at the boundary, which only holds while this type stays
/// inside the crate. The budgets above are public; the identity is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileMapId {
    slot: u32,
    generation: u32,
}

impl TileMapId {
    /// The table index this map occupies, unique among the live maps of one
    /// table. Read only by a fixture: M2-1 makes slot reuse a contract clause,
    /// so a test presenting a stale identity has to prove the slot was actually
    /// recycled, and cannot without this. The table itself uses the field.
    #[cfg(test)]
    pub fn slot(self) -> u32 {
        self.slot
    }

    /// Only a fixture reads this: a live handle carries its generation and
    /// never compares it, so exposing it outside tests would be an accessor
    /// with no caller.
    #[cfg(test)]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapTableError {
    /// More live maps than [`MAX_TILEMAPS`].
    Limit,
    /// More cells than [`MAX_AGGREGATE_CELLS`], counting `live - replaced +
    /// candidate`.
    AggregateCells,
    /// A removed, replaced-slot or never-issued identity.
    Invalid,
}

impl fmt::Display for MapTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Interpolated rather than written out. The map count already moved
        // from 32 to 64 once, and a message restating a constant twelve lines
        // below its definition is the defect the contract's freeze pass exists
        // to catch, sitting where no freeze pass runs.
        match self {
            Self::Limit => write!(f, "a session holds at most {MAX_TILEMAPS} maps"),
            Self::AggregateCells => {
                write!(f, "maps hold at most {MAX_AGGREGATE_CELLS} cells in total")
            }
            Self::Invalid => f.write_str("stale or removed map handle"),
        }
    }
}
impl std::error::Error for MapTableError {}

/// One table entry. `generation` advances on removal, so a live slot's
/// generation is stable for the life of the map installed in it.
struct Slot {
    generation: u32,
    map: Option<TileMap>,
}

/// The generation a slot takes when its map is removed, or `None` when the
/// counter is exhausted and the slot must be retired instead of reused.
///
/// A free function so both ends of the arithmetic can be read without a table
/// driven through 4,294,967,296 removals, which nothing can build. The branch
/// stays unreachable; it stops being unexamined.
fn advance(generation: u32) -> Option<u32> {
    generation.checked_add(1)
}

/// Every live map, addressed by generation-checked slot.
pub(crate) struct MapTable {
    slots: Vec<Slot>,
    /// Freed slots, most recently freed last, so `pop` is the "most recently
    /// freed" the contract froze (M2-1). A `Vec` used as a stack rather than a
    /// list threaded through the slots: the order is a contract clause and
    /// costs 256 bytes at this table size, so it is worth having obvious.
    free: Vec<u32>,
    /// Live maps and their summed cells, tracked rather than recomputed so
    /// admission does not walk the table.
    live: u32,
    cells: u32,
}

impl Default for MapTable {
    fn default() -> Self {
        Self::new()
    }
}

impl MapTable {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            cells: 0,
        }
    }

    /// Live maps, bounded by [`MAX_TILEMAPS`].
    pub fn len(&self) -> u32 {
        self.live
    }

    /// Cells summed across every live map, bounded by [`MAX_AGGREGATE_CELLS`].
    pub fn cells(&self) -> u32 {
        self.cells
    }

    /// Live cell and solid-flag storage in bytes, summed across every map.
    ///
    /// Deliberately the same quantity M1's single-map accessor reported, so a
    /// session holding one map measures exactly what it measured before the
    /// table existed. The table's own overhead is [`Self::table_bytes`], kept
    /// separate for that reason.
    pub fn storage_bytes(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| slot.map.as_ref())
            .map(TileMap::storage_bytes)
            .sum()
    }

    /// The table's own overhead: slot entries and the free list, excluding the
    /// maps themselves. Reported rather than folded into
    /// [`Self::storage_bytes`] so the aggregate budget stays a statement about
    /// cells.
    pub fn table_bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Slot>() + self.free.capacity() * size_of::<u32>()
    }

    /// Cells a candidate would add, given what it replaces.
    ///
    /// The subtraction is the whole admission rule (M2-7): charging `live +
    /// candidate` would stop a session that used the budget it was granted from
    /// replacing any map at all, including with an identical one.
    fn admit(&self, candidate: usize, replaced: usize) -> Result<(), MapTableError> {
        // Written as `live + candidate > budget + replaced` rather than
        // `live - replaced + candidate > budget`. Identical over integers, and
        // this form has no subtraction in it at all. The subtracting form is
        // sound only while every caller passes a live map's own cell count,
        // which is a proof that has to be redone for each new caller; Phase 2
        // adds one.
        let total = u64::from(self.cells) + candidate as u64;
        if total > u64::from(MAX_AGGREGATE_CELLS) + replaced as u64 {
            return Err(MapTableError::AggregateCells);
        }
        Ok(())
    }

    /// Whether a candidate of `cells` cells would be admitted in place of the
    /// map at `id`, without installing it.
    ///
    /// Split out so a caller that has work to do between the decision and the
    /// swap - revalidating the outgoing map's members - can refuse on budget
    /// before charging for that work. [`Self::replace`] checks again, so the
    /// swap stays atomic on its own terms rather than trusting this.
    pub fn admits_replacement(&self, id: TileMapId, cells: usize) -> Result<(), MapTableError> {
        let outgoing = self.get(id)?.info().cell_count();
        self.admit(cells, outgoing)
    }

    fn slot(&self, id: TileMapId) -> Result<usize, MapTableError> {
        let index = id.slot as usize;
        match self.slots.get(index) {
            Some(slot) if slot.generation == id.generation && slot.map.is_some() => Ok(index),
            _ => Err(MapTableError::Invalid),
        }
    }

    /// Install a map in a fresh slot.
    ///
    /// Both budgets are checked before a slot is taken, so a refusal leaves the
    /// free list, the count and the storage exactly as they were.
    pub fn insert(&mut self, map: TileMap) -> Result<TileMapId, MapTableError> {
        if self.live >= MAX_TILEMAPS {
            return Err(MapTableError::Limit);
        }
        let cells = map.info().cell_count();
        self.admit(cells, 0)?;
        let slot = match self.free.pop() {
            Some(slot) => {
                self.slots[slot as usize].map = Some(map);
                slot
            }
            None => {
                // Reachable only while every existing slot is occupied, so the
                // table never grows past MAX_TILEMAPS plus the slots retired by
                // generation exhaustion, which is unreachable.
                let slot = self.slots.len() as u32;
                self.slots.push(Slot {
                    generation: 0,
                    map: Some(map),
                });
                slot
            }
        };
        self.live += 1;
        self.cells += cells as u32;
        Ok(TileMapId {
            slot,
            generation: self.slots[slot as usize].generation,
        })
    }

    /// Swap one map's contents, keeping its identity.
    ///
    /// The generation does not advance: M2-4 makes the handle stay valid across
    /// a replacement, which is what distinguishes it from remove-then-create.
    pub fn replace(&mut self, id: TileMapId, map: TileMap) -> Result<(), MapTableError> {
        let index = self.slot(id)?;
        let outgoing = self.slots[index]
            .map
            .as_ref()
            .expect("a resolved slot holds a map")
            .info()
            .cell_count();
        let cells = map.info().cell_count();
        self.admit(cells, outgoing)?;
        self.slots[index].map = Some(map);
        self.cells = self.cells - outgoing as u32 + cells as u32;
        Ok(())
    }

    /// Retire a map, invalidating every identity that named it.
    ///
    /// The caller owns the emptiness rule: this module knows nothing about
    /// colliders, so a map with members must be refused before it gets here.
    pub fn remove(&mut self, id: TileMapId) -> Result<TileMap, MapTableError> {
        let index = self.slot(id)?;
        let map = self.slots[index]
            .map
            .take()
            .expect("a resolved slot holds a map");
        self.live -= 1;
        self.cells -= map.info().cell_count() as u32;
        // A slot whose generation cannot advance is never handed out again,
        // rather than wrapping to a generation a live handle already holds.
        // 4,294,967,296 reuses is 828 days of create-and-remove at 60 Hz, so
        // this is defence with its arithmetic attached rather than a path -
        // the same treatment `KernelError::Capacity` gets. Retiring costs the
        // session one slot; wrapping would cost it the generation check.
        if let Some(next) = advance(self.slots[index].generation) {
            self.slots[index].generation = next;
            self.free.push(id.slot);
        }
        Ok(map)
    }

    pub fn get(&self, id: TileMapId) -> Result<&TileMap, MapTableError> {
        let index = self.slot(id)?;
        Ok(self.slots[index]
            .map
            .as_ref()
            .expect("a resolved slot holds a map"))
    }

    pub fn get_mut(&mut self, id: TileMapId) -> Result<&mut TileMap, MapTableError> {
        let index = self.slot(id)?;
        Ok(self.slots[index]
            .map
            .as_mut()
            .expect("a resolved slot holds a map"))
    }

    /// Release every map and the table itself.
    ///
    /// Used by `Kernel::stop`, which releases storage rather than merely
    /// denying access to it. Identities are not invalidated by advancing
    /// generations here: a stopped session refuses at `require_active` before
    /// any slot is examined (M2-1), and bumping would make the two refusals
    /// indistinguishable.
    ///
    /// **This drops the generations, so a later insert would reissue
    /// `{ slot 0, generation 0 }` and a pre-clear identity would resolve
    /// against it.** That is safe for exactly one reason: `Kernel::stop` cannot
    /// be restarted, so there is no later insert. `require_active` is the
    /// weaker guarantee - a restartable session would refuse and then collide.
    /// Any future caller that is not terminal has to bump instead of dropping.
    pub fn clear(&mut self) {
        self.slots = Vec::new();
        self.free = Vec::new();
        self.live = 0;
        self.cells = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilemap::TileMapInfo;

    /// A map of exactly `cells` cells, given a factorisation the dimension
    /// limits accept.
    fn sized(columns: u32, rows: u32) -> TileMap {
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

    fn cell() -> TileMap {
        sized(1, 1)
    }

    #[test]
    fn a_generation_advances_until_it_cannot_and_then_retires_its_slot() {
        // The retirement branch in `remove` is unreachable - 828 days of
        // create-and-remove at 60 Hz - so its arithmetic is checked here
        // instead of through a table nothing can drive to this point.
        assert_eq!(advance(0), Some(1));
        assert_eq!(advance(u32::MAX - 1), Some(u32::MAX));
        assert_eq!(
            advance(u32::MAX),
            None,
            "an exhausted counter must retire its slot rather than wrap onto a live identity"
        );
    }

    #[test]
    fn reuse_takes_the_most_recently_freed_slot() {
        let mut table = MapTable::new();
        let first = table.insert(cell()).unwrap();
        let second = table.insert(cell()).unwrap();
        let third = table.insert(cell()).unwrap();
        assert_eq!((first.slot(), second.slot(), third.slot()), (0, 1, 2));

        table.remove(first).unwrap();
        table.remove(second).unwrap();
        // Freed 0 then 1, so 1 is handed back first. A first-in-first-out list
        // would return 0 here, which is the whole difference the contract froze.
        assert_eq!(
            table.insert(cell()).unwrap().slot(),
            1,
            "the most recently freed slot must be handed out first"
        );
        assert_eq!(
            table.insert(cell()).unwrap().slot(),
            0,
            "then the slot freed before it"
        );
        // Nothing was recycled twice and nothing grew.
        assert_eq!(
            table.insert(cell()).unwrap().slot(),
            3,
            "an exhausted free list must take the next unused index"
        );
        assert_eq!(table.len(), 4);
    }

    #[test]
    fn a_stale_identity_refuses_after_its_slot_is_reused() {
        let mut table = MapTable::new();
        let first = table.insert(cell()).unwrap();
        table.remove(first).unwrap();
        let second = table.insert(cell()).unwrap();
        // Asserted before the stale identity is presented: without an actual
        // reuse the refusal below would prove only that an empty slot refuses.
        assert_eq!(
            second.slot(),
            first.slot(),
            "the fixture needs a real reuse"
        );
        assert_ne!(second.generation(), first.generation());

        assert_eq!(
            table.get(first).err(),
            Some(MapTableError::Invalid),
            "a stale identity must refuse after its slot is reused"
        );
        assert_eq!(
            table.remove(first).err(),
            Some(MapTableError::Invalid),
            "a stale identity must refuse after its slot is reused"
        );
        assert_eq!(
            table.replace(first, cell()).err(),
            Some(MapTableError::Invalid),
            "a stale identity must refuse after its slot is reused"
        );
        assert!(table.get(second).is_ok(), "the live map at that slot reads");
    }

    #[test]
    fn a_replaced_map_keeps_its_identity_and_a_removed_one_does_not() {
        let mut table = MapTable::new();
        let id = table.insert(sized(4, 4)).unwrap();
        table.replace(id, sized(8, 8)).unwrap();
        assert_eq!(table.get(id).unwrap().info().columns, 8);
        assert_eq!(table.cells(), 64);

        table.remove(id).unwrap();
        assert_eq!(table.get(id).err(), Some(MapTableError::Invalid));
        assert_eq!(table.cells(), 0);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn the_sixty_fifth_map_refuses_without_disturbing_the_table() {
        let mut table = MapTable::new();
        let ids: Vec<_> = (0..MAX_TILEMAPS)
            .map(|_| table.insert(cell()).unwrap())
            .collect();
        assert_eq!(table.len(), MAX_TILEMAPS);

        assert_eq!(
            table.insert(cell()).err(),
            Some(MapTableError::Limit),
            "the sixty-fifth map must refuse"
        );
        assert_eq!(table.len(), MAX_TILEMAPS, "a refusal installed nothing");
        // Written out rather than as MAX_TILEMAPS: 64 maps of one cell is 64
        // cells by coincidence, and punning the count constant as a cell count
        // misleads anyone checking how the two budgets relate.
        assert_eq!(table.cells(), 64, "one cell each");
        // Every identity still resolves, so the refusal took no slot from them.
        assert!(ids.iter().all(|id| table.get(*id).is_ok()));

        // A full table measures 5,120 bytes of slots on this target - 64 of
        // 80, measured rather than derived from `size_of`. Asserted as a bound
        // because the exact figure is a property of the layout rather than of
        // the contract.
        assert!(table.table_bytes() <= 5_120, "{}", table.table_bytes());

        // And the limit is a count rather than a high-water mark.
        table.remove(ids[0]).unwrap();
        assert!(table.insert(cell()).is_ok());

        // Freeing every slot is where the free list reaches its own maximum,
        // which is the half of the overhead a full table cannot show: 256 bytes
        // more, for 5,376 against the megabyte of cells the same table admits.
        // Half a percent, which is why the aggregate budget stays a statement
        // about cells and this cost is reported apart from it.
        for id in ids.iter().skip(1) {
            table.remove(*id).unwrap();
        }
        assert_eq!(table.len(), 1);
        assert!(table.table_bytes() <= 5_376, "{}", table.table_bytes());
    }

    #[test]
    fn replacement_charges_the_difference_rather_than_the_whole_candidate() {
        // 262,144 + 262,143 + 1 fills the aggregate exactly, with one map small
        // enough to grow. A budget charging `live + candidate` refuses the
        // first replacement below; charging `live - replaced + candidate`
        // admits it, which is the difference the contract turns on.
        let mut table = MapTable::new();
        let full = table.insert(sized(512, 512)).unwrap();
        table.insert(sized(511, 513)).unwrap();
        let small = table.insert(cell()).unwrap();
        assert_eq!(table.cells(), MAX_AGGREGATE_CELLS);

        table
            .replace(full, sized(512, 512))
            .expect("an equal-sized replacement fits a full aggregate");
        assert_eq!(table.cells(), MAX_AGGREGATE_CELLS);

        assert_eq!(
            table.replace(small, sized(2, 1)).err(),
            Some(MapTableError::AggregateCells),
            "one cell larger than the budget must refuse"
        );
        assert_eq!(table.cells(), MAX_AGGREGATE_CELLS);
        assert_eq!(
            table.get(small).unwrap().info().cell_count(),
            1,
            "a refused replacement left the outgoing map installed"
        );

        // Smaller is always admissible, and frees what it gave up.
        table.replace(full, sized(256, 512)).unwrap();
        assert_eq!(table.cells(), MAX_AGGREGATE_CELLS - 131_072);
    }

    #[test]
    fn a_refused_admission_leaves_the_count_storage_and_free_list_unchanged() {
        // One cell over half the budget, so the largest map a session may build
        // no longer fits beside it.
        let mut table = MapTable::new();
        let first = table.insert(sized(512, 512)).unwrap();
        let second = table.insert(cell()).unwrap();
        let third = table.insert(cell()).unwrap();
        table.remove(second).unwrap();
        assert_eq!(table.cells(), 262_145);
        let bytes = table.storage_bytes();

        assert_eq!(
            table.insert(sized(512, 512)).err(),
            Some(MapTableError::AggregateCells),
            "the aggregate budget must refuse a candidate that does not fit"
        );
        assert_eq!(table.len(), 2);
        assert_eq!(table.cells(), 262_145);
        assert_eq!(table.storage_bytes(), bytes);
        // The refused insert must not have consumed the freed slot.
        assert_eq!(
            table.insert(cell()).unwrap().slot(),
            second.slot(),
            "a refused insert must not consume the freed slot"
        );
        assert!(table.get(first).is_ok() && table.get(third).is_ok());
    }

    /// A map with the largest solid table the limits allow, so storage is
    /// measured with its flags rather than with the single flag the other
    /// fixtures use.
    fn flagged(columns: u32, rows: u32) -> TileMap {
        TileMap::new(
            TileMapInfo {
                columns,
                rows,
                tile_width: 32,
                tile_height: 32,
                origin_x: 0,
                origin_y: 0,
            },
            vec![true; crate::tilemap::MAX_SOLID_IDS],
            vec![0; (columns * rows) as usize],
        )
        .expect("test map is within the frozen limits")
    }

    #[test]
    fn the_full_aggregate_measures_what_the_phase_0_probe_predicted() {
        // The probe measured 1,114,112 bytes live and 1,639,424 at peak over a
        // prototype `Vec<TileMap>`, and recorded both as properties of the
        // prototype rather than as evidence about the kernel. This is the same
        // arrangement through the production table: 64 maps of 128x64, which is
        // the shape whose 64 copies fill the aggregate to the cell.
        let mut table = MapTable::new();
        let first = table.insert(flagged(128, 64)).unwrap();
        for _ in 1..MAX_TILEMAPS {
            table.insert(flagged(128, 64)).unwrap();
        }
        assert_eq!(table.cells(), MAX_AGGREGATE_CELLS, "filled to the cell");
        assert_eq!(
            table.storage_bytes(),
            1_114_112,
            "the full aggregate must measure what the probe measured"
        );

        // Peak is the aggregate plus one staging candidate, because the
        // candidate's storage is not live until it is admitted. The largest a
        // candidate may be is a whole single-map allowance.
        let candidate = flagged(512, 512);
        assert_eq!(
            table.storage_bytes() + candidate.storage_bytes(),
            1_639_424,
            "peak storage must be the aggregate plus one map"
        );
        // Reached through a replacement rather than an insert: with the table
        // full, a sixty-fifth map is refused by the count before its cells are
        // ever weighed, so the largest candidate that can coexist with a full
        // aggregate is one offered to `replace`. It is refused too - 524,288 -
        // 8,192 + 262,144 does not fit - which is what keeps the peak a
        // transient rather than a second budget a session may hold.
        assert_eq!(
            table.replace(first, candidate).err(),
            Some(MapTableError::AggregateCells),
            "the largest staging candidate must refuse against a full aggregate"
        );
        assert_eq!(
            table.storage_bytes(),
            1_114_112,
            "and a refused replacement must leave the aggregate where it was"
        );

        // Both budgets are exhausted here, so this pins which one answers. The
        // count is checked first and names itself; nothing in the contract
        // decides that, so it is fixed by this assertion rather than left to
        // whichever check an implementation happens to write first.
        assert_eq!(
            table.insert(flagged(128, 64)).err(),
            Some(MapTableError::Limit),
            "a candidate over both budgets must name the count"
        );
    }

    #[test]
    fn storage_counts_every_live_map_and_clearing_releases_all_of_them() {
        let mut table = MapTable::new();
        table.insert(sized(512, 512)).unwrap();
        table.insert(sized(4, 4)).unwrap();
        // Cells at two bytes each, plus one solid flag per map. The same
        // quantity M1's single-map accessor reported, summed.
        assert_eq!(table.storage_bytes(), (262_144 + 16) * 2 + 2);
        assert!(table.table_bytes() > 0);

        table.clear();
        assert_eq!(
            table.storage_bytes(),
            0,
            "stopping must release storage, not merely deny access to it"
        );
        assert_eq!(table.table_bytes(), 0, "the table itself is released too");
        assert_eq!(table.len(), 0);
        assert_eq!(
            table.cells(),
            0,
            "every accounting accessor must agree that a cleared table holds nothing"
        );
    }
}
