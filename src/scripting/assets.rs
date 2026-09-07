//! Callback-scoped bundle image bindings.
//!
//! The store is the only authority on a live image; a handle is the script's
//! view of one registry entry. No store or userdata borrow is held across VM
//! work, and the canonical wrapper is published as the last step of admission,
//! so a failed VM allocation rolls the job back and burns its identity instead
//! of leaving an admitted image no script can name or unload.

use super::utilities::UtilityBudget;
use crate::assets::{AssetStore, ImageId, ImageState, ImageStatus};
use mlua::{
    AnyUserData, FromLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Scope, Table, UserData,
    UserDataMethods, Value,
};
use std::{
    cell::{Ref, RefCell},
    collections::HashMap,
    path::Path,
    time::Duration,
};

/// The script's view of one logical image.
///
/// It carries the scalar identity plus the bounded terminal state copied out of
/// the store when the entry settled, so a retained handle still reports why its
/// image failed, or that it was unloaded, after the registry slot is gone. It
/// holds no store reference and can never resurrect an entry.
pub(crate) struct ImageHandle {
    id: ImageId,
    terminal: RefCell<Option<ImageStatus>>,
}

impl ImageHandle {
    pub(super) fn id(&self) -> ImageId {
        self.id
    }
}

impl UserData for ImageHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: AnyUserData| {
            Ok(other
                .borrow::<Self>()
                .is_ok_and(|other| this.id == other.id))
        });
    }
}

/// The host's asset store, the script-visible view of it, and the VM wrappers
/// that name its entries.
pub(super) struct Images {
    store: RefCell<AssetStore>,
    /// What scripts see, keyed by image number. Worker progress reaches this
    /// map only at a publication boundary, so script-visible readiness advances
    /// at the fixed tick rate rather than the presentation frame rate, and a
    /// zero-tick frame cannot reveal a completion the next update has not
    /// published. A script's own request and unload update it immediately: the
    /// boundary governs worker results, not the caller's own action.
    ///
    /// Rust callers read the live store through `store()` instead. Entries are
    /// added at admission and removed when a terminal transition is committed,
    /// so this is bounded by the registry exactly as the wrapper cache is.
    committed: RefCell<HashMap<u32, ImageStatus>>,
    /// One canonical wrapper per live registry entry, keyed by image number, so
    /// coalesced requests and ready cache hits return the same userdata for
    /// `rawequal` and table-key identity. The table holds strong references and
    /// is bounded by the registry: an entry leaves it when its terminal
    /// transition is committed, which is also when its status is copied onto
    /// the handle.
    wrappers: Table,
}

impl Images {
    pub(super) fn new(lua: &Lua, root: &Path) -> mlua::Result<Self> {
        Ok(Self {
            store: RefCell::new(AssetStore::new(root).map_err(mlua::Error::external)?),
            committed: RefCell::new(HashMap::new()),
            wrappers: lua.create_table()?,
        })
    }

