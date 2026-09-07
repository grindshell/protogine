# Protogine agent guide

## Project intent

Protogine (Prototype Engine) is a simple, tile-based game engine in the vein of
RPG Maker. Prioritize straightforward game creation and a small, understandable
engine over general-purpose engine complexity.

The editor and standalone game player follow a Godot-inspired architecture:
both build on the same core kernel, with additions specific to authoring or
running a shipped game. Game behavior is primarily authored in Luau, shipped
alongside the release binary. A C API supports dynamically loaded native plugins
for performance-sensitive work.

## Current repository

The Cargo workspace contains `protogine` version `0.1.0` (Rust edition `2024`),
the dependency-free `protogine-plugin-api` SDK, and the separate header generator.
The `protogine-player` binary opens a Macroquad window with a "Missing game data"
fallback. Its default `player` feature enables graphics, scripting and native
plugins; the shared library also builds with `--no-default-features`.

- [Shared library](src/lib.rs) and [bundle discovery](src/bundle.rs): look for a
  readable `game/main.luau` beside the executable, independent of the working
  directory. This is the initial unpacked bundle convention.
- [Player](src/bin/player.rs): bundle execution through `GameRuntime`, physical
  input, owned-command rendering, startup and game-error presentation.
- [Built-in capture](src/bin/player/capture.rs): `PLAYER_CAPTURE` saves a PNG and
  exits; optional `PLAYER_CAPTURE_FRAME`, `PLAYER_WIDTH`, and `PLAYER_HEIGHT`
  control timing and dimensions. Use this for agent-driven visual checks.
- [README](README.md): run/build commands, bundle layout, and current limits.
- [Discovery tests](tests/bundle.rs): filesystem boundaries and failure cases.
- [Capture smoke test](tests/player_capture.rs): opt-in GPU verification of the
  actual Player, including pixel repeatability and process exit codes.

The optional `scripting` feature adds a headless
[Luau host](src/scripting.rs), bundle-local modules, lifecycle/fault handling,
and bounded callback logging. [Data](src/scripting/data.rs) and
[filesystem](src/scripting/filesystem.rs) utilities expose tot and JSON/YAML/TOML
export, bundle reads, and explicit data-root writes through scoped callbacks.
The [persistence example](examples/games/persistence/main.luau) owns its save schema
and write timing. [GameRuntime](src/runtime.rs) supplies the shared
[kernel](src/kernel.rs), scoped world/input APIs, fixed ticks, frame catch-up,
and input edge consumption. Position/velocity integration follows successful
script updates. The [movement example](examples/games/movement/main.luau) is
replayed headlessly at different frame rates by [runtime tests](tests/runtime.rs).
Run the [headless example](examples/script_host.rs) to exercise the runtime.
The [drawing API](src/drawing.rs) exposes owned clear/rectangle commands, shared
by headless tools and the Player. The [tile sample](examples/games/tiles/main.luau)
runs from copied source beside the Player. Capture mode seeds the VM before
module loading and steps once per frame with neutral input and alpha 0.
The optional `assets` feature, which `scripting` enables, adds the bundle-rooted
[image service](src/assets.rs) with its [store](src/assets/store.rs) and
[bounded worker](src/assets/worker.rs). Identity, status and bound types compile
without a decoder, a VM or a window. [Rooted traversal](src/rooted_path.rs) is
shared with `ctx.fs` and differs only in its final-node policy. The
[asset bindings](src/scripting/assets.rs) expose that service as `ctx.assets`,
and `ctx.draw.sprite` publishes owned `DrawCommand::Sprite` values. The
[loading sample](examples/games/loading/main.luau) requests during update and
draws a progress bar until its sheet is ready.
The optional `graphics` feature, which `player` enables, adds the
[shared renderer](src/rendering.rs): a context-lifetime pool of GPU allocation
slots, bounded staged uploads and ordered command submission, with no VM
dependency. Its [GPU harness](examples/renderer_harness.rs) drives the
production renderer and store under a live context.
The optional [manifest](src/manifest.rs) and [native loader](src/plugins.rs) add
trusted C plugin startup/cleanup before/after the VM. The [SDK](sdk/src/lib.rs)
generates [the C header](include/protogine_plugin.h) with the separate
[headergen tool](tools/headergen/src/main.rs). The [native buffer adapter](src/scripting/native.rs)
supplies synchronous batch calls; the [distance-field example](examples/games/native_distance/SCHEMA.md)
includes a typed Luau wrapper and parity implementation. Kira audio, the editor,
and export tooling remain unimplemented.

