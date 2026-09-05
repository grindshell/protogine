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

The repository contains a single Cargo package, `protogine` version `0.1.0`, using
Rust edition `2024`. The minimal `protogine-player` binary opens a Macroquad
window with a "Missing game data" fallback. Its default `player` feature enables
graphics; the shared library also builds with `--no-default-features`.

- [Shared library](src/lib.rs) and [bundle discovery](src/bundle.rs): look for a
  readable `game/main.luau` beside the executable, independent of the working
  directory. This is the initial unpacked bundle convention.
- [Player](src/bin/player.rs): startup presentation, including detected and
  inaccessible bundle states. Detection does not yet execute scripts.
- [Built-in capture](src/bin/player/capture.rs): `PLAYER_CAPTURE` saves a PNG and
  exits; optional `PLAYER_CAPTURE_FRAME`, `PLAYER_WIDTH`, and `PLAYER_HEIGHT`
  control timing and dimensions. Use this for agent-driven visual checks.
- [README](README.md): run/build commands, bundle layout, and current limits.
- [Discovery tests](tests/bundle.rs): filesystem boundaries and failure cases.
- [Capture smoke test](tests/player_capture.rs): opt-in GPU verification of the
  actual Player, including pixel repeatability and process exit codes.

Only Macroquad and PNG encoding are wired into the runtime so far. Simulation
with hecs, Luau execution, Kira audio, native plugins, the editor, and export
tooling remain unimplemented. The remaining sections describe intended architecture and
implementation guidance. Keep this status current as implementation lands.

## Required technology choices

| Responsibility | Crate | Requested version | Configuration |
| --- | --- | --- | --- |
| Graphics and window/frame integration | `macroquad` | `0.4.16` | Use for rendering |
| ECS foundation for the core update loop | `hecs` | `0.11.1` | Engine-owned update ordering |
| Audio | `kira` | `0.12.4` | Route engine audio through Kira |
| Luau scripting host | `mlua` | `0.12.1` | Enable `luau-jit` |

These versions are project requirements. Verify versions, features, and target
support when wiring dependencies; report incompatibilities instead of silently
substituting versions or libraries. Keep `Cargo.lock` updated with resolved
application dependencies.

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
implementations of gameplay. The initial layout is one package with a shared
library and a feature-gated Player binary. Further crate/module splits remain
open; introduce only the structure needed for the current implementation slice.

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
- Use the same script APIs and loading rules during editor playtesting and in
  exported games. Resolve content through an explicit project/package root,
  rather than relying on the process working directory.
- Report script failures with useful source context. Define lifecycle, reload,
  and host-access rules when implementing the scripting boundary; none are
  implemented yet.

## Native plugin boundary

Provide a C-compatible API/ABI for loading native shared libraries at runtime.
Do not expose Rust's native ABI, references, collections, or internal `hecs` and
`mlua` objects across this boundary.

When implementing it, use `extern "C"` entry points, `#[repr(C)]` data where
needed, opaque handles, and explicit API version negotiation. Document ownership,
allocation/freeing, error reporting, callback lifetime, and thread affinity.
Keep unsafe code localized with safety invariants; prevent Rust panics from
unwinding across C calls. Keep libraries loaded while their code or data remains
reachable. Hot unloading/reloading is not an established requirement.

The loader library, supported operating systems, exported symbols, and ABI
compatibility policy remain to be designed. Treat native plugins as trusted code.

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
