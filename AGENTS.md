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

The Cargo workspace contains the `protogine` 0.1.0 engine (Rust edition 2024),
the dependency-free `protogine-plugin-api` SDK, and a separate header generator.
The default `player` feature enables graphics, scripting and native plugins;
the shared library also builds with `--no-default-features`.

| Surface | Ownership / entry points |
| --- | --- |
| Startup | [Bundle discovery](src/bundle.rs) finds readable `game/main.luau` beside the executable, independent of cwd; [Player](src/bin/player.rs) owns window, input, startup/error screens and [capture](src/bin/player/capture.rs) |
| Simulation | [Kernel](src/kernel.rs), [input](src/input.rs), [GameRuntime](src/runtime.rs): entities, fixed ticks, immediate script writes, then velocity integration |
| Luau | [ScriptHost](src/scripting.rs), [modules](src/scripting/modules.rs), [data](src/scripting/data.rs), [filesystem](src/scripting/filesystem.rs), [world](src/scripting/world.rs) and [draw bindings](src/scripting/drawing.rs) |
| Assets | [Types](src/assets.rs) compile without a decoder; `assets` enables the [store](src/assets/store.rs)/[worker](src/assets/worker.rs); [rooted traversal](src/rooted_path.rs) is shared with filesystem utilities |
| Drawing | [Owned commands](src/drawing.rs) contain clear/rectangle/sprite data; [asset bindings](src/scripting/assets.rs) work headlessly; `graphics` enables the [shared renderer](src/rendering.rs) without a VM |
| Native | [Manifest](src/manifest.rs), [loader](src/plugins.rs), [buffer bridge](src/scripting/native.rs), [SDK](sdk/src/lib.rs), generated [C header](include/protogine_plugin.h) and [headergen](tools/headergen/src/main.rs) |

[README](README.md) documents running games and the implemented APIs/limits.
[TODO](TODO.md) owns the remaining feature backlog; the root
[tilemap/collision plan](TILEMAP_COLLISION_PLAN.md) records accepted T1-T8 with
implementation unstarted. Single-map collision is the first milestone;
simultaneous maps, independent layers and streaming are required before full
plan completion. Colliders are hecs components; T6's first-milestone lifecycle
must be revised for later scope. Detailed proposals remain marked in the plan;
accepted directions do not imply implemented behavior.
[Development and verification](docs/DEVELOPMENT.md) owns the required check
matrix and harness guidance. [Implementation records](docs/implementation/README.md)
retain decisions, phase contracts, evidence and deferred scope. Both scripting/C
API and PNG/sprite milestones are complete; Kira audio, editor and export tooling
remain unimplemented. The [examples](examples/README.md) cover authoring and art
provenance; the [distance schema](examples/games/native_distance/SCHEMA.md) and
[benchmark](examples/games/native_distance/BENCHMARK.md) cover native computation.

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

Read the relevant [scripting/C API contracts](docs/implementation/SCRIPTING_C_API_PLAN.md)
and [PNG/sprite contracts](docs/implementation/PNG_SPRITE_PLAN.md) before changing
these boundaries. Preserve these invariants:

- Luau owns game behavior; Rust supplies deliberate service APIs with explicit
  handles and mutation timing. Ship source and assets beside the Player;
  ordinary script/PNG edits need no engine rebuild. Editor playtesting and
  exported games must use the same APIs and explicit bundle root. Source
  modules, declared native plugins and full runtime restart are the accepted
  milestone; archive/export formats and live reload remain separate work.
- Validate the plain init/update/draw/shutdown table and expire scoped functions
  after each call. Restart creates a new VM. Faults stop callbacks immediately;
  only normal stop runs Luau shutdown. Drop releases resources without invoking
  scripts. Keep source/import/log/heap limits aligned with the README.
- Cancellation uses mlua `catch_rust_panics(false)` and a private host unwind
  payload; scripting requires `panic=unwind`. Ordinary caught allocation errors
  are recoverable; host-detected budget failures latch outside Luau. Check every
  VM interrupt and every host-initiated boundary exactly: expensive built-ins
  invalidate fixed-stride clock sampling. Retest protected calls/metamethods and
  expensive built-ins, and rerun the distance benchmark after deadline changes.
- `GameRuntime` owns kernel mutation/timing; standalone `ScriptHost` omits world
  and input. Release all hecs/RefCell borrows before VM work. World reads return
  owned values/opaque handles; only init/update mutate. Validate session and
  generation on every access. The private weak wrapper cache packs (session,
  hecs slot) into an exact Lua number and compares the stored handle before
  reuse. Roll back unpublished entities if wrapper/cache allocation fails.
  Fixed systems follow successful updates; session faults invalidate the kernel.
- `ctx.draw` exists only in draw and publishes a fresh owned list on success.
  Validate finite values before f32 conversion. Fault/stop clears commands;
  rejected alpha preserves them. Keep clear-discard, ordered alpha blending and
  empty-frame black semantics identical in headless data and rendered output.
  The combined 10,000-command cap includes sprites and latches on overflow.
- Sprite commands own an `ImageId`, integer half-open source, destination,
  flips and tint. Keep shared scalar predicates and `Sprite::check` in
  `src/drawing.rs`. Options must be plain, typed and copied; key inspection is
  bounded by field count and stops at the first unexpected key. Require a live
  CPU-ready image. Count all 10,000 sprite attempts, including refusals,
  separately from the 256-call asset budget; drawing never advances loading.