    /// Read-only access for Rust tools. Holding it across a callback would
    /// conflict with the bindings' own borrow, so take it between calls.
    pub(super) fn store(&self) -> Ref<'_, AssetStore> {
        self.store.borrow()
    }

    /// One bounded, non-blocking service pass.
    pub(super) fn service(&self) {
        self.store.borrow_mut().service();
    }

    /// One bounded pass that waits up to `wait` for the outstanding grant. It
    /// grants no more work per pass than `service` does.
    pub(super) fn advance(&self, wait: Duration) {
        self.store.borrow_mut().advance(wait);
    }

    /// Service passes until every admitted job settles, under its own watchdog.
    pub(super) fn drain(&self, timeout: Duration) -> bool {
        self.store.borrow_mut().drain(timeout)
    }

    pub(super) fn service_fault(&self) -> Option<String> {
        self.store.borrow().service_fault().map(str::to_string)
    }

    /// Publish everything the worker produced since the last commit: refresh
    /// the script-visible view, then copy each terminal status onto its
    /// retained handle and drop the canonical wrapper, so the strong cache
    /// never outlives the registry entry it mirrors and no terminal diagnostic
    /// is lost when the slot is released.
    ///
    /// This is the only place worker progress becomes script-visible.
    pub(super) fn commit(&self) -> mlua::Result<()> {
        // Taking the transitions is what releases terminal registry slots, so
        // refresh the surviving entries from the store afterwards and let the
        // terminal pass below carry the entries the store just dropped.
        let settled = self.store.borrow_mut().take_settled();
        {
            let store = self.store.borrow();
            let mut committed = self.committed.borrow_mut();
            for status in committed.values_mut() {
                if let Some(fresh) = store.status(status.id) {
                    *status = fresh;
                }
            }
        }
        for status in settled {
            if !status.state.is_terminal() {
                continue;
            }
            self.committed.borrow_mut().remove(&status.id.number());
            let key = f64::from(status.id.number());
            let Some(value) = self.wrappers.raw_get::<Option<AnyUserData>>(key)? else {
                continue;
            };
            if let Ok(handle) = value.borrow::<ImageHandle>() {
                *handle.terminal.borrow_mut() = Some(status);
            }
            self.wrappers.raw_set(key, Value::Nil)?;
        }
        Ok(())
    }

    /// Stop admitting, cancel outstanding work, join the worker and release the
    /// published view and wrapper cache. Idempotent, and invokes no script code.
    pub(super) fn shutdown(&self) {
        self.store.borrow_mut().shutdown();
        self.committed.borrow_mut().clear();
        let keys: Vec<Value> = self
            .wrappers
            .clone()
            .pairs::<Value, Value>()
            .filter_map(|pair| pair.ok().map(|(key, _)| key))
            .collect();
        for key in keys {
            let _ = self.wrappers.raw_set(key, Value::Nil);
        }
    }

    /// Record the caller's own transition immediately. A request and an unload
    /// are script actions, not worker results, so they do not wait for a
    /// publication boundary the calling callback is already past.
    ///
    /// This reads the store, and `AssetStore::unload` builds its terminal status
    /// from the store's live stage and byte count, so the exception depends on
    /// an invariant: no CPU service pass may run between a commit and the
    /// callbacks that follow it. Every entry point that can reach here today
    /// satisfies it — `frame`, `step` and `update` service and then commit
    /// immediately before running update, `init` sees a fresh store, and `draw`
    /// refuses both operations — so the view and the store always agree here.
    /// A later phase that services the CPU store elsewhere in a frame would let
    /// an unload publish progress the boundary had withheld.
    fn publish_now(&self, id: ImageId) {
        if let Some(status) = self.store.borrow().status(id) {
            self.committed.borrow_mut().insert(id.number(), status);
        }
    }

    /// The published view of a live entry, or the terminal snapshot the handle
    /// retained after its registry slot was released.
    fn snapshot(&self, handle: &ImageHandle) -> Option<ImageStatus> {
        self.committed
            .borrow()
            .get(&handle.id.number())
            // Image numbers are append-only, so a published entry under this
            // number is always this handle's; check rather than assume it.
            .filter(|status| status.id == handle.id)
            .cloned()
            .or_else(|| handle.terminal.borrow().clone())
    }

    /// The dimensions of a live CPU-ready image. Drawing never forces work, so
    /// a pending, failed or unloaded handle is an ordinary catchable refusal.
    pub(super) fn drawable(&self, handle: &ImageHandle) -> mlua::Result<(u32, u32)> {
        self.check_session(handle.id)?;
        match self.snapshot(handle) {
            Some(status) if status.state == ImageState::Ready => status
                .width
                .zip(status.height)
                .ok_or_else(|| mlua::Error::runtime("ready image has no validated dimensions")),
            Some(status) => Err(mlua::Error::runtime(format!(
                "sprite requires a ready image; this one is {}",
                status.state.as_str()
            ))),
            None => Err(mlua::Error::runtime("unknown image handle")),
        }
    }

    /// Only a Rust harness can build a handle from another store, exactly as in
    /// the world bindings, so this refusal is catchable rather than a fault.
    fn check_session(&self, id: ImageId) -> mlua::Result<()> {
        if id.session() == self.store.borrow().session() {
            Ok(())
        } else {
            Err(mlua::Error::runtime("foreign image handle"))
        }
    }

    fn wrapper(&self, lua: &Lua, id: ImageId) -> mlua::Result<AnyUserData> {
        let key = f64::from(id.number());
        if let Some(existing) = self.wrappers.raw_get::<Option<AnyUserData>>(key)? {
            return Ok(existing);
        }
        let value = lua.create_userdata(ImageHandle {
            id,
            terminal: RefCell::new(None),
        })?;
        self.wrappers.raw_set(key, value.clone())?;
        Ok(value)
    }

    pub(super) fn bind<'s>(
        &'s self,
        lua: &Lua,
        scope: &'s Scope<'s, '_>,
        budget: &'s UtilityBudget<'_>,
        writable: bool,
    ) -> mlua::Result<Table> {
        let api = lua.create_table()?;
        api.raw_set(
            "request_png",
            scope.create_function(move |lua, args: MultiValue| {
                budget.asset_call()?;
                if !writable {
                    return Err(mlua::Error::runtime(
                        "asset requests require init or update",
                    ));
                }
                let path = LuaString::from_lua_multi(args, lua)?;
                let path = path.to_str()?.to_string();
                // Resolution is synchronous on a miss so that coalescing and
                // handle identity are decided before returning; it performs
                // metadata I/O only, never content reads or decoding.
                let (id, admitted) = {
                    let mut store = self.store.borrow_mut();
                    let before = store.counters().admitted;
                    let id = store.request_png(&path).map_err(mlua::Error::external)?;
                    let admitted = store.counters().admitted != before;
                    (id, admitted)
                };
                match self.wrapper(lua, id) {
                    Ok(value) => {
                        // Only a fresh admission publishes a view. A hit names
                        // an image whose published state must keep waiting for
                        // the next boundary like every other worker result.
                        if admitted {
                            self.publish_now(id);
                        }
                        Ok(value)
                    }
                    // Only a fresh admission may be rolled back: a coalesced or
                    // cached hit named an image other callers already own.
                    Err(error) if admitted => {
                        self.store.borrow_mut().roll_back(id);
                        Err(error)
                    }
                    Err(error) => Err(error),
                }
            })?,
        )?;
        api.raw_set(
            "status",
            scope.create_function(move |lua, args: MultiValue| {
                budget.asset_call()?;
                let value = AnyUserData::from_lua_multi(args, lua)?;
                let status = {
                    let handle = value.borrow::<ImageHandle>()?;
                    self.check_session(handle.id)?;
                    self.snapshot(&handle)
                };
                let Some(status) = status else {
                    return Err(mlua::Error::runtime("unknown image handle"));
                };
                status_table(lua, &status)
            })?,
        )?;
        api.raw_set(
            "size",
            scope.create_function(move |lua, args: MultiValue| {
                budget.asset_call()?;
                let value = AnyUserData::from_lua_multi(args, lua)?;
                let size = {
                    let handle = value.borrow::<ImageHandle>()?;
                    self.check_session(handle.id)?;
                    // Known from the validated header onwards and frozen from
                    // then on; a failed or unloaded image refuses even when its
                    // header was read.
                    match self.snapshot(&handle) {
                        Some(status) if !status.state.is_terminal() => {
                            status.width.zip(status.height)
                        }
                        _ => None,
                    }
                };
                let Some((width, height)) = size else {
                    return Err(mlua::Error::runtime(
                        "image dimensions are unknown, failed or unloaded",
                    ));
                };
                let table = lua.create_table()?;
                table.raw_set("width", width)?;
                table.raw_set("height", height)?;
                Ok(table)
            })?,
        )?;
        api.raw_set(
            "unload",
            scope.create_function(move |lua, args: MultiValue| {
                budget.asset_call()?;
                if !writable {
                    return Err(mlua::Error::runtime("asset unload requires init or update"));
                }
                let value = AnyUserData::from_lua_multi(args, lua)?;
                let handle = value.borrow::<ImageHandle>()?;
                self.check_session(handle.id)?;
                let unloaded = self.store.borrow_mut().unload(handle.id);
                if unloaded {
                    self.publish_now(handle.id);
                }
                Ok(unloaded)
            })?,
        )?;
        api.set_readonly(true);
        Ok(api)
    }
}

