//! Per-callback resource accounting shared by the data, filesystem, asset and
//! drawing bindings.
//!
//! The three call counters are independent on purpose: a metadata query must
//! not consume the utility budget, and a per-tile draw loop's internal
//! dimension lookups must not consume the asset budget.

use super::Budget;
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
