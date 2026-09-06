# ADR-002: Bundle PNG assets and sprite drawing

**Status:** Accepted design; Phases 0 and 1 complete. Phases 2-4 not started.
P1-P8, including the P7 manual-eviction extension, accepted 2026-09-06.
The [Phase 0 record](PNG_SPRITE_PHASE0.md) freezes the implementation contracts
and records dependency/worker/GPU feasibility evidence. Phase 1 implemented the
headless asset service; its record is at the end of this document. The Luau
bindings, runtime integration, shared renderer and sample remain unimplemented.
**Date:** 2026-09-06.
**Decider:** Project owner.
**Baseline:** `61a72adb93c94c4a5dada0a7c0384f2f44490976`.

The selected next feature is bundle-local PNG loading, sprite/tileset drawing,
and shared runtime support. This document records the accepted decisions,
implementation contracts to finalize, and phase exits. It does not describe
implemented APIs. Record contract changes and
verification evidence here as work proceeds.

## Outcome and scope

A copied Player runs a small room drawn from a PNG tileset, with an animated,
controllable character drawn from a spritesheet. Replacing either PNG and
restarting the same executable changes the artwork without rebuilding Rust.
The same assets and drawing commands can be validated by the headless runtime.

Include update-time asset requests and staged processing, bounded PNG decoding,
reusable image handles with script-controlled eviction and metadata utilities,
cropped sprite drawing, destination size,
horizontal/vertical flips, tint/opacity, nearest-neighbor
filtering, shared rendering, and behavioral/capture evidence.

Tile layout and animation frame selection belong to sample Luau code. Engine
tilemaps, collision, cameras, rotation/pivots, engine animation components,
asset hot reload, texture packing, other image formats, audio, editor UI, and
export tooling are outside this milestone. Kernel-owned tilemaps and solid-tile
collision remain a likely next feature, not a dependency or deliverable here.
Phase 0 selected bounded worker-assisted processing and logical image unload.
Direct pixel-buffer access and a dedicated script API for
job cancellation remain future extensions; safe internal cancellation/teardown
is required by this milestone.

## Current contracts to preserve

- [Drawing data](../../src/drawing.rs) contains owned `Clear` and `Rect`
  commands and has no graphics dependency. [Script bindings](../../src/scripting/drawing.rs)
  validate before f32 conversion; a successful draw publishes one fresh list.
  Fault/stop clears it. Rejected host `alpha` preserves the accepted list.
- [ScriptHost](../../src/scripting.rs) supplies drawing independently of
  [GameRuntime](../../src/runtime.rs). Keep PNG/sprite authoring available in
  both. Runtime-only world/input/native APIs retain their existing boundaries.
- `GameRuntime::finish_call` releases the entire `ScriptHost` as soon as a
  session reaches `Stopped` or `Faulted`, before native teardown. Everything the
  host owns disappears at that moment, so a host-owned asset store cannot outlive
  termination and renderer-held GPU resources are the only surviving state.
- [Player](../../src/bin/player.rs) owns a private rectangle renderer today.
  Its capture path draws, invokes shutdown, then reads the framebuffer.
  New GPU resources must survive any queued drawing through that readback.
- [Filesystem bindings](../../src/scripting/filesystem.rs) canonicalize roots,
  enforce [portable names](../../src/portable_path.rs), and reject links/reparse
  points below each root. `resolve` does not canonicalize the final node or
  recheck containment; segment validation plus per-descendant link rejection
  carry that weight today. Their 1 MiB file and 8 MiB callback transfer limits
  are utility contracts; PNG loading needs its own accounting.
- Fixed ticks, input edges, world pixels, callback deadlines, VM cancellation,
  native teardown order, and script-owned persistence stay as documented in
  [ADR-001](SCRIPTING_C_API_PLAN.md) and the [agent guide](../../AGENTS.md).
- `game/game.tot` remains the optional version-1 native-plugin manifest.
  This feature adds no manifest fields and no asset scanning.

## Accepted decisions and implementation follow-through

| ID | Status | Decision / implementation follow-through |
| --- | --- | --- |
| P1 | Accepted, revised | Games may request images during update through a staged queue. Init-only synchronous preloading is a test convenience, not a production restriction. Use the Phase 0 service ordering and readiness contracts. |
| P2 | Accepted | Use one CPU decoder shared by headless tools and the Player, feeding GPU upload. Retain CPU RGBA until unload or session end; track GPU residency separately. GPU captures verify rendered pixels; headless tests need no graphics context. |
| P3 | Accepted | PNG paths are relative to the bundle root; no new asset manifest is required. |
| P4 | Accepted | Opaque session IDs and image identities, with a bounded session-local registry/cache. |
| P5 | Accepted | One shared Macroquad renderer in the library. |
| P6 | Accepted | Explicit source rectangles and destination dimensions, flips/tint, and nearest filtering. |
| P7 | Accepted, including extension | `unload(image)` invalidates the logical image for all aliases; retire physical resources after outstanding owners/frames release them. Keep at most 128 reusable GPU slots/context. Separate GPU-only eviction and raw pixels remain deferred. |
| P8 | Accepted; coupled to P1 | One bounded worker processes reads/decode/conversion; the graphics thread transfers complete row bands. Use 32 KiB quanta, eight per grant/pass, soft 2 ms cutoffs and no accumulated credits. Non-preemptible stages and cancellation boundaries are explicit in Phase 0. |

### Headless validation versus repeatable GPU captures

GPU-only loading does not inherently make frames nondeterministic. Repeat seeded
captures still validate the actual renderer on the tested graphics stack. They
require a GPU context, including on a machine that creates an offscreen context
without showing a window. Protogine's existing headless configuration uses no
graphics context at all; it can validate paths, decoded pixels, dimensions,
handle lifetimes, and owned drawing commands if decoding is a shared CPU service.
Final rendered pixels still need the Player capture tests.

P2 establishes one CPU decode path that also feeds GPU upload. Retain RGBA until
logical unload or session retirement; count any remaining pinned owners until
released. This preserves the session snapshot and lets headless tools inspect
real pixels. A future different retention policy requires its own reload and
accounting contract.

### Staged processing

`request -> queued -> reading -> decoding/converting -> CPU ready -> uploading
-> drawable` separates independent work and publication boundaries. Init/update
may enqueue a request and retain an opaque image handle immediately; neither
callback waits for the file to finish. Ready cache hits reuse the handle. Scripts
can inspect status and draw a loading screen while other gameplay continues.

Use an engine-owned destination buffer for encoded chunks; do not expose a
partially decoded image to scripts. A 32 KiB read quantum is a starting point,
not the entire tick allowance. At exactly one quantum per 60 Hz tick, a 17 MiB
file needs 544 ticks, about 9.07 seconds, just for reading. Multiple quanta may
run under an aggregate service budget. Several concurrent requests share that
budget; the allowance must not multiply by the number of jobs.

Chunked reads alone do not spread PNG decompression, RGBA conversion, or texture
allocation/upload. The current whole-image `read_image` call cannot be placed
after EOF and described as incremental loading. The pinned `image` reader has
a row-based path for noninterlaced PNGs, but its interlaced path decodes a whole
frame when creating the reader. Header/metadata processing can also do work
independent of a pixel-row allowance. Phase 0 selected a bounded worker using the
pinned row reader, including its non-preemptible interlaced/metadata stages.
Do not drop interlaced support or substitute a dependency silently.

