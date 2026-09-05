# Protogine

Protogine (Prototype Engine) is a tile-based game engine inspired by RPG Maker,
with a shared kernel for its editor and standalone Player.

The current implementation is a minimal Player startup shell. It opens a
resizable window and displays **Missing game data** when no bundle is present.
Press Escape or close the window to quit. The startup screen needs no external
fonts, images, audio, or game files.

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
export tooling, simulation systems, audio, native plugins, and the editor are
future work. The headless scripting host below is available independently.

## Headless scripting

Enable the optional `scripting` feature to use `protogine::scripting::ScriptHost`.
The sample loads source modules and runs init, three fixed updates, draw callbacks,
and orderly shutdown without a window:

```text
cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
```

`ScriptHost::load` takes an absolute game-directory path and `ScriptLimits`.
`main.luau` returns a plain table containing optional `init`, `update`, `draw`,
and `shutdown` functions. Callbacks return no values. The host exposes
`ctx.log(message)`; stored context functions expire when their callback ends.
Logs from the last call can be retrieved with `take_logs()`.

Call `init()` once, then `update()` for each simulation tick and `draw(alpha)` for
each presentation frame. Updates receive `dt = 1/60`; alpha must be finite and
in `[0, 1)`. The host does not yet accumulate frame time, interpolate state, or
expose world/drawing operations. `shutdown()` is idempotent and skips game cleanup
if init did not complete or the session faulted. Dropping a host only releases
resources. Errors include a lifecycle phase and available Luau source context.

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
the heap cap is not a bound on total process memory. Module preparation is checked
against the deadline when control returns to the VM/host. There is no filesystem,
process, network, or native-plugin API exposed to scripts in this slice.

## Code and checks

- `src/lib.rs` exposes shared code; `src/bundle.rs` implements discovery.
- `src/bin/player.rs` owns the Macroquad window and startup presentation.
- `src/bin/player/capture.rs` handles capture configuration and PNG output.
- `src/scripting.rs` and `src/scripting/modules.rs` provide the optional Luau host.
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
```

See [AGENTS.md](AGENTS.md) for architectural requirements and contribution guidance.
