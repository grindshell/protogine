//! The session-local image registry, admission, accounting and service pump.
//!
//! The store runs on the runtime thread. It owns identities, the bounded
//! registry, the path and spelling caches, every byte reservation, and the
//! published immutable image contents. It grants work to one worker and never
//! blocks on it during ordinary service. Nothing here depends on mlua,
//! Macroquad or the kernel, so headless tools and the Player share one loader.

use super::worker::{self, Command, Reply};
use super::{
    AssetError, AssetErrorCode, DIMENSION_LIMIT, ENCODED_IMAGE_LIMIT, ENCODED_STAGING_LIMIT,
    GpuResidency, IMAGE_REGISTRY_LIMIT, IMAGE_RGBA_LIMIT, ImageId, ImageState, ImageStatus,
    JOB_QUEUE_LIMIT, LoadStage, QUANTA_PER_PASS, SPELLING_MEMO_LIMIT, STORE_RGBA_LIMIT,
    WORK_QUANTUM,
};
use crate::rooted_path::{self, PATH_BYTES, PathError, Policy};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Process-unique session identities. Zero is never issued, so it stays
/// available as the worker's "no job yet" sentinel.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

fn next_session() -> u64 {
    NEXT_SESSION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .expect("asset session identities are exhausted")
}

/// Immutable decoded content, published only after complete validation.
///
/// Tightly packed top-to-bottom RGBA8 with straight alpha, exactly as decoded:
/// no ICC or gamma correction, no premultiplication and no vertical flip.
pub struct ImageData {
    width: u32,
    height: u32,
    encoded: usize,
    rgba: Vec<u8>,
}

impl std::fmt::Debug for ImageData {
    /// Summarize rather than dumping up to 16 MiB of pixels into a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageData")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("encoded", &self.encoded)
            .field("rgba_bytes", &self.rgba.len())
            .finish()
    }
}

impl ImageData {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Encoded bytes that were read to produce this image.
    pub fn encoded_bytes(&self) -> usize {
        self.encoded
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Cumulative work counters. Tests use them to prove that a completed image is
/// not read, decoded or converted again, and that service passes stay bounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetCounters {
    pub requests: u64,
    pub admitted: u64,
    pub refused: u64,
    pub spelling_hits: u64,
    pub coalesced: u64,
    pub service_passes: u64,
    pub worker_passes: u64,
    pub read_calls: u64,
    pub bytes_read: u64,
    pub decode_bands: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
}

#[derive(Default)]
struct Accounting {
    encoded: usize,
    rgba: usize,
    scratch: usize,
}

enum Entry {
    /// Admitted with a job still in the queue.
    Pending,
    Ready(Arc<ImageData>),
    /// Failed or unloaded, and out of every path lookup already. The slot is
    /// released when the host drains it with `take_settled`.
    Terminal(ImageStatus),
}

struct Job {
    id: ImageId,
    logical: String,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    stage: LoadStage,
    started: bool,
    /// Cancelled by unload, rollback or a store-detected failure. The entry is
    /// already settled; the job survives only until the worker releases it.
    cancelled: bool,
    bytes_read: usize,
    granted: usize,
    width: Option<u32>,
    height: Option<u32>,
    channels: usize,
    encoded: usize,
    rgba: usize,
    scratch: usize,
}

impl Job {
    fn new(id: ImageId, logical: String, path: PathBuf) -> Self {
        Self {
            id,
            logical,
            path,
            cancel: Arc::new(AtomicBool::new(false)),
            stage: LoadStage::Waiting,
            started: false,
            cancelled: false,
            bytes_read: 0,
            granted: 0,
            width: None,
            height: None,
            channels: 0,
            encoded: 0,
            rgba: 0,
            scratch: 0,
        }
    }