Accepted and completed plans are indexed in [docs/implementation/README.md](docs/implementation/README.md).
The [PNG and sprite plan](docs/implementation/PNG_SPRITE_PLAN.md) records the accepted
next milestone. [Phase 0](docs/implementation/PNG_SPRITE_PHASE0.md) completed its
contracts and CPU/GPU feasibility probes. Phase 1 implemented the headless asset
service, Phase 2 its Luau and runtime integration, and Phase 3 the shared
renderer, GPU residency and Player integration; the authoring sample remains
unimplemented.
The [scripting and C API plan](docs/implementation/SCRIPTING_C_API_PLAN.md) records
accepted decisions, completed phases, verification evidence, and deferred scope.

## Required technology choices

| Responsibility | Crate | Requested version | Configuration |
| --- | --- | --- | --- |
| Graphics and window/frame integration | `macroquad` | `0.4.16` | Use for rendering |
| ECS foundation for the core update loop | `hecs` | `0.11.1` | Engine-owned update ordering |
| Audio | `kira` | `0.12.4` | Route engine audio through Kira |
| Luau scripting host | `mlua` | `0.12.1` | Enable `luau-jit` |
| Structured game data and manifest | `tot` | `0.2.0` | Git dependency from `https://github.com/totlang/tot`; use `game/game.tot` |
| Runtime native-library loading | `libloading` | `0.9.0` | Keep behind the native plugin boundary |

These versions are project requirements. Verify versions, features, and target
support when wiring dependencies; report incompatibilities instead of silently
substituting versions or libraries. Keep `Cargo.lock` updated with resolved
application dependencies.

Prefer the GitHub source for tot; `../tot` is available for source inspection,
not the normal dependency source. Pin a reviewed Git revision as described in
the scripting plan. Cargo itself continues to use `Cargo.toml`.

