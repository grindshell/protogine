# Protogine

Protogine (Prototype Engine) is a tile-based game engine inspired by RPG Maker,
with a shared kernel for its editor and standalone Player.

The current Player is a minimal startup shell. It opens a
resizable window and displays **Missing game data** when no bundle is present.
Press Escape or close the window to quit. The startup screen needs no external
fonts, images, audio, or game files.
The shared library also provides a headless Luau runtime with an entity kernel,
fixed updates, input handling, and script data/filesystem utilities.

## Run and build

Use a Rust toolchain supporting edition 2024. From the repository root:

```text
cargo run --bin protogine-player
cargo build --release --bin protogine-player
```

`cargo run` also selects the Player by default. The release executable is
`target/release/protogine-player` (`.exe` on Windows). Copy it into a distribution
directory to run independently of this repository.

## Screenshot capture

The Player includes a capture mode for snapshot tests and agent-driven visual
checks. In PowerShell:

```powershell
$env:PLAYER_CAPTURE = 'target/captures/missing.png'
cargo run --bin protogine-player
Remove-Item Env:PLAYER_CAPTURE
```

| Environment variable | Default | Purpose |
| --- | --- | --- |
| `PLAYER_CAPTURE` | Unset | Write a PNG to this path, then exit |
| `PLAYER_CAPTURE_FRAME` | `4` | One-based rendered frame to capture, after drawing |
| `PLAYER_WIDTH` | `960` | Initial window width |
| `PLAYER_HEIGHT` | `600` | Initial window height |

For a smaller capture, set `PLAYER_WIDTH=400` and `PLAYER_HEIGHT=260` before
launching. Width and height also work in interactive mode. Frame numbers must be
positive integers; dimensions must be in `1..=65535`, subject to platform window
and graphics limits. `PLAYER_CAPTURE_FRAME` requires `PLAYER_CAPTURE`.

Capture paths are relative to the working directory, unlike game bundle paths.
Parent directories are created as needed; an existing output file is overwritten.
The image is always PNG. Capture mode disables resizing and DPI scaling, verifies
that the framebuffer matches the requested dimensions, and needs a working
graphics context. It uses the actual Player renderer and whichever startup state
bundle discovery selects.

Exit codes are `0` for a saved capture, `1` for capture failure or interruption,
and `2` for invalid environment configuration. Errors go to stderr. Unset the
capture variables to return to interactive mode. On Windows, `cargo run` waits
for the Player; scripts launching the release GUI executable directly should use
a process API that waits and collects its exit code.

The current static startup screen produces repeatable pixels on the same
graphics stack. Capture frame selection does not yet establish deterministic
simulation timing or guarantee identical rendering across platforms/drivers.

The opt-in smoke test runs the compiled Player from an isolated distribution,
checks all startup states, repeatability, dimensions, and failure exit codes:

```text
cargo test --test player_capture -- --ignored
```

## Initial bundle convention

For this first slice, a bundle is an unpacked `game` directory beside the
executable, containing a readable `main.luau` file:

```text
My Game/
  protogine-player.exe
  game/
    main.luau
```

The Player checks this location once at startup. The working directory does not
affect discovery. During `cargo run`, this means `target/debug/game/main.luau`.
An absent directory or entry point shows **Missing game data**. An invalid file
type or access failure shows **Game data unavailable**, with details on stderr
in console builds.

A detected entry point currently shows **Game data detected** and explains that
loading is not implemented. Detection checks file type and readability, not
Luau syntax or complete bundle validity. Player script execution, archive formats,
export tooling, audio, native plugins, and the editor are future work. The
headless runtime below already provides simulation independently of the Player.

## Headless scripting

Enable the optional `scripting` feature to use `protogine::runtime::GameRuntime`.
The sample loads source modules and runs init, three fixed updates, draw callbacks,
and orderly shutdown without a window:

```text
cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
```

`GameRuntime::load` takes an absolute game-directory path and `ScriptLimits`.
`main.luau` returns a plain table containing optional `init`, `update`, `draw`,
and `shutdown` functions. Callbacks return no values. The host exposes
`ctx.log(message)`, `ctx.data`, `ctx.fs`, `ctx.world`, and `ctx.input`;
stored context functions expire when
their callback ends, while returned data values can be retained.
Logs from the last call/frame can be retrieved in callback order with `take_logs()`.

Call `init()` once, then `frame(elapsed_seconds, input)` for each presentation
frame. It runs fixed updates and calls draw once. For exact stepping, use
`step(input)` and `draw(alpha)` separately. Updates receive `dt = 1/60`; alpha
must be finite and in `[0, 1)`. The runtime does not interpolate state or expose
drawing commands yet. `shutdown()` is idempotent and skips game cleanup
if init did not complete or the session faulted. Dropping a host only releases
resources. Errors include a lifecycle phase and available Luau source context.