    fn status(&self) -> ImageStatus {
        ImageStatus {
            id: self.id,
            state: if self.started {
                ImageState::Loading
            } else {
                ImageState::Queued
            },
            stage: self.stage,
            bytes_read: self.bytes_read,
            width: self.width,
            height: self.height,
            error: None,
            gpu: GpuResidency::Unavailable,
        }
    }

    /// Cancel both halves together. The store's flag settles the entry and the
    /// shared flag stops the worker, and a job that carried only one of them
    /// would either publish after settling or never be released, so the two are
    /// never set apart.
    fn cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One bounded image service for one session, rooted at a canonical bundle.
pub struct AssetStore {
    root: PathBuf,
    session: u64,
    next_number: u32,
    entries: HashMap<u32, Entry>,
    by_path: HashMap<PathBuf, ImageId>,
    spellings: HashMap<String, ImageId>,
    spelling_order: VecDeque<String>,
    jobs: VecDeque<Job>,
    /// Unloaded or retired content whose bytes are still pinned by an owner.
    retiring: Vec<Arc<ImageData>>,
    settled: Vec<ImageStatus>,
    accounting: Accounting,
    counters: AssetCounters,
    commands: Option<SyncSender<Command>>,
    replies: Option<Receiver<(ImageId, Reply)>>,
    worker: Option<JoinHandle<()>>,
    /// The image whose grant is outstanding. At most one may be in flight.
    awaiting: Option<ImageId>,
    fault: Option<String>,
    stopped: bool,
}

impl AssetStore {
    /// Root the store at an existing absolute directory, as the filesystem
    /// bindings do. The canonical root itself may have been reached by a link.
    pub fn new(root: &Path) -> Result<Self, AssetError> {
        if !root.is_absolute() {
            return Err(AssetError::new(
                AssetErrorCode::Path,
                "",
                "asset root must be absolute",
            ));
        }
        let root = root
            .canonicalize()
            .map_err(|error| AssetError::new(AssetErrorCode::Io, "", error))?;
        if !root.is_dir() {
            return Err(AssetError::new(
                AssetErrorCode::Path,
                "",
                "asset root must be a directory",
            ));
        }
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("protogine-assets".into())
            .spawn(move || worker::run(command_rx, reply_tx))
            .map_err(|error| AssetError::new(AssetErrorCode::Io, "", error))?;
        Ok(Self {
            root,
            session: next_session(),
            next_number: 1,
            entries: HashMap::new(),
            by_path: HashMap::new(),
            spellings: HashMap::new(),
            spelling_order: VecDeque::new(),
            jobs: VecDeque::new(),
            retiring: Vec::new(),
            settled: Vec::new(),
            accounting: Accounting::default(),
            counters: AssetCounters::default(),
            commands: Some(command_tx),
            replies: Some(reply_rx),
            worker: Some(worker),
            awaiting: None,
            fault: None,
            stopped: false,
        })
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn counters(&self) -> AssetCounters {
        self.counters
    }

    /// A fatal service invariant violation. The runtime turns this into a
    /// session fault; it is not an inspectable per-image failure.
    pub fn service_fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Admitted jobs that have not settled, including the active one.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Reserved, live and pinned RGBA8 bytes across this store.
    pub fn resident_bytes(&self) -> usize {
        self.accounting.rgba
    }

    /// Outstanding encoded staging bytes, including partial and failed reads.
    pub fn staged_bytes(&self) -> usize {
        self.accounting.encoded
    }

    /// Reserved decoder workspace: the native frame an interlaced image decodes
    /// eagerly, plus one conversion band. Neither is retained image storage.
    pub fn scratch_bytes(&self) -> usize {
        self.accounting.scratch
    }

    // ---- requests -------------------------------------------------------

    /// Validate, resolve and admit a bundle-relative PNG path.
    ///
    /// Path validation and rooted canonical resolution are synchronous on a
    /// cache miss so that coalescing and handle identity are decided before
    /// returning. That performs metadata I/O, never content reads or decoding.
    pub fn request_png(&mut self, path: &str) -> Result<ImageId, AssetError> {
        self.counters.requests += 1;
        let result = self.admit(path);
        if result.is_err() {
            self.counters.refused += 1;
        }
        result
    }

    fn admit(&mut self, path: &str) -> Result<ImageId, AssetError> {
        if self.stopped || self.fault.is_some() {
            return Err(AssetError::new(
                AssetErrorCode::Path,
                path,
                "asset store is no longer admitting requests",
            ));
        }
        check_png_path(path)?;
        if let Some(id) = self.spellings.get(path).copied() {
            if self.live(id) {
                self.counters.spelling_hits += 1;
                return Ok(id);
            }
            self.forget_spelling(path);
        }
        let resolved = rooted_path::resolve(
            &self.root,
            path,
            Policy {
                canonical_file: true,
                ..Policy::default()
            },
        )
        .map_err(|error| path_error(path, error))?;
        if let Some(id) = self.by_path.get(&resolved).copied() {
            if self.live(id) {
                self.remember_spelling(path, id);
                self.counters.coalesced += 1;
                return Ok(id);
            }
            self.by_path.remove(&resolved);
        }
        // Every refusal happens before the first mutation, so a rejected
        // request leaves no partial admission and burns no identity.
        if self.entries.len() >= IMAGE_REGISTRY_LIMIT {
            return Err(AssetError::new(
                AssetErrorCode::Limit,
                path,
                "image registry is full",
            ));
        }
        if self.jobs.len() >= JOB_QUEUE_LIMIT {
            return Err(AssetError::new(
                AssetErrorCode::Limit,
                path,
                "asset job queue is full",
            ));
        }
        let number = self.next_number;
        self.next_number = number.checked_add(1).ok_or_else(|| {
            AssetError::new(
                AssetErrorCode::Limit,
                path,
                "image identities are exhausted for this session",
            )
        })?;
        let id = ImageId::new(self.session, number);
        self.entries.insert(number, Entry::Pending);
        self.by_path.insert(resolved.clone(), id);
        self.remember_spelling(path, id);
        self.jobs
            .push_back(Job::new(id, path.to_string(), resolved));
        self.counters.admitted += 1;
        Ok(id)
    }

    /// Undo an admission whose caller-side publication failed, without ever
    /// reissuing its image number. Cancels the job and clears every lookup.
    pub fn roll_back(&mut self, id: ImageId) -> bool {
        if !self.live(id) {
            return false;
        }
        self.forget_lookups(id);
        // Retire first so a ready image's bytes stay accounted until released.
        self.retire_ready(id);
        self.entries.remove(&id.number());
        self.cancel_job(id);
        // The handle was never published, so no caller may learn about it.
        self.settled.retain(|status| status.id != id);
        true
    }

    // ---- inspection -----------------------------------------------------

    /// An owned snapshot, or `None` for a foreign, unknown or drained identity.
    pub fn status(&self, id: ImageId) -> Option<ImageStatus> {
        if id.session() != self.session {
            return None;
        }
        match self.entries.get(&id.number())? {
            Entry::Ready(data) => Some(ImageStatus {
                id,
                state: ImageState::Ready,
                stage: LoadStage::Complete,
                bytes_read: data.encoded,
                width: Some(data.width),
                height: Some(data.height),
                error: None,
                gpu: GpuResidency::Unavailable,
            }),
            Entry::Terminal(status) => Some(status.clone()),
            Entry::Pending => self.job(id).map(Job::status),
        }
    }

    /// Validated dimensions. Refused while unknown, failed or unloaded.
    pub fn size(&self, id: ImageId) -> Option<(u32, u32)> {
        if id.session() != self.session {
            return None;
        }
        match self.entries.get(&id.number())? {
            Entry::Ready(data) => Some((data.width, data.height)),
            // Known as soon as the header is validated, and frozen from then on.
            Entry::Pending => self.job(id).and_then(|job| job.width.zip(job.height)),
            Entry::Terminal(_) => None,
        }
    }

    /// Immutable published content. Holding the returned owner pins its bytes,
    /// so an unload cannot free them until the owner is dropped.
    pub fn image(&self, id: ImageId) -> Option<Arc<ImageData>> {
        if id.session() != self.session {
            return None;
        }
        match self.entries.get(&id.number())? {
            Entry::Ready(data) => Some(Arc::clone(data)),
            _ => None,
        }
    }

    /// Take the state transitions observed since the last drain, and release
    /// the registry slots of the terminal entries among them.
    ///
    /// A terminal image is out of every path lookup as soon as it settles, but
    /// keeps its slot until its transition is taken, so a host that never
    /// drains stops admitting rather than accumulating an unbounded history.
    /// After the drain, terminal status lives only in whatever the caller kept.
    pub fn take_settled(&mut self) -> Vec<ImageStatus> {
        let settled = std::mem::take(&mut self.settled);
        for status in &settled {
            if status.state.is_terminal()
                && matches!(
                    self.entries.get(&status.id.number()),
                    Some(Entry::Terminal(_))
                )
            {
                self.entries.remove(&status.id.number());
            }
        }
        settled
    }

    // ---- eviction -------------------------------------------------------

    /// Invalidate a logical image for every alias: drop the path and spelling
    /// lookups, cancel any pending job, and schedule its storage for release
    /// once no owner still pins it. True once, then false.
    pub fn unload(&mut self, id: ImageId) -> bool {
        if !self.live(id) {
            return false;
        }
        self.forget_lookups(id);
        // Keep the stage the image had reached, so an unloaded pending job is
        // distinguishable from an unloaded complete one.
        let reached = self.status(id);
        let status = ImageStatus {
            id,
            state: ImageState::Unloaded,
            stage: reached.as_ref().map_or(LoadStage::Waiting, |s| s.stage),
            bytes_read: reached.as_ref().map_or(0, |s| s.bytes_read),
            width: None,
            height: None,
            error: None,
            gpu: GpuResidency::Unavailable,
        };
        self.retire_ready(id);
        self.cancel_job(id);
        self.entries
            .insert(id.number(), Entry::Terminal(status.clone()));
        self.settled.push(status);
        self.counters.cancelled += 1;
        true
    }

    // ---- service --------------------------------------------------------

    /// Memoized request spellings. Bounded, and unrelated to image residency.
    pub fn memoized_spellings(&self) -> usize {
        self.spellings.len()
    }

    /// Advance loading by one bounded pass. Never blocks on the worker.
    pub fn service(&mut self) {
        self.pass(None);
    }

    /// Run exactly one bounded pass, waiting up to `wait` for an outstanding
    /// grant to come back. Preload drivers and deterministic readiness traces
    /// use this; it grants no more work per pass than `service` does.
    pub fn advance(&mut self, wait: Duration) {
        self.pass(Some(wait));
    }

    /// Run service passes until every admitted job settles. Preload and test
    /// drivers use it; it invokes no scripts and advances no simulation, and
    /// its watchdog is separate from any script deadline.
    pub fn drain(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.jobs.is_empty() && self.fault.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            self.pass(Some(remaining.min(Duration::from_millis(50))));
        }
        self.collect_retiring();
        self.fault.is_none()
    }