`hecs` supplies an ECS world and queries; it does not supply a scheduler or an
application event loop. Protogine must define its simulation update order around
that world, with the application driving updates and presentation. See the
[hecs documentation](https://github.com/Ralith/hecs). Use `mlua`'s `luau-jit`
feature, which selects Luau with its JIT backend; `luajit` selects a different
language runtime. See the [mlua feature documentation](https://github.com/mlua-rs/mlua#feature-flags).

## Architecture boundaries

- **Shared kernel:** Own tile/map state, ECS components, simulation updates, and
  shared game-facing contracts. Keep simulation logic usable without opening a
  window or an audio device so it can be tested independently.
- **Shared runtime services:** Integrate rendering, audio, Luau, asset loading,
  and native plugins with the kernel. Editor playtesting and the shipped player
  must use the same gameplay behavior and content-loading contracts.
- **Editor:** Add authoring UI, inspection, map editing, and export tooling.
  Keep editor-only dependencies and state out of the standalone player.
- **Standalone player:** Add game startup, loading of shipped content, and
  distribution-specific behavior. A Cargo `--release` build is an optimization
  profile, not the architectural separation between editor and player.

Keep dependencies directed toward the shared kernel; the kernel must not depend
on the editor. Use shared code rather than maintaining separate editor and player
implementations of gameplay. The engine package has a shared library and a
feature-gated Player binary; SDK definitions and development-only header generation
are separate workspace members. Introduce further splits only as needed.

Keep tile coordinates and world/pixel coordinates explicit. Prefer simple map
storage where appropriate; using an ECS does not require every static tile to be
an entity. Define update timing and script execution order before implementing
behavior that depends on them.

## Scripting and shipped games

- Make Luau the primary game-authoring interface. Rust owns engine services;
  game-specific behavior belongs in scripts unless native performance is needed.
- Expose deliberate engine APIs to scripts rather than leaking internal Rust
  types or ECS storage details. Define handle validity and mutation timing.
- Ship scripts and game assets alongside the player through a documented project
  layout or package format. Do not make ordinary script edits require rebuilding
  the engine. The initial unpacked layout is documented in the README; a future
  archive format and full export contract remain open.
- The accepted first scripting milestone uses source modules, explicit native
  plugin declarations, and complete runtime restart rather than live reload.
- Use the same script APIs and loading rules during editor playtesting and in
  exported games. Resolve content through an explicit project/package root,
  rather than relying on the process working directory.
- The headless host validates init/update/draw/shutdown callbacks and expires
  scoped context functions after every call. Faults stop the session; drop never
  invokes Luau callbacks, but does release native instances. Restart creates a new
  host/VM. Source, import, logging, and VM
  heap limits are documented in the README and implementation record.
- VM cancellation uses mlua's protected panic propagation with
  `catch_rust_panics(false)` and a host-owned cancellation payload. Keep
  `panic=unwind` for scripting builds; test protected calls and metamethods when
  changing interruption. Ordinary caught allocation errors are recoverable;
  host-detected budget failures remain latched outside Lua. Check the deadline
  at every VM interrupt: built-ins and VM operations can do substantial work
  between interrupts, so a fixed interrupt stride cannot bound elapsed time.
  Keep host-initiated checks exact and re-run the distance benchmark when
  changing deadline enforcement.
- `GameRuntime` owns kernel mutation and timing; standalone `ScriptHost` calls
  omit world/input bindings. World operations return owned data/opaque handles
  and release all hecs/RefCell borrows before VM work. Only init/update mutate
  world state. Validate session identity and generation on every handle access.
  Canonical wrappers in a private weak VM cache preserve Luau table-key identity;
  it packs (session, hecs slot) into one exact Lua number and compares the stored
  handle, so a reused slot replaces its stale wrapper instead of returning it.
  Roll back unpublished entities if wrapper/cache allocation fails during spawn.
  Fixed systems run after a successful callback; session faults invalidate the
  kernel. See the Phase 2 contract for entity/operation limits and input timing.
- `ctx.draw` exists only in draw and publishes a fresh owned list only after a
  successful callback. Validate finite coordinates/sizes/colors before f32
  conversion; the 10,000-command cap latches and counts sprites. Fault/stop clears
  published commands, while a rejected `alpha` is recoverable and preserves the
  published list.
  Keep renderer semantics (clear discards previous drawing, ordered alpha-blended
  rectangles, empty frame black) aligned with headless data and GPU tests.
  See Phase 3 for capture seeding, shutdown, input mappings and fault code 3.
- `ctx.draw.sprite` takes an asset handle and appends an owned `Sprite` of an
  `ImageId`, an integer half-open source rectangle, a destination rectangle,
  flips and a tint. Keep the scalar range predicates in `src/drawing.rs` so the
  bindings and the future renderer validate identically. Option tables must be
  plain, typed rather than coerced, and copied during the call; key inspection is
  bounded by the field count and stops at the first unexpected key. A sprite
  needs a live CPU-ready image and never advances loading. Sprite attempts are
  capped at 10,000 per draw, including refusals, and are counted separately from
  the asset budget so internal dimension lookups cannot exhaust it.
- `ctx.data` preserves integer text, null, and array/object identity; ordinary
  Luau numbers encode as finite floats. `data.array` checks only the keys of the
  table it marks, since the script may replace elements afterwards; element
  values are validated wherever they are converted. Do not silently narrow integers or omit
  unsupported export values. Data strings/tables remain in VM-owned storage.
  `ctx.fs` reads only canonical bundle/data roots; writes use a synced temporary
  file and atomic replacement in the configured data root. Keep draw writes and
  stale bindings rejected. See the Phase 1a contract for limits and path policy.
  Listing classifies rather than refuses: entries whose names the path policy
  cannot represent, links, and other node types are reported as `unsupported`
  so one entry cannot hide a directory's siblings, while traversing them stays
  refused. A failed `mkdir` unwinds only the directories that call created.
- `AssetStore` roots PNG requests at the canonical bundle, resolving spellings
  synchronously so coalescing and handle identity are decided before returning.
  Content work is staged across bounded passes on one worker that owns no VM,
  kernel, plugin or GPU object. The store grants every allowance and reserves
  every buffer before the worker allocates it; at most one grant is outstanding,
  so a busy worker accumulates no credits. Admission, path and queue refusals
  are immediate errors; post-admission failures are inspectable failed jobs
  with `io`, `format`, `unsupported`, `limit` or `capacity` codes, and only
  worker disconnection or a broken invariant is a service fault. Identities are
  append-only per session and never reissued, including after a rolled back
  publication. Unload invalidates every alias but does not free pixels a caller
  still pins. See the Phase 1 record for limits and the frozen status schema.
- `ScriptHost` owns the store, so a standalone host loads real PNGs without a
  window. `ctx.assets` exists in every callback; `request_png` and `unload` need
  init or update, and the 256-call budget counts hits, refusals and wrong-phase
  calls before argument conversion. The canonical VM wrapper is published as the
  last step of admission, so a failed allocation rolls the job back and burns its
  identity. Wrappers live exactly as long as their registry entry: committing a
  terminal transition copies its status onto the handle and drops the wrapper,
  which is also what releases the slot. One service pass runs per valid `frame`
  or `step` and none for catch-up ticks, `init`, `draw` or a refused call.
  Scripts read a published view of the store, not the store, and worker progress
  reaches that view only at the first update boundary of a frame, so a zero-tick
  frame cannot reveal a completion and `bytes_read` advances at the tick rate. A
  script's own request and unload publish immediately; the boundary governs
  worker results. Rust callers read the live store through `assets()` instead.
  `advance_assets`/`drain_assets` drive the same service
  without running callbacks or advancing simulation. Stop, fault and drop release
  the store and join its worker; a fault does so immediately and runs no Luau
  shutdown. Worker loss and broken invariants become session faults, while a
  failed job stays inspectable.
- The renderer owns a pool of at most 128 GPU allocation slots for the graphics
  context's lifetime, created lazily through the single `Texture2D::from_rgba8`
  call site and reused by resizing in place. Recreating one would grow the
  batcher's unbatched list and the backend's texture records, neither of which
  ordinary collection removes, so `identities_stable` must hold. Uploads move at
  most eight row bands and 256 KiB per pass with one destination allocation, and
  an image publishes its mapping only after every band succeeds. A slot the
  queued frame draws from is pinned until that frame is presented; transient
  pressure yields with the admission intact rather than faulting. Attach retires
  the previous session first, and retirement shrinks every slot to a transparent
  1x1 texel, so no session inherits another's mapping or content. Validation
  covers the whole list before anything is queued and refuses missing, foreign
  and unloaded images, which the runtime turns into a presentation fault through
  the existing primary-fault path. Keep `build_textures_atlas` and
  `reset_textures_atlas` prohibited, keep the `get_internal_gl` adapter confined
  to `src/rendering.rs`, and never let a `Texture2D` or a raw handle escape it.
- Save handling belongs to game scripts: schema, file layout, timing, restoration,
  and migrations. The engine may expose general filesystem access, tot parsing
  and formatting, and tot-export utilities for JSON, YAML, and TOML. Do not add
  engine-owned save slots or automatic world serialization. Verify reusable
  exporter conversion rules: tot 0.2.0 provides YAML/TOML library APIs behind
  the `yaml`/`toml` features, enabled by `scripting`. Use TOML `NullPolicy::Error`
  to reject nulls. Conversion diagnostics use tot paths with zero-based indices.

## Native plugin boundary

Provide a C-compatible API/ABI for loading native shared libraries at runtime.
Do not expose Rust's native ABI, references, collections, or internal `hecs` and
`mlua` objects across this boundary.

Use `extern "C"` entry points, `#[repr(C)]` data where
needed, opaque handles, and explicit API version negotiation. Document ownership,
allocation/freeing, error reporting, callback lifetime, and thread affinity.
Keep unsafe code localized with safety invariants; prevent Rust panics from
unwinding across C calls. Keep libraries loaded while their code or data remains
reachable. Hot unloading/reloading is not an established requirement.

Use `libloading` 0.9.0. ABI 1 currently supports `x86_64-pc-windows-msvc` with
exact C layout/table sizes and one `protogine_plugin_query` export. The immutable
declarations specify IDs, schema IDs/versions and batch signatures. Phase 5 supplies
actual invocation and Luau buffers. Keep engine command buffers as a future
option; scheduling and mutation timing are separate contracts.

- `game.tot` is optional; present files require integer `version 1` and an optional
  ordered `plugins` array of `{id "org.example.name" library "plugins/name.dll"}`.
  Reject unknown fields, duplicate IDs/primary paths, unsupported versions and
  invalid paths. Every declaration is required. Limits: 64 KiB manifest, 16 plugins,
  64 functions/plugin, 128-byte dotted lowercase IDs, 1024-byte relative DLL paths.
- Resolve every primary path before any load, and query/validate every library
  before init. Primary paths reject symlinks/reparse points below the canonical
  bundle root. Windows searches adjacent dependencies and System32 only; loaded
  module basename reuse still requires unique private dependency names. Keep
  bundle/dependency files stable; this is trusted execution, not a native sandbox.
- Safe `GameRuntime::load` variants reject native declarations. Unsafe
  `load_trusted(root, data_root, limits, seed)` accepts the ABI/trust obligations.
  Player and the native-enabled headless example opt into trusted loading.
  Standalone `ScriptHost` is VM-only and does not load native declarations.
- Initialize in manifest order before VM creation. Failed init cleans up itself
  and publishes null; successful init publishes a non-null plugin-owned instance.
  On stop/fault/drop, destroy all VM references before reverse native teardown.
  Only normal stop invokes Luau shutdown; faults/drop skip it. All libraries stay
  loaded until every initialized instance has received shutdown, including on
  partial startup failure. Shutdown consumes the instance on every return status.
  Continue after cleanup errors and preserve the primary fault.
- Host pointers/logging are scoped to synchronous calls on the runtime thread.
  Workers must join before return. Diagnostics use 1024 host-owned UTF-8 bytes;
  pointer/capacity cannot change. Logging is capped at 4 KiB/message and 64 KiB/call;
  contract faults latch even if foreign code ignores a status. Prevent unwind
  across extern C boundaries with the SDK's `catch_status` shim. Memory corruption,
  aborts, foreign exceptions and native hangs cannot be contained in process.
  Guard panic-payload disposal too; if disposal panics, intentionally forget the
  secondary payload so another destructor cannot unwind through the C boundary.
- `ctx.native.call(plugin_id, function_id, input_buffer, output_buffer)` runs only
  in init/update and returns written bytes. Native functions expire with their
  callback; `ctx.native.plugins` contains owned read-only schema metadata, built
  once from the frozen registry and shared by every callback rather than rebuilt
  per call. Copy
  input before FFI, use disjoint zeroed host output, and publish only the validated
  success prefix. Preserve the suffix and all output on failure, even for aliased
  VM buffers. Never retry automatically or pass VM/kernel pointers to plugins.
- Native statuses 1..4 are recoverable; panic/contract/unknown statuses, malformed
  diagnostics/output and host-service faults poison the registry and latch the
  first detailed failure outside pcall. Skip systems and tear down the session.
  Enforce 16 MiB combined buffers/call, 64 MiB requested bytes and 128 call attempts
  per callback, counting malformed arguments and draw/shutdown phase refusals. Script and native logs share the callback's 64 KiB budget in order.
  Check deadlines around native work; native hangs remain process-level failures.
- Edit SDK definitions, then regenerate with `cargo run -p protogine-headergen`.
  cbindgen 0.29.2 is pinned in the development tool; the checked-in C header is a
  generated artifact and normal engine builds do not run generation. The tool
  decorates the generated extern prototype with the Windows export macro.
  `.gitattributes` pins the generated header to LF for exact-byte drift checks,
  including Windows checkouts with `core.autocrlf=true`.

## Working in this repository

- Read this file, `Cargo.toml`, and the relevant source before making changes.
  Check Git status and preserve unrelated work.
- Keep changes focused and reversible. Avoid speculative subsystems and record
  consequential design decisions in repository documentation as they are made.
- Distinguish implemented behavior from plans. Do not assume RPG Maker or Godot
  feature parity, file compatibility, or workflows beyond the requirements above.
- For Rust changes, run the applicable checks from the repository root:

  ```text
  cargo fmt --all -- --check
  cargo check --workspace --all-targets
  cargo test --workspace
  cargo test --workspace --no-default-features
  cargo clippy --workspace --all-targets -- -D warnings
  cargo build --release --bin protogine-player
  ```

  Asset changes also require the decoder-only configuration, which builds and
  tests the store without a VM or a window:

  ```text
  cargo test --workspace --no-default-features --features assets
  cargo clippy --workspace --all-targets --no-default-features --features assets -- -D warnings
  ```

  Renderer changes also require the graphics-only configuration, which builds
  and tests the renderer without a VM, and the GPU harness below:

  ```text
  cargo test --workspace --no-default-features --features graphics
  cargo clippy --workspace --all-targets --no-default-features --features graphics -- -D warnings
  ```

  Scripting changes also require the real headless feature configuration and
  the combined Player/scripting configuration:

  ```text
  cargo test --workspace --no-default-features --features scripting
  cargo check --workspace --all-targets --all-features
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --release --no-default-features --features scripting --lib --test kernel --test runtime --test drawing --test scripting --test scripting_feasibility --test scripting_utilities --test assets --test script_assets
  cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
  cargo run --example script_host --no-default-features --features scripting -- --ticks 200 examples/games/loading
  cargo run --example script_host --no-default-features --features scripting -- --preload --ticks 4 examples/games/loading
  ```

  The last two are the staged and preloaded asset drivers. The three-tick
  lifecycle run is not a completion check for staged loading: read the printed
  `assets [...]` trace and confirm the sheet actually completed.

  Native/SDK changes also require:

  ```text
  cargo check --no-default-features --features native-plugins
  cargo test --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
  cargo test --release --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
  cargo test --release -p protogine-plugin-api
  cargo run -p protogine-headergen -- --check
  cargo test --test plugins -- --ignored
  ```

  The supported Windows `plugins` tests require `clang` (or a compiler path in
  `CLANG`) plus the MSVC/Windows SDK. C fixtures compile using only the generated
  header and platform headers. Native execution runs in subprocesses with an
  independent 15-second watchdog; preserve that timeout when adding fault cases.
  The ignored native Player test requires a graphics context. See the README for
  the independently compiled lifecycle example and dependency lookup policy.
  The SDK's `panic_boundary` suite runs C entry points in child processes with a
  10-second watchdog; it covers ordinary and recursively panicking payload cleanup.
  The native suite also covers buffer aliasing/zero lengths, failure publication,
  poison/refusal, deadlines/limits, callback expiry and distance-field parity.
  Its ignored captures include the actual native distance result and repeat PNGs.
  Build the standalone example with `pwsh -NoProfile -File tools/build_native_distance.ps1`.
  Run `cargo run --release --example native_benchmark -- target/native-distance-demo/game`
  for end-to-end typed-wrapper timings; the example's BENCHMARK.md records the
  method and measured limits. Do not report FFI-only timing as authoring performance.

  `scripting_feasibility` runs potentially runaway fixtures in child processes
  with a 10-second watchdog. Keep that outer timeout independent of the VM.
  `assets` verifies rooting, path policy, coalescing and identity, staged
  passes, decoded pixels, admission and storage bounds, eviction, cancellation
  at every stage, and worker teardown, with no VM or graphics context. It runs
  the adversarial decoder fixtures in child processes with a 10-second watchdog,
  because decoder stages are not preemptible and a store joins its worker on
  drop, so an in-process deadline cannot bound one that never returns. Its small
  PNG fixtures and expected RGBA bytes are committed under `tests/fixtures/assets`
  and regenerated with `python tools/asset_fixtures.py`; the maximum-size
  fixtures are built in the test from stored DEFLATE blocks. Both specify
  expected bytes independently of this engine's decoder, so regenerate rather
  than recording whatever an implementation produced.
  `script_assets` verifies the Luau asset and sprite bindings: pending handles
  and loading spread across callbacks, canonical wrappers, phase and budget
  ceilings, failed jobs, eviction with retained terminal status, the
  update-boundary publication, sprite defaults/options/refusals, command
  ordering, and cleanup on stop, fault and drop. It reuses the committed Phase 1
  PNG fixtures and needs no graphics context. Foreign handles and rolled back
  publication need a harness the VM cannot reach and are unit tests in
  `src/scripting/assets.rs`, as the world bindings do.
  `rendering` verifies the renderer's command validation and admission
  bookkeeping with no graphics context: `MacroquadRenderer::new` allocates
  nothing, `attach` on an empty pool touches no texture, and `validate` is
  exactly the check `render` runs before it queues anything.
  `scripting_utilities` verifies conversions, rooted I/O, retained-value/stale-call
  behavior, limits, file replacement failure, and script-owned save/load.
  `kernel` and `runtime` cover handles, immediate writes, system ordering, scoped
  operations, latched resource limits, catch-up input, and fixed-input replay.
  `drawing` verifies command publication/validation, expired bindings, faults,
  and seeded sample state without graphics. `player_capture` verifies the actual
  copied Player, source edits, seeded PNG bytes, movement, compositing and faults.
  On Windows, run `cargo build --release --bin protogine-player` followed by
  `pwsh -NoProfile -File tests/player_input.ps1` when changing physical input or
  Player shutdown. The probe requires PowerShell 7 and a working graphics context;
  it checks each key mapping, held/edge state, Escape/close cleanup and fault exit.

  Check additional target/feature configurations as they are introduced and
  document their actual commands here. `cargo run --bin protogine-player` opens
  the current Player.
- For rendering changes, use the built-in capture mode documented in the README;
  do not reintroduce a separate copy of the renderer as a capture harness. Run
  `cargo test --test player_capture -- --ignored` when a graphics context is
  available. Capture mode requires graphics even though it exits unattended.
- The shared renderer's GPU evidence comes from
  [`examples/renderer_harness.rs`](examples/renderer_harness.rs), which drives
  the production `MacroquadRenderer` and `AssetStore` rather than a second copy
  of either. A `cargo test` harness cannot own the main thread a graphics
  context needs, so build it and run each mode through its watchdog:

  ```text
  cargo build --release --no-default-features --features graphics --example renderer_harness
  powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode cycles
  powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode bands
  powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode eviction
  powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode pressure
  powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode recreate
  ```

  `cycles` runs 100 attach/load/retire cycles and asserts the pool's lifetime
  bounds and retirement; `bands` proves bounded per-pass progress and that
  drawing advances no upload; `eviction` cancels a staged upload and reuses its
  slot; `pressure` fills the pool, pins every slot with a queued frame, and
  requires a yield rather than a fault. `recreate` is a negative control and
  must fail at its named assertion. The runner enforces an independent
  60-second watchdog per mode and requires each mode's own `PASS mode=` marker,
  since exiting zero does not prove the assertions ran. The script works under
  Windows PowerShell 5.1 and PowerShell 7.
- The PNG/sprite Phase 0 dependency probe has its own bounded runner. It probes
  backend APIs and reuses the Player PNG writer; it is not the production renderer.
  When changing that probe, run `cargo build --release --example png_sprite_probe
  --locked --offline`, then `pwsh -NoProfile -File tools/run_png_sprite_probe.ps1
  -Mode <mode>` for `cpu`, `gpu`, `gpu-recreate`, and `gpu-early-retire`. The CPU
  runner uses Python stdlib fixtures and a 10-second child watchdog; GPU modes use
  30 seconds. Negative controls must fail at their named assertion. Generated
  files go under `target/png-sprite-probe/`; durable evidence and contracts are
  linked from the Phase 0 record.
- Add focused behavioral tests for simulation and boundary changes. Exercise
  script errors and invalid handles, plugin ABI mismatches, and packaged-game
  loading when those capabilities exist. Verify export behavior by launching a
  shipped game independently of the source tree and editor.
- For graphics, editor interaction, or audio changes, perform an appropriate
  runtime check and state any verification gaps. Compilation alone does not
  demonstrate correct rendering, interaction, or playback.
- Documentation-only changes need a content/diff review rather than a new test
  suite. Report what changed, what was checked, and any unresolved limitations.
