//! The bounded asset worker: file reads, decoder construction, native pixel
//! expansion and RGBA conversion for one job at a time.
//!
//! The worker owns no VM, kernel, plugin, renderer or GPU object, and it
//! decides no policy: the store grants every allowance, reserves every buffer
//! before it is allocated, and publishes results. Exactly one command may be
//! outstanding, so the worker performs one bounded pass per grant and a busy
//! worker accumulates no missed credits.

use super::{
    AssetError, AssetErrorCode, DECODER_ALLOC_LIMIT, DIMENSION_LIMIT, ENCODED_IMAGE_LIMIT, ImageId,
    PASS_TARGET, WORK_QUANTUM,
};
use image::{ColorType, ImageDecoder, codecs::png::PngDecoder};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender},
    },
    time::Instant,
};

pub(super) enum Command {
    Start {
        id: ImageId,
        path: PathBuf,
        logical: String,
        cancel: Arc<AtomicBool>,
    },
    /// Read at most `allowance` bytes, in whole quanta, into the encoded buffer.
    Read {
        allowance: usize,
    },
    /// Parse the header. Non-preemptible, and the only stage in its pass.
    Header,
    /// Allocate the output and conversion band the store has already reserved.
    Allocate {
        width: usize,
        height: usize,
        channels: usize,
    },
    /// Construct the reader, then convert up to `bands` complete row bands.
    Decode {
        bands: usize,
    },
    /// Release the active job's resources without publishing anything.
    Abandon,
    Stop,
}

pub(super) enum Reply {
    Started,
    Read {
        bytes: usize,
        calls: u64,
        eof: bool,
    },
    Header {
        width: u32,
        height: u32,
        channels: usize,
    },
    Allocated,
    Decoded {
        bands: u64,
    },
    Done {
        bands: u64,
        rgba: Vec<u8>,
    },
    Failed(AssetError),
    /// The job released everything and published nothing, whether it observed
    /// the cancel flag or was abandoned outright.
    Cancelled,
    /// The store sent a command with no active job, or a stage arrived out of
    /// order. That is an invariant violation on the runtime thread rather than
    /// a recoverable asset failure.
    Protocol,
}

impl Reply {
    /// Whether the worker has released this job's file, decoder, encoded bytes
    /// and scratch, so the store may release its matching reservations.
    pub(super) fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Failed(_) | Self::Cancelled | Self::Protocol
        )
    }
}

struct Job {
    id: ImageId,
    logical: String,
    cancel: Arc<AtomicBool>,
    file: Option<File>,
    encoded: Vec<u8>,
    decoder: Option<PngDecoder<Cursor<Vec<u8>>>>,
    reader: Option<Box<dyn Read>>,
    width: usize,
    height: usize,
    channels: usize,
    rows: usize,
    row: usize,
    rgba: Vec<u8>,
    native: Vec<u8>,
}

impl Job {
    fn new(id: ImageId, logical: String, cancel: Arc<AtomicBool>) -> Self {
        Self {
            id,
            logical,
            cancel,
            file: None,
            encoded: Vec::new(),
            decoder: None,
            reader: None,
            width: 0,
            height: 0,
            channels: 0,
            rows: 0,
            row: 0,
            rgba: Vec::new(),
            native: Vec::new(),
        }
    }

    fn error(&self, code: AssetErrorCode, message: impl ToString) -> Reply {
        Reply::Failed(AssetError::new(code, &self.logical, message))
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn start(&mut self, path: &Path) -> Reply {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => return self.error(AssetErrorCode::Io, error),
        };
        // Metadata length is an early refusal only. The accepted size is still
        // decided by bytes actually read, plus the one-byte overflow probe.
        let length = match file.metadata() {
            Ok(meta) if meta.len() > ENCODED_IMAGE_LIMIT as u64 => {
                return self.error(
                    AssetErrorCode::Limit,
                    format!("encoded PNG exceeds {ENCODED_IMAGE_LIMIT} bytes"),
                );
            }
            Ok(meta) => meta.len() as usize,
            Err(error) => return self.error(AssetErrorCode::Io, error),
        };
        // One exact allocation for the whole file, from the length just refused
        // against, plus the probe. Growing by a grant per pass instead would
        // reallocate and copy every pass at a cost proportional to the bytes
        // already read, and that copy would sit outside the pass cutoff. A file
        // that grows past its metadata still cannot pass the per-image cap,
        // because the store bounds every grant by the bytes already read.
        self.encoded.reserve_exact(length + 1);
        self.file = Some(file);
        Reply::Started
    }

