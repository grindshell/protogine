# Protogine

Protogine (Prototype Engine) is a tile-based game engine inspired by RPG Maker,
with a shared kernel for its editor and standalone Player.

The Player runs shipped Luau games through the shared runtime. It opens a
resizable window and displays **Missing game data** when no bundle is present.
Press Escape or close the window to quit. The startup screen needs no external
fonts, images, audio, or game files.
The shared library also provides a headless Luau runtime with an entity kernel,
fixed updates, input handling, and script data/filesystem utilities.
Trusted C plugins initialize and shut down alongside the runtime and expose
synchronous batch computations to Luau through owned byte buffers.

## Run and build

Use a Rust toolchain supporting edition 2024. From the repository root:

```text
cargo run --bin protogine-player
cargo build --release --bin protogine-player
```

`cargo run` also selects the Player by default. The release executable is
`target/release/protogine-player` (`.exe` on Windows). Copy it into a distribution
directory to run independently of this repository.

To run the moving tile sample in PowerShell:

```powershell
cargo build --release --bin protogine-player
New-Item -ItemType Directory -Force target/tiles-demo/game | Out-Null
Copy-Item target/release/protogine-player.exe target/tiles-demo/
Copy-Item examples/games/tiles/*.luau target/tiles-demo/game/
& ./target/tiles-demo/protogine-player.exe
```

The tile cruises horizontally. Arrows steer, Space pauses, and Backspace resets
its position. Edit the copied Luau files and relaunch to change the game without
rebuilding Rust. The sample uses code-drawn tiles and no external assets.

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
graphics context. It uses the actual Player renderer for games and startup screens.

Exit codes are `0` for normal exit or a saved capture, `1` for capture failure or
interruption, `2` for invalid environment configuration, and `3` for game loading
or runtime faults. A game fault shows **Game error** and retains code `3` even
when its diagnostic screenshot is saved or the PNG write also fails. Shutdown
runs before saving the final PNG; shutdown errors receive the same treatment.
Errors and script/native logs go to stderr. Unset the
capture variables to return to interactive mode. On Windows, `cargo run` waits
for the Player; scripts launching the release GUI executable directly should use
a process API that waits and collects its exit code.

Game capture seeds Luau's `math.random` with `0` before loading any module, uses
neutral input, and performs exactly one fixed tick followed by draw(alpha=0)
per render frame. Capture frame N observes N completed ticks after init.
Startup screens run no simulation. The sample's state and PNG bytes repeat on
the tested graphics stack. Native plugin state is not seeded by capture mode.
Script reseeding, changing external files, or changing
gameplay upvalues in draw can affect reproducibility; identical pixels across
platforms/drivers are not guaranteed. Interactive play uses the VM's default RNG
initialization. Headless tools can use `GameRuntime::load_seeded(root, limits, seed)`
(also on `ScriptHost`) with an i32 seed for the same initialization order.

The opt-in smoke test runs the compiled Player from an isolated distribution,
checks startup states, sample movement, source edits without rebuilding, seeded
repeatability, dimensions, drawing order/blending, and failure exit codes:

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

A detected entry point loads a new runtime and calls init once. Each interactive
frame samples input, advances fixed simulation ticks, then draws. Normal exit
runs shutdown once; faults stop callbacks and show **Game error**, with the phase
and available source traceback on stderr. Restart the Player to reload source.
An optional `game.tot` declares required native plugins, described below. Archive
formats, export tooling, audio, and the editor remain
future work.

## Native plugins

Native loading currently supports `x86_64-pc-windows-msvc`. The Player enables
`native-plugins` and treats its shipped libraries and dependencies as trusted
executable code. Plugins are declared explicitly; there is no DLL scanning or
hot reload. Omit `game.tot` for a script-only game, or use this versioned tot file:

```text
version 1
plugins [
    {id "org.protogine.lifecycle" library "plugins/lifecycle.dll"}
]
```

