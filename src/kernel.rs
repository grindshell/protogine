//! Graphics-independent entity storage and fixed-step systems.

use crate::collision::{
    self, CollisionError, MAX_CALLBACK_WORK, MAX_FIXED_PASS_WORK, MAX_LIVE_COLLIDERS, TileCollider,
    WorkBudget,
};
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
    NoTileMap,
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
    /// The single installed map (T1). Dense storage outside hecs, so a static
    /// tile is never an entity.
    tilemap: Option<TileMap>,
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
            tilemap: None,
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
        self.tilemap = None;
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
            let map = self.tilemap.as_ref().ok_or(KernelError::NoTileMap)?;
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

    fn map(&self) -> Result<&TileMap, KernelError> {
        self.require_active()?;
        self.tilemap.as_ref().ok_or(KernelError::NoTileMap)
    }

    /// Install or replace the map (T6).
    ///
    /// Every live collider is revalidated against the candidate before the
    /// swap: extents are capped relative to tile size, so a body legal on the
    /// installed map can be illegal on the new one even at the same position.
    /// Validation runs entirely against the local candidate, so a refusal
    /// leaves the installed map untouched and no reader can observe a partial
    /// one; the candidate arrives already schema-checked by [`TileMap::new`].
    pub fn set_tilemap(&mut self, map: TileMap) -> Result<(), KernelError> {
        self.require_active()?;
        let Self {
            world,
            tilemap,
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
                    collision::check_placement(&map, collider, position.x, position.y, &mut work)
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
        *tilemap = Some(map);
        Ok(())
    }

    /// Remove the map, succeeding when none is installed.
    ///
    /// T6 refuses while any collider remains attached. The M1 transition is
    /// therefore detach, replace, reposition, reattach; M2-M4 must revise that.
    pub fn clear_tilemap(&mut self) -> Result<(), KernelError> {
        self.require_active()?;
        if self.colliders > 0 {
            return Err(KernelError::CollidersAttached);
        }
        self.tilemap = None;
        Ok(())
    }

    /// Owned dimensions, tile size and origin, or `None` when no map exists.
    pub fn tilemap(&self) -> Result<Option<TileMapInfo>, KernelError> {
        self.require_active()?;
        Ok(self.tilemap.as_ref().map(TileMap::info))
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
        let Self {
            world,
            tilemap,
            callback_work,
            ..
        } = self;
        let map = tilemap.as_mut().ok_or(KernelError::NoTileMap)?;
        // Schema first: a malformed edit is refused before anything is scanned
        // or charged.
        let previous = map.tile(column, row)?;
        let becomes_solid = map.solid_definition(id)? && !map.solid_definition(previous)?;
        // Metered like every other entry point, so `MAX_CALLBACK_WORK` bounds
        // this one too rather than relying on the collider limit to do it.
        let mut work = Self::remaining_work(*callback_work);
        let mut outcome = work.charge(1).map_err(KernelError::from);
        if becomes_solid && outcome.is_ok() {
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
        let map = self.tilemap.as_ref().ok_or(KernelError::NoTileMap)?;
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

    /// Live map storage in bytes, for the memory accounting stress runs record.
    /// Zero with no map, including after [`Self::stop`] releases it.
    pub fn tilemap_storage_bytes(&self) -> usize {
        self.tilemap.as_ref().map_or(0, TileMap::storage_bytes)
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
            tilemap,
            bodies,
            fixed_work,
            ..
        } = self;
        *fixed_work = 0;
        // A collider cannot be attached without a map, and the map cannot be
        // cleared while one is attached, so this is defence rather than a path.
        let map = tilemap.as_ref().ok_or(KernelError::NoTileMap)?;
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