The lower-level `protogine::scripting::ScriptHost` remains available for standalone
VM/data work. Its `update()` invokes one fixed callback without kernel systems;
it supplies only log/data/filesystem bindings, with no world, input, or clock.

Modules use extensionless relative paths, such as `require("./counter")` or
`require("../shared")` within the bundle. Files must be UTF-8 `.luau` source.
Aliases, dotted path segments, directory init modules, and paths escaping the
canonical bundle root are rejected. Successfully loaded module values are cached
once per VM; loaded source changes require a new host. Cycles and failed imports
return script errors; failed results are not cached. Keep bundle files stable
while the session is running.

Defaults are 64 MiB of VM heap, one second for load/init/shutdown execution, and
100 ms per update/draw. Each source file is limited to 256 KiB, with at most 256
compiled modules and 64 active imports including the entry module. Logging is
limited to 4 KiB per message and 64 KiB per callback, including line separators.
Host-detected budget failures fault the session even when caught in Luau.
Allocation failures caught by scripts remain recoverable under the VM heap cap;
uncaught allocation errors fault the session.

Cancellation requires Rust `panic=unwind` and uses mlua's protected panic handling
so `pcall`, `xpcall`, and metamethods cannot swallow the host's cancellation.
This does not preempt filesystem I/O, source/JIT compilation, or native functions;
the heap cap is not a bound on total process memory. Native work is checked
against the deadline when control returns to the VM/host. Process, network,
and native-plugin APIs remain unavailable.

## World and fixed input

Each entity has a finite f64 position in world pixels and velocity in pixels per
second. After a successful script update, the kernel integrates velocity over
`1/60` second. Invalid system results fault the session. There is no collision,
tile-map API, or generic component API yet.

| World API | Behavior |
| --- | --- |
| `ctx.world.spawn(x, y)` | Return an opaque entity handle; initial velocity is zero |
| `ctx.world.despawn(entity)` | Remove the entity and invalidate its handle |
| `ctx.world.position(entity)` / `velocity(entity)` | Return an owned `{x, y}` table |
| `ctx.world.set_position(entity, x, y)` / `set_velocity(entity, x, y)` | Apply an immediate change |
| `ctx.world.entities()` | Return an owned array of handles ordered by internal entity identifier |

World writes are allowed during init/update; draw/shutdown can only read.
Returned tables are copies. Handles may be retained across callbacks, but stale,
foreign-session, and stopped/faulted-session handles reject access. Handle equality
compares identity; it does not establish that the entity is still alive.
Enumeration reuses retained userdata, so handles also work as Luau table keys;
unused wrappers are eligible for garbage collection. Callback errors do not roll
back prior mutations, but a spawn that fails to publish its handle removes the
unpublished entity. Live entities are capped at 16,384, with
4,096 world operations per callback; exceeding either limit faults the session
even through `pcall`. Ordinary validation and permission errors remain catchable.

`ctx.input.held(name)`, `pressed(name)`, and `released(name)` read logical buttons
`up`, `down`, `left`, `right`, `action`, and `cancel`. The Rust caller supplies an
`InputSnapshot` with held buttons and optional press/release events. The runtime
also derives edges from held-state transitions. Explicit events preserve quick
taps; repeated events before a tick coalesce into booleans. Physical key mappings
will be added with Player integration.

Frames clamp incoming time to 250 ms and run at most five ticks. Excess whole
ticks are discarded, retaining fractional progress as draw alpha. `FrameReport`
reports tick count, discarded ticks, clamped seconds, and alpha; `overloads()`
counts affected frames. Invalid time leaves the clock/input/session untouched.
Tick boundaries tolerate `1e-12` of a tick of rounding error; other fractions,
including tiny inputs near zero, remain accumulated.
Pending press/release edges wait through zero-tick frames, reach only the first
catch-up tick, and then clear. Held state uses the latest sample. Draw sees the
current frame's edges independently; init/shutdown see neutral input.
`step(input)` consumes pending edges and runs exactly one tick without changing
the frame accumulator. `draw(alpha)` uses the latest sampled input.

The [movement sample](examples/games/movement/main.luau) sets velocity from input.
`tests/runtime.rs` replays right for half a second, then down for half a second:
30, 60, 100, 120, 144, 180, and 240 FPS all complete 60 ticks at `(46, 46)` from
`(16, 16)`. This verifies
fixed-input state on the tested target; it does not promise cross-platform float
or RNG determinism. Renderer integration and game captures are Phase 3.

## Script data and filesystem utilities

These APIs are available on each lifecycle callback's context. Calling a retained
function after its callback ends raises an error. Validation, conversion, and I/O
errors can be caught with `pcall`; resource-limit failures fault the session.

