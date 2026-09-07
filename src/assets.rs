//! Image identities, status snapshots and bounds, plus the optional loader.
//!
//! The identity and status types compile without image decoding, a VM or a
//! window so that owned drawing commands, the renderer and the scripting
//! bindings can all name an image without depending on each other. The store,
//! rooted reader and PNG decoding live behind the `assets` feature.

#[cfg(feature = "assets")]
mod store;
#[cfg(feature = "assets")]
mod worker;

#[cfg(feature = "assets")]
pub use store::{AssetCounters, AssetStore, ImageData};

/// Admitted pending or resident logical images per store. A terminal entry
/// keeps its slot only until the host drains it with `take_settled`.
pub const IMAGE_REGISTRY_LIMIT: usize = 128;
/// Memoized successful request spellings, replaced in insertion order.
pub const SPELLING_MEMO_LIMIT: usize = 256;
/// Admitted unfinished jobs. One is active; the rest wait in FIFO order.
pub const JOB_QUEUE_LIMIT: usize = 8;
/// Encoded bytes accepted for one image, checked against actual reads.
pub const ENCODED_IMAGE_LIMIT: usize = 17 * 1024 * 1024;
/// Aggregate outstanding encoded staging, including partial and failed reads.
pub const ENCODED_STAGING_LIMIT: usize = 34 * 1024 * 1024;
/// Both image dimensions must lie in `1..=DIMENSION_LIMIT`.
pub const DIMENSION_LIMIT: u32 = 2048;
/// Retained RGBA8 for one image.
pub const IMAGE_RGBA_LIMIT: usize = 16 * 1024 * 1024;
/// Reserved, live and pinned RGBA8 across one store.
pub const STORE_RGBA_LIMIT: usize = 64 * 1024 * 1024;
/// One work unit: encoded input read, or RGBA output produced in whole rows.
pub const WORK_QUANTUM: usize = 32 * 1024;
/// Work units in one service pass. A pass also allows one non-preemptible stage.
pub const QUANTA_PER_PASS: usize = 8;
/// Best-effort decoder allocation limit. It is not a process memory guarantee.
pub const DECODER_ALLOC_LIMIT: u64 = 32 * 1024 * 1024;
/// Soft cutoff checked between operations in one service pass. Filesystem
/// calls, allocation, decompression and drivers may still finish later, so this
/// is a scheduling target and not a wall-clock guarantee.
pub const PASS_TARGET: std::time::Duration = std::time::Duration::from_millis(2);
/// UTF-8 bytes retained for one job's error message.
pub const ERROR_MESSAGE_BYTES: usize = 1024;

/// Rows in one conversion band: whole rows sized from the output quantum, and
/// never more than the image has. The store reserves scratch with this and the
/// worker allocates with it, so the two cannot disagree.
#[cfg(feature = "assets")]
pub(crate) fn band_rows(width: usize, height: usize) -> usize {
    (WORK_QUANTUM / (width * 4)).max(1).min(height)
}

/// A logical image within one store session.
///
/// Both halves are monotonic scalars with private construction: the session
/// counter is process-unique and the image number is append-only, so no
/// identity is issued twice even when a publication fails and burns its number.
/// Keeping it a plain `Copy` scalar is what lets owned draw commands stay
/// `Copy` and lets a stale identity be refused by inspection rather than by
/// reasoning about a reusable address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageId {
    session: u64,
    number: u32,
}

impl ImageId {
    #[cfg(feature = "assets")]
    pub(crate) fn new(session: u64, number: u32) -> Self {
        Self { session, number }
    }

    pub fn session(self) -> u64 {
        self.session
    }

    pub fn number(self) -> u32 {
        self.number
    }
}

/// CPU-side lifecycle of one logical image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageState {
    /// Admitted and waiting behind another job.
    Queued,
    /// The active job is reading, decoding or converting.
    Loading,
    /// Validated, immutable RGBA8 content is published.
    Ready,
    /// The job failed after admission; `ImageStatus::error` describes it.
    Failed,
    /// Explicitly unloaded. Cancellation by unload is not a job failure.
    Unloaded,
}

impl ImageState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Unloaded => "unloaded",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Unloaded)
    }
}

/// Coarse progress within a job.
///
/// Decoder construction, including the whole native frame an interlaced PNG
/// decodes eagerly, reports as `Decode`. The largest non-preemptible step
/// deliberately has no separate stage, so polling cannot distinguish it from
/// row decoding and no caller can mistake it for incremental work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStage {
    Waiting,
    Read,
    Header,
    Allocate,
    Decode,
    Complete,
}

