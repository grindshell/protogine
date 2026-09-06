//! Callback-scoped kernel bindings. No VM work happens while a kernel is borrowed.

use super::utilities::UtilityBudget;
use crate::{
    input::{Button, InputSnapshot},
    kernel::{EntityHandle, Kernel, KernelError, Position, Velocity},
};
use mlua::{
    AnyUserData, FromLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Scope, Table, UserData,
    UserDataMethods,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

impl UserData for EntityHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: AnyUserData| {
            Ok(other.borrow::<Self>().is_ok_and(|other| *this == *other))
        });
    }
}

/// Canonical wrappers while Lua retains them, including as table keys. Weak
/// values let the VM reclaim unused wrappers and cache entries under its heap cap.
pub(super) struct EntityCache {
    values: Table,
    /// Sessions seen by this cache, in key order. Holding the marker keeps each
    /// index stable and stops a freed session's address from being reused.
    sessions: RefCell<Vec<Rc<()>>>,
}

/// Enough sessions for any real host while keeping every packed key exact.
const SESSION_LIMIT: usize = 1 << 20;
/// 2^32, one whole u32 slot space per session, so packed keys never collide.
const SLOT_SPACE: f64 = 4_294_967_296.0;

impl EntityCache {
    pub(super) fn new(lua: &Lua) -> mlua::Result<Self> {
        let values = lua.create_table()?;
        let meta = lua.create_table()?;
        meta.raw_set("__mode", "v")?;
        meta.set_readonly(true);
        values.set_metatable(Some(meta))?;
        Ok(Self {
            values,
            sessions: RefCell::new(Vec::new()),
        })
    }

    /// Pack (session, slot) into one exact Lua number. Slots are u32 and the
    /// session index is bounded above, so the result stays below 2^52 and needs
    /// no allocation, unlike the byte-string key this replaces.
    fn key(&self, entity: &EntityHandle) -> mlua::Result<f64> {
        let mut sessions = self.sessions.borrow_mut();
        let index = match sessions
            .iter()
            .position(|session| Rc::ptr_eq(session, entity.session()))
        {
            Some(index) => index,
            None => {
                if sessions.len() >= SESSION_LIMIT {
                    return Err(mlua::Error::runtime("too many world sessions"));
                }
                sessions.push(entity.session().clone());
                sessions.len() - 1
            }
        };
        Ok(index as f64 * SLOT_SPACE + f64::from(entity.slot()))
    }

    fn get(&self, lua: &Lua, entity: &EntityHandle) -> mlua::Result<AnyUserData> {
        let key = self.key(entity)?;
        // A slot is unique among one session's live entities, so a cached
        // wrapper that does not match this handle belongs to a despawned entity
        // whose slot was reused; replace it rather than returning a stale one.
        if let Some(value) = self.values.raw_get::<Option<AnyUserData>>(key)?
            && value
                .borrow::<EntityHandle>()
                .is_ok_and(|held| *held == *entity)
        {
            return Ok(value);
        }
        let value = lua.create_userdata(entity.clone())?;
        self.values.raw_set(key, value.clone())?;
        Ok(value)
    }
}

pub(crate) struct EngineContext<'a> {
    kernel: RefCell<&'a mut Kernel>,
    input: InputSnapshot,
    calls: Cell<usize>,
    #[cfg(feature = "native-plugins")]
    pub(super) plugins: Option<&'a mut crate::plugins::PluginSet>,
}

impl<'a> EngineContext<'a> {
    pub(crate) fn new(kernel: &'a mut Kernel, input: InputSnapshot) -> Self {
        Self {
            kernel: RefCell::new(kernel),
            input,
            calls: Cell::new(0),
            #[cfg(feature = "native-plugins")]
            plugins: None,
        }
    }

    #[cfg(feature = "native-plugins")]
    pub(crate) fn with_plugins(
        mut self,
        plugins: Option<&'a mut crate::plugins::PluginSet>,
    ) -> Self {
        self.plugins = plugins;
        self
    }

    // Count attempts before fallible conversion so malformed arguments cannot
    // bypass the world operation budget.
    fn begin(
        &self,
        budget: &UtilityBudget<'_>,
        mutation: bool,
        writable: bool,
    ) -> mlua::Result<()> {
        self.calls.set(self.calls.get() + 1);
        budget.limit(self.calls.get() > 4096, "world operation limit exceeded")?;
        if mutation && !writable {
            return Err(mlua::Error::runtime(
                "world mutation requires init or update",
            ));
        }
        Ok(())
    }

