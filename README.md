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
Luau syntax or complete bundle validity. Script execution, archive formats,
export tooling, simulation, audio, native plugins, and the editor are future
work. This directory convention can evolve with the export format.

## Code and checks

- `src/lib.rs` exposes shared code; `src/bundle.rs` implements discovery.
- `src/bin/player.rs` owns the Macroquad window and startup presentation.
- `src/bin/player/capture.rs` handles capture configuration and PNG output.
- The default `player` Cargo feature enables graphics. The shared library can
  be built and tested without graphics dependencies.

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
```

See [AGENTS.md](AGENTS.md) for architectural requirements and contribution guidance.
