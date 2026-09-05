//! Graphics-independent entity storage and fixed-step systems.

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
    /// Process-local identity for private VM caches, never a persisted/public ID.
    #[cfg(feature = "scripting")]
    pub(crate) fn cache_key(&self) -> [u8; size_of::<usize>() + 8] {
        let mut key = [0; size_of::<usize>() + 8];
        key[..size_of::<usize>()]
            .copy_from_slice(&(Rc::as_ptr(&self.session) as usize).to_le_bytes());
        key[size_of::<usize>()..].copy_from_slice(&self.entity.to_bits().get().to_le_bytes());
        key
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
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Inactive => "world session is stopped or faulted",
            Self::InvalidHandle => "stale or foreign entity handle",
            Self::Nonfinite => "position and velocity must remain finite",
            Self::EntityLimit => "entity limit exceeded",
        })
    }
}
impl std::error::Error for KernelError {}

/// Engine-owned world. No ECS references or query borrows escape this API.
pub struct Kernel {
    world: World,
    session: Rc<()>,
    active: bool,
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
        }
    }

    /// Invalidate all handles for subsequent world access. Cannot be restarted.
    pub fn stop(&mut self) {
        self.active = false;
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