All entries are required and initialize in manifest order. A present manifest
requires integer `version 1`; unknown fields, unsupported versions, malformed
entries and duplicate IDs/library paths fail startup. Source is limited to 64 KiB
and 16 plugins. IDs have at least two dot-separated segments, each beginning with
a lowercase ASCII letter and continuing with lowercase letters, digits or `_`,
up to 128 bytes. Library paths are relative to the game directory, use `/`, end
in `.dll`, and are at most 1024 UTF-8 bytes. Traversal, Windows device names,
invalid path characters, symlinks and reparse points below the root are rejected.
Keep bundle files and dependencies stable while the runtime is using them.

The loader resolves all primary paths, then loads and validates every descriptor
before initializing any plugin. Dependency lookup uses the primary DLL's own
directory and System32; the working directory, executable directory and PATH
are excluded. Windows can still reuse an already loaded module by basename,
so use unique private dependency names.

The dependency-free [Rust SDK](sdk/src/lib.rs) defines ABI 1. Its generated
[C header](include/protogine_plugin.h) is sufficient for C authors; no engine or
Luau headers are needed. Export `protogine_plugin_query` with `PG_PLUGIN_EXPORT`.
It negotiates exact ABI/descriptor sizes and returns init/shutdown callbacks and
up to 64 immutable function declarations. IDs, schema IDs/versions, required
callbacks, table sizes and zero reserved fields are checked. The runtime copies
validated function pointers and keeps their libraries alive through invocation.

Host services currently provide bounded UTF-8 logging: info/warn/error, 4 KiB per
message and 64 KiB per native call. Diagnostics use a 1024-byte host-owned buffer.
The header documents pointer lifetimes, instance ownership and status values.
Calls are synchronous on the runtime thread; no host pointers may be retained
and workers must finish before return. Rust plugin shims can use `catch_status`
to translate unwind panics. Native code is not sandboxed or preempted: invalid
pointers, process crashes, foreign exceptions and hangs remain plugin faults
that an in-process ABI cannot contain.

Successful init transfers a non-null instance to the host; failed init cleans up
its own allocations and publishes null. On normal exit the runtime calls Luau
shutdown once, destroys the VM, shuts down native instances in reverse order,
then unloads libraries. Script faults skip further Luau callbacks but still clean
up native instances. Cleanup continues after native errors, preserving an earlier
fault. Dropping a runtime destroys the VM and cleans up native instances without
implicitly calling Luau shutdown. Plugin shutdown consumes its instance even on
an error return. Startup/shutdown errors use Player exit code 3.

For Rust embedders, safe `GameRuntime::load` variants reject native declarations.
The unsafe `GameRuntime::load_trusted(root, data_root, limits, seed)` explicitly
accepts native execution and ABI safety obligations. `data_root` and `seed` are
optional. The lower-level `ScriptHost` is a VM utility and does not load plugins.

Build the [lifecycle plugin](examples/plugins/lifecycle.c) using Clang and the
MSVC/Windows SDK, then run its copied game with the headless host:

```powershell
New-Item -ItemType Directory -Force target/native-demo/game/plugins | Out-Null
Copy-Item examples/games/native_lifecycle/* target/native-demo/game/
clang --target=x86_64-pc-windows-msvc -std=c11 -shared -Werror -I include examples/plugins/lifecycle.c -o target/native-demo/game/plugins/lifecycle.dll
cargo run --example script_host --no-default-features --features scripting,native-plugins -- target/native-demo/game
```

To run the same game visually, copy `target/release/protogine-player.exe` into
`target/native-demo/` and launch it there. The example logs native init before
Luau init, then Luau shutdown before native shutdown.

### Luau batch calls

With native support enabled, runtime contexts expose:

```luau
local written = ctx.native.call(pluginId, functionId, inputBuffer, outputBuffer)
local info = ctx.native.plugins[pluginId][functionId]
-- info.schema and info.schema_version identify the byte format.
```

Calls are allowed only in init/update and expire with that callback. Metadata is
an owned read-only snapshot; declarations are frozen when the registry loads, so
the host builds it once and every callback shares the same table (`rawequal`
holds across calls, and a retained reference stays valid). Buffers can be
retained. A runtime with no plugins has an empty registry. Standalone ScriptHost
supplies no native API.

