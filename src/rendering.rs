//! The shared Macroquad renderer: a context-lifetime texture pool, bounded
//! staged uploads, and ordered submission of owned drawing commands.
//!
//! This module has no scripting dependency, so it can never reference
//! `GameRuntime`. It validates every command through the same predicates the
//! script bindings use, and it owns no game state: a session's image mapping
//! lives only while that session is attached.
//!
//! # Application preconditions
//!
//! The application must supply a screen-space default camera and ordinary
//! blend and material state before calling [`MacroquadRenderer::render`], and
//! must never call `build_textures_atlas` or `reset_textures_atlas`. Packing
//! would move drawing to an atlas whose filtering is independent of each slot,
//! and a reset would invalidate unrelated font state. A future editor viewport
//! has to honour all three.

use crate::{
    assets::{
        AssetStore, GpuResidency, ImageData, ImageId, ImageState, UploadAck, WORK_QUANTUM,
        band_rows,
    },
    drawing::{
        DRAW_COMMAND_LIMIT, DrawCommand, Sprite, valid_color, valid_coordinate, valid_extent,
    },
};
use macroquad::{
    color::BLACK,
    math::{Rect, vec2},
    miniquad::{RenderingBackend, TextureId},
    shapes::draw_rectangle,
    texture::{DrawTextureParams, FilterMode, Texture2D, draw_texture_ex},
    window::clear_background,
};
use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

/// GPU allocation slots over one graphics context's lifetime. Slots are created
/// lazily, reused across sessions, and never recreated: `Texture2D::from_rgba8`
/// appends a batcher entry that ordinary texture collection does not remove,
/// and the pinned Windows backend also keeps a record per created texture.
pub const POOL_LIMIT: usize = 128;
/// Row bands transferred in one upload pass.
pub const BANDS_PER_PASS: usize = 8;
/// Bytes transferred in one upload pass, being `BANDS_PER_PASS` work quanta.
pub const UPLOAD_PASS_BYTES: usize = BANDS_PER_PASS * WORK_QUANTUM;
/// Soft cutoff checked between operations in one upload pass. The driver may
/// still finish later; this is a scheduling target, not a wall-clock guarantee.
pub const UPLOAD_PASS_TARGET: Duration = Duration::from_millis(2);

/// One transparent 1x1 RGBA8 texel: what an idle or retired slot holds.
const CLEARED: [u8; 4] = [0; 4];

