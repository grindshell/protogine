# PNG/sprite Phase 0: contracts and feasibility

**Status:** Phase 0 complete, 2026-09-06. Production asset APIs
and the shared renderer are not implemented. This record resolves the Phase 0
contracts in [ADR-002](PNG_SPRITE_PLAN.md); its concrete choices supersede the
earlier alternatives in that plan.

## Selected implementation

Use one bounded asset worker per live store, with at most one active file/decoder
and seven waiting jobs. The runtime thread admits requests, owns identities and
VM wrappers, grants work, and publishes completed results. The worker owns file
reading, decoder construction, native pixel expansion, and RGBA conversion. It
never owns or calls a VM, kernel, plugin, renderer, or GPU object.

The alternative of doing every decoder call cooperatively on the runtime thread
fails the responsiveness goal: the pinned interlaced reader constructor took
about 20 ms, and compressed ICC metadata processing took about 15 ms. Keep
`image` 0.24.9 / png 0.17.16 and use `PngDecoder::with_limits` followed by its
row reader. `into_reader` is deprecated in this pinned release; isolate its use
and require new feasibility evidence before a future decoder upgrade.

For ordinary PNGs, preserve the reader between passes and request complete native
row bands, converting into a reserved RGBA destination. The reader can decode
one extra native row ahead of the caller's requested output. For interlaced PNGs,
reader construction performs the bounded whole native frame on the worker; later
bands only copy/convert that frame. Header parsing, interlaced decode and texture
allocation are explicitly non-preemptible stages, not claimed incremental work.

Keep immutable CPU RGBA until explicit `unload` or session termination. This
preserves the session snapshot, supports headless inspection, and keeps upload
independent of filesystem changes. No automatic LRU, GPU-only eviction, pixel
editing, or dedicated script cancellation API is included. These choices keep
the accepted manual eviction useful without introducing two residency APIs.

## Work grants, bounds and ordering

| Resource | Frozen initial policy |
| --- | --- |
| Registry | 128 pending/ready logical images per store; failed/unloaded entries leave the live registry |
| Request spellings | 256 memo entries with replacement; removing a spelling does not unload its image |
| Job queue | 8 admitted unfinished jobs, one active worker job; FIFO order; cache hits coalesce |
| Encoded image | 17 MiB, actual reads plus a one-byte overflow probe |
| Encoded storage | 34 MiB outstanding capacity including probes and staging copies; reserve before growth, never rely on Vec's implicit doubling |
| Dimensions | Integer width and height in `1..=2048`; checked products |
| CPU RGBA | 16 MiB/image; 64 MiB total reserved/live/pinned output per store |
| Scratch | One active decoder with 32 MiB best-effort library limit; separately reserve up to 16 MiB for interlaced native output and 32 KiB for conversion bands |
| Work quantum | Up to 32 KiB encoded input or RGBA output in complete rows; native row lookahead is additional decoder scratch, at most 8 KiB |
| Worker grant | At most eight quanta and one non-preemptible stage per service pass; stop between operations after a soft 2 ms target; no grant accumulation |
| GPU pass | At most 256 KiB transferred in up to eight row bands, one destination allocation, and a soft 2 ms target checked between operations |
| Script attempts | 256 asset calls/callback shared by request, status, size and unload; malformed calls, refusals and cache hits count |
| Rendering | Existing 10,000 total commands and 10,000 sprite attempts; at most 128 lifetime GPU allocation slots/context |

Byte/storage ceilings are enforceable accounting rules. The 2 ms targets are
scheduling cutoffs, not wall-clock guarantees. Filesystem calls, allocation,
decompression and the driver may finish later. `max_alloc` is best-effort and
does not bound all library/process allocations. In particular, png's bounded ICC
decompression can consume work before discarding an over-limit profile. No total
process-memory or driver-reclamation guarantee follows from the table.

One pass moves at most eight quanta, so a large image is frame-bound rather than
worker-bound. A 17 MiB encoded file is 544 read quanta, its 16 MiB of RGBA output
is 512 conversion bands, and its upload is 64 GPU passes; an image starts uploading
only once its CPU content is complete, so that is about 196 service passes, roughly
3.3 seconds at 60 Hz. The Kenney sheet needs about ten. Scripts must therefore
expect multi-second readiness for maximum-size images and draw a loading state,
and Phase 4's sample cannot treat a small sheet as representative of that wait.
Raising the per-pass multiplier would revise the frozen grant above, so it needs
its own measured evidence rather than a Phase 1-3 convenience adjustment.

