# Protogine

Protogine (Prototype Engine) is a tile-based game engine inspired by RPG Maker,
with a shared kernel for the standalone Player and planned editor.

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

The [sprite sample](examples/games/sprites/main.luau) draws a tiled room and an
animated character from PNGs the bundle carries, so its `assets` directory has
to travel with its source:

```powershell
cargo build --release --bin protogine-player
New-Item -ItemType Directory -Force target/sprites-demo/game/assets | Out-Null
Copy-Item target/release/protogine-player.exe target/sprites-demo/
Copy-Item examples/games/sprites/*.luau target/sprites-demo/game/
Copy-Item examples/games/sprites/assets/*.png target/sprites-demo/game/assets/
& ./target/sprites-demo/protogine-player.exe
```

Both images are requested from the first update, so the first frames show a
loading state with a progress bar per image while input already works. Arrows
move, Backspace returns to the spawn, and Space unloads both images so the next
tick requests them again and the loading state runs live. Replacing either PNG
in the copied bundle changes the artwork without rebuilding anything; see
[examples/README.md](examples/README.md) for their provenance and the shapes a
replacement has to keep.

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

A bundle is an unpacked `game` directory beside the executable, containing a
readable `main.luau` file:

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
and `shutdown` functions; unknown fields and metatables are rejected. Callbacks
return no values. The host exposes
`ctx.log(message)`, `ctx.data`, `ctx.fs`, `ctx.assets`, `ctx.world`, `ctx.input`,
and (during draw) `ctx.draw`, plus `ctx.native` with native support enabled. Stored context
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
VM/data work. Its `update()` runs one asset service pass and invokes one fixed
callback without kernel systems; it supplies log/data/filesystem/asset/draw
bindings, with no world, input, native calls, or clock.

Modules use extensionless relative paths, such as `require("./counter")` or
`require("../shared")` within the bundle. Files must be UTF-8 `.luau` source. A
single leading byte order mark is accepted and skipped after the source-byte
limit and UTF-8 checks. Other `U+FEFF` characters remain source text for the
compiler; filesystem reads preserve all bytes.
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
| `ctx.draw.sprite(image,x,y,options?)` | Append a cropped draw of a ready image |

Coordinates use a top-left origin with positive y downward. Colors are normalized
RGBA in `[0,1]`; coordinates are in `[-1_000_000,1_000_000]`, and sizes are in
`[0,1_000_000]`. All numbers must be finite and are converted to f32 after
validation. Zero-size rectangles and sprites are allowed. Invalid arguments are
catchable. Commands composite in insertion order with alpha blending.

`sprite` takes an image handle from `ctx.assets` and optional
`source = {x, y, width, height}`, `width`, `height`, `flip_x`, `flip_y`, and
`tint = {r, g, b, a}`. Omitted options use the whole image at its natural size,
no flips and an opaque white tint; with a source present, omitted destination
sizes default to the source's own width and height. Source coordinates are image
pixels, never tile indices: they are whole numbers, `width`/`height` are
positive, and the half-open extents must fit the image. Negative sizes are
errors, not flip shorthand; flips mirror the crop inside the same destination
footprint. Option tables must be plain tables without metatables or unknown
fields, values must have the stated type rather than being coerced, and they are
copied during the call, so mutating them afterwards changes no published command.
A sprite requires a live, CPU-ready image; pending, failed, unloaded and foreign
handles are catchable refusals, and drawing never advances loading. At most
10,000 sprite attempts per draw are allowed, counting refusals; drawing an image
does not consume the `ctx.assets` budget.

Each frame starts opaque black and each draw starts with an empty command list.
Only a successful draw publishes its commands; fault/stop clears the list. A
rejected `draw(alpha)` argument is recoverable and leaves the previously
published list intact rather than blanking the accepted frame.
Drawing nothing leaves a black frame. The 10,000-command cap counts clears,
rectangles and sprites together and faults the session even through `pcall`.
Drawing tables are read-only and
their functions expire with the callback. World writes remain prohibited in draw;
keep gameplay changes in update, including changes to script upvalues.

The Player draws clears, rectangles and sprites through one shared renderer in
`protogine::rendering`, available on its own with the `graphics` feature and
without a VM. The renderer owns a pool of at most 128 GPU allocation slots for
the lifetime of the graphics context, reused across sessions and never
recreated. Images upload in bounded passes of at most eight row bands and
256 KiB; an image becomes drawable only after every band has transferred, and a
sprite whose upload is still incomplete is skipped without holding up the rest
of the frame or forcing the upload. The draw traversal reads no file, decodes
nothing and allocates no texture.

Command validation runs over the whole list before anything is queued, so a
refused frame draws nothing. Missing, foreign and unloaded images are refused
rather than skipped, and a refusal is a presentation fault: the session stops
with exit code 3 through the same path a script fault uses, without running
Luau shutdown. An inspectable failed asset job is not a renderer fault and
stops nothing.

## Bundle images

`ctx.assets` exists in every callback and loads PNGs from the bundle root
through a bounded staged service. Admission resolves paths synchronously and
returns an opaque handle without waiting for content reads or decoding. Later
service passes read, decode and convert while gameplay continues.

| Asset API | Behavior |
| --- | --- |
| `ctx.assets.request_png(path)` | Init/update: admit a bundle-relative `.png` and return its handle |
| `ctx.assets.status(image)` | Any callback: an owned progress or failure snapshot |
| `ctx.assets.size(image)` | Any callback: `{width, height}` once validated |
| `ctx.assets.unload(image)` | Init/update: invalidate the image; true once, then false |