The adapter copies input to host memory and uses disjoint zeroed output scratch.
On validated success, it copies only the written prefix back, preserving the
output suffix. Failure leaves the entire output unchanged. Input/output may be
the same Luau buffer; no VM pointers cross the C ABI. Zero-length native spans
use null pointers. No automatic retry occurs, including for insufficient capacity.

Unknown IDs, bad arguments and native statuses INVALID_ARGUMENT, UNSUPPORTED,
ERROR and BUFFER_TOO_SMALL raise recoverable script errors. PANIC, CONTRACT_ERROR,
unknown statuses, malformed diagnostics/output and host-service violations poison
the registry and fault the session even through `pcall`. Native failures include
plugin/function context; cleanup preserves the primary fault.

Limits are 16 MiB combined input/output capacity per call, 64 MiB requested bytes
and 128 call attempts per callback. Every attempt counts, including malformed
arguments and calls refused because draw or shutdown cannot use the plugin. Native and script logs share the callback's
64 KiB total in execution order. Limits and elapsed deadlines latch outside Luau;
native calls that never return cannot be interrupted in process.

The [distance-field example](examples/games/native_distance/SCHEMA.md) computes
four-neighbor tile distances in one call. Its typed wrapper accepts tile arrays
and returns distance arrays; the pure-Luau implementation supplies parity.

```powershell
pwsh -NoProfile -File tools/build_native_distance.ps1
cargo run --example script_host --no-default-features --features scripting,native-plugins -- target/native-distance-demo/game
./target/native-distance-demo/protogine-player.exe
cargo run --release --example native_benchmark -- target/native-distance-demo/game
```

The build script uses Clang `-O2`, the C header and platform SDK, then copies the
game and release Player. [Benchmark results and method](examples/games/native_distance/BENCHMARK.md)
include validation, packing, FFI copies, decoding, GC and runtime callback costs.

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
`ctx.log(message)`, `ctx.data`, `ctx.fs`, `ctx.world`, `ctx.input`, and (during draw)
`ctx.draw`, plus `ctx.native` with native support enabled. Stored context
functions expire when their callback ends, while
returned data values can be retained.
Logs from the last call/frame can be retrieved in callback order with `take_logs()`.

Call `init()` once, then `frame(elapsed_seconds, input)` for each presentation
frame. It runs fixed updates and calls draw once. For exact stepping, use
`step(input)` and `draw(alpha)` separately. Updates receive `dt = 1/60`; alpha
must be finite and in `[0, 1)`. The runtime does not interpolate state automatically.
`draw_commands()` exposes its owned presentation list. `shutdown()` is idempotent
and skips game cleanup if init did not complete or the session faulted. Dropping
a host only releases resources. Errors include a lifecycle phase and available
Luau source context.

The lower-level `protogine::scripting::ScriptHost` remains available for standalone
VM/data work. Its `update()` invokes one fixed callback without kernel systems;
it supplies log/data/filesystem/draw bindings, with no world, input, native calls,
or clock.

Modules use extensionless relative paths, such as `require("./counter")` or
`require("../shared")` within the bundle. Files must be UTF-8 `.luau` source.
Aliases, dotted path segments, directory init modules, and paths escaping the
canonical bundle root are rejected. Successfully loaded module values are cached
once per VM; loaded source changes require a new host. Cycles and failed imports
return script errors; failed results are not cached. Keep bundle files stable
while the session is running.

The standard-library allowlist is Luau's sandboxed base functions plus `table`,
`string`, `utf8`, `math`, `bit32`, `buffer`, `vector`, and `debug`. Luau's `debug`
contains only `info` and `traceback`, retained for game-authored diagnostics; host
error tracebacks are captured independently. Registry access, local/upvalue
mutation and debug hooks are unavailable. The host removes `loadstring`, `getfenv`,
`setfenv`, `collectgarbage`, `newproxy`, and `print`; games log through `ctx.log`.

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
against the deadline when control returns to the VM/host. Process execution,
network access and script-selected DLL loading remain unavailable.