- Data preserves integer text, null and array/object identity; ordinary Luau
  numbers serialize as finite floats. `data.array` checks only its table's keys;
  conversions validate elements. Keep strings/tables in VM storage and refuse
  unsupported exports without narrowing or omission. Use tot's library exporters,
  YAML/TOML features and TOML `NullPolicy::Error`; diagnostic indices are zero-based.
- Filesystem reads use canonical bundle/data roots. Writes require an explicit
  disjoint data root and synced temporary-file replacement. Refuse draw writes,
  stale functions and descendant links/reparse points. Listing classifies
  unrepresentable names/links/other nodes as `unsupported` without hiding siblings;
  traversal remains refused. Failed mkdir unwinds only directories it created.
  Scripts own save schema, files, timing, restoration and migration; no engine
  save slots or automatic ECS/VM serialization.
- Asset admission resolves canonical bundle paths synchronously for coalescing
  and identity. One worker owns no VM, kernel, plugin or GPU object; the store
  grants work and reserves buffers before allocation. At most one grant is
  outstanding, with no accumulated credits. Admission/path/queue refusals are
  immediate errors; admitted failures are inspectable `io`, `format`,
  `unsupported`, `limit` or `capacity` jobs. Worker loss/broken invariants fault
  the service. Image IDs are append-only, including burned publication IDs;
  unload invalidates every alias but pinned pixels remain accounted until released.
- `ScriptHost` owns the store. `ctx.assets` exists in every callback; request/unload
  require init/update. The 256-call budget counts hits, malformed arguments and
  wrong-phase refusals before conversion. Publish canonical wrappers last;
  failed allocation rolls back new admission and burns its ID. Copy terminal
  status onto retained handles and remove wrappers when draining registry slots.
- One CPU service pass runs per valid frame/step or standalone host update,
  never per catch-up tick, init, draw or refused call. Scripts read a published
  view: worker progress reaches it at the first update boundary, so zero-tick
  frames reveal no new status. Own request/unload effects publish immediately;
  preserve the no-service-between-commit-and-callback invariant this requires.
  Rust `assets()` reads the live store. Host advance/drain helpers use the same
  service without callbacks or simulation. Stop/fault/drop release and join the
  worker; a fault skips Luau shutdown and may wait for non-preemptible work.
- The renderer owns at most 128 context-lifetime GPU slots, lazily created at
  one `Texture2D::from_rgba8` call site and reused by resize. Recreation grows
  backend/batcher records; keep `identities_stable`. Each upload pass transfers
  at most eight row bands/256 KiB with one destination allocation; publish only
  after all bands succeed. Queued-frame slots remain pinned through presentation;
  transient pressure yields with admission intact. Attach retires the old session.
- Retirement clears slots to transparent 1x1 texels; track cleared contents
  separately from dimensions because a populated 1x1 image still needs clearing.
  Interactive faults retire after presentation while the error screen remains
  open; capture retires after final readback. Validate the entire command list
  before queueing, refusing missing/foreign/unloaded images through the runtime's
  primary presentation-fault path. Keep `get_internal_gl` confined to
  `src/rendering.rs`, let no texture/raw handle escape, and prohibit atlas
  build/reset. Use a live owning context, default screen-space camera and ordinary
  blend/material state; flush queued work before resizing referenced slots.

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
declarations specify IDs, schema IDs/versions and batch signatures; the Luau
buffer bridge invokes them synchronously. Keep engine command buffers as a future
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
  per call. Copy input before FFI, use disjoint zeroed host output, and publish only the validated
  success prefix. Preserve the suffix and all output on failure, even for aliased
  VM buffers. Never retry automatically or pass VM/kernel pointers to plugins.
- Native statuses 1..4 are recoverable; panic/contract/unknown statuses, malformed
  diagnostics/output and host-service faults poison the registry and latch the
  first detailed failure outside pcall. Skip systems and tear down the session.
  Enforce 16 MiB combined buffers/call, 64 MiB requested bytes and 128 call attempts
  per callback, counting malformed arguments and draw/shutdown phase refusals.
  Script and native logs share the callback's 64 KiB budget in order.
  Check deadlines around native work; native hangs remain process-level failures.
- Edit SDK definitions and regenerate the C header with the separate cbindgen
  0.29.2 tool. Preserve the Windows export macro and exact LF bytes; follow the
  [SDK generation/drift checks](docs/DEVELOPMENT.md#native-plugins-and-sdk).

## Working in this repository

- Read this guide, `Cargo.toml`, the relevant contracts and source before editing.
  Check Git status and preserve unrelated work.
- Keep changes focused and reversible; avoid speculative subsystems. Record
  consequential decisions in repository documentation as they are made.
- Distinguish implementation, historical evidence and proposals. Do not assume
  RPG Maker/Godot parity, compatibility or workflows beyond these requirements.
- Run the applicable [development checks](docs/DEVELOPMENT.md), including feature,
  release, native, capture and live-input gates for affected code. Preserve the
  independent watchdogs and use the production renderer for visual evidence.
- Documentation-only edits need content, link and diff review. Report changes,
  checks actually run and unresolved limitations; do not imply fresh execution
  evidence from a historical phase record.