/// An owned copy of the frozen status schema. The script owns the tables and
/// strings it receives; nothing here borrows the store or expires with the call.
fn status_table(lua: &Lua, status: &ImageStatus) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.raw_set("state", status.state.as_str())?;
    table.raw_set("stage", status.stage.as_str())?;
    table.raw_set("bytes_read", status.bytes_read as f64)?;
    table.raw_set("gpu", status.gpu.as_str())?;
    if let Some(width) = status.width {
        table.raw_set("width", width)?;
    }
    if let Some(height) = status.height {
        table.raw_set("height", height)?;
    }
    if let Some(error) = &status.error {
        let owned = lua.create_table()?;
        owned.raw_set("code", error.code.as_str())?;
        owned.raw_set("path", error.path.as_str())?;
        owned.raw_set("message", error.message.as_str())?;
        table.raw_set("error", owned)?;
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::{ScriptHost, ScriptLimits, ScriptState};
    use std::time::Duration;

    /// A bundle with one committed 2x3 fixture image, whose pixels and framing
    /// are specified independently of this engine's decoder.
    fn bundle(main: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.luau"), main).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/rgba.png"),
            root.path().join("art.png"),
        )
        .unwrap();
        root
    }

    /// Two live stores issue the same image numbers, so only the session half
    /// separates their handles. Only a Rust harness can build one, exactly as
    /// in the world bindings.
    #[test]
    fn foreign_image_handles_are_refused_catchably_by_every_operation() {
        let main = r#"
            return {
                init = function(ctx)
                    local own = ctx.assets.request_png("art.png")
                    assert(own ~= foreign)
                    for _, call in {ctx.assets.status, ctx.assets.size, ctx.assets.unload} do
                        local ok, err = pcall(call, foreign)
                        assert(not ok, "a foreign handle was accepted")
                        assert(string.find(tostring(err), "foreign image handle"), tostring(err))
                    end
                    assert(ctx.assets.status(own).state == "queued")
                end,
                draw = function(ctx)
                    local ok, err = pcall(ctx.draw.sprite, foreign, 0, 0)
                    assert(not ok and string.find(tostring(err), "foreign image handle"))
                end,
            }
        "#;
        let (own_root, other_root) = (bundle(main), bundle(main));
        let mut host = ScriptHost::load(own_root.path(), ScriptLimits::default()).unwrap();
        let other = ScriptHost::load(other_root.path(), ScriptLimits::default()).unwrap();
        let foreign = ImageId::new(other.images.store.borrow().session(), 1);
        assert_ne!(foreign.session(), host.images.store.borrow().session());
        host.lua
            .globals()
            .raw_set(
                "foreign",
                host.lua
                    .create_userdata(ImageHandle {
                        id: foreign,
                        terminal: RefCell::new(None),
                    })
                    .unwrap(),
            )
            .unwrap();

        host.init().unwrap();
        // The host's own first image took the same image number, so the
        // refusals above turned on session identity alone.
        assert!(
            host.images
                .wrappers
                .raw_get::<Option<AnyUserData>>(1.0)
                .unwrap()
                .is_some()
        );
        assert!(host.drain_assets(Duration::from_secs(10)).unwrap());
        host.draw(0.0).unwrap();
        assert_eq!(host.state(), ScriptState::Running);
        assert!(host.draw_commands().is_empty());
        // Neither store lost an image to the other's operations.
        assert_eq!(host.images.store.borrow().counters().completed, 1);
        assert_eq!(other.images.store.borrow().counters().requests, 0);
    }

    /// Publication is the last step of admission. Injecting a refusal at the
    /// wrapper cache is the only way to exercise it, and a structurally
    /// unexercised rollback is not behavioral proof.
    #[test]
    fn a_failed_wrapper_publication_rolls_back_its_admission_and_burns_the_identity() {
        let root = bundle(
            r#"
            return {
                init = function(ctx)
                    assert(not pcall(ctx.assets.request_png, "art.png"))
                end,
                update = function(ctx)
                    local image = ctx.assets.request_png("art.png")
                    ctx.log(ctx.assets.status(image).state)
                end,
            }
        "#,
        );
        let mut host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
        // Refuse the final cache write, after the store has already admitted.
        host.images.wrappers.set_readonly(true);
        host.init().unwrap();
        {
            let store = host.images.store.borrow();
            assert_eq!(store.counters().admitted, 1);
            // The admission was undone: no job, no lookup and no reservation.
            assert_eq!(store.pending_jobs(), 0);
            assert_eq!(store.staged_bytes(), 0);
            assert_eq!(store.resident_bytes(), 0);
            assert_eq!(store.memoized_spellings(), 0);
        }
        assert!(host.images.wrappers.raw_len() == 0);

        host.images.wrappers.set_readonly(false);
        host.update().unwrap();
        assert_eq!(host.take_logs(), ["queued"]);
        // Image number 1 was burned; the retry is a new identity.
        assert!(
            host.images
                .wrappers
                .raw_get::<Option<AnyUserData>>(1.0)
                .unwrap()
                .is_none()
        );
        let retry = host
            .images
            .wrappers
            .raw_get::<Option<AnyUserData>>(2.0)
            .unwrap()
            .expect("the retry published a wrapper");
        assert_eq!(retry.borrow::<ImageHandle>().unwrap().id.number(), 2);
        assert!(host.drain_assets(Duration::from_secs(10)).unwrap());
        assert_eq!(host.images.store.borrow().counters().completed, 1);
    }
}
