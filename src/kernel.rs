//! Graphics-independent entity storage and fixed-step systems.

use crate::collision::{
    self, CollisionError, MAX_CALLBACK_WORK, MAX_FIXED_PASS_WORK, MAX_LIVE_COLLIDERS, TileCollider,
    WorkBudget,
};
use crate::maps::{MapTable, MapTableError, TileMapId};
use crate::tilemap::{Axis, TileMap, TileMapError, TileMapInfo};
use hecs::{Entity, World};
use std::{fmt, rc::Rc};

pub const FIXED_DT: f64 = 1.0 / 60.0;
pub const ENTITY_LIMIT: u32 = 16_384;

/// World pixels, independent of tile coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// World pixels per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Velocity {
    pub x: f64,
    pub y: f64,
}

/// An opaque session identity and generation-checked entity reference.
#[derive(Clone, Debug)]
pub struct EntityHandle {
    session: Rc<()>,
    entity: Entity,
}

impl PartialEq for EntityHandle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.session, &other.session) && self.entity == other.entity
    }
}
impl Eq for EntityHandle {}

impl EntityHandle {
    /// Session marker for private VM caches, never a persisted/public ID.
    #[cfg(feature = "scripting")]
    pub(crate) fn session(&self) -> &Rc<()> {
        &self.session
    }

    /// The hecs slot, unique among the live entities of one session. Generation
    /// is deliberately excluded: a cache keyed by slot detects a reused slot by
    /// comparing handles, so stale wrappers are replaced instead of accumulating.
    #[cfg(feature = "scripting")]
    pub(crate) fn slot(&self) -> u32 {
        self.entity.id()
    }
}

/// An opaque session identity and generation-checked map reference.
///
/// The session marker is what a bare [`TileMapId`] cannot carry: two kernels
/// that each hold one map name it identically, so without the `Rc` a foreign
/// handle would resolve against whatever occupies that slot here (M2-1).
#[derive(Clone, Debug)]
pub struct TileMapHandle {
    session: Rc<()>,
    id: TileMapId,
}

impl PartialEq for TileMapHandle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.session, &other.session) && self.id == other.id
    }
}
impl Eq for TileMapHandle {}

impl TileMapHandle {
    /// The table slot, unique among the live maps of one session.
    ///
    /// Crate-visible, exactly as [`EntityHandle::slot`] is. M2-1 makes slot
    /// *reuse* a contract clause about the allocator, which a fixture has to
    /// read to prove it forced one; it does not make the slot *number*
    /// something a game observes. A game sees reuse only as a stale handle
    /// refusing, so this has no caller outside a fixture and is compiled for
    /// one.
    #[cfg(test)]
    pub(crate) fn slot(&self) -> u32 {
        self.id.slot()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntitySnapshot {
    pub entity: EntityHandle,
    pub position: Position,
    pub velocity: Velocity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    Inactive,
    InvalidHandle,
    Nonfinite,
    EntityLimit,
    /// A map operation on a session that has none installed.
    ///
    /// Retires in Phase 3 with the implicit map itself (M2-8); until then it is
    /// what the un-migrated surface still answers.
    NoTileMap,
    /// A stale, removed or foreign map handle (M2-8).
    InvalidTileMap,
    /// More live maps than [`MAX_TILEMAPS`](crate::maps::MAX_TILEMAPS).
    TileMapLimit,
    /// More cells across live maps than
    /// [`MAX_AGGREGATE_CELLS`](crate::maps::MAX_AGGREGATE_CELLS).
    AggregateCellLimit,
    TileMap(TileMapError),
    Collision(CollisionError),
    /// An operation that requires no attached collider found one (T6).
    CollidersAttached,
    ColliderLimit,
    /// The integration scratch could not be reserved.
    Capacity,
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive => f.write_str("world session is stopped or faulted"),
            Self::InvalidHandle => f.write_str("stale or foreign entity handle"),
            Self::Nonfinite => f.write_str("position and velocity must remain finite"),
            Self::EntityLimit => f.write_str("entity limit exceeded"),
            Self::NoTileMap => f.write_str("no tile map is installed"),
            Self::InvalidTileMap => f.write_str("stale or foreign tile map handle"),
            Self::TileMapLimit => MapTableError::Limit.fmt(f),
            Self::AggregateCellLimit => MapTableError::AggregateCells.fmt(f),
            Self::TileMap(error) => error.fmt(f),
            Self::Collision(error) => error.fmt(f),
            Self::CollidersAttached => {
                f.write_str("the map cannot be cleared while a collider is attached")
            }
            Self::ColliderLimit => f.write_str("collider limit exceeded"),
            Self::Capacity => f.write_str("could not reserve collision scratch"),
        }
    }
}
impl std::error::Error for KernelError {}

impl From<TileMapError> for KernelError {
    fn from(error: TileMapError) -> Self {
        Self::TileMap(error)
    }
}

impl From<CollisionError> for KernelError {
    fn from(error: CollisionError) -> Self {
        Self::Collision(error)
    }
}