    pub(super) fn bind<'s>(
        &'s self,
        lua: &Lua,
        scope: &'s Scope<'s, '_>,
        budget: &'s UtilityBudget<'_>,
        writable: bool,
        cache: &'s EntityCache,
    ) -> mlua::Result<(Table, Table)> {
        let world = lua.create_table()?;
        world.raw_set(
            "spawn",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let (x, y) = <(f64, f64)>::from_lua_multi(args, lua)?;
                let result = self.kernel.borrow_mut().spawn(Position { x, y });
                let entity = kernel_result(budget, result)?;
                match cache.get(lua, &entity) {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        // Allocation of the key, userdata, or cache entry can fail.
                        // No game code runs between spawn and publishing its handle.
                        self.kernel
                            .borrow_mut()
                            .despawn(&entity)
                            .expect("unpublished entity remains live");
                        Err(error)
                    }
                }
            })?,
        )?;
        world.raw_set(
            "despawn",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let handle = AnyUserData::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self.kernel.borrow_mut().despawn(&handle);
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "position",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let handle = AnyUserData::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self.kernel.borrow().position(&handle);
                let value = kernel_result(budget, result)?;
                pair(lua, value.x, value.y)
            })?,
        )?;
        world.raw_set(
            "velocity",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let handle = AnyUserData::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self.kernel.borrow().velocity(&handle);
                let value = kernel_result(budget, result)?;
                pair(lua, value.x, value.y)
            })?,
        )?;
        world.raw_set(
            "set_position",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let (handle, x, y) = <(AnyUserData, f64, f64)>::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self
                    .kernel
                    .borrow_mut()
                    .set_position(&handle, Position { x, y });
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "set_velocity",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let (handle, x, y) = <(AnyUserData, f64, f64)>::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self
                    .kernel
                    .borrow_mut()
                    .set_velocity(&handle, Velocity { x, y });
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "entities",
            scope.create_function(move |lua, ()| {
                self.begin(budget, false, writable)?;
                let result = self.kernel.borrow().entities();
                let entities = kernel_result(budget, result)?;
                let table = lua.create_table_with_capacity(entities.len(), 0)?;
                for (index, entity) in entities.into_iter().enumerate() {
                    table.raw_set(index + 1, cache.get(lua, &entity)?)?;
                }
                Ok(table)
            })?,
        )?;
        world.set_readonly(true);

        let input = lua.create_table()?;
        for (name, buttons) in [
            ("held", self.input.held),
            ("pressed", self.input.pressed),
            ("released", self.input.released),
        ] {
            input.raw_set(
                name,
                scope.create_function(move |_, name: LuaString| {
                    budget.check()?;
                    let button = Button::from_name(&name.to_str()?)
                        .ok_or_else(|| mlua::Error::runtime("unknown input button"))?;
                    Ok(buttons.contains(button))
                })?,
            )?;
        }
        input.set_readonly(true);
        Ok((world, input))
    }
}

fn entity(value: AnyUserData) -> mlua::Result<EntityHandle> {
    Ok(value.borrow::<EntityHandle>()?.clone())
}

fn pair(lua: &Lua, x: f64, y: f64) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.raw_set("x", x)?;
    table.raw_set("y", y)?;
    Ok(table)
}