    fn pass(&mut self, wait: Option<Duration>) {
        self.counters.service_passes += 1;
        self.collect_retiring();
        if self.fault.is_some() {
            return;
        }
        if self.awaiting.is_some() {
            self.receive(wait);
        }
        if self.awaiting.is_none() {
            self.dispatch();
        }
    }

    fn receive(&mut self, wait: Option<Duration>) {
        let Some(replies) = self.replies.as_ref() else {
            return;
        };
        let message = match wait {
            None => replies
                .try_recv()
                .map_err(|error| error == TryRecvError::Disconnected),
            Some(wait) => replies
                .recv_timeout(wait)
                .map_err(|error| error == RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(message) => {
                self.awaiting = None;
                self.counters.worker_passes += 1;
                self.apply(message);
            }
            Err(true) => self.fail_service("asset worker disconnected"),
            // Still working on the outstanding grant; no credit accumulates.
            Err(false) => {}
        }
    }

    fn dispatch(&mut self) {
        while let Some(job) = self.jobs.front() {
            if !job.cancelled {
                break;
            }
            let (id, started) = (job.id, job.started);
            if started {
                // The worker still holds its file, decoder and buffers.
                self.send(id, Command::Abandon);
                return;
            }
            let job = self.jobs.pop_front().expect("checked above");
            self.release_work(&job);
            self.release_output(&job);
        }
        let Some(job) = self.jobs.front() else {
            return;
        };
        let (id, started, stage) = (job.id, job.started, job.stage);
        if !started {
            let job = self.jobs.front_mut().expect("checked above");
            job.started = true;
            job.stage = LoadStage::Read;
            let command = Command::Start {
                id,
                path: job.path.clone(),
                logical: job.logical.clone(),
                cancel: Arc::clone(&job.cancel),
            };
            self.send(id, command);
            return;
        }
        let command = match stage {
            LoadStage::Read => self.grant_read(),
            LoadStage::Header => Some(Command::Header),
            LoadStage::Allocate => self.grant_allocate(),
            LoadStage::Decode => Some(Command::Decode {
                bands: QUANTA_PER_PASS,
            }),
            LoadStage::Waiting | LoadStage::Complete => {
                self.fail_service("active asset job reached an impossible stage");
                None
            }
        };
        if let Some(command) = command {
            self.send(id, command);
        }
    }

    fn send(&mut self, id: ImageId, command: Command) {
        let Some(commands) = self.commands.as_ref() else {
            return;
        };
        // The channel holds one command and the store never sends a second
        // before consuming the reply, so this cannot block.
        if commands.try_send(command).is_err() {
            self.fail_service("asset worker is not accepting work");
            return;
        }
        self.awaiting = Some(id);
    }

    /// Bound this pass by the quanta allowance, the remaining per-image
    /// encoded cap plus its one-byte overflow probe, and free staging.
    fn grant_read(&mut self) -> Option<Command> {
        let job = self.jobs.front().expect("active job");
        let remaining = (ENCODED_IMAGE_LIMIT + 1).saturating_sub(job.bytes_read);
        let want = (QUANTA_PER_PASS * WORK_QUANTUM).min(remaining);
        let free = ENCODED_STAGING_LIMIT.saturating_sub(self.accounting.encoded);
        let allowance = want.min(free);
        if allowance == 0 {
            self.fail_active(
                AssetErrorCode::Capacity,
                "encoded staging capacity is exhausted",
            );
            return None;
        }
        self.accounting.encoded += allowance;
        let job = self.jobs.front_mut().expect("active job");
        job.encoded += allowance;
        job.granted = allowance;
        Some(Command::Read { allowance })
    }

    /// Reserve the decoded output and decoder scratch before the worker
    /// allocates either of them.
    fn grant_allocate(&mut self) -> Option<Command> {
        let job = self.jobs.front().expect("active job");
        let (Some(width), Some(height)) = (job.width, job.height) else {
            self.fail_service("asset job reached allocation without dimensions");
            return None;
        };
        let (width, height, channels) = (width as usize, height as usize, job.channels);
        let rgba = width * height * 4;
        let rows = super::band_rows(width, height);
        // The pinned decoder does not report the interlace method, so reserve
        // the whole native frame it would decode eagerly for an Adam7 image,
        // plus one conversion band. Both are decoder scratch and are released
        // when the job ends; they are not retained image storage.
        let frame = width * height * channels;
        let band = rows * width * channels;
        // Neither has a budget to be refused against: the frozen 16 MiB frame
        // and 32 KiB band both fall out of the validated dimensions, and only
        // the active job holds workspace, so nothing may already be
        // outstanding. Name those ceilings where the reservation is taken
        // rather than leaving them to be re-derived from `applied_header` and
        // `band_rows`; a unit test in `src/assets.rs` proves they hold across
        // the legal range, so a failure here is a broken invariant rather than
        // a property of the image.
        if frame > IMAGE_RGBA_LIMIT || band > WORK_QUANTUM || self.accounting.scratch != 0 {
            self.fail_service("asset decoder workspace exceeded its frozen reservation");
            return None;
        }
        if self.accounting.rgba + rgba > STORE_RGBA_LIMIT {
            self.fail_active(
                AssetErrorCode::Capacity,
                "retained image storage has no room for this image",
            );
            return None;
        }
        self.accounting.rgba += rgba;
        self.accounting.scratch += frame + band;
        let job = self.jobs.front_mut().expect("active job");
        job.rgba = rgba;
        job.scratch = frame + band;
        Some(Command::Allocate {
            width,
            height,
            channels,
        })
    }

    fn apply(&mut self, (id, reply): (ImageId, Reply)) {
        let Some(job) = self.jobs.front() else {
            self.fail_service("asset reply arrived with no active job");
            return;
        };
        if job.id != id {
            self.fail_service("asset reply named a different image than the active job");
            return;
        }
        if matches!(reply, Reply::Protocol) {
            self.fail_service("asset worker refused a command out of order");
            return;
        }
        // An unloaded or rolled-back job discards whatever the worker produced.
        if job.cancelled {
            if reply.terminal() {
                let job = self.jobs.pop_front().expect("checked above");
                self.release_work(&job);
                self.release_output(&job);
            }
            return;
        }
        match reply {
            Reply::Started => {}
            Reply::Read { bytes, calls, eof } => self.applied_read(bytes, calls, eof),
            Reply::Header {
                width,
                height,
                channels,
            } => self.applied_header(width, height, channels),
            Reply::Allocated => {
                self.jobs.front_mut().expect("active job").stage = LoadStage::Decode;
            }
            Reply::Decoded { bands } => self.counters.decode_bands += bands,
            Reply::Done { bands, rgba } => {
                self.counters.decode_bands += bands;
                self.publish(rgba);
            }
            Reply::Failed(error) => self.fail_job(error),
            Reply::Cancelled => {
                // Only a cancelled job can produce this, and that is handled
                // above, so reaching here means the store lost track of it.
                self.fail_service("asset worker cancelled a job the store still owns");
            }
            Reply::Protocol => unreachable!("handled above"),
        }
    }

    fn applied_read(&mut self, bytes: usize, calls: u64, eof: bool) {
        let job = self.jobs.front_mut().expect("active job");
        let unused = job.granted.saturating_sub(bytes);
        job.granted = 0;
        job.encoded = job.encoded.saturating_sub(unused);
        job.bytes_read += bytes;
        let over = job.bytes_read > ENCODED_IMAGE_LIMIT;
        job.stage = if eof {
            LoadStage::Header
        } else {
            LoadStage::Read
        };
        self.accounting.encoded = self.accounting.encoded.saturating_sub(unused);
        self.counters.read_calls += calls;
        self.counters.bytes_read += bytes as u64;
        if over {
            self.fail_active(
                AssetErrorCode::Limit,
                format!("encoded PNG exceeds {ENCODED_IMAGE_LIMIT} bytes"),
            );
        }
    }

    fn applied_header(&mut self, width: u32, height: u32, channels: usize) {
        // The decoder's own header check already refused larger dimensions;
        // check the whole accepted range here rather than trusting that.
        if !(1..=DIMENSION_LIMIT).contains(&width) || !(1..=DIMENSION_LIMIT).contains(&height) {
            self.fail_active(
                AssetErrorCode::Limit,
                format!("image dimensions must be 1..={DIMENSION_LIMIT}, got {width}x{height}"),
            );
            return;
        }
        let rgba = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        match rgba {
            Some(rgba) if rgba <= IMAGE_RGBA_LIMIT => {}
            _ => {
                self.fail_active(
                    AssetErrorCode::Limit,
                    format!("decoded image exceeds {IMAGE_RGBA_LIMIT} bytes"),
                );
                return;
            }
        }
        let job = self.jobs.front_mut().expect("active job");
        job.width = Some(width);
        job.height = Some(height);
        job.channels = channels;
        job.stage = LoadStage::Allocate;
    }

    fn publish(&mut self, rgba: Vec<u8>) {
        let job = self.jobs.pop_front().expect("active job");
        self.release_work(&job);
        let (Some(width), Some(height)) = (job.width, job.height) else {
            self.fail_service("completed asset job had no validated dimensions");
            return;
        };
        if rgba.len() != job.rgba {
            self.release_output(&job);
            self.fail_service("completed asset job returned an unexpected pixel count");
            return;
        }
        let data = Arc::new(ImageData {
            width,
            height,
            encoded: job.bytes_read,
            rgba,
        });
        let status = ImageStatus {
            id: job.id,
            state: ImageState::Ready,
            stage: LoadStage::Complete,
            bytes_read: job.bytes_read,
            width: Some(width),
            height: Some(height),
            error: None,
            gpu: GpuResidency::Unavailable,
        };
        self.entries.insert(job.id.number(), Entry::Ready(data));
        self.settled.push(status);
        self.counters.completed += 1;
    }

    /// A failure the worker reported. Its resources are already released there.
    fn fail_job(&mut self, error: AssetError) {
        let job = self.jobs.pop_front().expect("active job");
        self.release_work(&job);
        self.release_output(&job);
        self.settle_failure(job.status(), error);
    }

    /// A failure the store detected. The worker still holds the job, so mark it
    /// cancelled and let the next dispatch abandon it.
    fn fail_active(&mut self, code: AssetErrorCode, message: impl ToString) {
        let job = self.jobs.front_mut().expect("active job");
        job.cancel();
        let error = AssetError::new(code, &job.logical, message);
        let status = job.status();
        self.settle_failure(status, error);
    }

    fn settle_failure(&mut self, status: ImageStatus, error: AssetError) {
        let status = ImageStatus {
            state: ImageState::Failed,
            error: Some(error),
            ..status
        };
        self.forget_lookups(status.id);
        self.entries
            .insert(status.id.number(), Entry::Terminal(status.clone()));
        self.settled.push(status);
        self.counters.failed += 1;
    }

    fn fail_service(&mut self, message: &str) {
        if self.fault.is_none() {
            self.fault = Some(message.to_string());
        }
    }

    // ---- bookkeeping ----------------------------------------------------

    fn live(&self, id: ImageId) -> bool {
        id.session() == self.session
            && matches!(
                self.entries.get(&id.number()),
                Some(Entry::Pending | Entry::Ready(_))
            )
    }

    fn job(&self, id: ImageId) -> Option<&Job> {
        self.jobs.iter().find(|job| job.id == id)
    }

    /// Move a ready image's storage to the retiring list. Its bytes stay
    /// counted until every owner releases them; a logical unload alone does
    /// not make pinned pixels free.
    fn retire_ready(&mut self, id: ImageId) {
        if let Some(Entry::Ready(data)) = self.entries.remove(&id.number()) {
            self.retiring.push(data);
        }
    }

    fn cancel_job(&mut self, id: ImageId) {
        let Some(index) = self.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        if self.jobs[index].started {
            self.jobs[index].cancel();
            return;
        }
        // A queued job owns no worker resources, so drop it immediately.
        let job = self.jobs.remove(index).expect("index from position");
        self.release_work(&job);
        self.release_output(&job);
    }

    fn collect_retiring(&mut self) {
        let mut released = 0;
        self.retiring.retain(|data| {
            if Arc::strong_count(data) == 1 {
                released += data.rgba.len();
                false
            } else {
                true
            }
        });
        self.accounting.rgba = self.accounting.rgba.saturating_sub(released);
    }

    fn release_work(&mut self, job: &Job) {
        self.accounting.encoded = self.accounting.encoded.saturating_sub(job.encoded);
        self.accounting.scratch = self.accounting.scratch.saturating_sub(job.scratch);
    }

    fn release_output(&mut self, job: &Job) {
        self.accounting.rgba = self.accounting.rgba.saturating_sub(job.rgba);
    }

    fn forget_lookups(&mut self, id: ImageId) {
        self.by_path.retain(|_, value| *value != id);
        self.spellings.retain(|_, value| *value != id);
        let spellings = &self.spellings;
        self.spelling_order
            .retain(|key| spellings.contains_key(key));
    }

    fn forget_spelling(&mut self, path: &str) {
        self.spellings.remove(path);
        self.spelling_order.retain(|key| key != path);
    }

    /// Bounded memoization with insertion-order replacement. Dropping a
    /// spelling never unloads its image; the next request just resolves again.
    fn remember_spelling(&mut self, path: &str, id: ImageId) {
        if self.spellings.insert(path.to_string(), id).is_none() {
            self.spelling_order.push_back(path.to_string());
            while self.spelling_order.len() > SPELLING_MEMO_LIMIT {
                if let Some(oldest) = self.spelling_order.pop_front() {
                    self.spellings.remove(&oldest);
                }
            }
        }
    }

    /// Stop admitting, cancel outstanding work, join the worker and release
    /// everything this store owns. Idempotent, and safe to call after a fault.
    pub fn shutdown(&mut self) {
        self.stopped = true;
        // Release what each job reserved rather than resetting the counters:
        // retained storage is shared with published and pinned content, so only
        // a job's own bytes may be dropped here. Encoded staging and scratch
        // reach zero through the same release, which keeps a leak visible
        // instead of hiding it behind a blanket reset.
        for mut job in std::mem::take(&mut self.jobs) {
            job.cancel();
            self.release_work(&job);
            self.release_output(&job);
        }
        for (_, entry) in self.entries.drain() {
            if let Entry::Ready(data) = entry {
                self.retiring.push(data);
            }
        }
        self.by_path.clear();
        self.spellings.clear();
        self.spelling_order.clear();
        self.settled.clear();
        // Wake an idle worker, then close both channels before joining:
        // neither a worker waiting for a grant nor one returning a reply can
        // stay blocked. An in-progress non-preemptible stage may still delay
        // the join, and no worker is ever detached to avoid that.
        if let Some(commands) = self.commands.as_ref() {
            let _ = commands.try_send(Command::Stop);
        }
        drop(self.commands.take());
        drop(self.replies.take());
        self.awaiting = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.collect_retiring();
    }
}

impl Drop for AssetStore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn check_png_path(path: &str) -> Result<(), AssetError> {
    if path.is_empty() || path.len() > PATH_BYTES {
        return Err(AssetError::new(
            AssetErrorCode::Path,
            path,
            format!("asset path must be 1..={PATH_BYTES} bytes"),
        ));
    }
    rooted_path::segments(path, false).map_err(|error| path_error(path, error))?;
    let bytes = path.as_bytes();
    if bytes.len() < 4 || !bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".png") {
        return Err(AssetError::new(
            AssetErrorCode::Path,
            path,
            "asset path must name a .png file",
        ));
    }
    Ok(())
}

fn path_error(path: &str, error: PathError) -> AssetError {
    let code = match error {
        PathError::Io(_) => AssetErrorCode::Io,
        _ => AssetErrorCode::Path,
    };
    AssetError::new(code, path, error)
}
