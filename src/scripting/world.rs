//! Callback-scoped kernel bindings. No VM work happens while a kernel is borrowed.

use super::tilemap;
use super::utilities::UtilityBudget;
use crate::{
    collision::CollisionError,
    input::{Button, InputSnapshot},
    kernel::{
        ColliderPlacement, EntityHandle, Kernel, KernelError, Position, TileMapHandle, Velocity,
    },
    tilemap::{MAX_REGION_CELLS, TileMap},
};
use mlua::{
    AnyUserData, FromLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Scope, Table, UserData,
    UserDataMethods, Value,
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

/// A map handle a script holds, compared by session, slot and generation.
///
/// `Eq` is the whole surface: a handle names a map and is passed back, and
/// nothing a script can do to one changes what it names. In particular the
/// comparison is the same [`PartialEq`] the kernel resolves against, so a
/// handle read back from `tile_collider` equals the one `create_tilemap`
/// returned, and a handle to a removed map equals nothing live - including a
/// later map that reused its slot, which differs by generation.
///
/// Deliberately without the canonical-wrapper cache [`EntityCache`] gives
/// entities. That cache exists so a wrapper can be a *table key*, which `__eq`
/// cannot provide: Lua hashes userdata by identity and never consults the
/// metamethod. So `{[map_a] = …}` keyed by two separately obtained wrappers for
/// one map has two entries, while `map_a == map_b` is true. The limitation is
/// recorded rather than left to be discovered; a game indexing by room should
/// key on its own room number, which it has and the engine does not.
impl UserData for TileMapHandle {
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

/// Owned tile IDs one callback may receive from `tiles_region`.
///
/// The per-call cap belongs to the map; this aggregate lives here for the same
/// reason the 4,096 world-call attempts do. It bounds what crosses into the VM,
/// which is a property of the callback rather than of the map, and the kernel
/// cannot see a callback. The tile-work ceiling goes the other way and is
/// enforced inside the kernel, because only the kernel can stop a single call
/// part-way through the work it is doing.
const REGION_ID_LIMIT: u64 = 262_144;

pub(crate) struct EngineContext<'a> {
    kernel: RefCell<&'a mut Kernel>,
    input: InputSnapshot,
    calls: Cell<usize>,
    region_ids: Cell<u64>,
    #[cfg(feature = "native-plugins")]
    pub(super) plugins: Option<&'a mut crate::plugins::PluginSet>,
}

impl<'a> EngineContext<'a> {
    pub(crate) fn new(kernel: &'a mut Kernel, input: InputSnapshot) -> Self {
        // Exactly one context is built per callback, so this is the boundary
        // the kernel's per-callback tile-work ceiling is measured from.
        //
        // That equivalence is the load-bearing part, and it lives here rather
        // than in `begin_callback`: a future context built for something that is
        // *not* a callback - a tool, a bridge, a fixture - would silently
        // restart the accounting and hand it a fresh ceiling. If one is ever
        // needed, it needs a constructor that does not do this.
        kernel.begin_callback();
        Self {
            kernel: RefCell::new(kernel),
            input,
            calls: Cell::new(0),
            region_ids: Cell::new(0),
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

    /// Charge a region request before either allocation: the kernel's owned
    /// copy and the Lua array it becomes.
    ///
    /// A refused request is charged like an accepted one, so a repeatedly
    /// refused read is bounded rather than free to issue. It is charged for what
    /// it could have returned, not for what it asked for: no call can return
    /// more than [`MAX_REGION_CELLS`], so charging a 600x600 request for 360,000
    /// IDs would charge for output that was never possible - and would exhaust
    /// this ceiling on its own, turning an ordinary out-of-bounds argument into
    /// a latched session fault. Bounds errors stay catchable.
    fn region_output(&self, budget: &UtilityBudget<'_>, ids: u64) -> mlua::Result<()> {
        let charged = ids.min(u64::from(MAX_REGION_CELLS));
        self.region_ids
            .set(self.region_ids.get().saturating_add(charged));
        budget.limit(
            self.region_ids.get() > REGION_ID_LIMIT,
            "region output limit exceeded",
        )
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
        self.bind_tilemap(scope, budget, writable, &world)?;
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

impl EngineContext<'_> {
    /// One description table, validated and copied into an owned candidate.
    ///
    /// Shared by every call that installs a map, so the schema cannot fork into
    /// one per entry point - the failure M2-6 has to avoid, since a description
    /// accepted by `create_tilemap` and refused by `replace_tilemap` would be a
    /// difference no contract states.
    ///
    /// Copying charges the callback's tile-work ceiling as it goes. The borrow
    /// lives for the length of one addition and is released before the next
    /// element is read, which is what the no-borrow-across-conversion rule asks
    /// for: it forbids holding a borrow while the VM runs, not charging between
    /// batches.
    fn candidate(&self, desc: &Table, budget: &UtilityBudget<'_>) -> mlua::Result<TileMap> {
        let info = tilemap::description_info(desc)?;
        let mut charge = |units: u64| {
            let result = self.kernel.borrow_mut().charge_callback_work(units);
            kernel_result(budget, result)
        };
        let solids = tilemap::solid_flags(desc, budget, &mut charge)?;
        let cells = tilemap::cell_ids(desc, info.cell_count(), budget, &mut charge)?;
        // The candidate is complete and owned before the kernel sees it, so a
        // refused install cannot leave a partial map behind.
        TileMap::new(info, solids, cells).map_err(mlua::Error::external)
    }

    /// The map and collider calls, beneath the same `ctx.world` table and
    /// sharing its attempt budget (T7).
    ///
    /// Every one of them converts its arguments to owned Rust values first,
    /// borrows the kernel for the call alone, and builds any output afterwards,
    /// so no kernel borrow is ever live across VM conversion, VM allocation or
    /// a deadline check.
    ///
    /// M1's nine are M2-6's nine plus the three lifecycle names it splits
    /// `set_tilemap` and `clear_tilemap` into; the count is left out of this
    /// sentence deliberately, because a number here is a second place to keep
    /// in step with the table below and the migration table in M2-6.
    fn bind_tilemap<'s>(
        &'s self,
        scope: &'s Scope<'s, '_>,
        budget: &'s UtilityBudget<'_>,
        writable: bool,
        world: &Table,
    ) -> mlua::Result<()> {
        world.raw_set(
            "set_tilemap",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let desc = Table::from_lua_multi(args, lua)?;
                let map = self.candidate(&desc, budget)?;
                // The handle is discarded: this call is M1's and addresses the
                // implicit map. `create_tilemap` below is its M2-6 successor and
                // the two coexist only until this phase finishes migrating the
                // surface; a shipped surface carrying both would be exactly the
                // "two ways to name a map" M2-6 forbids.
                let result = self.kernel.borrow_mut().set_tilemap(map).map(|_| ());
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "create_tilemap",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let desc = Table::from_lua_multi(args, lua)?;
                let map = self.candidate(&desc, budget)?;
                let result = self.kernel.borrow_mut().create_tilemap(map);
                let handle = kernel_result(budget, result)?;
                // The first map call that mutates and *then* allocates
                // something to publish, which is `spawn`'s shape and needs
                // `spawn`'s rollback: a failure here would leave a map holding
                // a slot and its cells against both budgets with no handle
                // anywhere that names it, unremovable for the life of the
                // session. No game code runs between the insert and this.
                //
                // `only_create_tilemap_returns_a_value_that_could_fail_to_allocate`
                // in `tests/script_tilemap.rs` is the test that says which map
                // calls have this shape, and it named this one before it was
                // written.
                //
                // **Structurally unexercised, and measured rather than
                // assumed.** `src/scripting/assets.rs` records that an
                // unexercised rollback is not behavioural proof, and both
                // rollbacks that satisfy it are injected at a *table write*
                // after the userdata already allocated - a seam this call does
                // not have, because publishing the handle is its last step.
                // The candidate seam was `Lua::set_memory_limit`, which
                // `src/scripting.rs` already calls. It does not work: with
                // 503,176 bytes in use, a limit of 491 still let every
                // `create_userdata` here succeed, including the first one in
                // the VM's life, which also builds the metatable. The limit is
                // enforced - a 400,000-element table under the same limit fails
                // with "not enough memory" - so small allocations are served
                // from Luau's existing pool without the allocator being asked.
                // Giving this call a wrapper cache purely to manufacture that
                // seam would buy the proof with a second weak table, a session
                // vector and a staleness invariant, which is the larger debt.
                // The rule says an unexercised rollback is not proof; it does
                // not say build machinery until it is.
                //
                // **What makes this note stale, so it is a claim and not a
                // permission.** A record of a measured gap invites the next
                // reader not to re-measure, which is wrong the moment the
                // ground moves - so here is the move to watch for: *if any
                // fallible step is ever added after the `create_userdata`
                // below, the seam appears for free and this note is void.*
                // That is exactly what separates this call from `spawn`, whose
                // rollback is exercised by injecting at the cache write that
                // follows its allocation, and it is checkable by reading the
                // twenty lines under it.
                match lua.create_userdata(handle.clone()) {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        self.kernel
                            .borrow_mut()
                            .remove_tilemap(&handle)
                            .expect("an unpublished map has no members");
                        Err(error)
                    }
                }
            })?,
        )?;
        world.raw_set(
            "replace_tilemap",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let (handle, desc) = <(AnyUserData, Table)>::from_lua_multi(args, lua)?;
                let handle = map_handle(handle)?;
                // The description is copied and charged before the handle is
                // resolved, so replacing through a stale handle pays for the
                // description it built and then refuses. That ordering is
                // deliberate and is not the kernel's: `Kernel::replace_map`
                // admits before it walks the outgoing map's members, because
                // there it has both in hand. Here, refusing first would mean
                // probing validity through a second kernel call and classifying
                // its refusals separately, and it would protect nothing - the
                // same description costs the same through `create_tilemap`,
                // which has no handle to check.
                let map = self.candidate(&desc, budget)?;
                let result = self.kernel.borrow_mut().replace_tilemap(&handle, map);
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "remove_tilemap",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let handle = AnyUserData::from_lua_multi(args, lua)?;
                let handle = map_handle(handle)?;
                let result = self.kernel.borrow_mut().remove_tilemap(&handle);
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "clear_tilemap",
            scope.create_function(move |_, ()| {
                self.begin(budget, true, writable)?;
                let result = self.kernel.borrow_mut().clear_tilemap();
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "tilemap_info",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let map = AnyUserData::from_lua_multi(args, lua)?;
                let map = map_handle(map)?;
                // No `nil` arm any more, and the absence is the migration: M1
                // answered `nil` for "no map is installed", which was a
                // question about the session. A handle is a question about one
                // map, and every way it can fail to name a live one is a
                // refusal rather than an absence.
                let result = self.kernel.borrow().tilemap_info(&map);
                let info = kernel_result(budget, result)?;
                tilemap::info_table(lua, info)
            })?,
        )?;
        world.raw_set(
            "tile",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let (map, column, row) = <(AnyUserData, Value, Value)>::from_lua_multi(args, lua)?;
                let map = map_handle(map)?;
                let column = tilemap::index(&column, "column")?;
                let row = tilemap::index(&row, "row")?;
                let result = self.kernel.borrow().tile(&map, column, row);
                Ok(f64::from(kernel_result(budget, result)?))
            })?,
        )?;
        world.raw_set(
            "tile_solid",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let (map, column, row) = <(AnyUserData, Value, Value)>::from_lua_multi(args, lua)?;
                let map = map_handle(map)?;
                let column = tilemap::index(&column, "column")?;
                let row = tilemap::index(&row, "row")?;
                // The one call that accepts indices outside the grid, because
                // outside *the named map* is solid (T3). M2-3: the sentence is
                // M1's and the meaning is narrower, since another live map may
                // cover the same world coordinates and this answers nothing
                // about it.
                let result = self.kernel.borrow().tile_solid(&map, column, row);
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "tiles_region",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let (map, column, row, columns, rows) =
                    <(AnyUserData, Value, Value, Value, Value)>::from_lua_multi(args, lua)?;
                let map = map_handle(map)?;
                let column = tilemap::index(&column, "column")?;
                let row = tilemap::index(&row, "row")?;
                let columns = tilemap::extent(&columns, "columns")?;
                let rows = tilemap::extent(&rows, "rows")?;
                self.region_output(budget, u64::from(columns) * u64::from(rows))?;
                let result = self
                    .kernel
                    .borrow()
                    .tiles_region(&map, column, row, columns, rows);
                let ids = kernel_result(budget, result)?;
                tilemap::region_table(lua, &ids, budget)
            })?,
        )?;
        world.raw_set(
            "set_tile",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                let (map, column, row, id) =
                    <(AnyUserData, Value, Value, Value)>::from_lua_multi(args, lua)?;
                let map = map_handle(map)?;
                let column = tilemap::index(&column, "column")?;
                let row = tilemap::index(&row, "row")?;
                let id = tilemap::tile_id(&id)?;
                let result = self.kernel.borrow_mut().set_tile(&map, column, row, id);
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "set_tile_collider",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, true, writable)?;
                // An absent second argument is indistinguishable from nil in
                // Lua, so both remove the collider.
                let (handle, options) = <(AnyUserData, Option<Table>)>::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let placement = options
                    .as_ref()
                    .map(tilemap::placement)
                    .transpose()?
                    .map(|(map, collider)| Ok::<_, mlua::Error>((map_handle(map)?, collider)))
                    .transpose()?;
                // The script names the map, so nothing is resolved here. The
                // implicit fallback this replaces was the one thing M2-6
                // forbids by name: it would have worked until a game created a
                // second map and then changed what existing calls meant.
                //
                // `position` is `None` at every attachment. M2-5's transfer is
                // the only thing that sets it, and it arrives with its own
                // call rather than as optional fields here - a placement that
                // could carry a position would make every transfer restate the
                // box, and a mis-restated box lands in a solid cell only
                // sometimes, which refuses intermittently and looks tested.
                let result = self.kernel.borrow_mut().set_tile_collider(
                    &handle,
                    placement.map(|(map, collider)| ColliderPlacement {
                        map,
                        collider,
                        position: None,
                    }),
                );
                kernel_result(budget, result)
            })?,
        )?;
        world.raw_set(
            "tile_collider",
            scope.create_function(move |lua, args: MultiValue| {
                self.begin(budget, false, writable)?;
                let handle = AnyUserData::from_lua_multi(args, lua)?;
                let handle = entity(handle)?;
                let result = self.kernel.borrow().tile_collider(&handle);
                match kernel_result(budget, result)? {
                    // The map is dropped here and returned alongside the box in
                    // Phase 3, which is where M2-6 puts it. Until the script
                    // can name a map, a handle it could read would have nothing
                    // to be passed to.
                    Some((_, collider)) => {
                        Ok(Value::Table(tilemap::collider_table(lua, &collider)?))
                    }
                    None => Ok(Value::Nil),
                }
            })?,
        )?;
        Ok(())
    }
}