At most one grant and one reply may be outstanding on bounded channels. A busy
worker receives no extra credits; a later frame cannot accumulate a burst of
missed allowances. At most one job has reader/decoder scratch. Reserve its output
after validating the header and before allocation. If retained/pinned RGBA leaves
insufficient capacity, fail that job as `capacity`; do not let it stall the FIFO
indefinitely. Never admit eight partial buffers that collectively prevent any
job from completing. All cancellation, failed admission, and late-result paths
release their reservations exactly once, when the resources actually cease to
be owned; logical unload alone does not make pinned bytes free.

Request path validation and rooted canonical resolution remain synchronous on
cache misses to preserve canonical-path coalescing and identical userdata before
returning a handle. This performs metadata I/O, never file-content reads/decode.
Check the script deadline before and after it; it is still subject to filesystem
latency and cannot promise a nonblocking callback. Successful spelling cache hits
avoid it. This is a deliberate limit of the initial unpacked-bundle contract.

One valid `GameRuntime::frame` starts one CPU service pass, regardless of zero,
one or five catch-up ticks. `step` starts one pass; standalone `ScriptHost::update`
does likewise. Internal update calls within `frame` do not grant again. No work
is granted by `draw` or an invalid frame/alpha call. An explicit host-only
`advance_assets` operation supports preload tools without invoking scripts or
advancing simulation; it uses exactly the same service and limits.

At the first update boundary, publish the previously received worker results and
renderer acknowledgements, then run update and fixed systems in today's order.
Do not poll again between catch-up ticks. Zero-tick frames may advance work but
hold new script-visible snapshots until the next update. `init` may enqueue;
preload helpers explicitly commit snapshots before the first measured update.
Script callback deadlines remain unchanged and exclude these host service passes.

The Player's order is: service GPU uploads from the last committed CPU snapshot;
queue acknowledgements; runtime frame/step; draw callback; renderer submission.
GPU work remains on the context's owning thread. Draw traversal never reads,
decodes, allocates textures or advances uploads. Skipping an upload-pending sprite
does not reorder other commands. A clear still discards earlier drawing.

## Script API and lifecycle

| API | Contract |
| --- | --- |
| `ctx.assets.request_png(path)` | Init/update only; returns the canonical pending/ready handle. Coalesced and ready hits preserve `rawequal` and table-key identity. An explicit retry after failure/unload receives a new image identity. |
| `ctx.assets.status(image)` | All live callbacks; owned read-only snapshot described below. Terminal status stays inspectable on retained handles in the same live session. |
| `ctx.assets.size(image)` | All live callbacks; returns width, height once validated; refuses while unknown, failed or unloaded. |
| `ctx.assets.unload(image)` | Init/update only; invalidates the logical image for every alias, removes path/spelling lookups and schedules resource retirement. Returns true on the first unload and false for an already failed/unloaded handle in this session. Pending unload cancels the job internally. |
| `ctx.draw.sprite(image, x, y, options?)` | Draw only; requires CPU-ready live image, validates the existing ADR-002 scalar/crop contract, and appends an owned command. Pending/failed/unloaded handles are catchable refusals. |

Status has `state` (`queued`, `loading`, `ready`, `failed`, `unloaded`), `stage`
(`waiting`, `read`, `header`, `allocate`, `decode`, `complete`), `bytes_read`,
`width`/`height` when known, and `error` when failed. Reader construction, including
the whole interlaced native frame, reports as `decode`; the largest non-preemptible
step deliberately has no separate script-visible stage, so a poll cannot distinguish
it from row decoding. Error is an owned table with
`code`, logical `path`, and a UTF-8 `message` capped at 1024 bytes. `gpu` is
`unavailable` in a headless host, otherwise `pending`, `resident` or `released`.
CPU-ready and GPU residency are distinct; `state == "ready"` means validated CPU
content. Scripts can draw a loading screen until `gpu == "resident"` in Player.
The renderer skips CPU-ready sprites whose GPU upload is pending, without forcing
work or faulting the session. Missing/foreign/unloaded IDs still refuse.

Every asset operation checks session identity, callback expiry and phase before
access. Foreign or stopped-session handles refuse catchably. Status metadata and
terminal diagnostics retained by Luau use VM-owned strings/tables, not a growing
Rust terminal-history map. The userdata keeps its scalar identity and bounded
terminal state; live canonical wrappers leave the strong cache on failure/unload.
Rust tools receive owned status/error snapshots and no immortal terminal registry.

Malformed paths/arguments, wrong phases, missing files or refused rooted paths at
request time, and queue/registry admission exhaustion return catchable errors
without publishing a handle. Read/header/decode failures after admission publish
failed jobs with codes `io`, `format`, `unsupported`, `limit` or `capacity`.
These job failures are inspectable and do not stop gameplay. Cancellation caused
by unload produces `unloaded`, not a failed-job diagnostic. Normal slice exhaustion
yields. API-attempt abuse and callback deadline failures stay latched outside
`pcall`/`xpcall`. Worker panic/disconnect, inconsistent IDs/accounting, impossible
completion shape or renderer invariants are fatal service faults and use runtime
teardown/Player code 3. Driver OOM/panic and stuck OS work are not contained.

