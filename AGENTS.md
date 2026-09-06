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
The optional [manifest](src/manifest.rs) and [native loader](src/plugins.rs) add
trusted C plugin startup/cleanup before/after the VM. The [SDK](sdk/src/lib.rs)
generates [the C header](include/protogine_plugin.h) with the separate
[headergen tool](tools/headergen/src/main.rs). The [native buffer adapter](src/scripting/native.rs)
supplies synchronous batch calls; the [distance-field example](examples/games/native_distance/SCHEMA.md)
includes a typed Luau wrapper and parity implementation. Kira audio, the editor,
and export tooling remain unimplemented.

Completed plans are indexed in [docs/implementation/README.md](docs/implementation/README.md).
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
  host-detected budget failures remain latched outside Lua.
- `GameRuntime` owns kernel mutation and timing; standalone `ScriptHost` calls
  omit world/input bindings. World operations return owned data/opaque handles
  and release all hecs/RefCell borrows before VM work. Only init/update mutate
  world state. Validate session identity and generation on every handle access.
  Canonical wrappers in a private weak VM cache preserve Luau table-key identity.
  Roll back unpublished entities if wrapper/cache allocation fails during spawn.
  Fixed systems run after a successful callback; session faults invalidate the
  kernel. See the Phase 2 contract for entity/operation limits and input timing.
- `ctx.draw` exists only in draw and publishes a fresh owned list only after a
  successful callback. Validate finite coordinates/sizes/colors before f32
  conversion; the 10,000-command cap latches. Fault/stop clears published commands.
  Keep renderer semantics (clear discards previous drawing, ordered alpha-blended
  rectangles, empty frame black) aligned with headless data and GPU tests.
  See Phase 3 for capture seeding, shutdown, input mappings and fault code 3.
- `ctx.data` preserves integer text, null, and array/object identity; ordinary
  Luau numbers encode as finite floats. Do not silently narrow integers or omit
  unsupported export values. Data strings/tables remain in VM-owned storage.
  `ctx.fs` reads only canonical bundle/data roots; writes use a synced temporary
  file and atomic replacement in the configured data root. Keep draw writes and
  stale bindings rejected. See the Phase 1a contract for limits and path policy.
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
  callback; `ctx.native.plugins` contains owned read-only schema metadata. Copy
  input before FFI, use disjoint zeroed host output, and publish only the validated
  success prefix. Preserve the suffix and all output on failure, even for aliased
  VM buffers. Never retry automatically or pass VM/kernel pointers to plugins.
- Native statuses 1..4 are recoverable; panic/contract/unknown statuses, malformed
  diagnostics/output and host-service faults poison the registry and latch the
  first detailed failure outside pcall. Skip systems and tear down the session.
  Enforce 16 MiB combined buffers/call, 64 MiB requested bytes and 128 call attempts
  per callback. Script and native logs share the callback's 64 KiB budget in order.
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

  Scripting changes also require the real headless feature configuration and
  the combined Player/scripting configuration:

  ```text
  cargo test --workspace --no-default-features --features scripting
  cargo check --workspace --all-targets --all-features
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --release --no-default-features --features scripting --lib --test kernel --test runtime --test drawing --test scripting --test scripting_feasibility --test scripting_utilities
  cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
  ```

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
- Add focused behavioral tests for simulation and boundary changes. Exercise
  script errors and invalid handles, plugin ABI mismatches, and packaged-game
  loading when those capabilities exist. Verify export behavior by launching a
  shipped game independently of the source tree and editor.
- For graphics, editor interaction, or audio changes, perform an appropriate
  runtime check and state any verification gaps. Compilation alone does not
  demonstrate correct rendering, interaction, or playback.
- Documentation-only changes need a content/diff review rather than a new test
  suite. Report what changed, what was checked, and any unresolved limitations.