fn entity(value: AnyUserData) -> mlua::Result<EntityHandle> {
    Ok(value.borrow::<EntityHandle>()?.clone())
}

/// A map handle argument.
///
/// Passing an entity where a map belongs fails here, in the VM's type check,
/// rather than reaching the kernel - which is the right place for it, because
/// the kernel cannot tell a wrong *kind* of handle from a stale one and would
/// have to answer `InvalidTileMap` to both. M2-1's foreign handle is the
/// session check inside the kernel and is a different refusal from this one.
fn map_handle(value: AnyUserData) -> mlua::Result<TileMapHandle> {
    Ok(value.borrow::<TileMapHandle>()?.clone())
}

fn pair(lua: &Lua, x: f64, y: f64) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.raw_set("x", x)?;
    table.raw_set("y", y)?;
    Ok(table)
}

/// Convert a kernel result, latching the failures that are budget exhaustion.
///
/// Everything else stays an ordinary catchable error. The distinction is the
/// plan's: schema, geometry, bounds, missing map, bad handle, wrong phase and
/// overlapping placement are recoverable call errors, while an exhausted
/// aggregate ceiling latches outside `pcall` so a script cannot spend the rest
/// of its callback discovering the limit one refusal at a time.
/// **Exhaustive on purpose, with no `_` arm.** A catch-all made the default for
/// a new `KernelError` "ordinary catchable error", and got that wrong silently:
/// M2 added `TileMapLimit` and `AggregateCellLimit`, which M2-8 makes *latching*,
/// and they fell through it from the moment they existed. Nothing would have
/// said so - not a compile error, not a failing test, not a visible diff - and
/// the behaviour is a script `pcall`-ing past a budget ceiling and continuing to
/// spend, which is the one thing latching exists to prevent. They are unreachable
/// from Lua until Phase 3 gives a script a way to create a map, which is exactly
/// how long the defect would have stayed invisible.
///
/// Listing every variant converts that into a compile error: a new one cannot be
/// added without someone deciding which side of the line it is on. The cost is
/// one arm per variant; the alternative was a Phase 3 obligation nobody could be
/// reminded of. Found by the review session.
fn kernel_result<T>(budget: &UtilityBudget<'_>, result: Result<T, KernelError>) -> mlua::Result<T> {
    let exhausted = match &result {
        Ok(_) => None,
        // Latching: an exhausted ceiling that a script must not be able to
        // discover one refusal at a time.
        Err(KernelError::EntityLimit) => Some("entity limit exceeded"),
        Err(KernelError::ColliderLimit) => Some("collider limit exceeded"),
        Err(KernelError::TileMapLimit) => Some("map limit exceeded"),
        Err(KernelError::AggregateCellLimit) => Some("aggregate cell limit exceeded"),
        // Only a callback entry point reaches here; the fixed pass has its own
        // budget and faults the systems instead.
        Err(KernelError::Collision(CollisionError::Work)) => Some("tile work limit exceeded"),
        // Catchable: schema, geometry, bounds, handles, phase and placement are
        // recoverable call errors, and a script may `pcall` any of them.
        //
        // **M2-8 lists five latching failures and only four are arms above.**
        // The fifth, region output, latches upstream in `region_output`, which
        // charges at the call site before the kernel is reached - the aggregate
        // is a property of the callback and the kernel cannot see one. So a
        // reader holding M2-8 beside this match sees four of five and a gap
        // that is not there. The edit that gap invites is latching
        // `TileMap(TileMapError::Region)`, which would latch the **per-call**
        // 4,096 cap: an ordinary out-of-bounds argument, deliberately catchable,
        // and one the compiler cannot object to because the arm already exists
        // and would only move. Now that the match is total it looks complete,
        // which is exactly when that mistake becomes available.
        Err(KernelError::Inactive)
        | Err(KernelError::InvalidHandle)
        | Err(KernelError::Nonfinite)
        | Err(KernelError::NoTileMap)
        | Err(KernelError::InvalidTileMap)
        | Err(KernelError::TileMap(_))
        | Err(KernelError::Collision(_))
        | Err(KernelError::CollidersAttached)
        | Err(KernelError::Capacity) => None,
        // Never reaches here: an unpaired collider faults the fixed pass, which
        // has its own path. Classified anyway because the match is exhaustive,
        // and it is not budget exhaustion.
        Err(KernelError::UnpairedCollider) => None,
    };
    if let Some(message) = exhausted {
        budget.limit(true, message)?;
    }
    result.map_err(mlua::Error::external)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::{ScriptHost, ScriptLimits, ScriptState};
    use std::time::Duration;

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

    /// A one-cell map, which is the smallest thing the table will admit.
    fn one_cell() -> TileMap {
        TileMap::new(
            crate::tilemap::TileMapInfo {
                columns: 1,
                rows: 1,
                tile_width: 8,
                tile_height: 8,
                origin_x: 0,
                origin_y: 0,
            },
            vec![false],
            vec![0],
        )
        .expect("a one-cell map is well formed")
    }

    /// Two wrappers for one map, which is the only shape `__eq` exists for.
    ///
    /// **No script-level fixture can reach this yet, and that is worth stating
    /// rather than discovering.** Every wrapper a script can hold today came
    /// from its own `create_tilemap` call, so raw identity already separates two
    /// maps and already agrees with the metamethod on one:
    /// `a_script_holds_two_map_handles_across_callbacks_and_removes_them_one_at_a_time`
    /// passes with `__eq` deleted. What makes a second wrapper for a *live* map
    /// reachable from Lua is `tile_collider` returning the map (M2-6), and until
    /// that lands this is the only thing that says the metamethod works.
    #[test]
    fn two_wrappers_for_one_map_compare_equal_while_staying_distinct_values() {
        let lua = Lua::new();
        let mut kernel = Kernel::new();
        let first = kernel.create_tilemap(one_cell()).unwrap();
        let other = kernel.create_tilemap(one_cell()).unwrap();
        let entity = kernel.spawn(Position::default()).unwrap();
        let globals = lua.globals();
        for (name, value) in [
            ("a", lua.create_userdata(first.clone()).unwrap()),
            ("again", lua.create_userdata(first).unwrap()),
            ("b", lua.create_userdata(other).unwrap()),
            ("body", lua.create_userdata(entity).unwrap()),
        ] {
            globals.set(name, value).unwrap();
        }
        let ask = |source: &str| lua.load(source).eval::<bool>().unwrap();
        assert!(
            !ask("return rawequal(a, again)"),
            "two wrappers, two values"
        );
        assert!(ask("return a == again"), "and one map");
        assert!(!ask("return a == b"), "two maps never compare equal");
        // Both operands are userdata, so Luau consults `__eq` and the borrow
        // inside it is what refuses. A metamethod that answered on the pointer
        // alone would agree here and disagree two lines above.
        assert!(!ask("return a == body"), "a map is not an entity");
        assert!(!ask("return body == a"), "in either order");
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

    /// The deadline observed between conversion batches, witnessed by what a
    /// refusal leaves behind.
    ///
    /// This is the only place the check is observable at all. A latched
    /// deadline is terminal, so through `GameRuntime` the session faults either
    /// way and its kernel is stopped; here the kernel outlives the failed
    /// callback, so "the copy stopped before publishing" and "the copy finished
    /// and installed a map" are two different observable states. Remove the
    /// check in `dense` and the map below is installed.
    ///
    /// The timing is a premise, not the conclusion, and it is asserted rather
    /// than assumed: the fixture requires the fault to actually be the deadline.
    /// If a machine ever copies a quarter of a million elements inside the
    /// budget, this fails loudly instead of passing without having tested
    /// anything.
    #[test]
    fn a_description_copy_stops_at_the_deadline_without_installing_a_map() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("main.luau"),
            r#"
            local description
            return {
                init = function()
                    local cells = {}
                    for index = 1, 512 * 512 do cells[index] = 0 end
                    description = {
                        columns = 512, rows = 512, tile_width = 1, tile_height = 1,
                        origin_x = 0, origin_y = 0, solids = {true}, cells = cells,
                    }
                end,
                update = function(ctx) ctx.world.set_tilemap(description) end,
            }
        "#,
        )
        .unwrap();
        let mut host = ScriptHost::load(
            root.path(),
            ScriptLimits {
                startup_timeout: Duration::from_secs(120),
                callback_timeout: Duration::from_millis(2),
                ..ScriptLimits::default()
            },
        )
        .unwrap();
        let mut kernel = Kernel::new();
        host.init_in(Some(EngineContext::new(
            &mut kernel,
            InputSnapshot::default(),
        )))
        .unwrap();
        let error = host
            .update_in(Some(EngineContext::new(
                &mut kernel,
                InputSnapshot::default(),
            )))
            .unwrap_err();
        assert!(
            error.message.contains("script deadline exceeded"),
            "the copy must outlast a 2 ms budget for this to test anything: {}",
            error.message
        );
        assert!(
            kernel.tilemap().unwrap().is_none(),
            "the copy stopped at a batch boundary rather than publishing a map"
        );
        // **How far it got is the machine's; that it stopped on a boundary is
        // the code's, and only the second is asserted.**
        //
        // `dense` observes the deadline and *then* charges, once per batch, so a
        // refused copy has paid for the one-element solids array plus a whole
        // number of complete cell batches and nothing else. The upper bound is
        // what says it stopped rather than finished and declined to publish; a
        // completed 512 x 512 charges 262,145.
        //
        // This asserted `charged > 1`, which additionally required at least one
        // cell batch to fit inside 2 ms. That is a property of the machine, not
        // of the code: under load the deadline fires at the first check and
        // `charged` is 1, the solids alone, and the test fails having found
        // nothing wrong. The review session measured it failing twice in six
        // runs of the lib suite and reported the value; I could not reproduce it
        // in forty runs here, which is what a load-dependent flake looks like
        // from the machine that does not have the load. The old comment carried
        // "observed around 20,737, or batch 81 of 1,024" - one run's figure,
        // written where it reads as something the data fixes.
        //
        // Pre-existing: this test is unchanged since 7c66540. It is repaired
        // here rather than left because this commit moved the charging it
        // measures into the shared `candidate`.
        let charged = kernel.callback_work();
        assert!(
            (charged - 1).is_multiple_of(tilemap::CONVERSION_BATCH as u64) && charged < 262_145,
            "a refused copy must stop on a batch boundary short of the whole map: {charged}"
        );
        assert_eq!(host.state(), ScriptState::Faulted);
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
                -- The collider calls take identity from the same handles, so a
                -- foreign session's entity is refused there too.
                local ok, err = pcall(w.tile_collider, foreign)
                assert(not ok and string.find(tostring(err), 'foreign entity handle'))
                assert(not pcall(w.set_tile_collider, foreign, nil))
                assert(not pcall(w.set_tile_collider, foreign,
                    {offset_x = 0, offset_y = 0, width = 8, height = 8}))
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