Unload rejects future lookups/draws immediately, but does not clear/reuse a GPU
slot referenced by already queued work. Pin resources used by the current
published/pending frame until that frame is replaced/discarded and submitted GPU
work is flushed. Rust copies of old commands stay inspectable but cannot be newly
rendered after explicit unload. Rejected alpha still preserves command data; it
does not reverse a separately performed unload. Upload cancellation discards any
tentative GPU mapping and retires its private slot at the same safe boundary.

Cancellation sets a per-job flag, revokes queued grants and suppresses publication
by full session/image identity. Check it between operations and again when the
runtime receives completion; a late worker result cannot recreate an unloaded
entry. On stop, close grant/reply channels so neither a worker waiting for a grant
nor one returning a reply can remain blocked; release queued payloads. Join the single
worker before releasing its store or starting a replacement session; never detach
it and accumulate workers/buffers across restarts. An in-progress non-preemptible
operation may delay that join. Preserve VM destruction before native teardown;
the asset worker cannot hold VM or native-plugin references.

## Rust boundaries to implement in Phases 1–3

`AssetStore` owns admission, limits, immutable decoded images, job channels and
status. Its request/unload/status/size/image accessors and service-pass operation
are independent of mlua/Macroquad. Use scalar `ImageId` and owned `UploadAck`
(session, image, completed residency state) types in the assets module. Validate
every acknowledgement against the current live job; silently discard identified
late acknowledgements for unloaded/terminated images, fault on malformed current
ones. `ScriptHost` owns scoped wrappers; `GameRuntime` forwards a read-only view,
upload acknowledgements and the explicit host preload pump.

`MacroquadRenderer` exposes `attach(session)`, `service_uploads(store, budget)`,
`render(store, commands)` and `retire()`. Attachment allocates no full-size image.
Upload service returns bounded owned acknowledgements for the next update
boundary. It retains no borrowed store references across calls. Queued texture
owners and any temporary immutable CPU owners survive until their work completes;
account those owners until released. Retire is idempotent and flushes/discards
queued work before shrinking slots to transparent 1x1. Never recreate the pool
for a runtime restart. Drop it only inside the context future after final readback.

## Deterministic validation

Use the same loader and grant/pump functions for production and tests. A bounded
preload/drain helper may wait for replies and pump uploads outside simulation,
with a separate 10-second watchdog. It does not reset a Luau deadline, invoke
update, or consume gameplay RNG. Headless measured traces start after init assets
are ready. Mid-game loading tests use an explicit recorded completion schedule.

Capture mode drains outstanding requested assets after init and each measured
step, then calls draw and captures as today; service-only frames do not increment
the capture's simulation/frame counter. The drain includes GPU acknowledgements
and commits its snapshot before draw. Failed jobs count as settled; shaders cannot
silently wait forever for them. A timed-out drain fails capture rather than saving
an incomplete image. Interactive loading remains asynchronous. Preserve seeded
VM startup, neutral capture input, alpha 0, shutdown exactly once, and texture
ownership through `get_screen_data`. Production completion ticks may vary by
machine/frame rate; the fixed simulation timestep does not make I/O deterministic.

## Probe and observations

The [Rust probe](../../examples/png_sprite_probe.rs) exercises pinned dependency
APIs directly. It is neither the future renderer nor a second copy of the
Player renderer. It reuses the actual Player PNG writer. The
[fixture generator](../../tools/png_probe_fixtures.py) uses Python stdlib and
independent PNG/expected-pixel specifications; the Rust probe independently
constructs Adam7 scanlines and checks against its original RGBA bytes.

```text
cargo build --release --example png_sprite_probe --locked --offline
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode cpu
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu-recreate
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu-early-retire
```

The runner enforces independent 10-second CPU / 30-second GPU child watchdogs;
negative controls must fail at their named assertions, not merely exit nonzero.
Generated fixtures, captures and full logs go to `target/png-sprite-probe/`.

Observed on Windows MSVC x64, rustc 1.95.0, OpenGL `3.1.0 NVIDIA 610.62`.
The following are individual release observations on this machine, with warm/cold
cache and OS scheduling uncontrolled, not throughput promises or benchmark percentiles.