/// A fatal presentation error detected by engine validation.
///
/// These are renderer invariant violations, not the inspectable per-image job
/// failures the asset service reports. The application turns one into a session
/// presentation fault; it never retries or silently skips the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// Drawing or upload service was attempted with no attached session.
    Detached,
    /// The command names an image from a different store session.
    ForeignImage(ImageId),
    /// The command names an image this store no longer knows, or never did.
    UnknownImage(ImageId),
    /// The image failed, was unloaded, or has no validated CPU content.
    NotDrawable(ImageId),
    /// A scalar, crop or color outside the shared drawing contract.
    InvalidCommand(&'static str),
    /// More commands than the published cap allows.
    CommandLimit(usize),
    /// Every allocation slot is in use and the pool is at its lifetime cap.
    PoolExhausted,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Detached => write!(f, "no session is attached to the renderer"),
            Self::ForeignImage(id) => write!(
                f,
                "image {}/{} belongs to another session",
                id.session(),
                id.number()
            ),
            Self::UnknownImage(id) => write!(f, "image {} is not in this store", id.number()),
            Self::NotDrawable(id) => write!(f, "image {} has no drawable content", id.number()),
            Self::InvalidCommand(reason) => write!(f, "{reason}"),
            Self::CommandLimit(count) => {
                write!(f, "{count} commands exceed the {DRAW_COMMAND_LIMIT} cap")
            }
            Self::PoolExhausted => write!(f, "all {POOL_LIMIT} allocation slots are in use"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Cumulative work counters. Tests use them to prove that drawing triggers no
/// upload, that a resident image is not uploaded again, and that the pool stays
/// inside its lifetime cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCounters {
    /// `Texture2D::from_rgba8` calls. This is the pool's lifetime growth.
    pub creations: u64,
    /// Destination allocations, being resizes that discard a slot's contents.
    pub allocations: u64,
    /// Row bands transferred, and their bytes.
    pub bands: u64,
    pub band_bytes: u64,
    pub admitted: u64,
    pub completed: u64,
    /// Uploads abandoned because their image stopped being drawable.
    pub cancelled: u64,
    pub sessions: u64,
    /// Sprites drawn, and sprites skipped because their upload was incomplete.
    pub drawn: u64,
    pub skipped: u64,
}

/// One reusable GPU allocation. The managed owner never leaves the renderer.
struct Slot {
    texture: Texture2D,
    /// The backend identity this slot was created with. Reuse resizes in
    /// place, so it must never change; recreating a texture would.
    identity: TextureId,
    width: u32,
    height: u32,
}

/// An image being transferred. The `Arc` pins the store's immutable pixels for
/// the whole upload, so an unload cannot free them between bands.
struct Staging {
    slot: usize,
    data: Arc<ImageData>,
    rows_done: u32,
}

enum Upload {
    Staging(Staging),
    /// Every band succeeded, so the full-ID mapping is published.
    Resident {
        slot: usize,
    },
}

impl Upload {
    fn slot(&self) -> usize {
        match self {
            Self::Staging(staging) => staging.slot,
            Self::Resident { slot } => *slot,
        }
    }
}

/// The application's renderer, owned for the lifetime of one graphics context.
///
/// Its allocation pool outlives every game session: attaching a new session
/// clears the mapping and shrinks used slots, but never recreates a texture.
pub struct MacroquadRenderer {
    pool: Vec<Slot>,
    /// Slots holding no image, available for the next admission.
    free: Vec<usize>,
    /// Slots the currently queued frame draws from. Recycling one before that
    /// frame is submitted and presented would corrupt it.
    pinned: Vec<usize>,
    /// Slots released while pinned, returned to `free` at the next frame.
    deferred: Vec<usize>,
    session: Option<u64>,
    /// Upload state for the attached session, keyed by image number.
    images: HashMap<u32, Upload>,
    /// CPU-ready images accepted for upload but not yet started.
    admissions: Vec<ImageId>,
    counters: RenderCounters,
}

impl Default for MacroquadRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroquadRenderer {
    /// Allocate nothing. Slots are created on first use, so a renderer can be
    /// constructed before any image exists; every other operation needs a live
    /// graphics context on its owning thread.
    pub fn new() -> Self {
        Self {
            pool: Vec::new(),
            free: Vec::new(),
            pinned: Vec::new(),
            deferred: Vec::new(),
            session: None,
            images: HashMap::new(),
            admissions: Vec::new(),
            counters: RenderCounters::default(),
        }
    }

    pub fn counters(&self) -> RenderCounters {
        self.counters
    }

    /// Allocation slots created so far. Never exceeds [`POOL_LIMIT`].
    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }

    pub fn session(&self) -> Option<u64> {
        self.session
    }

    /// Every slot's current allocation, in creation order. No GPU readback, so
    /// this is cheap enough to check every cycle.
    pub fn slot_sizes(&self) -> Vec<(u32, u32)> {
        self.pool
            .iter()
            .map(|slot| (slot.width, slot.height))
            .collect()
    }

    /// Read every slot's pixels back and report the total payload and whether
    /// all of it is fully transparent.
    ///
    /// This synchronizes with the GPU, so it belongs at a retirement boundary
    /// rather than in a frame. After [`Self::retire`] every slot is 1x1, so the
    /// payload is four bytes per slot and nothing of a previous game survives.
    pub fn measure_payload(&self) -> (usize, bool) {
        let mut bytes = 0;
        let mut transparent = true;
        for slot in &self.pool {
            let pixels = slot.texture.get_texture_data().bytes;
            bytes += pixels.len();
            transparent &= pixels.chunks_exact(4).all(|texel| texel == CLEARED);
        }
        (bytes, transparent)
    }

    /// Whether every slot still holds the backend texture it was created with.
    ///
    /// The pool reuses storage by resizing in place; a renderer that recreated
    /// a texture would grow the backend's record list and the batcher's
    /// unbatched entries, neither of which ordinary collection removes.
    pub fn identities_stable(&self) -> bool {
        self.pool
            .iter()
            .all(|slot| slot.texture.raw_miniquad_id() == slot.identity)
    }

    /// Images admitted for upload whose transfer has not finished. A drain
    /// driver services until this reaches zero.
    pub fn pending_uploads(&self) -> usize {
        self.admissions.len()
            + self
                .images
                .values()
                .filter(|upload| matches!(upload, Upload::Staging(_)))
                .count()
    }

    /// Images whose upload has completed and which can be drawn.
    pub fn resident_images(&self) -> usize {
        self.images
            .values()
            .filter(|upload| matches!(upload, Upload::Resident { .. }))
            .count()
    }

    /// Attach a live store session. Any previous session is retired first, so a
    /// new session can never inherit an old mapping or an old image's content.
    pub fn attach(&mut self, session: u64) {
        self.retire();
        self.session = Some(session);
        self.counters.sessions += 1;
    }

    /// Accept a CPU-ready image for upload. Ordering follows admission, and an
    /// image already admitted or uploaded is ignored, so one ready transition
    /// produces exactly one upload sequence.
    pub fn admit(&mut self, id: ImageId) {
        if self.session != Some(id.session())
            || self.images.contains_key(&id.number())
            || self.admissions.contains(&id)
        {
            return;
        }
        self.admissions.push(id);
    }

    /// Run one bounded upload pass and return the acknowledgements it produced.
    ///
    /// At most one destination allocation and [`BANDS_PER_PASS`] row bands move
    /// per pass, with a soft cutoff between operations. No Luau work happens
    /// here, and the returned acknowledgements are applied by the caller at its
    /// next update boundary.
    pub fn service_uploads(&mut self, store: &AssetStore) -> Result<Vec<UploadAck>, RenderError> {
        let Some(session) = self.session else {
            return Err(RenderError::Detached);
        };
        if store.session() != session {
            return Err(RenderError::Detached);
        }
        // A queued frame has been submitted and presented by the time the next
        // pass runs, so slots it held can be recycled now.
        self.release_deferred();
        let mut acks = Vec::new();
        let deadline = Instant::now() + UPLOAD_PASS_TARGET;
        let mut allocated = false;
        let mut bands = 0;
        let mut bytes = 0;

        // Drop admissions and uploads whose image stopped being drawable, so a
        // cancelled or failed image retires its slot instead of holding it.
        self.cancel_undrawable(store, &mut acks);

        while bands < BANDS_PER_PASS && bytes < UPLOAD_PASS_BYTES {
            let Some(number) = self.next_staging() else {
                if allocated || Instant::now() >= deadline {
                    break;
                }
                // Starting an upload allocates destination storage, which is a
                // non-preemptible operation and is limited to one per pass.
                match self.start_upload(store, session) {
                    Ok(Some(ack)) => {
                        allocated = true;
                        acks.push(ack);
                        continue;
                    }
                    Ok(None) => break,
                    // A slot the queued frame still draws from is released at
                    // the next pass, so transient pressure yields with the
                    // admission intact rather than stopping the session. With
                    // nothing deferred the pool is genuinely exhausted, which
                    // the 128-entry registry makes impossible, so that stays
                    // fatal and keeps the invariant sharp.
                    Err(RenderError::PoolExhausted) if !self.deferred.is_empty() => break,
                    Err(error) => return Err(error),
                }
            };
            let transferred = self.transfer_band(number);
            bands += 1;
            bytes += transferred;
            if let Some(ack) = self.finish_if_complete(number) {
                acks.push(ack);
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        Ok(acks)
    }

    /// Validate a complete command list, then submit it in order.
    ///
    /// Validation happens before any drawing is queued, so a rejected list
    /// leaves the frame untouched. The traversal reads no file, decodes
    /// nothing, allocates no texture and advances no upload: a sprite whose
    /// upload is still incomplete is skipped without forcing it to finish.
    pub fn render(
        &mut self,
        store: &AssetStore,
        commands: &[DrawCommand],
    ) -> Result<(), RenderError> {
        let used = self.plan(store, commands)?;
        // The previous frame is presented, so its pins are released only now.
        self.release_deferred();
        self.pinned = used;

        clear_background(BLACK);
        for command in commands {
            match *command {
                DrawCommand::Clear(color) => clear_background(color.into()),
                DrawCommand::Rect {
                    x,
                    y,
                    width,
                    height,
                    color,
                } => draw_rectangle(x, y, width, height, color.into()),
                DrawCommand::Sprite(sprite) => self.draw_sprite(&sprite),
            }
        }
        Ok(())
    }

    /// Invalidate the session mapping and shrink every used slot to a
    /// transparent 1x1 texel, keeping the cleared slots for the next session.
    ///
    /// Idempotent. The caller must have submitted or discarded queued work
    /// first, and must still hold the graphics context on its owning thread:
    /// resizing a texture a queued frame still references would corrupt it.
    pub fn retire(&mut self) {
        self.session = None;
        self.images.clear();
        self.admissions.clear();
        self.pinned.clear();
        self.deferred.clear();
        self.free.clear();
        for index in 0..self.pool.len() {
            self.clear_slot(index);
            self.free.push(index);
        }
    }

    /// Check a complete command list without queueing or drawing anything.
    ///
    /// [`Self::render`] runs exactly this first, so a caller can test or
    /// pre-flight a list without a frame in progress. It needs no graphics
    /// context of its own.
    pub fn validate(
        &self,
        store: &AssetStore,
        commands: &[DrawCommand],
    ) -> Result<(), RenderError> {
        self.plan(store, commands).map(|_| ())
    }

    /// Validate every command and report the distinct slots the frame draws
    /// from.
    ///
    /// This reads the live store, while scripts read the host's published view
    /// of it, and the two agree here because of an invariant worth stating: the
    /// store changes only during a CPU service pass and during a script's own
    /// `request_png` or `unload`, and all of those happen strictly before
    /// `render` in every path. Unload is the only transition that removes
    /// readiness, and it publishes immediately, so a command the draw callback
    /// produced cannot name an image that stopped being drawable in between.
    /// `cancel_undrawable` reads the live store for the same reason and depends
    /// on the same invariant; a later phase that advances the store between the
    /// draw callback and submission would break both.
    fn plan(
        &self,
        store: &AssetStore,
        commands: &[DrawCommand],
    ) -> Result<Vec<usize>, RenderError> {
        let Some(session) = self.session else {
            return Err(RenderError::Detached);
        };
        if store.session() != session {
            return Err(RenderError::Detached);
        }
        if commands.len() > DRAW_COMMAND_LIMIT {
            return Err(RenderError::CommandLimit(commands.len()));
        }
        let mut used = Vec::new();
        for command in commands {
            match command {
                DrawCommand::Clear(color) => check_color(*color)?,
                DrawCommand::Rect {
                    x,
                    y,
                    width,
                    height,
                    color,
                } => {
                    check_color(*color)?;
                    if !valid_coordinate(f64::from(*x))
                        || !valid_coordinate(f64::from(*y))
                        || !valid_extent(f64::from(*width))
                        || !valid_extent(f64::from(*height))
                    {
                        return Err(RenderError::InvalidCommand(
                            "draw coordinates or sizes are outside the finite pixel range",
                        ));
                    }
                }
                DrawCommand::Sprite(sprite) => {
                    // One entry per slot, not per command: a frame may draw ten
                    // thousand sprites from at most 128 slots, and the pin set
                    // is scanned once per released slot.
                    if let Some(slot) = self.check_sprite(store, session, sprite)?
                        && !used.contains(&slot)
                    {
                        used.push(slot);
                    }
                }
            }
        }
        Ok(used)
    }

    // ---- uploads --------------------------------------------------------

    /// An image whose store entry stopped being drawable can never complete, so
    /// release its reservation instead of transferring into a stale slot.
    fn cancel_undrawable(&mut self, store: &AssetStore, acks: &mut Vec<UploadAck>) {
        let session = self.session.expect("checked by the caller");
        self.admissions.retain(
            |id| matches!(store.status(*id), Some(status) if status.state == ImageState::Ready),
        );
        let stale: Vec<u32> = self
            .images
            .keys()
            .copied()
            .filter(|number| {
                !matches!(
                    store.status(ImageId::new(session, *number)),
                    Some(status) if status.state == ImageState::Ready
                )
            })
            .collect();
        for number in stale {
            let Some(upload) = self.images.remove(&number) else {
                continue;
            };
            self.release_slot(upload.slot());
            self.counters.cancelled += 1;
            acks.push(UploadAck {
                id: ImageId::new(session, number),
                residency: GpuResidency::Released,
            });
        }
    }

    fn next_staging(&self) -> Option<u32> {
        self.images
            .iter()
            .find_map(|(number, upload)| matches!(upload, Upload::Staging(_)).then_some(*number))
    }

    /// Allocate destination storage for the next admitted image. Returns the
    /// pending acknowledgement, or `None` when nothing is waiting.
    fn start_upload(
        &mut self,
        store: &AssetStore,
        session: u64,
    ) -> Result<Option<UploadAck>, RenderError> {
        let Some(id) = self.admissions.first().copied() else {
            return Ok(None);
        };
        let Some(data) = store.image(id) else {
            // Checked immediately above by cancel_undrawable, so losing it here
            // would mean the store changed under a synchronous caller.
            self.admissions.remove(0);
            return Ok(None);
        };
        let slot = self.acquire_slot()?;
        // Reserve the mapping before touching the GPU: a later allocation
        // failure must not lose ownership of storage already committed.
        self.images.reserve(1);
        self.admissions.remove(0);
        let (width, height) = (data.width(), data.height());
        allocate(&self.pool[slot].texture, width, height);
        self.pool[slot].width = width;
        self.pool[slot].height = height;
        self.counters.allocations += 1;
        self.counters.admitted += 1;
        self.images.insert(
            id.number(),
            Upload::Staging(Staging {
                slot,
                data,
                rows_done: 0,
            }),
        );
        Ok(Some(UploadAck {
            id: ImageId::new(session, id.number()),
            residency: GpuResidency::Pending,
        }))
    }

    /// Transfer one complete row band and report its bytes.
    fn transfer_band(&mut self, number: u32) -> usize {
        let Some(Upload::Staging(staging)) = self.images.get_mut(&number) else {
            return 0;
        };
        let (width, height) = (staging.data.width(), staging.data.height());
        let rows = band_rows(width as usize, height as usize) as u32;
        let count = rows.min(height - staging.rows_done);
        if count == 0 {
            // Every row is already transferred; the caller publishes it next.
            return 0;
        }
        let start = (staging.rows_done as usize) * (width as usize) * 4;
        let len = (count as usize) * (width as usize) * 4;
        let band = &staging.data.rgba()[start..start + len];
        let texture = &self.pool[staging.slot].texture;
        update_region(texture, staging.rows_done, width, count, band);
        staging.rows_done += count;
        self.counters.bands += 1;
        self.counters.band_bytes += len as u64;
        len
    }

    /// Publish the full-ID mapping once, and only once every band succeeded.
    /// Already resident images are untouched.
    fn finish_if_complete(&mut self, number: u32) -> Option<UploadAck> {
        let session = self.session?;
        let upload = self.images.get_mut(&number)?;
        let Upload::Staging(staging) = upload else {
            return None;
        };
        if staging.rows_done < staging.data.height() {
            return None;
        }
        let slot = staging.slot;
        *upload = Upload::Resident { slot };
        self.counters.completed += 1;
        Some(UploadAck {
            id: ImageId::new(session, number),
            residency: GpuResidency::Resident,
        })
    }

    // ---- slots ----------------------------------------------------------

    fn acquire_slot(&mut self) -> Result<usize, RenderError> {
        if let Some(index) = self.free.pop() {
            return Ok(index);
        }
        if self.pool.len() >= POOL_LIMIT {
            return Err(RenderError::PoolExhausted);
        }
        // Reserve the owning storage before creating the texture, so a failed
        // Rust allocation cannot drop a registration the backend already holds.
        self.pool.reserve(1);
        self.free.reserve(1);
        let texture = Texture2D::from_rgba8(1, 1, &CLEARED);
        self.counters.creations += 1;
        texture.set_filter(FilterMode::Nearest);
        let identity = texture.raw_miniquad_id();
        self.pool.push(Slot {
            texture,
            identity,
            width: 1,
            height: 1,
        });
        Ok(self.pool.len() - 1)
    }

    /// Return a slot for reuse, or defer it while the queued frame draws it.
    fn release_slot(&mut self, index: usize) {
        if self.pinned.contains(&index) {
            self.deferred.push(index);
        } else {
            self.clear_slot(index);
            self.free.push(index);
        }
    }

    fn release_deferred(&mut self) {
        for index in std::mem::take(&mut self.deferred) {
            self.clear_slot(index);
            self.free.push(index);
        }
    }

    fn clear_slot(&mut self, index: usize) {
        let slot = &mut self.pool[index];
        if (slot.width, slot.height) == (1, 1) {
            return;
        }
        resize_cleared(&slot.texture);
        slot.width = 1;
        slot.height = 1;
    }

    // ---- validation and submission --------------------------------------

    /// Validate one sprite and report the slot it will draw from, or `None`
    /// when its upload is incomplete and the command is skipped.
    fn check_sprite(
        &self,
        store: &AssetStore,
        session: u64,
        sprite: &Sprite,
    ) -> Result<Option<usize>, RenderError> {
        if sprite.image.session() != session {
            return Err(RenderError::ForeignImage(sprite.image));
        }
        sprite.check().map_err(RenderError::InvalidCommand)?;
        let Some(status) = store.status(sprite.image) else {
            return Err(RenderError::UnknownImage(sprite.image));
        };
        if status.state != ImageState::Ready {
            return Err(RenderError::NotDrawable(sprite.image));
        }
        let (Some(width), Some(height)) = (status.width, status.height) else {
            return Err(RenderError::NotDrawable(sprite.image));
        };
        if !sprite.source.fits(width, height) {
            return Err(RenderError::InvalidCommand(
                "sprite source rectangle does not fit the image",
            ));
        }
        match self.images.get(&sprite.image.number()) {
            Some(Upload::Resident { slot }) => Ok(Some(*slot)),
            // CPU-ready but not yet transferred: skipping keeps every other
            // command's order and forces no upload work during the traversal.
            _ => Ok(None),
        }
    }

    fn draw_sprite(&mut self, sprite: &Sprite) {
        let Some(Upload::Resident { slot }) = self.images.get(&sprite.image.number()) else {
            self.counters.skipped += 1;
            return;
        };
        let source = sprite.source;
        draw_texture_ex(
            &self.pool[*slot].texture,
            sprite.x,
            sprite.y,
            sprite.tint.into(),
            DrawTextureParams {
                source: Some(Rect::new(
                    source.x as f32,
                    source.y as f32,
                    source.width as f32,
                    source.height as f32,
                )),
                dest_size: Some(vec2(sprite.width, sprite.height)),
                flip_x: sprite.flip_x,
                flip_y: sprite.flip_y,
                ..Default::default()
            },
        );
        self.counters.drawn += 1;
    }
}

fn check_color(color: [f32; 4]) -> Result<(), RenderError> {
    if valid_color(color.map(f64::from)) {
        Ok(())
    } else {
        Err(RenderError::InvalidCommand(
            "draw colors must be finite and in [0, 1]",
        ))
    }
}

// ---- the backend adapter ------------------------------------------------

/// Exclusive, short-lived access to the graphics backend.
///
/// # Safety
///
/// The caller must hold a live graphics context on its owning thread, must
/// retain no backend reference beyond this call, and must make no other
/// Macroquad call, Luau callback or await while the access is held. Queued work
/// referencing a texture must be flushed before that texture is resized.
fn with_backend<T>(work: impl FnOnce(&mut dyn RenderingBackend) -> T) -> T {
    // SAFETY: Every call site below obtains its texture id first, performs one
    // synchronous backend operation, and retains nothing. The renderer is only
    // driven from the thread that owns the graphics context, inside the window
    // future, and it invokes no Macroquad, VM or async work here.
    unsafe {
        let gl = macroquad::window::get_internal_gl();
        work(gl.quad_context)
    }
}

/// Allocate destination storage without publishing any pixels. The slot keeps
/// its existing backend record and registration; only its dimensions change.
fn allocate(texture: &Texture2D, width: u32, height: u32) {
    assert!(
        (1..=crate::assets::DIMENSION_LIMIT).contains(&width)
            && (1..=crate::assets::DIMENSION_LIMIT).contains(&height),
        "image dimensions are validated before admission"
    );
    let id = texture.raw_miniquad_id();
    with_backend(|backend| backend.texture_resize(id, width, height, None));
    // Macroquad reads the record's dimensions for size and source UVs, and
    // filtering has to be reapplied after a resize.
    texture.set_filter(FilterMode::Nearest);
}

fn resize_cleared(texture: &Texture2D) {
    let id = texture.raw_miniquad_id();
    with_backend(|backend| backend.texture_resize(id, 1, 1, Some(&CLEARED)));
    texture.set_filter(FilterMode::Nearest);
}

/// Transfer one complete band of rows into an already allocated slot.
///
/// Dimensions, region bounds, row alignment and the exact RGBA8 byte count are
/// all checked here, at the unsafe boundary, rather than being left to the
/// caller's arithmetic.
fn update_region(texture: &Texture2D, y: u32, width: u32, rows: u32, band: &[u8]) {
    assert!(rows > 0, "a band must carry at least one row");
    assert_eq!(
        band.len(),
        (rows as usize) * (width as usize) * 4,
        "a band must be whole RGBA8 rows"
    );
    let allocated_width = texture.width() as u32;
    let allocated_height = texture.height() as u32;
    assert_eq!(
        allocated_width, width,
        "band width must match the allocated destination"
    );
    assert!(
        y.checked_add(rows)
            .is_some_and(|bottom| bottom <= allocated_height),
        "band rows {y}..{} exceed the {allocated_height}-row destination",
        u64::from(y) + u64::from(rows)
    );
    let id = texture.raw_miniquad_id();
    with_backend(|backend| {
        backend.texture_update_part(id, 0, y as i32, width as i32, rows as i32, band);
    });
}