GPU work remains on the graphics thread. Upload complete RGBA row bands through
bounded region updates, keeping the image unavailable for drawing until every
band is transferred. Texture allocation/resizing is still a non-preemptible
operation that must be measured separately. Bounded bytes reduce work but do
not guarantee a wall-clock deadline for filesystem calls, decoding, or drivers.

Use the exact grant, publication and frontend order in the Phase 0 record: one
CPU grant per frame/step, no repeated grants across catch-up ticks, GPU service
before the runtime frame, and snapshots committed at the first update boundary.
Standalone ScriptHost uses the same CPU service without a renderer. Production
completion ticks may vary. Preload/completion-trace helpers drive the same service;
capture drains pending work without advancing simulation before drawing. They do
not introduce another loader, validation policy or script deadline override.

Asset service slices run outside the Luau callback's execution budget. Preserve
the existing 1 s startup and 100 ms update/draw cancellation contracts for script
execution; do not extend/reset those deadlines on every chunk or request.
Hitting a normal service-slice budget yields until a later pass. A separate job
deadline, if needed, needs an explicit failure/cancellation policy; elapsed time
for the whole staged job is not the duration of one Luau callback.

### Image access, eviction, and cancellation boundaries

The script's image handle is the public view of its session-local cache
entry: identity, status, dimensions when known, and deliberate utility operations.
Drawing passes that handle. The renderer's reusable GPU slot is an implementation
detail and must never become a script-visible mutable texture pointer.

Manual eviction uses `ctx.assets.unload(image)` in init/update. It invalidates
every alias and removes the path lookup; a later request gets a fresh identity.
Status remains inspectable on retained handles in the live session. The Phase 0
record defines idempotence, pending-job cancellation, pinned storage and frame
retirement. Separate GPU-only eviction and raw pixel-buffer access are deferred.

Cancellation should address the job identity and discard its partial buffers,
release file/decoder resources, and prevent late completion from publishing it.
If work runs on a worker, cancellation can suppress publication immediately but
cannot promise to interrupt an in-progress non-preemptible operation. Define
worker shutdown/join behavior as specified in Phase 0: wake, cancel and join one
worker without detached workers or late publication. A dedicated script
cancellation API remains deferred; unloading a pending image already cancels it.

Neither cancellation nor eviction may recycle a GPU slot while queued drawing
still references it. Pending/published frame references need pinning until their
work is submitted/discarded, and releases take effect at a documented boundary.
Implement eviction with resident-count accounting, wrapper-cache
cleanup, stale-handle behavior, and slot reuse together; the default session-end
residency rule below must not be applied to an explicitly evicted image.

## Ownership and feature boundaries

```mermaid
flowchart TD
    App[Player or future editor play session] --> Runtime[GameRuntime]
    Runtime --> Host[ScriptHost]
    Runtime --> Kernel[Kernel: entities and fixed updates]
    Host --> Assets[AssetStore: bounded jobs and ready image entries]
    Host --> Commands[Owned DrawCommand list with ImageId values]
    Assets --> Files[Shared rooted file reader and PNG decoder]
    App --> Renderer[Shared MacroquadRenderer: GPU textures]
    Renderer --> Assets
    Renderer --> Commands
```

- Add image identifier/metadata types to `src/assets.rs`; keep those types and
  drawing data available without graphics or scripting. Put file/decode/store
  implementation behind an `assets` feature enabling the existing `image`
  dependency. No new workspace member is needed.
- `ScriptHost` owns an `AssetStore` rooted at the same canonical bundle as its
  filesystem API, plus the VM handle cache. This follows the existing
  `FileSystem` precedent rather than the runtime-owned `EngineContext` pattern,
  because standalone hosts must load PNGs. `GameRuntime` exposes read-only
  access through its host, and that accessor yields an empty view once the host
  has been released on stop or fault. Rust tools can borrow image
  metadata/pixels; scripts receive metadata copies and handles with scoped
  eviction operations. A raw pixel-buffer API is deferred.
- Store/path/decoder errors and accounting remain independent of mlua. Rust
  loading uses an explicit load budget with the same byte/count limits; bindings
  translate immediate argument errors and expose staged job failures without
  depending on a currently executing Luau callback. Supply service budgets and
  cancellation through a host hook rather than making the asset module depend
  on the scripting host. Publish immutable image contents only on successful
  completion; the registry can accept further requests during update.
- Add `src/scripting/assets.rs` for scoped bindings. Make `scripting` enable
  `assets`. Standalone `ScriptHost` loads real PNGs without a window.
- Add `src/rendering.rs` under a `graphics` feature enabling `assets` and
  Macroquad. Change `player` to enable `graphics`, `scripting`, and
  `native-plugins`. Capture's `image` use is satisfied through `assets`.
- The application owns one renderer for its graphics context's lifetime and
  reuses it across runtime restarts. Session-to-texture mappings expire on
  retirement; a bounded pool of cleared GPU allocation slots remains. This is
  allocation reuse, not a cache of prior game assets or their contents.
- `graphics` does not enable `scripting`, so the renderer cannot reference
  `GameRuntime`. Keep the typed renderer error in `src/rendering.rs`, the
  presentation-fault entry point on `GameRuntime`, and any command validation
  shared by bindings and renderer in a feature-free module: `src/drawing.rs`,
  or the unconditional part of `src/assets.rs`. Neither feature implies the
  other, so shared validation cannot live under either one.
- Keep Macroquad 0.4.16 with default features disabled and the current PNG-only
  `image` dependency. Pin it exactly as `image = "=0.24.9"`: this feature depends
  on specific decoder and limit semantics that a caret range could move
  underneath it.
  Verify the feature graph and lockfile; do not combine this feature with a
  decoder/library upgrade.
- The kernel does not own images, textures, sprite components, or the renderer.
  A future editor drives these same runtime/rendering interfaces. Startup text
  and window management remain Player responsibilities.

| Configuration | Required behavior |
| --- | --- |
| `--no-default-features` | Core and command/identifier types compile without image decoding, Luau, or Macroquad |
| `--no-default-features --features assets` | Rust asset decoding/store tests run without VM or graphics |
| `--no-default-features --features scripting` | Real Luau asset loading and sprite commands work headlessly |
| `--no-default-features --features graphics` | Shared renderer compiles without scripting/native plugins; constructing it requires a live Macroquad context |
| Default `player` / `--all-features` | Existing Player and native behavior plus shared sprite rendering |

## Bundle loading and image lifetime

Example distribution:

```text
My Game/
  protogine-player.exe
  game/
    main.luau
    room.luau
    assets/
      tiles.png
      character.png
```

The proposed `ctx.assets.request_png("assets/tiles.png")` addresses the bundle
root, including when called from a required submodule. It never resolves against
the module directory, working directory, executable directory, or writable data
root. `assets/` is a convention, not a mandatory directory.

1. Validate a UTF-8 string of 1..4096 bytes with portable `/`-separated segments
   and a `.png` suffix, compared ASCII-insensitively. Do not coerce numbers into
   paths. Reject empty/dot/parent segments, absolute/drive/UNC paths, backslashes,
   device names, and other names refused by the existing filesystem policy.
2. On a cache miss, traverse beneath the canonical root, rejecting symlinks and
   Windows reparse points at every descendant, non-directory intermediates, and
   non-regular final nodes. Canonicalize the resolved file and check containment.
   The canonical root itself may have been reached through a link, as today.
3. Key successful loads by the resolved canonical path, using platform path
   equality. Memoize successful request spellings so repeat requests use the
   session snapshot without reopening the file. Case aliases resolving to the
   same canonical path reuse the image; distinct hard-link paths need not dedupe.
   Do not globally lowercase paths on case-sensitive filesystems.