| Data API | Behavior |
| --- | --- |
| `ctx.data.parse(text)` | Parse UTF-8 tot into Luau values |
| `ctx.data.format(value)` | Write a tot document from those values |
| `ctx.data.export(value, format)` | Export `"json"`, `"yaml"`, or `"toml"` |
| `ctx.data.integer(decimal_text)` | Create an exact integer with immutable `.text`; `tostring` returns its digits |
| `ctx.data.number(value)` | Read a finite float or convert an integer within `±(2^53−1)` |
| `ctx.data.array(table)` | Validate and mark a dense one-based array, including an empty array |
| `ctx.data.null` | Distinct null value; unlike nil, it survives in tables |
| `ctx.data.kind(value)` | `object`, `array`, `integer`, `float`, `null`, `boolean`, or `string` |

Ordinary string-keyed Luau tables represent objects. Parsed arrays retain their
array identity and remain mutable. All ordinary Luau numbers encode as floats;
use `integer("42")` when integer identity matters. Integers keep their full text;
parsed floats use f64 precision. Object keys are sorted for repeatable output.
Comments, source key order, and float spelling are not retained. Mixed/sparse
tables, cycles, custom metatables, unsupported userdata, invalid UTF-8 data,
nil values, NaN, and infinities cannot be serialized.

JSON preserves integer digits, though another reader may round them. YAML export
supports signed/unsigned 64-bit integers. TOML requires an object root, signed
64-bit integers, and no null values anywhere. Unsupported values raise errors
with their data path; no values are silently dropped. The TOML dependency is
used for export only; game manifests still use tot.

`ScriptHost::load` grants bundle reads. To grant writes, the embedding application
calls `GameRuntime::load_with_data_root(bundle, data, limits)` (also available on
`ScriptHost`) with two existing,
absolute directories. They are canonicalized and must be disjoint. The application
selects the data location; the engine supplies no save filename or schema.

| Filesystem API | Behavior |
| --- | --- |
| `ctx.fs.read(root, path)` | Read a regular file as a binary-safe Luau string; root is `"bundle"` or `"data"` |
| `ctx.fs.list(root, path)` | Sorted `{name, kind}` entries; use `""` to list the root |
| `ctx.fs.mkdir(path)` | Create nested data directories; existing directories succeed |
| `ctx.fs.write(path, bytes)` | Create/replace a data file atomically; parent directory must exist |

Writes and directory creation are allowed in init, update, and orderly shutdown.
Draw can read and list. Paths are relative `/`-separated names, up to 4096 UTF-8
bytes. Absolute paths, dot/parent segments, backslashes, Windows device names,
reserved characters, and trailing dots/spaces are rejected. Symlinks/reparse
points below either root are refused. These roots must not be concurrently
replaced by another process; filesystem race isolation is not promised.

Writes sync a temporary file in the destination directory, then replace the
destination. A failed replacement preserves the old file and cleans the temporary
file. Replacement creates a new file; old file metadata is not retained. Directory
entry crash durability is not promised. Synchronous I/O can stall a tick, so games
choose when to perform it. A fault may skip shutdown; do not rely on it alone.

Limits are 1 MiB per file/data input/output and per converted tree's string bytes,
16,384 data nodes, nesting 64, and 1024 directory entries. Each callback allows
128 utility calls and 8 MiB of file transfers. These bounds supplement the VM
heap and callback deadline; native parsing, conversion, and I/O are not preemptible.

The example creates a chosen data directory, then lets Luau restore and persist
its own progress. Run this command twice to see the counter resume:

```text
cargo run --example script_host --no-default-features --features scripting -- examples/games/persistence target/example-data
```

The script writes `progress.tot` during update and rejects an unsupported schema
version without overwriting the existing data. There is no engine save lifecycle.

## Code and checks

- `src/lib.rs` exposes shared code; `src/bundle.rs` implements discovery.
- `src/bin/player.rs` owns the Macroquad window and startup presentation.
- `src/bin/player/capture.rs` handles capture configuration and PNG output.
- `src/scripting.rs` and `src/scripting/modules.rs` provide the optional Luau host.
- `src/kernel.rs`, `src/input.rs`, and `src/runtime.rs` own entities, logical input,
  and fixed-step execution; `src/scripting/world.rs` supplies scoped bindings.
- `src/scripting/data.rs`, `data/export.rs`, `filesystem.rs`, and `utilities.rs`
  implement scoped data/I/O services and their shared callback limits.
- The default `player` Cargo feature enables graphics. The shared library can
  be built and tested without graphics dependencies.

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-default-features --features scripting
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --release --no-default-features --features scripting --lib --test kernel --test runtime --test scripting --test scripting_feasibility --test scripting_utilities
```

See [AGENTS.md](AGENTS.md) for architectural requirements and contribution guidance.
