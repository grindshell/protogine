//! Per-callback resource accounting and shared option-table inspection, used by
//! the data, filesystem, asset, drawing and world bindings.
//!
//! The three call counters are independent on purpose: a metadata query must
//! not consume the utility budget, and a per-tile draw loop's internal
//! dimension lookups must not consume the asset budget.

use super::Budget;
use mlua::{Table, Value};
use std::cell::Cell;

pub(super) const BYTE_LIMIT: usize = 1024 * 1024;
/// `ctx.assets` attempts per callback, shared by every entry point there.
pub(super) const ASSET_CALL_LIMIT: usize = 256;
/// `ctx.draw.sprite` attempts per draw callback, accepted or refused.
pub(super) const SPRITE_ATTEMPT_LIMIT: usize = 10_000;

pub(super) struct UtilityBudget<'a> {
    budget: &'a Budget,
    calls: Cell<usize>,
    assets: Cell<usize>,
    sprites: Cell<usize>,
    transferred: Cell<usize>,
}

impl<'a> UtilityBudget<'a> {
    pub(super) fn new(budget: &'a Budget) -> Self {
        Self {
            budget,
            calls: Cell::new(0),
            assets: Cell::new(0),
            sprites: Cell::new(0),
            transferred: Cell::new(0),
        }
    }

    pub(super) fn check(&self) -> mlua::Result<()> {
        match self.budget.check() {
            Some(message) => Err(mlua::Error::runtime(message)),
            None => Ok(()),
        }
    }

    pub(super) fn limit(&self, exceeded: bool, message: &'static str) -> mlua::Result<()> {
        if exceeded {
            self.budget.fail(message);
        }
        self.check()
    }

    /// Count attempts before fallible argument conversion, including missing or
    /// malformed arguments that mlua would reject before a typed closure runs.
    pub(super) fn begin(&self) -> mlua::Result<()> {
        self.calls.set(self.calls.get() + 1);
        self.limit(self.calls.get() > 128, "utility call limit exceeded")
    }

    /// Count an asset attempt before its phase check and argument conversion,
    /// so cache hits, wrong-phase calls and malformed arguments all count.
    pub(super) fn asset_call(&self) -> mlua::Result<()> {
        self.assets.set(self.assets.get() + 1);
        self.limit(
            self.assets.get() > ASSET_CALL_LIMIT,
            "asset call limit exceeded",
        )
    }

    /// Count a sprite attempt before argument and option conversion. A rejected
    /// sprite may already have cost two option-table inspections, so refusals
    /// need their own ceiling rather than relying on the callback deadline.
    pub(super) fn sprite_call(&self) -> mlua::Result<()> {
        self.sprites.set(self.sprites.get() + 1);
        self.limit(
            self.sprites.get() > SPRITE_ATTEMPT_LIMIT,
            "sprite call limit exceeded",
        )
    }

    pub(super) fn bytes(&self, size: usize) -> mlua::Result<()> {
        self.limit(size > BYTE_LIMIT, "utility byte limit exceeded")
    }

    pub(super) fn transfer(&self, size: usize) -> mlua::Result<()> {
        self.bytes(size)?;
        self.transferred.set(self.transferred.get() + size);
        self.limit(
            self.transferred.get() > 8 * BYTE_LIMIT,
            "filesystem transfer limit exceeded",
        )
    }
}

/// Reject metatables, unknown names and numeric keys, stopping at the first
/// unexpected key. Values are read without metamethods.
///
/// The counter makes the iteration bound evident rather than leaving it to be
/// re-derived: keys are unique, so a table can carry at most `fields.len()`
/// known names before an unknown one stops the walk either way. Which of the
/// two refusals reports first depends on the VM's iteration order, so neither
/// message is a contract; both refuse the same tables.
pub(super) fn plain(table: &Table, fields: &[&str], what: &str) -> mlua::Result<()> {
    if table.metatable().is_some() {
        return Err(mlua::Error::runtime(format!(
            "{what} must be a plain table with no metatable"
        )));
    }
    let mut seen = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        seen += 1;
        if seen > fields.len() {
            return Err(mlua::Error::runtime(format!("{what} has too many fields")));
        }
        let known = match &key {
            Value::String(name) => name
                .to_str()
                .is_ok_and(|name| fields.iter().any(|field| *field == name.as_ref())),
            _ => false,
        };
        if !known {
            return Err(mlua::Error::runtime(format!("{what} has an unknown field")));
        }
    }
    Ok(())
}