impl KernelError {
    /// Translate rather than wrap, so `MapTableError` stays inside the crate
    /// and M2-8's three refusals are named at this layer.
    fn from_table(error: MapTableError) -> Self {
        match error {
            MapTableError::Limit => Self::TileMapLimit,
            MapTableError::AggregateCells => Self::AggregateCellLimit,
            MapTableError::Invalid => Self::InvalidTileMap,
        }
    }
}

/// One body's sweep inputs, and after the sweep its resolved position.
///
/// Only colliders need scratch. An entity in free flight recomputes
/// `position + velocity * FIXED_DT` identically in the commit pass, which is
/// what today's allocation-free integration already relies on, so buffering it
/// would cost 393,216 bytes to store a value that is cheaper to recompute.
#[derive(Clone, Copy)]
struct Candidate {
    entity: Entity,
    position: Position,
    velocity: Velocity,
    collider: TileCollider,
}

/// Engine-owned world. No ECS references or query borrows escape this API.
pub struct Kernel {
    world: World,
    session: Rc<()>,
    active: bool,
    /// Every live map (M2-1). Dense storage outside hecs, so a static tile is
    /// never an entity and a map is never a game entity.
    maps: MapTable,
    /// The map M1's implicit-map API addresses, and the one map colliders may
    /// be members of until Phase 2 gives them membership of their own.
    ///
    /// **The Phase 1 invariant is that every live collider is a member of
    /// `current`**, and it holds structurally: attaching needs `current`,
    /// `current` changes only by an in-place replacement that keeps its
    /// identity, and removing it is refused while any collider exists. That is
    /// what lets M2-R1 be *specialised* here rather than deferred - a map that
    /// is not `current` has no members, so removing it, replacing it and
    /// editing its cells are all correctly map-local today. Phase 2 replaces
    /// this field with a per-entity component and the three rules read the
    /// same; Phase 3 retires it with the rest of the implicit-map surface.
    current: Option<TileMapId>,
    /// Live `TileCollider` components, tracked alongside hecs so the limit and
    /// the T6 clear restriction do not cost a query.
    colliders: u32,
    /// Reusable sweep scratch, reserved only once a collider exists.
    bodies: Vec<Candidate>,
    /// Cell-visit units charged by map and collider calls in this callback,
    /// against [`MAX_CALLBACK_WORK`].
    callback_work: u64,
    /// Cell-visit units the most recent fixed pass charged, for stress runs.
    fixed_work: u64,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel {
    pub fn new() -> Self {
        Self {
            world: World::new(),
            session: Rc::new(()),
            active: true,
            maps: MapTable::new(),
            current: None,
            colliders: 0,
            bodies: Vec::new(),
            callback_work: 0,
            fixed_work: 0,
        }
    }

    /// Invalidate all handles for subsequent world access. Cannot be restarted.
    ///
    /// Map storage is released here rather than left to drop: a stopped session
    /// can outlive its last callback while a host tears down, and half a megabyte
    /// of cells has no reader once map access is inactive.
    pub fn stop(&mut self) {
        self.active = false;
        // Generations are deliberately not advanced: `require_active` runs
        // first at every entry point, so a handle to a live map refuses
        // `Inactive` here where the same handle to a removed map in a live
        // session refuses `InvalidTileMap` (M2-1). Dropping the table is for
        // storage, exactly as M1 dropped its single map.
        self.maps.clear();
        self.current = None;
        self.bodies = Vec::new();
        // Zeroed with the rest, so all three accounting accessors agree that a
        // stopped session holds nothing. Nothing can attach or detach after
        // this, so the count cannot go out of step with the components again.
        self.colliders = 0;
    }

    fn require_active(&self) -> Result<(), KernelError> {
        if self.active {
            Ok(())
        } else {
            Err(KernelError::Inactive)
        }
    }

    fn validate(&self, handle: &EntityHandle) -> Result<Entity, KernelError> {
        self.require_active()?;
        if !Rc::ptr_eq(&self.session, &handle.session) || !self.world.contains(handle.entity) {
            return Err(KernelError::InvalidHandle);
        }
        Ok(handle.entity)
    }

    fn handle(&self, entity: Entity) -> EntityHandle {
        EntityHandle {
            session: self.session.clone(),
            entity,
        }
    }

    pub fn spawn(&mut self, position: Position) -> Result<EntityHandle, KernelError> {
        self.require_active()?;
        finite(position.x, position.y)?;
        if self.world.len() >= ENTITY_LIMIT {
            return Err(KernelError::EntityLimit);
        }
        let entity = self.world.spawn((position, Velocity::default()));
        Ok(self.handle(entity))
    }

    /// Remove an entity, releasing its collider capacity if it had one.
    pub fn despawn(&mut self, entity: &EntityHandle) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        let had_collider = self.world.get::<&TileCollider>(entity).is_ok();
        self.world
            .despawn(entity)
            .map_err(|_| KernelError::InvalidHandle)?;
        if had_collider {
            self.colliders -= 1;
        }
        Ok(())
    }

    pub fn position(&self, entity: &EntityHandle) -> Result<Position, KernelError> {
        let entity = self.validate(entity)?;
        Ok(*self
            .world
            .get::<&Position>(entity)
            .expect("every entity has a position"))
    }