| Input/stage | Observed work / time |
| --- | --- |
| Incompressible RGBA 2048x2048 | 16,780,612 encoded bytes; 514 read calls including EOF, 12.812 ms summed read/copy time; 512 decode bands, 3.649 ms decode + 2.066 ms conversion |
| Adam7 RGBA 2048x2048 | 16,782,404 bytes; 12.749 ms read/copy; 19.259 ms reader construction + 0.709 ms band copies + 2.322 ms conversion |
| Adam7 again on the gated worker | 19.277 ms reader construction + 1.140 ms band copies + 15.006 ms conversion; 551 main-thread polls |
| Kenney 784x352 indexed sheet | 17,497 bytes; 0.099 ms read/copy; 36 output bands; 0.307 ms decode + 0.839 ms conversion |
| Encoded cap boundary | 17,825,792 bytes admitted in 545 read calls, being 544 quanta plus the one-byte overflow probe at EOF, and decoded exactly; 17,825,793 bytes refused as `encoded limit` before header parsing |
| Compressed metadata representing 40 MiB | 40,867-byte files; ignored zTXt header 0.045 ms; over-limit iCCP header 15.312 ms, pixels preserved |
| 2048 GPU upload | 512 region calls, 3.268 ms summed loop; 64 actual passes across frames at 256 KiB/pass; largest pass 0.128 ms |
| Kenney GPU upload | 36 region calls, 0.163 ms total |

The worker row repeats the same interlaced input through the gated mailbox. Its
conversion cost is several times the main-thread row above it, reproducibly, because
the worker parks and unparks between bands and loses cache locality. The selected
design pays that cost, so the worker row is the representative conversion figure.
It does not threaten the budget: about 0.029 ms per band leaves eight bands far
below the 2 ms pass target.

GPU destination allocation returned in 0.009 ms (2048) / 0.007 ms (Kenney).
Drivers may defer real allocation cost to later calls; this measures API return
time only. CPU zeroed allocation is similarly affected by lazy page commitment.

Exact checks passed for the large ordinary/interlaced originals, a copy padded to
exactly the encoded cap, RGB, grayscale, gray-alpha, palette transparency and
Kenney decoding/upload. Huge dimensions, truncation, one byte over the encoded cap,
APNG and 16-bit input were refused. Each refusal names its fixture, byte count and
reported reason in the receipt, so the distinct causes are visible rather than
inferred, and a control that stopped refusing would fail loudly instead of passing
as a silent assert. Metadata fixtures stress parser
work; the ICC payload deliberately is not a usable color profile.

The worker probe adds an explicit 40 ms stage delay to make scheduler evidence
observable. Main-thread polling continued through that delay and actual Adam7
reader work; cancellation during the delay permitted late reader work to finish
but prevented ready publication. These are dependency/protocol probes, not claims
that production GameRuntime loading or its future queue fairness tests exist.

GPU checks passed 100 empty/one/128-image session cycles with changing dimensions
and contents, identical raw slot IDs, exactly 128 registrations, and every retired
slot reading back as transparent 1x1 (512-byte aggregate pixel payload). Pixel
assertions covered cropped nearest sampling, each flip/both, integer scaling,
guard texels, ordered rectangle/sprite alpha blending, repeated encoded captures,
and dropping CPU pixels after queueing but before final readback.
Recreating a slot failed the registration limit control. Retiring before readback
turned an expected red pixel black and failed the pixel control. Saved receipts:
[CPU](evidence/png-sprite-phase0-cpu.txt),
[GPU](evidence/png-sprite-phase0-gpu.txt), and
[negative controls](evidence/png-sprite-phase0-controls.txt).
The [176x96 capture](evidence/png-sprite-phase0.png) was also visually inspected;
the generated repeat capture had identical PNG bytes.

These observations establish the selected backend path on this graphics stack.
They do not establish cross-GPU exact pixels, driver memory reclamation, production
callback publication/eviction behavior, or full runtime queue safety. Those remain
the explicit behavioral exits of Phases 1–3, using the actual shared service and
Player once implemented.

## Check results and handoff

Passed:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --bin protogine-player
cargo test --workspace --no-default-features --features scripting
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --test player_capture -- --ignored
```

The first sandboxed workspace test run could not compile C fixtures (`clang:
permission denied`). The authorized rerun with toolchain access passed, including
all four non-ignored native tests. No native fixture, engine source, SDK or Player
code was changed to obtain a pass. Physical input and native GPU-specific suites
were not rerun because those implementations were unchanged.

The release probe build and all four watchdog modes passed their positive or
expected-negative assertions. `cargo tree --locked --offline` confirmed image
0.24.9/png 0.17.16 and Macroquad 0.4.16/Miniquad 0.4.11. Cargo.lock was unchanged.
The Kenney source PNG hash remained
`0d9abf9b812a441e1673bc166d61abb6236c0bc46b3775700bd4d4364ba170e0`.

Phase 1 starts with the headless AssetStore and rooted-reader extraction, keeping
the service out of the kernel and production GPU implementation out of that slice.
No correctness/feasibility decision remains open for that phase; the unimplemented
runtime, script, eviction and real-renderer regressions remain its and later
phases' delivery requirements. No commit or push was made.
