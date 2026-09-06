//! Per-callback resource accounting shared by data and filesystem bindings.

use super::Budget;
use std::cell::Cell;

pub(super) const BYTE_LIMIT: usize = 1024 * 1024;

pub(super) struct UtilityBudget<'a> {
    budget: &'a Budget,
    calls: Cell<usize>,
    transferred: Cell<usize>,
}

impl<'a> UtilityBudget<'a> {
    pub(super) fn new(budget: &'a Budget) -> Self {
        Self {
            budget,
            calls: Cell::new(0),
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