    pub fn velocity(&self, entity: &EntityHandle) -> Result<Velocity, KernelError> {
        let entity = self.validate(entity)?;
        Ok(*self
            .world
            .get::<&Velocity>(entity)
            .expect("every entity has a velocity"))
    }

    /// Move an entity immediately.
    ///
    /// T5: this stays a teleport. The path is never swept and the body is never
    /// depenetrated, but a collider's destination box must be a legal placement,
    /// so teleporting across a wall to a free cell succeeds while teleporting
    /// into one refuses.
    pub fn set_position(
        &mut self,
        entity: &EntityHandle,
        position: Position,
    ) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        finite(position.x, position.y)?;
        if let Some(collider) = self
            .world
            .get::<&TileCollider>(entity)
            .ok()
            .map(|body| *body)
        {
            // M2-R2: validated against the map the body is a member of, which
            // under the Phase 1 invariant is `current`. A destination legal on
            // some other live map is still refused - changing maps is M2-5's
            // transfer and nothing else.
            let id = self.current.ok_or(KernelError::NoTileMap)?;
            let map = self.maps.get(id).expect("current names a live map");
            let mut work = Self::remaining_work(self.callback_work);
            let placement =
                collision::check_placement(map, &collider, position.x, position.y, &mut work);
            self.callback_work = self.callback_work.saturating_add(work.used());
            placement?;
        }
        *self
            .world
            .get::<&mut Position>(entity)
            .expect("every entity has a position") = position;
        Ok(())
    }

    pub fn set_velocity(
        &mut self,
        entity: &EntityHandle,
        velocity: Velocity,
    ) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        finite(velocity.x, velocity.y)?;
        *self
            .world
            .get::<&mut Velocity>(entity)
            .expect("every entity has a velocity") = velocity;
        Ok(())
    }

    pub fn entities(&self) -> Result<Vec<EntityHandle>, KernelError> {
        self.require_active()?;
        let mut entities: Vec<_> = self
            .world
            .query::<Entity>()
            .iter()
            .map(|e| self.handle(e))
            .collect();
        entities.sort_by_key(|handle| handle.entity.id());
        Ok(entities)
    }

    /// Owned state in entity-identifier order, suitable for rendering or replay checks.
    pub fn snapshot(&self) -> Result<Vec<EntitySnapshot>, KernelError> {
        self.entities()?
            .into_iter()
            .map(|entity| {
                Ok(EntitySnapshot {
                    position: self.position(&entity)?,
                    velocity: self.velocity(&entity)?,
                    entity,
                })
            })
            .collect()
    }

    /// The map M1's implicit-map API addresses.
    ///
    /// `None` is `NoTileMap`, exactly as before. A `current` the table refuses
    /// is an invariant break rather than a refusal, so it panics rather than
    /// answering `NoTileMap`: a plausible wrong error on a surface 133 call
    /// sites depend on is worse than a loud one.
    fn map(&self) -> Result<&TileMap, KernelError> {
        self.require_active()?;
        let id = self.current.ok_or(KernelError::NoTileMap)?;
        Ok(self.maps.get(id).expect("current names a live map"))
    }

    /// Resolve a public handle: session first, then slot and generation.
    ///
    /// [`Self::require_active`] runs before either, so a stopped session
    /// refuses `Inactive` rather than `InvalidTileMap` (M2-1).
    fn validate_map(&self, handle: &TileMapHandle) -> Result<TileMapId, KernelError> {
        self.require_active()?;
        if !Rc::ptr_eq(&self.session, &handle.session) {
            return Err(KernelError::InvalidTileMap);
        }
        self.maps.get(handle.id).map_err(KernelError::from_table)?;
        Ok(handle.id)
    }

    /// Validate a complete description and install it in a fresh slot (M2-4).
    ///
    /// Admission is decided before the kernel allocates anything: the table
    /// weighs the candidate's cells against `live - replaced + candidate`
    /// before it takes a slot, so a refusal leaves the count, the storage and
    /// the free list exactly as they were. The candidate is moved rather than
    /// copied, so exactly one copy of it exists at peak.
    pub fn create_tilemap(&mut self, map: TileMap) -> Result<TileMapHandle, KernelError> {
        self.require_active()?;
        let id = self.maps.insert(map).map_err(KernelError::from_table)?;
        Ok(TileMapHandle {
            session: self.session.clone(),
            id,
        })
    }

    /// Replace one map's contents in place, keeping its handle valid (M2-4).
    pub fn replace_tilemap(
        &mut self,
        handle: &TileMapHandle,
        map: TileMap,
    ) -> Result<(), KernelError> {
        let id = self.validate_map(handle)?;
        self.replace_map(id, map)
    }

    /// Retire one map, refusing while a collider is a member of it (M2-4).
    pub fn remove_tilemap(&mut self, handle: &TileMapHandle) -> Result<(), KernelError> {
        let id = self.validate_map(handle)?;
        self.remove_map(id)
    }

    /// Owned dimensions, tile size and origin of one named map (M2-4).
    pub fn tilemap_info(&self, handle: &TileMapHandle) -> Result<TileMapInfo, KernelError> {
        let id = self.validate_map(handle)?;
        Ok(self
            .maps
            .get(id)
            .expect("a validated handle names a live map")
            .info())
    }

    /// Swap one map's contents, revalidating only *that map's* members (M2-R1).
    ///
    /// Extents are capped relative to tile size, so a body legal on the
    /// outgoing map can be illegal on the candidate even at the same position.
    /// Validation runs entirely against the local candidate, so a refusal
    /// leaves the installed map untouched and no reader can observe a partial
    /// one; the candidate arrives already schema-checked by [`TileMap::new`].
    ///
    /// Under the Phase 1 invariant a map that is not `current` has no members,
    /// so nothing is scanned and no work is charged for it - which is the
    /// map-local half of M2-R1, reachable today.
    fn replace_map(&mut self, id: TileMapId, map: TileMap) -> Result<(), KernelError> {
        // Admission first: it is O(1), and a candidate that cannot be installed
        // should not charge for walking the bodies of the map it would replace.
        self.maps
            .admits_replacement(id, map.info().cell_count())
            .map_err(KernelError::from_table)?;
        if self.current == Some(id) {
            let Self {
                world,
                callback_work,
                ..
            } = self;
            let info = map.info();
            let mut work = Self::remaining_work(*callback_work);
            let mut refusal = Ok(());
            for (position, collider) in world.query::<(&Position, &TileCollider)>().iter() {
                refusal = collider
                    .check(&info)
                    .and_then(|()| {
                        collision::check_placement(
                            &map, collider, position.x, position.y, &mut work,
                        )
                    })
                    .map_err(KernelError::from);
                if refusal.is_err() {
                    break;
                }
            }
            // Charged whether or not the swap happens, so a repeatedly refused
            // install cannot walk the map for free.
            *callback_work = callback_work.saturating_add(work.used());
            refusal?;
        }
        self.maps.replace(id, map).map_err(KernelError::from_table)
    }

    /// Retire one map, refusing while a collider is a member of it (M2-R1).
    ///
    /// Under the Phase 1 invariant every collider is a member of `current`, so
    /// a map that is not `current` has none and its removal cannot be refused
    /// by a body on another map - which is the clause M2-R1 exists to state.
    fn remove_map(&mut self, id: TileMapId) -> Result<(), KernelError> {
        if self.current == Some(id) && self.colliders > 0 {
            return Err(KernelError::CollidersAttached);
        }
        // On `?` where the five `current` resolutions are on `expect`, and the
        // difference is the message rather than the mechanism. Both callers
        // pre-validate - `clear_tilemap` passes `current`, `remove_tilemap`
        // passes an id `validate_map` has already resolved - so this cannot
        // fail from either. But `remove_map` is the one helper whose id need
        // not be `current`, so asserting "current names a live map" here would
        // state something false about half its callers, and no other wording
        // would be true of both. It stays a refusal for that reason and not by
        // residue.
        self.maps.remove(id).map_err(KernelError::from_table)?;
        if self.current == Some(id) {
            self.current = None;
        }
        Ok(())
    }

    /// Install or replace the map addressed implicitly (T6).
    ///
    /// Phase 3 retires this with the rest of the implicit-map surface. Until
    /// then it is `create` on an empty session and `replace` on a live one,
    /// which is what keeps the identity stable across a swap and the M1
    /// contract - a refused replacement leaving the installed map whole -
    /// exactly as it was.
    pub fn set_tilemap(&mut self, map: TileMap) -> Result<(), KernelError> {
        self.require_active()?;
        match self.current {
            Some(id) => self.replace_map(id, map),
            None => {
                let id = self.maps.insert(map).map_err(KernelError::from_table)?;
                self.current = Some(id);
                Ok(())
            }
        }
    }

    /// Remove the implicit map, succeeding when none is installed.
    ///
    /// T6 refuses while any collider remains attached, which under the Phase 1
    /// invariant is exactly "while a collider is a member of it".
    pub fn clear_tilemap(&mut self) -> Result<(), KernelError> {
        self.require_active()?;
        match self.current {
            Some(id) => self.remove_map(id),
            None => Ok(()),
        }
    }

    /// Owned dimensions, tile size and origin, or `None` when no map exists.
    pub fn tilemap(&self) -> Result<Option<TileMapInfo>, KernelError> {
        self.require_active()?;
        Ok(self
            .current
            .map(|id| self.maps.get(id).expect("current names a live map").info()))
    }

    pub fn tile(&self, column: i32, row: i32) -> Result<u16, KernelError> {
        Ok(self.map()?.tile(column, row)?)
    }

    /// Whether a tile blocks. Outside the installed map is solid (T3).
    pub fn tile_solid(&self, column: i32, row: i32) -> Result<bool, KernelError> {
        Ok(self.map()?.is_solid(column, row))
    }

    /// An owned row-major copy of a wholly in-bounds rectangle.
    pub fn tiles_region(
        &self,
        column: i32,
        row: i32,
        columns: u32,
        rows: u32,
    ) -> Result<Vec<u16>, KernelError> {
        Ok(self.map()?.region(column, row, columns, rows)?)
    }

    /// Change one cell immediately, refusing an edit that would trap a body (T6).
    ///
    /// Only a transition to solid can introduce overlap, so an edit that leaves
    /// the cell non-solid, or replaces one solid ID with another, skips the
    /// collider scan entirely and charges one unit.
    pub fn set_tile(&mut self, column: i32, row: i32, id: u16) -> Result<(), KernelError> {
        self.require_active()?;
        let target = self.current.ok_or(KernelError::NoTileMap)?;
        self.set_map_tile(target, column, row, id)
    }

    /// Change one cell of a named map, scanning only *that map's* members.
    ///
    /// **This is the entry point whose map argument no longer implies its
    /// collider set** (M2-R1). M1's global collider query was correct while
    /// there was one map and every collider was on it; under one shared
    /// coordinate space a body on another map standing over these world
    /// coordinates would make the edit refuse. Under the Phase 1 invariant the
    /// members of a map that is not `current` are none, so the scan is skipped
    /// entirely rather than filtered.
    fn set_map_tile(
        &mut self,
        target: TileMapId,
        column: i32,
        row: i32,
        id: u16,
    ) -> Result<(), KernelError> {
        let Self {
            world,
            maps,
            current,
            callback_work,
            ..
        } = self;
        let scan_members = *current == Some(target);
        // The fifth `current` resolution, and it takes the same treatment as
        // the other four. Production reaches this only from `set_tile` with
        // `target = current`, and Phase 3's by-handle form will reach it with a
        // target `validate_map` has already resolved, so a failure here is an
        // invariant break rather than a refusal - and surfacing it as
        // `InvalidTileMap` on M1's `set_tile` is exactly the plausible
        // substitution this rule exists to prevent.
        let map = maps.get_mut(target).expect("current names a live map");
        // Schema first: a malformed edit is refused before anything is scanned
        // or charged.
        let previous = map.tile(column, row)?;
        let becomes_solid = map.solid_definition(id)? && !map.solid_definition(previous)?;
        // Metered like every other entry point, so `MAX_CALLBACK_WORK` bounds
        // this one too rather than relying on the collider limit to do it.
        let mut work = Self::remaining_work(*callback_work);
        let mut outcome = work.charge(1).map_err(KernelError::from);
        if becomes_solid && scan_members && outcome.is_ok() {
            for (position, collider) in world.query::<(&Position, &TileCollider)>().iter() {
                // Charged before the refusal, so a repeatedly refused edit
                // cannot scan the map for free.
                if let Err(error) = work.charge(1) {
                    outcome = Err(error.into());
                    break;
                }
                if collision::overlaps_cell(map, collider, position.x, position.y, column, row) {
                    outcome = Err(CollisionError::Placement.into());
                    break;
                }
            }
        }
        *callback_work = callback_work.saturating_add(work.used());
        outcome?;
        Ok(map.set_tile(column, row, id)?)
    }

    /// Attach, replace or remove an entity's collider (T3).
    ///
    /// Attaching requires an installed map and checks every cell the box
    /// covers, not a sweep. A refusal leaves the previous collider, or its
    /// absence, exactly as it was.
    pub fn set_tile_collider(
        &mut self,
        entity: &EntityHandle,
        collider: Option<TileCollider>,
    ) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        let attached = self.world.get::<&TileCollider>(entity).is_ok();
        let Some(collider) = collider else {
            if attached {
                self.world
                    .remove_one::<TileCollider>(entity)
                    .map_err(|_| KernelError::InvalidHandle)?;
                self.colliders -= 1;
            }
            return Ok(());
        };
        if !attached && self.colliders >= MAX_LIVE_COLLIDERS {
            return Err(KernelError::ColliderLimit);
        }
        // A collider may only be attached to `current` in Phase 1: a map
        // created through the registry has no way to name itself here until
        // Phase 2's membership arrives, so attaching to one is refused rather
        // than silently resolved to the implicit map.
        let id = self.current.ok_or(KernelError::NoTileMap)?;
        let map = self.maps.get(id).expect("current names a live map");
        collider.check(&map.info())?;
        let position = *self
            .world
            .get::<&Position>(entity)
            .expect("every entity has a position");
        let mut work = Self::remaining_work(self.callback_work);
        let placement =
            collision::check_placement(map, &collider, position.x, position.y, &mut work);
        self.callback_work = self.callback_work.saturating_add(work.used());
        placement?;
        self.world
            .insert_one(entity, collider)
            .map_err(|_| KernelError::InvalidHandle)?;
        if !attached {
            self.colliders += 1;
        }
        Ok(())
    }

    /// An owned copy of an entity's collider, or `None` when it has none. The
    /// entity is validated either way.
    pub fn tile_collider(
        &self,
        entity: &EntityHandle,
    ) -> Result<Option<TileCollider>, KernelError> {
        let entity = self.validate(entity)?;
        Ok(self
            .world
            .get::<&TileCollider>(entity)
            .ok()
            .map(|body| *body))
    }

    /// Colliders currently attached, bounded by
    /// [`MAX_LIVE_COLLIDERS`](crate::collision::MAX_LIVE_COLLIDERS).
    pub fn live_colliders(&self) -> u32 {
        self.colliders
    }

    /// Live sweep scratch in bytes, for the memory accounting stress runs
    /// record. Zero until a collider exists, and after [`Self::stop`].
    pub fn collision_scratch_bytes(&self) -> usize {
        self.bodies.capacity() * size_of::<Candidate>()
    }

    /// Cell-visit units charged by map and collider calls in this callback,
    /// against [`MAX_CALLBACK_WORK`](crate::collision::MAX_CALLBACK_WORK).
    ///
    /// This is enforced, not merely observed: every entry point below budgets
    /// itself against what remains rather than against the whole ceiling, so
    /// the last call in an exhausted callback is refused instead of being
    /// allowed one more full call's worth of work. The fixed pass has its own
    /// budget and never counts here.
    pub fn callback_work(&self) -> u64 {
        self.callback_work
    }

    /// Begin a callback's tile-work accounting.
    ///
    /// The kernel cannot see callback boundaries on its own, so the caller
    /// driving them says when one starts. Nothing else resets this: a session
    /// that never calls it accumulates across its whole life and eventually
    /// refuses, which is the safe direction for a caller that forgot.
    pub fn begin_callback(&mut self) {
        self.callback_work = 0;
    }

    /// Charge tile work performed outside the kernel against this callback's
    /// ceiling, such as description elements a binding copied and validated.
    ///
    /// Charged before the refusal, exactly as [`WorkBudget`] is, so a request
    /// that is rejected after walking a quarter of a million elements still
    /// pays for them.
    pub fn charge_callback_work(&mut self, units: u64) -> Result<(), KernelError> {
        self.callback_work = self.callback_work.saturating_add(units);
        if self.callback_work > MAX_CALLBACK_WORK {
            return Err(CollisionError::Work.into());
        }
        Ok(())
    }

    /// A budget for the tile work this callback has left.
    fn remaining_work(callback_work: u64) -> WorkBudget {
        WorkBudget::new(MAX_CALLBACK_WORK.saturating_sub(callback_work))
    }

    /// Cell-visit units the most recent fixed pass charged, against
    /// [`MAX_FIXED_PASS_WORK`](crate::collision::MAX_FIXED_PASS_WORK). Zero for
    /// a pass with no collider, which visits no cells at all.
    pub fn fixed_pass_work(&self) -> u64 {
        self.fixed_work
    }

    /// The world coordinate of a tile face, for callers converting between tile
    /// and world coordinates. Refuses with `NoTileMap` when none is installed.
    pub fn tile_face(&self, axis: Axis, index: i32) -> Result<f64, KernelError> {
        Ok(self.map()?.face(axis, index))
    }

    /// The cell containing a world coordinate, saturated one cell outside the
    /// grid. Refuses with `NoTileMap` when none is installed.
    pub fn tile_at(&self, axis: Axis, world: f64) -> Result<i32, KernelError> {
        Ok(self.map()?.cell_at(axis, world))
    }

    /// Live map storage in bytes, summed across every map, for the memory
    /// accounting stress runs record. Zero with no map, including after
    /// [`Self::stop`] releases it.
    ///
    /// Cells and solid flags only, which is exactly what M1's single-map
    /// accessor reported: a session holding one map measures what it measured
    /// before the registry existed. The registry's own overhead is
    /// [`Self::tilemap_table_bytes`], kept apart so the aggregate budget stays
    /// a statement about cells.
    pub fn tilemap_storage_bytes(&self) -> usize {
        self.maps.storage_bytes()
    }

    /// The slot table's own overhead in bytes, excluding the maps themselves.
    pub fn tilemap_table_bytes(&self) -> usize {
        self.maps.table_bytes()
    }

    /// Live maps, bounded by [`MAX_TILEMAPS`](crate::maps::MAX_TILEMAPS).
    pub fn tilemap_count(&self) -> u32 {
        self.maps.len()
    }

    /// Cells across every live map, bounded by
    /// [`MAX_AGGREGATE_CELLS`](crate::maps::MAX_AGGREGATE_CELLS).
    pub fn tilemap_cells(&self) -> u32 {
        self.maps.cells()
    }

    /// Complete one fixed tick. Overflow refuses the entire integration pass.
    ///
    /// Every candidate is validated, and every collider resolved, before any
    /// position is committed, so a failure anywhere preserves the post-callback
    /// state of every entity.
    pub fn fixed_update(&mut self) -> Result<(), KernelError> {
        self.require_active()?;
        for (position, velocity) in self.world.query::<(&Position, &Velocity)>().iter() {
            finite(
                position.x + velocity.x * FIXED_DT,
                position.y + velocity.y * FIXED_DT,
            )?;
        }
        if self.colliders == 0 {
            // Today's allocation-free integration, unchanged. An installed map
            // cannot reach it: only a collider opts an entity into collision.
            for (position, velocity) in self.world.query_mut::<(&mut Position, &Velocity)>() {
                position.x += velocity.x * FIXED_DT;
                position.y += velocity.y * FIXED_DT;
            }
            return Ok(());
        }
        self.sweep_bodies()?;
        for (position, velocity, collider) in
            self.world
                .query_mut::<(&mut Position, &Velocity, Option<&TileCollider>)>()
        {
            if collider.is_none() {
                position.x += velocity.x * FIXED_DT;
                position.y += velocity.y * FIXED_DT;
            }
        }
        for candidate in &self.bodies {
            *self
                .world
                .get::<&mut Position>(candidate.entity)
                .expect("a swept body was live when it was collected") = candidate.position;
        }
        Ok(())
    }

    /// Resolve every collider against the map, leaving results in `self.bodies`.
    ///
    /// Nothing is published here. A refused sweep, an exhausted work budget or
    /// a violated clamp invariant returns before the caller commits anything.
    fn sweep_bodies(&mut self) -> Result<(), KernelError> {
        let Self {
            world,
            maps,
            current,
            bodies,
            fixed_work,
            ..
        } = self;
        *fixed_work = 0;
        // A collider cannot be attached without a map, and the map cannot be
        // removed while one is attached, so this is defence rather than a path.
        // Every body sweeps against `current` under the Phase 1 invariant;
        // Phase 2 resolves a map per body instead, which is the change the
        // fixed-pass equality and the per-body prediction are there to police.
        let id = current.ok_or(KernelError::NoTileMap)?;
        let map = maps.get(id).expect("current names a live map");
        bodies.clear();
        bodies
            .try_reserve_exact(MAX_LIVE_COLLIDERS as usize)
            .map_err(|_| KernelError::Capacity)?;
        for (entity, position, velocity, collider) in world
            .query::<(Entity, &Position, &Velocity, &TileCollider)>()
            .iter()
        {
            bodies.push(Candidate {
                entity,
                position: *position,
                velocity: *velocity,
                collider: *collider,
            });
        }
        // Stable entity-identifier order, matching `entities`' hecs-slot
        // ordering. Bodies never affect one another, so this decides nothing
        // about the result; it decides which body is being charged when a
        // budget or invariant failure surfaces, which is what makes a fault
        // reproducible rather than archetype-dependent.
        bodies.sort_unstable_by_key(|candidate| candidate.entity.id());
        let mut work = WorkBudget::new(MAX_FIXED_PASS_WORK);
        let mut outcome = Ok(());
        for candidate in bodies.iter_mut() {
            let travel = (
                candidate.velocity.x * FIXED_DT,
                candidate.velocity.y * FIXED_DT,
            );
            match collision::solve(
                map,
                &candidate.collider,
                (candidate.position.x, candidate.position.y),
                travel,
                &mut work,
            ) {
                Ok((x, y)) => candidate.position = Position { x, y },
                Err(error) => {
                    outcome = Err(KernelError::from(error));
                    break;
                }
            }
        }
        // Recorded even when the pass refuses, so a fault reports what it cost.
        *fixed_work = work.used();
        outcome
    }
}