impl LoadStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Read => "read",
            Self::Header => "header",
            Self::Allocate => "allocate",
            Self::Decode => "decode",
            Self::Complete => "complete",
        }
    }
}

/// GPU residency, tracked separately from CPU readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuResidency {
    /// No graphics context is attached to this store.
    Unavailable,
    Pending,
    Resident,
    Released,
}

impl GpuResidency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Pending => "pending",
            Self::Resident => "resident",
            Self::Released => "released",
        }
    }
}

/// Why a request was refused, or why an admitted job failed.
///
/// `Path` and `Io` also occur before admission, where they are ordinary
/// catchable refusals that publish no handle. After admission every failure is
/// an inspectable failed job rather than a stopped session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetErrorCode {
    /// Refused by the path policy: spelling, traversal, links or node type.
    Path,
    /// An operating system error opening or reading the file.
    Io,
    /// The bytes are not a PNG this decoder accepts.
    Format,
    /// A valid PNG feature outside this milestone: APNG or 16-bit channels.
    Unsupported,
    /// A fixed engine bound: encoded bytes, dimensions, queue or registry.
    Limit,
    /// Retained or pinned storage left no room for this job.
    Capacity,
}

impl AssetErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Io => "io",
            Self::Format => "format",
            Self::Unsupported => "unsupported",
            Self::Limit => "limit",
            Self::Capacity => "capacity",
        }
    }
}

/// An owned diagnostic. `path` is the logical request spelling, never a host path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetError {
    pub code: AssetErrorCode,
    pub path: String,
    pub message: String,
}

impl AssetError {
    #[cfg(feature = "assets")]
    pub(crate) fn new(code: AssetErrorCode, path: &str, message: impl ToString) -> Self {
        let mut message = message.to_string();
        // Diagnostics are copied into VM-owned values, so cap them here rather
        // than letting a decoder or OS message set the retained size.
        if message.len() > ERROR_MESSAGE_BYTES {
            let mut end = ERROR_MESSAGE_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self {
            code,
            path: path.to_string(),
            message,
        }
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.code.as_str(),
            self.message,
            self.path
        )
    }
}

impl std::error::Error for AssetError {}

/// An owned snapshot. Nothing here borrows the store, so a caller may keep it
/// across service passes or copy it into VM-owned values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageStatus {
    pub id: ImageId,
    pub state: ImageState,
    pub stage: LoadStage,
    pub bytes_read: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub error: Option<AssetError>,
    pub gpu: GpuResidency,
}

/// A completed renderer upload, acknowledged at the next update boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadAck {
    pub id: ImageId,
    pub residency: GpuResidency,
}

#[cfg(all(test, feature = "assets"))]
mod tests {
    use super::{DIMENSION_LIMIT, IMAGE_RGBA_LIMIT, WORK_QUANTUM, band_rows};

    /// The store reserves one native frame and one conversion band per job, and
    /// the frozen contract caps them at 16 MiB and 32 KiB. Neither is refused
    /// against a budget at runtime: both fall out of the dimension bound, so
    /// prove them here rather than leaving the arithmetic to inspection.
    ///
    /// The sweep is exhaustive for the worst case. `band_rows` clamps with
    /// `min(height)`, which only lowers the row count, so the largest band for
    /// any width is the one a tall image produces; the small heights cover the
    /// clamped branch, and both products grow with the channel count.
    #[test]
    fn band_and_frame_reservations_stay_inside_their_frozen_ceilings() {
        let limit = DIMENSION_LIMIT as usize;
        for width in 1..=limit {
            for height in [1, 2, 3, 7, 8, limit - 1, limit] {
                let rows = band_rows(width, height);
                assert!(rows >= 1, "{width}x{height}: empty band");
                assert!(rows <= height, "{width}x{height}: band exceeds the image");
                for channels in 1..=4 {
                    assert!(
                        width * height * channels <= IMAGE_RGBA_LIMIT,
                        "{width}x{height}x{channels}: native frame over its ceiling"
                    );
                    assert!(
                        rows * width * channels <= WORK_QUANTUM,
                        "{width}x{height}x{channels}: conversion band over its ceiling"
                    );
                }
            }
        }
    }
}