    fn read(&mut self, allowance: usize) -> Reply {
        let Some(mut file) = self.file.take() else {
            return Reply::Protocol;
        };
        let reply = self.read_into(&mut file, allowance);
        self.file = Some(file);
        reply
    }

    fn read_into(&mut self, file: &mut File, allowance: usize) -> Reply {
        // The destination was sized once at `start`, so every operation this
        // pass performs is inside the cutoff below.
        let start = Instant::now();
        let mut block = [0u8; WORK_QUANTUM];
        let (mut bytes, mut calls, mut eof) = (0usize, 0u64, false);
        while bytes < allowance {
            if self.cancelled() {
                return Reply::Cancelled;
            }
            let want = WORK_QUANTUM.min(allowance - bytes);
            let read = match file.read(&mut block[..want]) {
                Ok(read) => read,
                Err(error) => return self.error(AssetErrorCode::Io, error),
            };
            calls += 1;
            if read == 0 {
                eof = true;
                break;
            }
            self.encoded.extend_from_slice(&block[..read]);
            bytes += read;
            if start.elapsed() >= PASS_TARGET {
                break;
            }
        }
        Reply::Read { bytes, calls, eof }
    }

    fn header(&mut self) -> Reply {
        self.file = None;
        let mut limits = image::io::Limits::default();
        // Checked immediately after header parsing and before any output
        // allocation. These, together with the engine's own encoded cap, are
        // the effective bounds against a hostile file; max_alloc is
        // best-effort and excludes caller-supplied buffers.
        limits.max_image_width = Some(DIMENSION_LIMIT);
        limits.max_image_height = Some(DIMENSION_LIMIT);
        limits.max_alloc = Some(DECODER_ALLOC_LIMIT);
        let encoded = std::mem::take(&mut self.encoded);
        let decoder = match PngDecoder::with_limits(Cursor::new(encoded), limits) {
            Ok(decoder) => decoder,
            Err(error) => return self.error(classify(&error), error),
        };
        if decoder.is_apng() {
            return self.error(AssetErrorCode::Unsupported, "APNG is not supported");
        }
        // EXPAND already widened palette, tRNS and sub-byte grayscale; it
        // deliberately does not narrow 16-bit samples, which surface here.
        let channels = match decoder.color_type() {
            ColorType::L8 => 1,
            ColorType::La8 => 2,
            ColorType::Rgb8 => 3,
            ColorType::Rgba8 => 4,
            other => {
                return self.error(
                    AssetErrorCode::Unsupported,
                    format!("unsupported PNG sample format: {other:?}"),
                );
            }
        };
        let (width, height) = decoder.dimensions();
        self.decoder = Some(decoder);
        Reply::Header {
            width,
            height,
            channels,
        }
    }

    fn allocate(&mut self, width: usize, height: usize, channels: usize) -> Reply {
        self.width = width;
        self.height = height;
        self.channels = channels;
        // Whole rows per band, sized from the RGBA output quantum. One row can
        // never exceed a quantum because the width bound is 2048.
        self.rows = super::band_rows(width, height);
        self.rgba = vec![0; width * height * 4];
        self.native = vec![0; self.rows * width * channels];
        Reply::Allocated
    }

    fn decode(&mut self, bands: usize) -> Reply {
        if self.reader.is_none() {
            let Some(decoder) = self.decoder.take() else {
                return Reply::Protocol;
            };
            // Non-preemptible, and the whole native frame for an interlaced
            // PNG. Phase 0 deliberately pins this deprecated 0.24.9 entry
            // point; a decoder upgrade must revisit its row and cancellation
            // behavior before reusing this path.
            #[allow(deprecated)]
            let reader = match decoder.into_reader() {
                Ok(reader) => reader,
                Err(error) => return self.error(classify(&error), error),
            };
            self.reader = Some(Box::new(reader));
            return Reply::Decoded { bands: 0 };
        }
        let Some(mut reader) = self.reader.take() else {
            return Reply::Protocol;
        };
        let reply = self.convert(&mut reader, bands);
        self.reader = Some(reader);
        reply
    }