fn finite(x: f64, y: f64) -> Result<(), KernelError> {
    if x.is_finite() && y.is_finite() {
        Ok(())
    } else {
        Err(KernelError::Nonfinite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: u32, rows: u32, origin: i32) -> TileMap {
        TileMap::new(
            TileMapInfo {
                columns,
                rows,
                tile_width: 32,
                tile_height: 32,
                origin_x: origin,
                origin_y: origin,
            },
            vec![true],
            vec![0; (columns * rows) as usize],
        )
        .expect("test map is within the frozen limits")
    }

    fn body(kernel: &mut Kernel) -> EntityHandle {
        let entity = kernel.spawn(Position { x: 0.0, y: 0.0 }).unwrap();
        kernel
            .set_tile_collider(
                &entity,
                Some(TileCollider {
                    offset_x: 0.0,
                    offset_y: 0.0,
                    width: 32.0,
                    height: 32.0,
                }),
            )
            .unwrap();
        entity
    }

    #[test]
    fn a_reused_slot_refuses_the_handle_it_displaced() {
        let mut kernel = Kernel::new();
        let first = kernel.create_tilemap(grid(4, 4, 0)).unwrap();
        let slot = first.slot();
        kernel.remove_tilemap(&first).unwrap();
        let second = kernel.create_tilemap(grid(8, 8, 0)).unwrap();

        // Asserted before the stale handle is presented, and this is the whole
        // point of the fixture (M2-1). Without a forced reuse the refusal below
        // would be satisfied by a slot that was never recycled, which is the
        // easy case this exists to avoid.
        assert_eq!(second.slot(), slot, "the fixture needs a real reuse");

        assert_eq!(
            kernel.tilemap_info(&first),
            Err(KernelError::InvalidTileMap),
            "a handle to a removed map must refuse after its slot is reused"
        );
        assert_eq!(
            kernel.tilemap_info(&second).unwrap().columns,
            8,
            "while the map that took the slot reads through its own handle"
        );
    }

    #[test]
    fn a_foreign_handle_refuses_against_a_map_holding_the_same_slot() {
        // Both kernels hold a live map at slot 0, generation 0, so the identity
        // alone cannot tell them apart: `TileMapId` carries no session. Without
        // the `Rc::ptr_eq` in `validate_map` this resolves against the other
        // kernel's table and answers 8 instead of refusing.
        //
        // The obvious version of this fixture - one kernel with a map, one
        // without - passes with no session check at all, because an empty table
        // refuses on its own. Same shape as M2-1's argument for why the reuse
        // fixture must force the reuse rather than assert a refusal an
        // unrecycled slot would also satisfy.
        let mut first = Kernel::new();
        let mut second = Kernel::new();
        let foreign = first.create_tilemap(grid(4, 4, 0)).unwrap();
        let own = second.create_tilemap(grid(8, 8, 0)).unwrap();
        assert_eq!(foreign.slot(), own.slot(), "the fixture needs a collision");

        assert_eq!(
            second.tilemap_info(&foreign),
            Err(KernelError::InvalidTileMap),
            "a handle from another session must refuse, not read the slot it names"
        );
        assert_eq!(
            second.replace_tilemap(&foreign, grid(2, 2, 0)),
            Err(KernelError::InvalidTileMap)
        );
        assert_eq!(
            second.remove_tilemap(&foreign),
            Err(KernelError::InvalidTileMap)
        );
        // Nothing the foreign handle touched moved, and the local one still works.
        assert_eq!(second.tilemap_info(&own).unwrap().columns, 8);
        assert_eq!(first.tilemap_info(&foreign).unwrap().columns, 4);
    }

    #[test]
    fn stopping_refuses_by_session_where_removal_refuses_by_identity() {
        // M2-1 asks for both errors by name rather than for two situations that
        // both refuse. `require_active` runs ahead of every identity check, so
        // `stop` reaches `Inactive` without touching a generation - neither of
        // the two mechanisms a reader would otherwise infer.
        let mut kernel = Kernel::new();
        let live = kernel.create_tilemap(grid(4, 4, 0)).unwrap();
        let removed = kernel.create_tilemap(grid(4, 4, 0)).unwrap();
        kernel.remove_tilemap(&removed).unwrap();
        assert_eq!(
            kernel.tilemap_info(&removed),
            Err(KernelError::InvalidTileMap),
            "a removed map refuses by identity while the session is live"
        );

        kernel.stop();
        assert_eq!(
            kernel.tilemap_info(&live),
            Err(KernelError::Inactive),
            "a stopped session refuses before any slot is examined"
        );
        assert_eq!(
            kernel.tilemap_info(&removed),
            Err(KernelError::Inactive),
            "including for the handle that refused by identity a moment ago"
        );
        assert_eq!(
            kernel.create_tilemap(grid(4, 4, 0)),
            Err(KernelError::Inactive)
        );
        assert_eq!(
            kernel.tilemap_storage_bytes(),
            0,
            "and released its storage"
        );
        assert_eq!(kernel.tilemap_count(), 0);
    }

    #[test]
    fn removing_and_replacing_an_unrelated_map_ignores_another_maps_bodies() {
        // The Phase 1 form of M2-R1, and it needs two live maps and a collider
        // at once. `current` holds the body; B overlaps it exactly, so a rule
        // that scanned all colliders instead of the edited map's own would
        // refuse every operation below.
        let mut kernel = Kernel::new();
        kernel.set_tilemap(grid(8, 8, 0)).unwrap();
        let entity = body(&mut kernel);
        let overlapping = kernel.create_tilemap(grid(8, 8, 0)).unwrap();
        assert_eq!(kernel.tilemap_count(), 2, "two live maps and one collider");

        // Each map-local success below is paired with the same operation on
        // `current` refusing. Without the pair, every assertion here would also
        // pass against an implementation that had simply stopped checking.

        // A cell edit scans only the edited map's members. Cell (0,0) of B is
        // exactly where the body stands, so M1's global query - correct code,
        // which is why this rule matters more than its siblings - would refuse.
        kernel
            .set_map_tile(overlapping.id, 0, 0, 1)
            .expect("a cell edit must ignore bodies on another map");
        assert_eq!(
            kernel.set_tile(0, 0, 1),
            Err(KernelError::Collision(CollisionError::Placement)),
            "while the same edit on the body's own map still refuses"
        );

        // Replacement revalidates only its own members. This candidate sits a
        // thousand pixels away, so the body is outside it and would refuse if
        // anything were checked.
        kernel
            .replace_tilemap(&overlapping, grid(8, 8, 1_000))
            .expect("replacing another map must not revalidate this map's bodies");
        assert_eq!(
            kernel.set_tilemap(grid(8, 8, 1_000)),
            Err(KernelError::Collision(CollisionError::Placement)),
            "while the same replacement of the body's own map still refuses"
        );

        // Removal is refused only by a body on the map being removed.
        kernel
            .remove_tilemap(&overlapping)
            .expect("removing another map must not see this map's bodies");
        assert_eq!(
            kernel.clear_tilemap(),
            Err(KernelError::CollidersAttached),
            "while the map the body is on still refuses"
        );
        assert_eq!(kernel.tilemap_count(), 1);
        assert_eq!(kernel.tilemap().unwrap().unwrap().origin_x, 0, "unmoved");
        assert!(
            kernel.position(&entity).is_ok(),
            "and the body is untouched"
        );
    }
}