The deadline is checked at every Luau VM interrupt and at host checks in
bindings, native returns and callback completion. Interrupts cannot be sampled
at a fixed stride: built-ins such as `table.sort` and `buffer.fill` can do
substantial work between them. An individual non-preemptible operation may
overrun; cancellation occurs at the next interrupt or host check, without
allowing a batch of further expensive calls. Budget failures remain latched.

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
4,096 world call attempts per callback, including malformed or missing arguments;
exceeding either limit faults the session even through `pcall`. Ordinary validation
and permission errors remain catchable.

`ctx.input.held(name)`, `pressed(name)`, and `released(name)` read logical buttons
`up`, `down`, `left`, `right`, `action`, and `cancel`. The Rust caller supplies an
`InputSnapshot` with held buttons and optional press/release events. The runtime
also derives edges from held-state transitions. Explicit events preserve quick
taps; repeated events before a tick coalesce into booleans. The Player maps arrows
to directions, Space to `action`, and Backspace to `cancel`. Escape exits the Player.

Frames clamp incoming time to 250 ms and run at most five ticks. Excess whole
ticks are discarded, retaining fractional progress as draw alpha. `FrameReport`
reports tick count, discarded ticks, wall time discarded by the clamp
(`clamped_seconds`), and alpha; `overloads()` counts affected frames. A 1.0-second
frame reports 0.75 discarded seconds before any catch-up ticks are dropped.
Invalid time leaves the clock/input/session untouched.
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
or RNG determinism.

## Drawing

Only the draw callback exposes `ctx.draw`. It appends owned commands; it never
receives a graphics handle or borrowed world storage.

| Draw API | Behavior |
| --- | --- |
| `ctx.draw.clear(r,g,b,a)` | Replace the background and discard preceding drawing |
| `ctx.draw.rect(x,y,w,h,r,g,b,a)` | Draw a filled rectangle in pixel coordinates |

Coordinates use a top-left origin with positive y downward. Colors are normalized
RGBA in `[0,1]`; coordinates are in `[-1_000_000,1_000_000]`, and sizes are in
`[0,1_000_000]`. All numbers must be finite and are converted to f32 after
validation. Zero-size rectangles are allowed. Invalid arguments are catchable.
Rectangles composite in insertion order with alpha blending.

Each frame starts opaque black and each draw starts with an empty command list.
Only a successful draw publishes its commands; fault/stop clears the list. A
rejected `draw(alpha)` argument is recoverable and leaves the previously
published list intact rather than blanking the accepted frame.
Drawing nothing leaves a black frame. The 10,000-command cap counts clears too
and faults the session even through `pcall`. Drawing tables are read-only and
their functions expire with the callback. World writes remain prohibited in draw;
keep gameplay changes in update, including changes to script upvalues.

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
| `ctx.data.number(value)` | Read a finite Luau number or convert integer userdata within `±(2^53−1)` |
| `ctx.data.array(table)` | Mark a dense one-based array, including an empty array |
| `ctx.data.null` | Distinct null value; unlike nil, it survives in tables |
| `ctx.data.kind(value)` | `object`, `array`, `integer`, `float`, `null`, `boolean`, or `string` |

Ordinary string-keyed Luau tables represent objects. Parsed arrays retain their
array identity and remain mutable. All ordinary Luau numbers encode as floats;
use `integer("42")` when integer identity matters. Integers keep their full text;
parsed floats use f64 precision. Object keys are sorted for repeatable output.
Comments, source key order, and float spelling are not retained. Mixed/sparse
tables, cycles, custom metatables, unsupported userdata, invalid UTF-8 data,
nil values, NaN, and infinities cannot be serialized.

`array` checks the keys of the table it marks, not the elements: the script owns
that table afterwards and may replace any element, so element values are checked
where they are converted, by `format` and `export`. Marking a table whose
contents are not data therefore succeeds and fails at conversion.

JSON preserves integer digits, though another reader may round them. YAML export
supports signed/unsigned 64-bit integers. TOML requires an object root, signed
64-bit integers, and no null values anywhere. Unsupported values raise errors
with their data path; no values are silently dropped. Exports use the tot 0.2.0
library, with TOML's null policy explicitly set to error. Conversion diagnostics
use tot paths with zero-based array indices (Luau arrays remain one-based).
The `scripting` feature enables tot's YAML/TOML converters; `ctx.data.parse`
continues to read tot, and game manifests still use tot.