fn kernel_result<T>(budget: &UtilityBudget<'_>, result: Result<T, KernelError>) -> mlua::Result<T> {
    if matches!(&result, Err(KernelError::EntityLimit)) {
        budget.limit(true, "entity limit exceeded")?;
    }
    result.map_err(mlua::Error::external)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::{ScriptHost, ScriptLimits, ScriptState};

    #[test]
    fn entity_cache_retains_identity_without_retaining_unused_wrappers() {
        let lua = Lua::new();
        let cache = EntityCache::new(&lua).unwrap();
        let mut a = Kernel::new();
        let mut b = Kernel::new();
        let handle_a = a.spawn(Position::default()).unwrap();
        let handle_b = b.spawn(Position::default()).unwrap();
        let value_a = cache.get(&lua, &handle_a).unwrap();
        let value_b = cache.get(&lua, &handle_b).unwrap();
        assert_ne!(value_a.to_pointer(), value_b.to_pointer());
        lua.gc_collect().unwrap();
        assert_eq!(
            value_a.to_pointer(),
            cache.get(&lua, &handle_a).unwrap().to_pointer()
        );
        // A key in a Lua table is also a strong reference to the canonical wrapper.
        let state = lua.create_table().unwrap();
        state.raw_set(value_a.clone(), "player").unwrap();
        drop(value_a);
        drop(value_b);
        lua.gc_collect().unwrap();
        let again = cache.get(&lua, &handle_a).unwrap();
        assert_eq!(state.raw_get::<String>(again.clone()).unwrap(), "player");
        assert_eq!(cache.values.pairs::<f64, AnyUserData>().count(), 1);
        drop(again);
        drop(state);
        lua.gc_collect().unwrap();
        assert_eq!(cache.values.pairs::<f64, AnyUserData>().count(), 0);
        assert!(a.position(&handle_a).is_ok());
        assert!(cache.get(&lua, &handle_a).is_ok());
    }

    #[test]
    fn reused_slots_replace_their_stale_wrapper_without_growing_the_cache() {
        let lua = Lua::new();
        let cache = EntityCache::new(&lua).unwrap();
        let mut kernel = Kernel::new();
        let first = kernel.spawn(Position::default()).unwrap();
        let stale = cache.get(&lua, &first).unwrap();
        kernel.despawn(&first).unwrap();
        let second = kernel.spawn(Position::default()).unwrap();
        // hecs reuses the slot with a new generation, so both handles pack to
        // the same cache key and the generation check is what separates them.
        assert_eq!(first.slot(), second.slot());
        assert_ne!(first, second);

        let fresh = cache.get(&lua, &second).unwrap();
        assert_ne!(stale.to_pointer(), fresh.to_pointer());
        // The live entity is still canonical on every later lookup.
        assert_eq!(
            fresh.to_pointer(),
            cache.get(&lua, &second).unwrap().to_pointer()
        );
        // The stale wrapper was replaced, not accumulated beside the new one.
        assert_eq!(cache.values.pairs::<f64, AnyUserData>().count(), 1);
        // Holding the stale wrapper never revives its entity.
        assert!(kernel.position(&first).is_err());
        assert!(kernel.position(&second).is_ok());
        assert!(stale.borrow::<EntityHandle>().is_ok());
    }

    #[test]
    fn failed_cache_publication_rolls_back_spawn_and_allows_retry() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("main.luau"),
            r#"
            return {
                init = function(ctx)
                    assert(not pcall(ctx.world.spawn, 10, 20))
                    assert(#ctx.world.entities() == 0)
                end,
                update = function(ctx)
                    local handle = ctx.world.spawn(30, 40)
                    assert(rawequal(ctx.world.entities()[1], handle))
                    assert(ctx.world.position(handle).x == 30)
                end,
            }
        "#,
        )
        .unwrap();
        let mut host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
        let mut kernel = Kernel::new();
        // Inject refusal after userdata allocation, at the final cache write.
        host.entities.values.set_readonly(true);
        host.init_in(Some(EngineContext::new(
            &mut kernel,
            InputSnapshot::default(),
        )))
        .unwrap();
        assert!(kernel.entities().unwrap().is_empty());
        host.entities.values.set_readonly(false);
        host.update_in(Some(EngineContext::new(
            &mut kernel,
            InputSnapshot::default(),
        )))
        .unwrap();
        assert_eq!(kernel.entities().unwrap().len(), 1);
        assert_eq!(host.state(), ScriptState::Running);
    }

    #[test]
    fn lua_bridge_rejects_foreign_session_handles() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("main.luau"),
            r#"
            return {init = function(ctx)
                local w = ctx.world
                local own = w.spawn(10, 20)
                assert(own ~= foreign)
                for _, read in {w.position, w.velocity} do
                    local ok, err = pcall(read, foreign)
                    assert(not ok and string.find(tostring(err), 'foreign entity handle'))
                end
                assert(not pcall(w.set_position, foreign, 99, 99))
                assert(not pcall(w.set_velocity, foreign, 99, 99))
                assert(not pcall(w.despawn, foreign))
                assert(w.position(own).x == 10 and #w.entities() == 1)
            end}
        "#,
        )
        .unwrap();
        let mut foreign_kernel = Kernel::new();
        let foreign = foreign_kernel.spawn(Position::default()).unwrap();
        let mut kernel = Kernel::new();
        let mut host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
        // Only the test harness can inject a value from a different world/VM.
        host.lua
            .globals()
            .raw_set(
                "foreign",
                host.lua.create_userdata(foreign.clone()).unwrap(),
            )
            .unwrap();
        host.init_in(Some(EngineContext::new(
            &mut kernel,
            InputSnapshot::default(),
        )))
        .unwrap();
        assert_eq!(host.state(), ScriptState::Running);
        assert_eq!(
            foreign_kernel.position(&foreign).unwrap(),
            Position::default()
        );
        assert_eq!(kernel.snapshot().unwrap()[0].position.x, 10.0);
    }
}