    fn convert(&mut self, reader: &mut Box<dyn Read>, bands: usize) -> Reply {
        let start = Instant::now();
        let mut done = 0u64;
        while done < bands as u64 && self.row < self.height {
            if self.cancelled() {
                return Reply::Cancelled;
            }
            let rows = self.rows.min(self.height - self.row);
            let pixels = rows * self.width;
            if let Err(error) = reader.read_exact(&mut self.native[..pixels * self.channels]) {
                return self.error(AssetErrorCode::Format, error);
            }
            let channels = self.channels;
            let destination = &mut self.rgba[self.row * self.width * 4..][..pixels * 4];
            for (source, out) in self.native[..pixels * channels]
                .chunks_exact(channels)
                .zip(destination.chunks_exact_mut(4))
            {
                // Straight alpha, top to bottom, no gamma or premultiplication.
                match channels {
                    1 => out.copy_from_slice(&[source[0], source[0], source[0], 255]),
                    2 => out.copy_from_slice(&[source[0], source[0], source[0], source[1]]),
                    3 => out.copy_from_slice(&[source[0], source[1], source[2], 255]),
                    _ => out.copy_from_slice(source),
                }
            }
            self.row += rows;
            done += 1;
            if start.elapsed() >= PASS_TARGET {
                break;
            }
        }
        if self.row < self.height {
            return Reply::Decoded { bands: done };
        }
        match reader.read(&mut [0]) {
            Ok(0) => {}
            Ok(_) => return self.error(AssetErrorCode::Format, "PNG produced excess pixel data"),
            Err(error) => return self.error(AssetErrorCode::Format, error),
        }
        Reply::Done {
            bands: done,
            rgba: std::mem::take(&mut self.rgba),
        }
    }

    fn step(&mut self, command: Command) -> Reply {
        if self.cancelled() {
            return Reply::Cancelled;
        }
        match command {
            Command::Read { allowance } => self.read(allowance),
            Command::Header => self.header(),
            Command::Allocate {
                width,
                height,
                channels,
            } => self.allocate(width, height, channels),
            Command::Decode { bands } => self.decode(bands),
            // The store cancels a job before it abandons one, so the check
            // above normally answers first. Releasing on the command as well
            // keeps that ordering from being load-bearing: either way the job
            // publishes nothing and the store may reclaim its reservations.
            Command::Abandon => Reply::Cancelled,
            Command::Start { .. } | Command::Stop => Reply::Protocol,
        }
    }
}

/// Keep the frozen failure codes distinguishable: a header the library refuses
/// for exceeding a limit is not the same diagnostic as malformed bytes.
fn classify(error: &image::ImageError) -> AssetErrorCode {
    match error {
        image::ImageError::Limits(_) => AssetErrorCode::Limit,
        image::ImageError::Unsupported(_) => AssetErrorCode::Unsupported,
        image::ImageError::IoError(_) => AssetErrorCode::Io,
        _ => AssetErrorCode::Format,
    }
}

/// One worker thread per live store. It exits when either channel closes, so a
/// dropped store leaves it neither waiting for a grant nor blocked on a reply.
pub(super) fn run(commands: Receiver<Command>, replies: SyncSender<(ImageId, Reply)>) {
    let mut job: Option<Job> = None;
    // Sessions start at one, so this names no image until the first job starts.
    let mut current = ImageId::new(0, 0);
    while let Ok(command) = commands.recv() {
        let reply = match command {
            Command::Stop => break,
            Command::Start {
                id,
                path,
                logical,
                cancel,
            } => {
                current = id;
                let mut started = Job::new(id, logical, cancel);
                let reply = if started.cancelled() {
                    Reply::Cancelled
                } else {
                    started.start(&path)
                };
                job = matches!(reply, Reply::Started).then_some(started);
                reply
            }
            command => match job.as_mut() {
                Some(active) => {
                    current = active.id;
                    active.step(command)
                }
                None => Reply::Protocol,
            },
        };
        if reply.terminal() {
            job = None;
        }
        if replies.send((current, reply)).is_err() {
            break;
        }
    }
}