`ScriptHost::load` grants bundle reads. To grant writes, the embedding application
calls `GameRuntime::load_with_data_root(bundle, data, limits)` (also available on
`ScriptHost`) with two existing,
absolute directories. They are canonicalized and must be disjoint. The application
selects the data location; the engine supplies no save filename or schema.
The current Player grants bundle reads and no writable data root. Use the explicit
host API for persistence examples; Player data-directory selection is separate work.

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

Listing reports `kind` as `"file"`, `"directory"`, or `"unsupported"`. A name is
reported as usable only when it can be passed back to read, list, or write, so
links, other node types, and names outside the path policy above are listed as
`"unsupported"` rather than failing the whole call — one entry a game cannot
name never hides its siblings. An unsupported name may be lossy; do not use it
as a path. Refusing to traverse those entries is unchanged.

A failed `mkdir` removes the directories that call created, deepest first, so a
partial tree is not left behind. It never removes a directory that already
existed or one that is no longer empty.

Writes sync a temporary file in the destination directory, then replace the
destination. A failed replacement preserves the old file and cleans the temporary
file. Replacement creates a new file; old file metadata is not retained. Directory
entry crash durability is not promised. Synchronous I/O can stall a tick, so games
choose when to perform it. A fault may skip shutdown; do not rely on it alone.

Limits are 1 MiB per file/data input/output and per converted tree's string bytes,
16,384 data nodes, nesting 64, and 1024 directory entries. Each callback allows
128 utility call attempts (including malformed or missing arguments) and 8 MiB of
file transfers. These bounds supplement the VM heap and callback deadline; native
parsing, conversion, and I/O are not preemptible.

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
- `src/manifest.rs` validates tot declarations; `src/plugins.rs` owns native
  loading, foreign calls, descriptor validation and teardown.
- The workspace contains the engine, dependency-free `sdk`, and the separate
  `tools/headergen` development utility pinned to cbindgen 0.29.2. Regenerate with
  `cargo run -p protogine-headergen`; normal Player builds do not run codegen.
  `.gitattributes` keeps the generated header in LF form for exact-byte checks.
- The default `player` Cargo feature enables graphics, scripting and native
  plugins. The shared library can be built without graphics or native loading.

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-default-features --features scripting
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --release --no-default-features --features scripting --lib --test kernel --test runtime --test drawing --test scripting --test scripting_feasibility --test scripting_utilities
cargo check --no-default-features --features native-plugins
cargo test --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
cargo test --release --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
cargo test --release -p protogine-plugin-api
cargo run -p protogine-headergen -- --check
```

On the supported Windows native target, the `plugins` suite requires `clang`
(or a compiler path in `CLANG`) and the MSVC/Windows SDK. It compiles independent
C DLLs against a copied header, asserts C/Rust layout agreement, and runs native
loads/callbacks in child processes with an independent 15-second watchdog.
It covers refusal cases, dependency decoys, initialization order, script faults,
partial rollback and reverse teardown. Run `cargo test --test plugins -- --ignored`
with a graphics context to verify copied Player startup/fault captures with DLLs.

On Windows with PowerShell 7 and a working desktop/graphics context, run the
repeatable [input/shutdown probe](tests/player_input.ps1):

```powershell
cargo build --release --bin protogine-player
pwsh -NoProfile -File tests/player_input.ps1
```

It copies the Player into an isolated distribution under `target/player-input/`,
injects native key events, and checks each arrow/Space/Backspace mapping, held
state, press/release order, and shutdown exactly once on Escape/window-close.
It also verifies exit code 3 for a shutdown fault. Logs remain beside the copied
Player. Pass `-Player <executable>` to test another build. Capture/window
environment overrides are excluded from the child process.

See [AGENTS.md](AGENTS.md) for architectural requirements and contribution guidance,
and the [implementation archive](docs/implementation/README.md) for completed plans.