Paths are relative `/`-separated portable names ending in `.png`, resolved
against the canonical bundle root and never against the module, working or
executable directory. Traversal, absolute, device and link paths are refused, as
in `ctx.fs`. Repeat requests for one file return the same handle, so it works as
a table key and satisfies `rawequal`; a request after unload or failure is a new
identity instead.

`status` returns `state` (`queued`, `loading`, `ready`, `failed`, `unloaded`),
`stage` (`waiting`, `read`, `header`, `allocate`, `decode`, `complete`),
`bytes_read`, `gpu`, `width`/`height` once known, and `error` with `code`,
`path` and `message` when a job failed. CPU readiness and GPU residency are
separate: `gpu` is `unavailable` in a headless host, and otherwise `pending`
until the upload completes, `resident` once it has, or `released` for an image
that is not on the GPU and never will be. A game can draw a loading screen
until `gpu` reads `resident`. Malformed
paths, wrong phases and admission refusals are catchable errors that publish no
handle; failures after admission are inspectable failed jobs with `io`,
`format`, `unsupported`, `limit` or `capacity` codes and stop no gameplay.
Terminal status stays readable on a handle the script kept.
Asset diagnostics retain at most 4096 UTF-8 bytes of logical path and 1024 bytes
of message. A path longer than 4096 bytes is omitted from its refusal diagnostic;
catching the error does not retain a copy of that oversized input outside the VM.

The [Rust service contract](#bundle-image-loading) below defines supported PNGs
and storage/work bounds. Dimensions are fixed once validated. The 256-call
`ctx.assets` budget counts all attempts per callback, including malformed,
wrong-phase and refused calls. Read a size once and keep it rather than querying
inside a per-tile draw loop. All handle-taking operations reject foreign sessions;
`size` also refuses unknown dimensions and failed/unloaded images.

One CPU service pass runs per `frame` or `step`, whatever the catch-up tick
count, and none during `init`, `draw` or a refused call. Loading progress becomes
script-visible at the first update boundary of a frame, so a frame that runs no
tick cannot reveal a completed image and `status` advances at the fixed tick rate
rather than the presentation frame rate; a script's own request and unload are
visible immediately. A large image therefore
takes many frames; draw a loading state until its status is ready. Rust drivers
can call `advance_assets(wait)` for one pass or `drain_assets(timeout)` to settle
every admitted job under its own watchdog, without running callbacks or advancing
simulation. The [sprite sample](examples/games/sprites/main.luau) requests both
of its images during update and draws a progress bar for each until it is ready:

```text
cargo run --example script_host --no-default-features --features scripting -- --ticks 200 examples/games/sprites
cargo run --example script_host --no-default-features --features scripting -- --preload --ticks 4 examples/games/sprites
```

The first form stages loading across ticks and prints the work each tick
performed. `--preload` drains after init and after each step, which is the
deterministic readiness schedule capture mode uses. One job runs at a time in
request order, so the sample asks for its small character sheet first and draws
it while the much larger tileset is still decoding.

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

A failed `mkdir` attempts to remove directories that call created, deepest
first. It preserves pre-existing/nonempty directories and the primary error;
cleanup failures can leave a partial tree.

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

## Bundle image loading

The optional `assets` feature exposes `protogine::assets::AssetStore`, the Rust
service behind [ctx.assets](#bundle-images), without a VM or graphics context.
Both interfaces share path, admission, identity and failure rules. The
[PNG/sprite record](docs/implementation/PNG_SPRITE_PLAN.md) contains the design
and delivery evidence.

Accepted input is a static PNG with RGB, RGBA, grayscale, grayscale-alpha or
indexed color, including palette transparency and 1/2/4-bit grayscale, and
including interlaced images. APNG and 16-bit channels are refused explicitly
rather than silently reduced. Output is tightly packed top-to-bottom RGBA8 with
straight alpha, without ICC or gamma correction, premultiplication or flipping.

| Bound | Value |
| --- | --- |
| Registry | 128 pending or resident images per store |
| Job queue | 8 admitted jobs, one active worker and decoder, FIFO |
| Encoded file | 17 MiB, checked against actual reads plus a one-byte probe |
| Encoded staging | 34 MiB outstanding, including partial and failed reads |
| Dimensions | Both in `1..=2048` |
| Decoded image | 16 MiB per image, 64 MiB reserved, live and pinned per store |
| Work per pass | 32 KiB work units, eight per pass, one non-preemptible stage |
| Request spellings | 256 memoized, replaced in insertion order |

Case aliases that resolve to one canonical path reuse an identity on
case-insensitive filesystems; other paths are never lowercased to manufacture
aliases. Unload cancels pending work and invalidates every alias, but pixels a
Rust caller pins remain alive and accounted until released. Image IDs are
append-only; a later request after failure/unload receives a new identity.
Rust inspection refuses retired IDs, while Luau handles retain terminal status.
The byte ceilings are accounting rules; the 2 ms cutoff is a scheduling target,
not a latency guarantee for filesystem calls, decompression or allocation.

## Code and checks

The default `player` feature enables `graphics`, `scripting` and `native-plugins`.
Both `graphics` and `scripting` enable `assets`; neither enables the other.
`--no-default-features` retains core and owned command/identity types without a
VM, decoder or window. The workspace also contains the dependency-free plugin
SDK and the development-only header generator.

See [AGENTS.md](AGENTS.md) for source entry points and architectural rules,
[development and verification](docs/DEVELOPMENT.md) for the required feature/check
matrix and GPU/input probes, and [implementation records](docs/implementation/README.md)
for accepted decisions, completed phases and remaining scope.
