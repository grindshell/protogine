//! Raw validation and copying for the `ctx.world` map and collider calls.
//!
//! Everything here converts between VM values and owned Rust values. It holds
//! no engine state, borrows no kernel and owns no budget: the bindings stay in
//! [`super::world`], so there is still one `EngineContext` and one shared
//! world-call budget (T7).
//!
//! The schema is the one frozen in
//! [the Phase 0 record](../../docs/implementation/TILEMAP_COLLISION_PHASE0.md).
//! No defaults, numeric strings, truthy substitutes, metatables, unexpected
//! fields, holes or hash keys are accepted anywhere, and every range check runs
//! again at the Rust entry point, so none of this is the only validation layer.

use super::utilities::{UtilityBudget, plain};
use crate::{collision::TileCollider, tilemap::MAX_SOLID_IDS, tilemap::TileMapInfo};
use mlua::{Lua, Table, Value};

/// Exactly the fields a description carries. All eight are required.
///
/// The first six are also the shape `tilemap_info` reports and are read from
/// this slice in order, so a description and the info it produces cannot drift
/// apart into two lists that have to be kept in step by hand.
const DESCRIPTION_FIELDS: &[&str] = &[
    "columns",
    "rows",
    "tile_width",
    "tile_height",
    "origin_x",
    "origin_y",
    "solids",
    "cells",
];
/// The reported subset of the above, which is every field but the two arrays.
const INFO_FIELDS: usize = 6;
const COLLIDER_FIELDS: &[&str] = &["offset_x", "offset_y", "width", "height"];

/// Elements copied between two observations of the deadline and the work
/// budget. The plan caps this at 256 so that a maximum description - 262,144
/// cells - cannot run to completion unobserved.
const CONVERSION_BATCH: usize = 256;

/// Read the six info fields as exact integers and check them.
///
/// Nothing has been allocated yet and [`TileMapInfo::check`] runs before
/// anything is, so an oversized map is refused before it costs storage.
pub(super) fn description_info(desc: &Table) -> mlua::Result<TileMapInfo> {
    plain(desc, DESCRIPTION_FIELDS, "tilemap description")?;
    let info = TileMapInfo {
        columns: count(desc, DESCRIPTION_FIELDS[0])?,
        rows: count(desc, DESCRIPTION_FIELDS[1])?,
        tile_width: count(desc, DESCRIPTION_FIELDS[2])?,
        tile_height: count(desc, DESCRIPTION_FIELDS[3])?,
        origin_x: origin(desc, DESCRIPTION_FIELDS[4])?,
        origin_y: origin(desc, DESCRIPTION_FIELDS[5])?,
    };
    info.check().map_err(mlua::Error::external)?;
    Ok(info)
}

/// The `solids` array: 1 to 1,024 booleans, element `i` defining tile ID `i`.
pub(super) fn solid_flags(
    desc: &Table,
    budget: &UtilityBudget<'_>,
    charge: &mut dyn FnMut(u64) -> mlua::Result<()>,
) -> mlua::Result<Vec<bool>> {
    let table = array(desc, "solids")?;
    // The declared length is a claim to check, not an answer: `#` on a table
    // with a hole may report any border. Whichever border it reports, `dense`
    // refuses - a short one leaves keys outside the range, a long one leaves an
    // index missing.
    let claimed = table.raw_len();
    if !(1..=MAX_SOLID_IDS).contains(&claimed) {
        return Err(mlua::Error::runtime(
            "tilemap solids must define 1 to 1024 tile IDs",
        ));
    }
    dense(
        &table,
        claimed,
        "tilemap solids",
        "booleans",
        false,
        budget,
        charge,
        |value| match value {
            Value::Boolean(flag) => Some(flag),
            _ => None,
        },
    )
}

/// The `cells` array: exactly `columns * rows` IDs, row-major.
///
/// The expected length comes from the dimensions rather than from `#cells`, so
/// a short array is a refusal and never a smaller map than the one described.
pub(super) fn cell_ids(
    desc: &Table,
    expected: usize,
    budget: &UtilityBudget<'_>,
    charge: &mut dyn FnMut(u64) -> mlua::Result<()>,
) -> mlua::Result<Vec<u16>> {
    let table = array(desc, "cells")?;
    dense(
        &table,
        expected,
        "tilemap cells",
        "whole tile IDs",
        0,
        budget,
        charge,
        |value| {
            let id = number(&value)?;
            whole(id, 0.0, f64::from(u16::MAX)).then_some(id as u16)
        },
    )
}

