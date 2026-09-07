//! Graphics-independent entity storage and fixed-step systems.

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
        }
    }
}
impl std::error::Error for KernelError {}

impl From<TileMapError> for KernelError {
    fn from(error: TileMapError) -> Self {
        Self::TileMap(error)
    }
}

/// Engine-owned world. No ECS references or query borrows escape this API.
pub struct Kernel {
    world: World,
    session: Rc<()>,
    active: bool,
    /// The single installed map (T1). Dense storage outside hecs, so a static
    /// tile is never an entity.
    tilemap: Option<TileMap>,
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

    pub fn despawn(&mut self, entity: &EntityHandle) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        self.world
            .despawn(entity)
            .map_err(|_| KernelError::InvalidHandle)
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

    pub fn set_position(
        &mut self,
        entity: &EntityHandle,
        position: Position,
    ) -> Result<(), KernelError> {
        let entity = self.validate(entity)?;
        finite(position.x, position.y)?;
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

    /// Install or replace the map.
    ///
    /// The candidate arrives already validated by [`TileMap::new`], so this
    /// publishes it in one move and there is no window in which a reader could
    /// observe a partial map. A description that failed to validate never
    /// reaches here, leaving the previous map installed and intact.
    ///
    /// Phase 2 extends this to revalidate every live collider before the swap.
    pub fn set_tilemap(&mut self, map: TileMap) -> Result<(), KernelError> {
        self.require_active()?;
        self.tilemap = Some(map);
        Ok(())
    }

    /// Remove the map, succeeding when none is installed.
    ///
    /// Phase 2 adds T6's refusal while any collider remains attached.
    pub fn clear_tilemap(&mut self) -> Result<(), KernelError> {
        self.require_active()?;
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

    /// Change one cell immediately.
    ///
    /// Phase 2 extends this to refuse an edit that would trap a collider.
    pub fn set_tile(&mut self, column: i32, row: i32, id: u16) -> Result<(), KernelError> {
        self.require_active()?;
        let map = self.tilemap.as_mut().ok_or(KernelError::NoTileMap)?;
        Ok(map.set_tile(column, row, id)?)
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
    pub fn fixed_update(&mut self) -> Result<(), KernelError> {
        self.require_active()?;
        for (position, velocity) in self.world.query::<(&Position, &Velocity)>().iter() {
            finite(
                position.x + velocity.x * FIXED_DT,
                position.y + velocity.y * FIXED_DT,
            )?;
        }
        for (position, velocity) in self.world.query_mut::<(&mut Position, &Velocity)>() {
            position.x += velocity.x * FIXED_DT;
            position.y += velocity.y * FIXED_DT;
        }
        Ok(())
    }
}

fn finite(x: f64, y: f64) -> Result<(), KernelError> {
    if x.is_finite() && y.is_finite() {
        Ok(())
    } else {
        Err(KernelError::Nonfinite)
    }
}