4. Open/read with an encoded-byte cap plus a one-byte overflow probe; metadata
   length is only an early check. Retain bounded reader/decoder state between
   service passes and discard encoded/scratch buffers when no longer needed.
   Do not reread the entire file or restart decoding for every chunk.
5. Reserve a new image identity and publish its pending handle only after job
   admission, path-cache allocation and VM wrapper publication succeed. Roll
   back a failed admission and burn its identity. Pending requests for the same
   canonical path coalesce rather than reading/decoding twice. Ready image data
   is published atomically only after complete validation/decode/conversion.
   Failed/cancelled jobs release work resources and are removed from the path
   lookup so a later explicit request can retry; old handles must not silently
   become that new job. Retain terminal status as specified in Phase 0.
   Counters for attempted work are never rolled back.

Extract a small non-VM rooted-read/path helper from the current filesystem code
where needed; share traversal rules rather than routing assets through
`ctx.fs.read`. The two callers need different final-node policies: `ctx.fs`
keeps today's behavior, while asset loading adds canonicalization and a
containment recheck on the resolved file. Make that step a parameter of the
helper rather than changing `ctx.fs`. Keep existing utility limits, listing
classification, mkdir rollback, and writes intact, and run their regressions
after the extraction. The existing stable-bundle assumption applies: this is
not protection against concurrent hostile filesystem replacement.

Each `ImageId` contains a process-unique session identity and an append-only
image identity with private construction. Use checked session allocation, not
wrapping IDs or reusable addresses. No identity is handed out twice within a session,
including numbers burned by a failed publication: keep the sequence strictly
append-only so the no-reuse rule is auditable by inspection instead of by
re-deriving why an unpublished ID could not have escaped. Update-time requests
can burn more than one callback's allowance over a long session, so use a checked
monotonic counter independent of resident storage slots. Every lookup checks
session identity and entry state. GPU cache keys include the full ID; reusing a
physical allocation slot never reuses the image identity.

Keep `ImageId` a plain `Copy` scalar — a monotonic session counter plus a monotonic
image number — so `DrawCommand` retains its existing `Clone, Copy, Debug, PartialEq`
derivation. Do not model session identity with an `Rc` marker as `EntityHandle`
does: a reusable address cannot satisfy the cross-session rule and would cost
`Copy`.

Coalesced pending requests and ready cache hits return the same Luau userdata,
including `rawequal` and table-key identity. Keep canonical wrappers only while
the corresponding entry occupies the bounded registry; terminal-job and
eviction cleanup must not leave an unbounded strong cache. Retained stale handles
remain VM-owned values and cannot resurrect an entry. Bound request-spelling
memoization explicitly (256 entries with replacement), since per-callback
attempt limits no longer bound a cache's lifetime. Dropping a spelling memo does
not evict the image. Handles may be retained; context functions still expire.

Release asset-store and userdata borrows before VM work, including allocating
metadata tables/wrappers or inspecting option tables. Transfer owned IDs and
metadata across those steps; do not hold a `RefCell` borrow during Lua operations.
Keep publication rollback explicit across the separate Rust and VM allocations.

Successful init does not freeze the registry. Normal shutdown can inspect ready
metadata but admits no new jobs; after its callback, invalidate handles, cancel
pending jobs and release CPU assets. Faults invalidate immediately and skip Luau
shutdown; drop releases resources without invoking scripts. Apply this cleanup
to standalone `ScriptHost` as well as `GameRuntime`. Preserve destruction of VM
references before native teardown.

Owned sprite commands contain IDs and scalar data, not pixels or GPU references.
A Rust copy remains inspectable after shutdown but cannot be rendered against a
stopped or different store. Restart constructs new IDs, rereads files, and builds
a fresh session mapping in the existing renderer. Its cleared allocation pool
survives; no cross-session image-content cache is introduced.

## PNG representation and limits

Accept static PNGs with RGB/RGBA, grayscale/grayscale-alpha, or indexed color,
including palette transparency and 1/2/4-bit grayscale expansion. Decode
interlaced PNGs through the shared CPU service. Reject APNG and 16-bit channels
explicitly for this milestone; do not silently choose an animation frame or narrow 16-bit
samples. Normalize supported input to top-to-bottom, tightly packed RGBA8 with
straight alpha; opaque formats receive alpha 255. Preserve decoded sample bytes
without ICC/gamma correction, premultiplication, or vertical flipping.

The pinned decoder supplies each of these; Phase 0 tested the following mechanisms.
`PngDecoder::with_limits` sets
`png::Transformations::EXPAND`, which is what makes palette expansion, tRNS
alpha, and 1/2/4-bit grayscale work without extra handling, and which
deliberately does not narrow 16-bit samples. So 16-bit input surfaces as
`L16`/`La16`/`Rgb16`/`Rgba16` from `color_type()` on the constructed decoder and
is rejected before any output allocation; APNG is detected with `is_apng()`.
For the whole-image baseline, `read_image` asserts that the supplied buffer
length equals `total_bytes()` and panics on mismatch. That size uses the decoder's
native color type — three bytes per pixel for `Rgb8`, not four. A staged path must
likewise size native rows/scratch from the decoder's representation and account
for RGBA conversion separately; it need not retain a second full-image buffer.
RGBA input may reuse its validated output directly. Use the Phase 0 worker/reader
path rather than calling whole-image `read_image` on the runtime thread after
chunked reads.

| Resource | Bound and accounting (detailed grants/ownership in Phase 0) |
| --- | --- |
| Image registry | 128 admitted pending/resident entries; terminal entries leave the live registry, with retained status owned by VM handles |
| Encoded file | 17 MiB; check actual reads, not only metadata |
| Dimensions | Both integers in `1..=2048` |
| Retained RGBA8 | Checked `width * height * 4`; at most 16 MiB/image and 64 MiB/store |
| `ctx.assets` attempts | 256 per callback, shared by `request_png`, `status`, `size` and `unload`, before argument conversion/phase checks; hits and refusals count |
| In-flight jobs | 8 FIFO jobs sharing grants, one active worker/decoder; waiting jobs reserve no decoder workspace |
| Encoded staging storage | 34 MiB aggregate outstanding encoded data, including failed/partial reads and overflow probes until released; ready cache hits transfer zero |
| Work per service pass | 32 KiB work units, eight per worker grant/GPU pass, soft 2 ms cutoffs; one non-preemptible worker stage / GPU allocation per pass; no accumulated grants |
| Decoder header bound | Pass `max_image_width` and `max_image_height` of 2048 to `PngDecoder::with_limits`, which checks them immediately after header parsing and before `read_info` |
| Decoder allocation limit | One decoder with 32 MiB best-effort `max_alloc`; separately reserve up to 16 MiB native interlaced output and 32 KiB conversion scratch; see Phase 0 for row lookahead |
| Sprite attempts during draw | 10,000, counted before argument/option conversion, including refusals |
| Published drawing commands | Existing combined 10,000-command cap, including clears, rectangles, sprites, and zero-area commands |
| Renderer allocation slots | At most 128 over one graphics context's lifetime, allocated lazily and reused across sessions; every retired slot holds only a transparent 1x1 RGBA8 image |

These are separate from VM heap, utility, world, and native budgets. Asset
metadata queries must not consume the 128-call utility budget. Sprite drawing
must not consume the 256-call asset budget through internal dimension lookups.
Keep existing clear/rectangle accounting unchanged; the new sprite attempt
counter bounds rejected sprite validation as well as accepted calls. That
asymmetry is deliberate: `clear` and `rect` take scalars and are bounded
adequately by the deadline, while a rejected sprite may already have cost two
option-table inspections, so its refusals need their own ceiling.