/// Collider options: exactly the four fields, all real numbers.
///
/// Range checking belongs to `TileCollider::check`, which the kernel runs
/// against the installed map's tile sizes. The same box is legal on one map and
/// refused on another, so a wrapper with no map in hand cannot decide it.
pub(super) fn collider(options: &Table) -> mlua::Result<TileCollider> {
    plain(options, COLLIDER_FIELDS, "collider options")?;
    let mut values = [0.0f64; 4];
    for (slot, key) in values.iter_mut().zip(COLLIDER_FIELDS) {
        *slot = match options.raw_get::<Value>(*key)? {
            Value::Nil => {
                return Err(mlua::Error::runtime(format!("collider {key} is required")));
            }
            value => number(&value)
                .ok_or_else(|| mlua::Error::runtime(format!("collider {key} must be a number")))?,
        };
    }
    let [offset_x, offset_y, width, height] = values;
    Ok(TileCollider {
        offset_x,
        offset_y,
        width,
        height,
    })
}

/// A tile coordinate: an exact signed 32-bit integer.
///
/// A larger number is refused rather than narrowed, wrapped or walked toward,
/// so `tile(2^31, 0)` is a malformed call and never a scan.
pub(super) fn index(value: &Value, what: &str) -> mlua::Result<i32> {
    let index = number(value).filter(|index| whole(*index, INDEX_LOW, INDEX_HIGH));
    index.map(|index| index as i32).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "tile {what} must be a whole number within a signed 32-bit range"
        ))
    })
}

const INDEX_LOW: f64 = i32::MIN as f64;
const INDEX_HIGH: f64 = i32::MAX as f64;

/// A region extent in cells. The kernel refuses zero and anything above the
/// per-call cap; this only refuses what is not a count at all.
pub(super) fn extent(value: &Value, what: &str) -> mlua::Result<u32> {
    let extent = number(value).filter(|extent| whole(*extent, 0.0, f64::from(u32::MAX)));
    extent.map(|extent| extent as u32).ok_or_else(|| {
        mlua::Error::runtime(format!("region {what} must be a whole number of cells"))
    })
}

/// A tile ID argument. The kernel refuses IDs the installed map does not define.
pub(super) fn tile_id(value: &Value) -> mlua::Result<u16> {
    let id = number(value).filter(|id| whole(*id, 0.0, f64::from(u16::MAX)));
    id.map(|id| id as u16)
        .ok_or_else(|| mlua::Error::runtime("tile ID must be a whole number in 0..65535"))
}

/// Owned dimensions, tile size and origin. Mutating it cannot reach the map.
pub(super) fn info_table(lua: &Lua, info: TileMapInfo) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, INFO_FIELDS)?;
    let values = [
        f64::from(info.columns),
        f64::from(info.rows),
        f64::from(info.tile_width),
        f64::from(info.tile_height),
        f64::from(info.origin_x),
        f64::from(info.origin_y),
    ];
    for (key, value) in DESCRIPTION_FIELDS[..INFO_FIELDS].iter().zip(values) {
        table.raw_set(*key, value)?;
    }
    Ok(table)
}

/// An owned flat row-major array of IDs.
///
/// The kernel has already charged this call's output volume and bounded it at
/// the per-call cap, so the only thing left to bound here is the time the copy
/// itself takes without a VM interrupt.
pub(super) fn region_table(
    lua: &Lua,
    ids: &[u16],
    budget: &UtilityBudget<'_>,
) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(ids.len(), 0)?;
    for (batch, chunk) in ids.chunks(CONVERSION_BATCH).enumerate() {
        budget.check()?;
        for (offset, id) in chunk.iter().enumerate() {
            table.raw_set(batch * CONVERSION_BATCH + offset + 1, f64::from(*id))?;
        }
    }
    Ok(table)
}

/// An owned copy of a collider. Mutating it cannot reach the entity.
pub(super) fn collider_table(lua: &Lua, collider: &TileCollider) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, COLLIDER_FIELDS.len())?;
    let values = [
        collider.offset_x,
        collider.offset_y,
        collider.width,
        collider.height,
    ];
    for (key, value) in COLLIDER_FIELDS.iter().zip(values) {
        table.raw_set(*key, value)?;
    }
    Ok(table)
}

/// Copy a dense one-based array of exactly `expected` elements.
///
/// Keys are unique, so requiring every key to be an integer in `1..=expected`
/// and then counting exactly `expected` of them proves both that every index is
/// present and that nothing else is. Neither half is provable from `#` alone,
/// which is why the length a caller declares is only ever an input to this.
///
/// An array claiming a nine-cell map while carrying a quarter of a million
/// entries costs ten reads rather than all of them, but the index range check
/// below is what bounds that, not the early exit here: an array part is
/// traversed in index order, so the tenth key is already outside `1..=expected`.
/// The early exit states the postcondition where a reader looks for it and
/// covers nothing the range check does not.
///
/// Work is charged and the deadline observed before each bounded batch. Charging
/// before means a refusal partway through a batch is charged for the whole of
/// it, which is the safe direction for a caller probing with descriptions that
/// are large and malformed at the end; the last batch of an accepted call is
/// sized to what remains, so an honest description is charged exactly.
#[allow(clippy::too_many_arguments)]
fn dense<T: Copy>(
    table: &Table,
    expected: usize,
    what: &str,
    element: &str,
    fill: T,
    budget: &UtilityBudget<'_>,
    charge: &mut dyn FnMut(u64) -> mlua::Result<()>,
    convert: impl Fn(Value) -> Option<T>,
) -> mlua::Result<Vec<T>> {
    if table.metatable().is_some() {
        return Err(mlua::Error::runtime(format!(
            "{what} must be a plain array with no metatable"
        )));
    }
    let length = || mlua::Error::runtime(format!("{what} must hold exactly {expected} elements"));
    let mut out = Vec::new();
    out.try_reserve_exact(expected)
        .map_err(|_| mlua::Error::runtime(format!("could not reserve {what}")))?;
    out.resize(expected, fill);
    let mut seen = 0usize;
    for pair in table.clone().pairs::<Value, Value>() {
        if seen == expected {
            return Err(length());
        }
        if seen.is_multiple_of(CONVERSION_BATCH) {
            budget.check()?;
            charge(CONVERSION_BATCH.min(expected - seen) as u64)?;
        }
        let (key, value) = pair?;
        let slot = array_index(&key, expected)
            .ok_or_else(|| mlua::Error::runtime(format!("{what} must be a dense array")))?;
        out[slot] = convert(value)
            .ok_or_else(|| mlua::Error::runtime(format!("{what} must hold {element}")))?;
        seen += 1;
    }
    if seen != expected {
        return Err(length());
    }
    Ok(out)
}

/// A one-based array key inside `expected`, as a zero-based offset.
fn array_index(key: &Value, expected: usize) -> Option<usize> {
    // `expected` is at most 262,144, so its f64 form is exact.
    let index = number(key).filter(|index| whole(*index, 1.0, expected as f64))?;
    Some(index as usize - 1)
}

fn array(desc: &Table, key: &str) -> mlua::Result<Table> {
    match desc.raw_get::<Value>(key)? {
        Value::Table(table) => Ok(table),
        Value::Nil => Err(mlua::Error::runtime(format!("tilemap {key} is required"))),
        _ => Err(mlua::Error::runtime(format!(
            "tilemap {key} must be an array"
        ))),
    }
}

/// Actual numeric values only: a numeric string is a coercion, not a number.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::Number(value) => Some(*value),
        _ => None,
    }
}

/// An exact integer in an inclusive range. Phrased so that NaN falls through to
/// the refusal rather than passing an ordered comparison.
fn whole(value: f64, low: f64, high: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value >= low && value <= high
}

fn count(desc: &Table, key: &str) -> mlua::Result<u32> {
    let count = field(desc, key)?;
    if !whole(count, 0.0, f64::from(u32::MAX)) {
        return Err(mlua::Error::runtime(format!(
            "tilemap {key} must be a nonnegative whole number"
        )));
    }
    Ok(count as u32)
}

fn origin(desc: &Table, key: &str) -> mlua::Result<i32> {
    let origin = field(desc, key)?;
    if !whole(origin, INDEX_LOW, INDEX_HIGH) {
        return Err(mlua::Error::runtime(format!(
            "tilemap {key} must be a whole number of world pixels"
        )));
    }
    Ok(origin as i32)
}

fn field(desc: &Table, key: &str) -> mlua::Result<f64> {
    match desc.raw_get::<Value>(key)? {
        Value::Nil => Err(mlua::Error::runtime(format!("tilemap {key} is required"))),
        value => number(&value)
            .ok_or_else(|| mlua::Error::runtime(format!("tilemap {key} must be a number"))),
    }
}