The encoded cap includes 1 MiB of headroom above the 16 MiB RGBA8 maximum for
scanline filters, compression framing, and chunks. The pinned encoder produced
a valid 2048x2048 RGBA PNG of 16,780,612 bytes in the review probe, exceeding
16 MiB by 3,396 bytes. Both that file and two such files fit the revised 17/34
MiB allowances. Add an incompressible boundary fixture so this case stays covered.

Encoded size remains an independent restriction: arbitrary metadata, chunk
fragmentation, or inefficient encoding can exceed 17 MiB even within 2048x2048.
Document both limits; do not promise that every PNG within the dimension limit
fits. The aggregate staging allowance accommodates two files at the encoded cap,
or more smaller files. It limits concurrent storage, not lifetime throughput:
completed stages release capacity for subsequent requests. A bounded waiting
queue must distinguish temporary capacity pressure from an impossible request.
Admission/reservation must let at least one active job reach completion; partial
buffers from several jobs must not consume all staging capacity and leave every
job waiting for more. Include that case when measuring queue fairness.

Check dimensions/products before output allocation. Bound each read by the
remaining file/staging allowance plus the overflow probe, so a failing read cannot
allocate past the service's storage budget. Check retained
capacity before decoding, reserve work before performing it, and
release all temporary buffers on failure. Avoid cloning RGBA buffers for each
command or upload. Identify encoded storage, decoder workspace, conversion
scratch, retained CPU storage, and GPU storage separately in the implementation.
These are storage bounds and scheduling budgets, not measured latency
guarantees. The former 34 MiB encoded/128 MiB decoded cumulative init-work limits
are superseded by the Phase 0 staged-processing, scratch-reservation and
catch-up contracts.

Invalid arguments, wrong phases and admission/path-resolution errors are
catchable. After admission, I/O/format/unsupported/limit/capacity errors become
inspectable failed jobs, with no partial ready image. Normal slice exhaustion
yields. API-attempt abuse and callback deadlines remain latched outside protected
calls. Worker panic/disconnect and invariant faults stop the session through the
runtime service-fault path. Use the exact Phase 0 classifications/status schema.

Work between yield/cancellation boundaries is not preempted by VM interrupts.
Keep protected script cancellation and independently bound adversarial decoder
fixtures with a 10-second child-process watchdog. Never present an image allocation setting as a total process or GPU
memory guarantee: the decoder's `max_alloc` is best-effort, and driver/allocator
overhead is outside these counters. It is weaker than it looks. `max_alloc`
becomes `png::Limits { bytes }`, which in png 0.17.16 reserves roughly one
output line plus a little metadata, explicitly excludes caller-supplied buffers,
and is documented as best-effort. The effective bounds against a hostile file
are therefore the engine's own encoded-byte cap and the `max_image_width`/
`max_image_height` header check, not `max_alloc`. See the
[image 0.24.9 limit contract](https://docs.rs/image/0.24.9/image/io/struct.Limits.html).

## Luau API and drawing semantics (not yet implemented)

| API | Contract |
| --- | --- |
| `ctx.assets.request_png(path)` | Init/update; return a pending/cached canonical handle after synchronous rooted metadata resolution, without waiting for content/decode |
| `ctx.assets.status(image)` | All live callbacks; owned Phase 0 status/error snapshot, including CPU state and distinct GPU residency |
| `ctx.assets.size(image)` | All live callbacks; return validated known dimensions; unknown/failed/unloaded refuses |
| `ctx.assets.unload(image)` | Init/update; invalidate logical image/all aliases, remove path lookup, cancel pending work and retire physical resources safely; true once, false when already terminal |
| `ctx.draw.sprite(image, x, y, options?)` | Draw only; requires live CPU-ready image; validated owned command. Renderer skips pending GPU uploads without forcing work. |

`ctx.assets` is read-only and exists in all callbacks; requests outside init/update
refuse explicitly. `ctx.draw` still exists only in draw. Top-level modules
receive no context. Neither handles nor metadata tables expose engine internals.

`size` and `sprite` reject a handle whose session is not this host's as an
ordinary catchable error, mirroring the existing `foreign entity handle`
behavior in the world bindings. This is a binding-level check, separate from
the renderer's later validation of copied commands against a live store. Only
a Rust harness can produce such a handle, exactly as in the world tests.

The Phase 0 record freezes status and readiness. This callback fragment is an
illustration, not a complete runnable game:

```luau
-- Enqueue once during init/update; retain the handle between callbacks.
character = ctx.assets.request_png("assets/character.png")
local progress = ctx.assets.status(character)

-- During draw, after the ready-image condition has been met:
ctx.draw.sprite(character, x, y, {
    source = {x = frame * 16, y = 0, width = 16, height = 16},
    width = 32, height = 32,
    flip_x = false, flip_y = false,
    tint = {r = 1, g = 1, b = 1, a = 1},
})
```

- The 256-call `ctx.assets` budget is per callback, so a `size` query inside a
  per-tile draw loop will exhaust it on any real room. Dimensions are frozen
  once an image is ready; the sample must read them once and keep them, and the authoring
  documentation must say so rather than leaving authors to discover the ceiling
  as a fault.
- Coordinates are top-left screen pixels, positive x right and positive y down,
  matching rectangles. Source coordinates are image pixels, never tile indices.
  Source rectangles are half-open; integer x/y are nonnegative, integer
  width/height are positive, and their checked extents must fit the image.
- Omitted/nil options use the full image, its natural dimensions, no flips,
  and white/opaque tint. When a source is present, omitted destination dimensions
  default independently to its width/height. An explicit source supplies all
  four fields; an explicit tint supplies all four RGBA fields.
  Nil optional fields behave as omitted; non-nil fields must have the stated type.
- Optional tables must be plain tables with no metatable or unknown/numeric
  keys. Read fields without metamethods; enforce actual numeric/boolean types,
  reject coercions, and copy values during the call. Mutating options afterwards
  cannot change a published command. Bound key inspection by the allowed field
  count and stop at the first unexpected key.
- Validate x/y as finite f64 within +/-1,000,000 and destination width/height as
  finite values in `0..=1,000,000` before narrowing to f32. Zero-area sprites are
  valid counted no-ops; negative sizes are errors, not flip shorthand. Tint is
  finite normalized RGBA in `[0,1]`. Use the same ranges as existing rectangles.
- Flips mirror the selected source inside the same destination footprint. No
  rounding or implicit snapping is added; integer placement and integer scale
  give the predictable pixel-art case. Fractional placement/size are supported
  with normal rasterization and do not promise equal pixels across GPUs.
- Tint multiplies sampled RGBA, then uses the existing straight-alpha blend
  behavior and command order. Nearest filtering is fixed for this milestone;
  no mipmaps, repeat sampling, color management, or automatic texture packing.
- Preserve ordered drawing across image changes and rectangle/sprite mixtures.
  A clear discards earlier drawing, including queued sprites. An empty accepted
  draw leaves black. No sorting by texture or layer is allowed.
- A caught invalid sprite leaves the pending list unchanged. An uncaught error
  or latched budget failure discards the entire pending frame and clears the
  published list. Every valid sprite participates in the existing command cap.

Add a `DrawCommand::Sprite` variant with an `ImageId`, normalized explicit
source rectangle, destination rectangle, flips, and RGBA tint. Keep source
coordinates integer-valued until renderer conversion. Scalar owned data can
remain `Copy`; no Macroquad or mlua types enter this enum.

## Shared renderer and Player lifecycle

Expose a graphics-only `MacroquadRenderer` with session attachment, staged image
upload, render, and retirement operations as frozen in Phase 0. This replaces
the earlier single prepare-before-play transaction. The behavior is:

1. Attach a live session to a retired renderer and admit completed CPU images
   as they arrive, including during play. Lazily create missing
   pool slots once through `Texture2D::from_rgba8`; resize/repopulate existing
   slots through the scoped adapter below. New pool slots start as transparent
   1x1 images; allocate destination storage, then transfer bounded RGBA row bands
   with `texture_update_part`. Allocate storage once per admitted image rather
   than once per band. Set nearest filtering explicitly and retain strong owners.
   Never call Macroquad's file/image decoder from the renderer. Reuse free slots
   first, assigning each pending/resident image a distinct slot; grow only when
   no free slot exists. Reserve pool/mapping storage
   before GPU creation so a later Rust allocation failure cannot lose ownership
   of an already registered texture.
2. An image becomes presentation-ready only after all its upload bands succeed.
   Publish its full-ID mapping atomically, without disturbing already ready
   images. A failed/cancelled staged upload clears its private slot back to 1x1
   after queued work no longer references it and discards its tentative mapping.
   Keep already allocated slots for reuse; do not delete/recreate them on retry.
   Do not publish partially uploaded images. The upload service does no Luau
   work; readiness/failure is acknowledged at the agreed update boundary.
3. Render against the matching live store and attached session. Validate the
   complete command list, including Rust-supplied commands, before queueing it:
   IDs, sizes, source bounds, colors, and cap. Share validation with bindings
   where practical. Missing/foreign/stopped assets fail explicitly. The draw
   traversal performs no file reads, decoding, or uploads; those stages have
   separate service budgets. It must not force-complete a pending image as a
   side effect of drawing it. Skip CPU-ready commands whose texture upload is
   pending; reject foreign/unloaded IDs, as specified in Phase 0.
4. Use `draw_texture_ex` for source/destination/flips/tint and the same ordered
   clear/rectangle path. The application supplies a screen-space default camera
   and ordinary blend/material state, and must never call
   `build_textures_atlas` or `reset_textures_atlas`. Document all three
   preconditions together for future editor viewport integration. No editor
   viewport implementation is included.
5. Fatal renderer errors detected by engine validation enter an explicit runtime
   presentation-fault path, set `ScriptState::Faulted`, clear commands/assets,
   invalidate the kernel, release VM references, and perform reverse native
   teardown without Luau shutdown. Reuse the existing primary-fault/log handling
   rather than merely replacing Player's runtime with `None`.
   Repeated failure notification after a terminal state preserves the first
   failure and performs no callbacks or second native teardown. Ordinary failed
   asset jobs follow their inspectable error policy, distinct from renderer
   invariant faults and backend process failures.
6. Retire only after queued work is submitted/discarded, on the owning graphics
   thread with its context still alive. Invalidate the session mapping and
   shrink every used slot to transparent 1x1 RGBA8. Retain those cleared slots
   for the next session; retirement is idempotent. Do not allow a new
   session to inherit any old `ImageId` mapping or image content.

`Texture2D::from_rgba8` appends a weak entry to Macroquad's `unbatched` list;
ordinary texture garbage collection does not remove it. Miniquad 0.4.11's
Windows OpenGL backend also appends a texture record on creation without
removing that record on deletion. Recreating a renderer's textures each session
would grow both lists even if GPU storage was deleted correctly. The application
must therefore retain the same pool until its graphics context ends. Allocation
and registration counts attributable to game images cannot exceed 128 across
all successful sessions and job rollbacks in that context.

Use `macroquad::window::get_internal_gl` only inside a small synchronous adapter
in `src/rendering.rs`. Obtain a slot's `raw_miniquad_id()` before entering the
adapter, then use `quad_context.texture_resize(id, width, height, Some(bytes))`
to clear a retired slot without creating a texture record or batcher entry.
For a staged image, allocate with `texture_resize(..., None)`, keep it invisible,
then fill it with bounded `texture_update_part` calls. Validate dimensions,
region bounds, row alignment and exact checked RGBA8 lengths before these calls;
reapply nearest filtering and no mipmaps. The pinned Windows implementation
updates the existing texture record's dimensions, which Macroquad then reads
for size and source UV calculation. Phase 0 verified that path behaviorally;
Phase 3 repeats the checks through the actual shared renderer.

Document the adapter's unsafe invariants: a live context on its owning thread,
exclusive short-lived access, no retained backend references, and no other
Macroquad calls, Luau callbacks, or awaits while that access is held. Flush
queued work before resizing any referenced texture. Keep managed `Texture2D`
owners private to the renderer; no raw handle or wrapper clone may escape it.
Pool growth must respect the 128-slot cap, including slots created during failed
upload admission. Active image storage follows the 64 MiB decoded-content limit;
idle slots add at most 512 bytes of pixel payload. At retirement every slot is
1x1 transparent, so no previous game's full-size storage/content is retained.
These are payload bounds, not guarantees about driver memory reclamation.

Keep atlas build/reset prohibited: packing would switch drawing to an atlas
whose filtering is independent of each slot, while reset would invalidate
unrelated font state. Source cropping stays in image pixels with this unatlased
pool. `Texture2D` destruction still requires the live graphics context; drop the
renderer only at graphics-context shutdown, after pending work is flushed, inside
the `Window::from_config` future. Never recreate it for a runtime restart.

For capture, hold the renderer/textures independently of runtime CPU assets:
queue the final frame, invoke normal shutdown exactly once, and retain textures
until `get_screen_data()` has flushed/read the frame. On a shutdown fault, draw
the Game error screen as today before readback. Only then retire the session;
the exiting Player may also drop the renderer while the context is alive. Do
not clear/repopulate slots between queueing the final frame and its readback,
or render again from stopped assets to save it. Interactive Escape/window-close
and fault paths must likewise retire queued work and image contents safely.

Macroquad texture creation returns a texture, not a recoverable allocation
result. Do not promise containment of driver OOM, context loss, or backend
panics. Phase 0 established the 2048x2048 upload path on the tested Windows
MSVC stack and recorded the observable failure boundary; report incompatibilities
instead of silently reducing the asset contract or substituting a renderer.
Fatal asset-service/renderer failures retain Player exit code 3; an inspectable
failed job does not automatically fault the session. Capture/configuration codes,
fallback screens, seeding, and tick counts persist.

## Implementation phases and stop gates

All P1-P8 scope decisions are accepted. Phase 0 completed its five exits in the
linked record: shared decoding/retained CPU pixels; bounded worker selection;
job/error/budget/catch-up contracts; CPU/GPU readiness and deterministic draining;
logical unload with alias/pinning/cancellation rules. It establishes feasibility
and contracts, not production implementation. Phases 1-4 remain unstarted.

Each phase ends with a focused diff review and evidence recorded here. A failed
gate blocks dependent work; record the reason and proposed contract adjustment.
Do not mark a phase complete based only on compilation when its exit requires
behavior or pixels. Keep changes as separate logical slices.

| Phase | Work and primary files | Exit / stop gate |
| --- | --- | --- |
| 0. Contract and feasibility — complete | `examples/png_sprite_probe.rs`, fixture/watchdog tools, and `PNG_SPRITE_PHASE0.md`. | Contracts frozen; exact CPU/GPU pixels, metadata/interlace worker behavior, bounded upload passes, 100 pool cycles, capture lifetime and negative controls passed on the recorded Windows stack. Production integration remains later work. |
| 1. Headless asset service — complete | `src/assets.rs` and supporting modules; rooted-read extraction; `Cargo.toml`, `src/lib.rs`; `tests/assets.rs`. Implement admitted jobs, stage advancement, bounds, cache/identity, decode and teardown. | Identical decoded pixels through bounded service passes without VM/GPU. Rooting, coalescing, rollback, cancellation-safe teardown, queue fairness, storage pressure and stale/foreign IDs pass. Existing filesystem regressions remain intact. |
| 2. Luau and runtime integration | `src/scripting/assets.rs`, `src/scripting/drawing.rs`, `src/scripting.rs`, `src/runtime.rs`, `src/drawing.rs`; runtime/test drivers. | Init/update requests work in ScriptHost and GameRuntime; callbacks continue while loading. Status/error/readiness, eviction utilities, canonical wrappers, publication, budgets and cleanup obey frozen contracts. Preload/completion-trace tests use the same service path. No GPU dependency enters scripting-only builds. |
| 3. Shared renderer and Player | `src/rendering.rs`, `src/bin/player.rs`, readiness acknowledgements and fault integration; actual Player captures/input probes. | Bounded upload passes publish only complete images; rendering never forces loading. One admission/upload sequence per uncached image, explicit eviction/reload, queued-frame pinning, mixed ordering, shutdown captures and repeated-session pool bounds pass. Pending work is cleaned on faults/exit. Existing rectangle/startup/native captures pass. |
| 4. Authoring sample and delivery | `examples/games/sprites/` using the supplied Kenney sheet and provenance; README/AGENTS/index updates; complete verification matrix. | Copied Player shows a responsive loading state, then the room and animated controllable character. Requests during update and PNG replacement without rebuilding are demonstrated. Deterministic preload replay, live loading interaction, and limitations are recorded. |

Player subprocess evidence must identify per-image admissions/completions and
aggregate read/decode/upload work with bounded test diagnostics; a once-per-session
"prepared" line no longer proves dynamic loading behavior. Pair those receipts
with in-process stage counters and negative controls. The sample bundle needs
`include_bytes!` or `fs::copy` for PNGs, not `include_str!`.

## Behavioral verification

Use independently specified expected values and asymmetric fixtures so incorrect
implementations cannot pass by reproducing their own conversion or crop logic.

- **Rooting:** Run with unrelated cwd and a different-colored decoy at the same
  relative path there. Test a module in a subdirectory, spaces/Unicode in valid
  names, traversal/absolute/device paths, directories, links/reparse points,
  missing files, and platform case aliases. Record any OS permission required
  for link fixtures rather than silently skipping that boundary.
- **Decoding:** Commit tiny RGB, RGBA, grayscale, palette/tRNS, and interlaced
  fixtures with known dimensions and bytes. Include 16-bit, APNG, wrong-format,
  truncated IDAT, and corrupt PNG cases. Check exact limits and one-over limits,
  huge-dimension headers with small payloads, checked arithmetic, and aggregate
  exhaustion across different files. Generate deterministic incompressible
  2048x2048 RGBA input and verify that its PNG exceeds 16 MiB but fits 17 MiB,
  decodes back to the expected pixels, and two distinct asset files fit 34 MiB
  of aggregate encoded staging. Keep separate 17/34 MiB boundary and one-over tests; extra
  metadata remains subject to encoded limits. Verify decoder limits are installed
  before header parsing and conversion allocations. Commit a generator script or the
  documented byte listings alongside the fixtures, so expected values are
  specified independently rather than recorded from a first passing run.
  Keep `*.png -text` in `.gitattributes` for the same reason the generated C header
  is pinned: fixture bytes are compared exactly on every platform.
- **Accounting/publication:** Use mixed cache hits, malformed calls, wrong-phase
  calls and failed jobs. Exhaust per-pass work and aggregate staging independently;
  normal yielding must preserve the job and let gameplay continue. Check no
  retained image/canonical wrapper leaks after admission or completion
  failure, no bypass through `pcall`/`xpcall`, and no systems after a fault.
  Use targeted allocation-failure injection for wrapper publication if needed;
  a structurally unexercised rollback is not behavioral proof.
- **Scheduling and late work:** Exercise multiple concurrent requests, cache
  coalescing, partial/final chunks, EOF/error, interlaced decode, compressed
  metadata, conversion bands and GPU bands. A tiny encoded PNG with large output
  must not bypass decode/transfer budgets. Test aggregate fairness and catch-up
  frames; distinguish bytes-per-pass assertions from elapsed-time observations.
  Cancel/tear down jobs at every stage and race completion with cancellation or
  session shutdown; no late result may publish. Exercise the accepted script
  eviction operations with aliases, retained handles, published-frame pins and
  busy-slot reuse, including later reupload or a fresh logical request as defined
  by the operation. Use deterministic test scheduling for exact readiness traces.
- **Lifetime:** Query sizes in every legal callback; retain old binding functions;
  use the same image as a table key after a repeated load. Test two simultaneous
  stores with matching image numbers, repeated restart, standalone-host terminal
  cleanup, invalid host alpha preserving commands, and copied commands rejected
  by a foreign/stopped store. Check that `size` and `sprite` refuse an injected
  foreign handle catchably at the binding, and that a burned image number is
  never issued again after a failed publication. File replacement between
  sessions must reload.
- **Command semantics:** Verify every default, source edges, zero destination
  size, negative/non-finite/fractional-invalid inputs, unknown options/metatables,
  tint boundaries, mutations after append, mixed 10,000-command limits, and
  clear/rect/sprite order. Check a valid sprite following a caught bad call and
  no commands surviving an uncaught draw error.
- **Pixels:** An asymmetric sheet with distinctive corners and contrasting
  neighboring cells proves crop, flip axes, and no tile-edge bleed at integer
  scale. Cover each flip alone and both together, each composed with an explicit
  source rectangle and a destination size that differs from it, since that is
  where mirroring the destination footprint could diverge from swapping source
  coordinates. Test transparent texels over colored backgrounds, half-alpha pixels
  combined with tint alpha, nonwhite RGB tint, 1x/2x scales, offscreen clipping,
  image switches, intervening rectangles/clears, and an empty next frame. Use
  exact opaque/nearest samples and a stated small rounding tolerance for blend
  channels, not broad whole-image similarity.
- **Loading and retirement:** Count reads/decodes/uploads in test-owned
  instrumentation to prove draw traversal never triggers them and completed
  images are not read/decoded/uploaded again without an explicit residency action.
  Verify per-band progress during active loading. Run preloaded first-frame capture
  and shutdown-fault capture with sprites and the existing Escape/window-close
  probe. Reuse one renderer for at least 100 attach/load/retire cycles under one
  live Windows graphics context, alternating empty/small/128-image sessions,
  staggered admissions, image dimensions/content, and injected upload failures. Assert at most
  128 lifetime texture creations/registrations, no further creation after the
  pool reaches that size, and every retired slot measuring 1x1 transparent.
  Couple adapter creation counters to the sole `from_rgba8` call site; source
  review must verify that resize never calls creation or batcher registration.
  Include a control that recreates textures so the count assertion fails.
  Verify changed dimensions, source UVs, pixels and new IDs after reuse, and
  old IDs refusing access. Test final context shutdown after flushing, with
  each pool texture released once. Ensure native instances tear down once on
  presentation faults with their logs retained.
- **Shipped sample:** Copy the Player, Luau and PNGs into an isolated directory;
  use an unrelated cwd. Demonstrate requesting an image during update while
  input/movement continue, then drawing it only when ready. Check animation at
  selected tick numbers after deterministic asset preloading; use separate
  injected/headless movement input for control. Compare repeat seeded PNG bytes
  on the same graphics stack with the same readiness schedule. Replace a tileset
  and spritesheet between launches without changing the executable. Visually
  inspect full captures and exercise movement interactively.

Use the supplied [Kenney 1-Bit Pack sheet](../../examples/kenney_1-bit-pack_transparent-packed.png),
with attribution in [examples/README.md](../../examples/README.md). The local PNG
is 784x352, 17,497 encoded bytes, and indexed 1-bit color; RGBA8 output occupies
1,103,872 bytes. Its 16x16 tiles form 49 columns and 22 rows. It fits one 32 KiB
file-read unit but needs multiple decode/upload units, so it is useful coverage
for stage accounting. Add larger synthetic fixtures for multi-read tests.

The sample uses a small Luau tile array and fixed-screen layout, with at least
two animation frames and a documented movement/reset control scheme. Motion
uses the existing kernel timing; animation advances in update, not draw. State
and frame selection must replay at 30/60/144 presentation FPS for the same
fixed-input and controlled asset-completion trace, or with fixtures preloaded.
Do not claim production load-completion ticks are frame-rate independent solely
because simulation is fixed-step. Compare image content/command fields with session IDs mapped
by logical asset path, since IDs intentionally differ across runtime sessions.
Provide provenance/license information for committed art; asset replacement
instructions must copy PNGs as well as Luau. Keep the old rectangle-only tiles
sample as compatibility coverage.

## Required checks and completion record

For implementation, run the Rust and scripting suites from [AGENTS.md](../../AGENTS.md),
including release headless tests and the lifecycle example. Add these commands
once the new features and `assets` test target exist:

```text
cargo test --workspace --no-default-features --features assets
cargo clippy --workspace --all-targets --no-default-features --features assets -- -D warnings
cargo check --workspace --all-targets --no-default-features --features graphics
cargo clippy --workspace --all-targets --no-default-features --features graphics -- -D warnings
cargo test --release --no-default-features --features scripting --test assets --test drawing --test runtime
cargo test --test player_capture -- --ignored
cargo test --test plugins -- --ignored
cargo build --release --bin protogine-player
pwsh -NoProfile -File tests/player_input.ps1
```

Inspect `cargo tree --no-default-features --features scripting` and the corresponding
`assets` and `graphics` graphs to verify the feature boundaries above. Run the
native/SDK matrix in AGENTS.md when modifying native teardown integration; keep
the independent watchdogs. If exact deadline enforcement changes, rerun the
distance benchmark and its protected-call regressions. Add all newly required
feature/test commands to AGENTS.md when those features are implemented.

The current `script_host` example runs only three ticks; that is not a sufficient
completion check for staged assets. Phase 2 must add a bounded preload/drain or
completion-trace driver and document its actual command. Keep the existing
lifecycle example checks, and never report a loading sample as verified merely
because the three-tick process exited successfully.

Record each phase's changed files, commands and results, behavioral observations,
capture paths/dimensions/ticks, platform/graphics stack, and unresolved gaps.
Distinguish a missing GPU context from a failed rendering test. No test run or
image generated during implementation may be described as verified here before
it is actually inspected. Keep performance claims limited to measured evidence.

Completion requires all four delivery phases after feasibility, the full
applicable check matrix, copied-game and interactive evidence, documentation
matching final APIs/limits, and no unresolved correctness gate. Archive the plan
as completed only then. Until implementation starts, documentation changes need
only content, link, and diff review; this draft does not require running Rust tests.

## Phase 1 completion record

Completed 2026-09-06 on Windows 10 x64, MSVC, stable rustc. This phase touched
no graphics code and required no graphics context.

### Changed files

- `src/assets.rs`: identity, status, error and bound types with no dependency on
  a decoder, a VM or a window. `ImageId` is a `Copy` scalar of a process-unique
  session number and an append-only image number with private construction.
  `UploadAck` is defined here for Phase 3 and is not yet produced.
- `src/assets/store.rs`: the session-local registry, admission, path and
  spelling caches, byte accounting, grants, publication, eviction and teardown.
- `src/assets/worker.rs`: one bounded worker owning file reads, decoder
  construction, native row decoding and RGBA conversion.
- `src/rooted_path.rs`: traversal extracted from the filesystem bindings. The
  final-node policy is a parameter, so `ctx.fs` keeps today's behavior while
  asset loading adds the regular-file requirement, canonicalization and the
  containment recheck.
- `src/scripting/filesystem.rs`: uses the shared helper and maps its errors back
  to the existing diagnostics verbatim; no behavior change.
- `Cargo.toml`, `src/lib.rs`: the `assets` feature, `scripting` enabling it, and
  `image` pinned exactly at `=0.24.9`. `player` no longer names `image`
  directly; it reaches it through `scripting`.
- `tests/assets.rs`, `tests/fixtures/assets/`, `tools/asset_fixtures.py`.
  `.gitattributes` now also pins `*.rgba` alongside `*.png`: the expected-pixel
  files are raw bytes that can contain CR, and they are compared exactly.
- `README.md`, `AGENTS.md`, `docs/implementation/README.md`.

### Implemented contracts

`AssetStore::new(root)` canonicalizes an absolute root and starts one worker.
`request_png` validates, resolves and admits synchronously. `service` runs one
non-blocking pass; `advance(wait)` runs exactly one pass and waits for the
outstanding grant; `drain(timeout)` is the bounded preload helper with its own
watchdog. `status`, `size` and `image` return owned snapshots or a pinning
owner. `take_settled` hands the host the state transitions since the last drain.
`unload` and `roll_back` are the two eviction entry points; `shutdown` is
idempotent and joins the worker.

Store and worker exchange one command and one reply at a time over bounded
channels. The worker performs at most eight quanta or one non-preemptible stage
per grant, with a 2 ms soft cutoff between operations, and holds no policy: the
store computes each read allowance from the remaining per-image cap plus the
overflow probe and free staging, and reserves output and scratch before the
worker allocates either. Cancellation sets a per-job flag, marks the image
terminal immediately, and discards whatever the worker returns; a job the worker
still holds is abandoned explicitly before the next job starts, so no partial
buffer outlives its identity.

### Contract refinements made during implementation

- **Terminal entries and registry slots.** A failed or unloaded image leaves
  every path and spelling lookup the moment it settles, but keeps its registry
  slot until `take_settled` takes its transition. This keeps the frozen "no
  immortal terminal registry" rule while giving the host exactly one chance to
  copy terminal status into VM-owned values, and bounds undrained transitions to
  the registry size instead of letting them grow. A host that never drains stops
  admitting rather than accumulating history.
- **Decoder scratch is reserved conservatively.** `image` 0.24.9 does not expose
  a PNG's interlace method, so every job reserves the whole native frame the
  Adam7 path would decode eagerly, plus one conversion band. That is at most
  16 MiB and 32 KiB, matching the frozen ceilings, and it is a separate counter
  from the 64 MiB retained-content budget. Detecting interlacing would require a
  second PNG header parser in the engine and was not added.
- **`roll_back` is the Phase 2 publication hook.** Rust admission has no
  fallible step after its checks, so the burned-identity path is exposed as an
  explicit operation for the VM wrapper publication that Phase 2 adds, and is
  exercised from queued, loading and ready states.
- **The early metadata check is retained and is not the authority.** A file
  whose metadata already exceeds 17 MiB is refused before any read. A file at
  exactly the cap is still read in full and accepted, which is what the test
  asserts, so the accepted size is decided by bytes that actually arrived.

### Behavioral observations

- The Kenney sheet loads with 17,497 bytes read and 36 conversion bands, and its
  stage trace is exactly `waiting, read, header, allocate, decode, complete`.
  Both figures match the Phase 0 measurements for the same file, from a
  separate implementation of the same grant policy.
- The pass count for that sheet is deliberately not fixed. A pass is bounded by
  both the eight-band grant and the 2 ms soft cutoff, so it ranges from eleven
  passes when every grant is used in full to one band per pass when it is not.
  This debug run took 19. The test asserts that range rather than a constant,
  because a time cutoff that never bit would not be a cutoff.
- No pass exceeded eight work quanta of reads or eight conversion bands, and the
  final pixels are identical to the same file drained in one call.
- The maximum incompressible fixture encodes to 16,780,612 bytes: above the
  16 MiB decoded size and inside the 17 MiB encoded cap, confirming the headroom
  correction. That is byte-for-byte the size Phase 0's independently encoded
  fixture reached, since incompressible input lands in stored blocks either way.
  Two of them fit the 34 MiB aggregate staging allowance. Because
  exactly one job reads at a time, observed staging peaked near one encoded file
  rather than near the aggregate ceiling, so that ceiling is a bound rather than
  the binding constraint; the per-image cap and registry are what admission
  actually presses against.
- Filling the store to 64 MiB with four maximum images makes the fifth job fail
  as `capacity` after its dimensions are validated and before any output is
  allocated, and unloading one image lets the retry succeed.
- The 256-entry spelling memo can only be reached where the platform supplies
  aliases: at most 128 images can be live, and evicting an image drops its
  spellings with it. The bound is therefore covered on Windows through case
  aliases, and the portable test covers the live-only invariant instead.
  Nothing is lowercased to manufacture aliases on case-sensitive filesystems.
- Cancelling at each of ten pass counts released every reservation, published
  nothing, and left no service fault. Dropping a store mid-load joined its
  worker every time.
- Pinned pixels survive both `unload` and `shutdown` until the owner drops, and
  the store's resident byte count reflects that rather than the logical state.

### Commands and results

All passed on this machine:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --bin protogine-player
cargo test --workspace --no-default-features --features assets
cargo clippy --workspace --all-targets --no-default-features --features assets -- -D warnings
cargo test --workspace --no-default-features --features scripting
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --release --no-default-features --features scripting --test assets --test drawing --test runtime
cargo test --release --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
cargo test --test player_capture -- --ignored
cargo test --test plugins -- --ignored
```

`tests/assets.rs` contains 30 tests, three of them Windows-only, and runs in
about 3.5 seconds in debug and 1.3 seconds in release. The existing filesystem,
scripting, runtime, drawing, plugin and capture suites pass unchanged, including
the ignored GPU captures, which confirms the traversal extraction and the
feature reshuffle changed no existing behavior.

`cargo tree` confirms the boundaries: `--no-default-features` resolves only
hecs and tot; adding `assets` adds image 0.24.9 and png 0.17.16 with no mlua or
Macroquad; adding `scripting` adds mlua on top of that. `Cargo.lock` is
unchanged, so the exact `image` pin matched the already resolved version.

`tests/player_input.ps1` was not rerun: physical input, Player shutdown and the
renderer are untouched by this phase. The distance benchmark was not rerun for
the same reason, since deadline enforcement did not change.

### Unresolved gaps for later phases

- No Luau API exists yet. `ctx.assets`, the canonical VM wrappers, the attempt
  budget, phase checks and terminal status retained on handles are Phase 2, and
  `roll_back` is unused until then.
- `GpuResidency` is always `Unavailable`, and `UploadAck` has no producer until
  the Phase 3 renderer exists. Nothing here proves rendered pixels.
- `GameRuntime` and `ScriptHost` do not own a store yet, so the frozen
  once-per-frame grant, update-boundary publication and capture drain are not
  wired up. The store's pass function is the one those will call.
- Service faults are recorded and reported through `service_fault`, but no
  caller turns one into a session fault yet.

## Planning evidence

The observations below preceded Phase 0; its linked record contains the later
behavioral probes and current contract choices. Inspected at the baseline above:
Cargo manifests/lockfile; the asset-free draw
enum; script callback scopes/budgets and terminal cleanup; rooted filesystem
reads; GameRuntime lifecycle; Player render/capture/shutdown flow; drawing and
Player capture fixtures. No implementation or runtime probe was performed while
drafting this plan. A later review pass extended the dependency observations
below by reading the vendored png 0.17.16 and Macroquad 0.4.16 sources; it also
performed no runtime probe.

The subsequent encoded-size review did run an isolated probe against the
existing release `image` 0.24.9 library. It generated 2048x2048 RGBA bytes with
xorshift64 (seed `0x123456789abcdef`, shifts 13 left/7 right/17 left, low byte
per pixel channel), encoded with `CompressionType::Fast` and
`FilterType::NoFilter`, then decoded with the proposed dimension and 32 MiB
decoder limits and checked every byte. RGBA size was 16,777,216 bytes; PNG size
was 16,780,612 bytes; two files total 33,561,224 bytes. This supports the 17/34
MiB headroom correction, not completion of the asset-loading or timing gates.

The locally resolved `image` 0.24.9 source exposes
`PngDecoder::with_limits`, dimension checks during decoder construction, APNG
inspection, palette expansion, and 16-bit output types. `with_limits` sets
`Transformations::EXPAND` and calls `check_dimensions` immediately after header
parsing; `read_image` asserts its buffer equals `total_bytes()` in the native
color type. Its later `set_limits` does not update the underlying PNG reader's
allocation limit, which is why this plan requires limits at construction. The
png 0.17.16 source shows that limit reserving roughly one output line, excluding
caller buffers by its own documentation. These source observations were followed
by the Phase 0 adversarial fixtures; Phase 1 still needs service-level coverage.

The locally resolved Macroquad 0.4.16 source exposes source rectangles,
destination size and flips in `DrawTextureParams`; RGBA8 texture upload takes
u16 dimensions; `get_screen_data` flushes queued rendering. Flips negate the
destination extent rather than swapping source coordinates, which is where this
plan's destination-footprint wording comes from; the reversed winding is
inconsequential only because no `cull_face` state appears anywhere in the crate.
`from_rgba8` enrolls textures with the batcher, `build_textures_atlas` has no
internal callers, and its atlas is constructed with `FilterMode::Linear`.
Batcher entries survive ordinary texture collection. Miniquad 0.4.11's
`GlContext::new_texture` appends to `Textures(Vec<Texture>)`; deletion does not
remove that record, while `texture_resize` updates the existing record in place.
Those observations motivate a context-lifetime pool with cleared contents at
session retirement. Phase 0 subsequently verified resize/reuse and registration
counts; Phase 3 must verify production integration. The earlier plan correction
itself performed no GPU probe.
Texture destruction uses the live graphics context. The [texture API documentation](https://docs.rs/macroquad/0.4.16/macroquad/texture/struct.Texture2D.html)
also documents RGBA8 upload and explicit filtering. Source support alone does
not verify the proposed lifetime, blend, or pixel behavior.
