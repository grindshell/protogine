# Development and verification

Run commands from the repository root. Apply every section relevant to the
change; feature-specific checks supplement the Rust baseline. Record actual
results and any toolchain, platform, graphics or interaction gaps. Compilation
alone does not prove rendering, input or audio behavior.

Documentation-only changes require content, local-link and diff review, not a
new test suite. For implementation changes, add focused behavioral coverage of
the affected simulation or boundary contract. Test packaged games independently
of the source tree and editor. Keep new target/feature commands in this file.

## Rust baseline

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --bin protogine-player
```

## Assets

The decoder-only configuration exercises the store without a VM or window:

```text
cargo test --workspace --no-default-features --features assets
cargo clippy --workspace --all-targets --no-default-features --features assets -- -D warnings
```

Regenerate small PNG fixtures and independent expected RGBA bytes with
`python tools/asset_fixtures.py`; maximum-size fixtures are built in the tests
from stored DEFLATE blocks. Never record the engine decoder's output as its own
expected result. `python tools/sample_sprites.py` must rewrite the committed
sample PNGs identically and report their sizes.

## Scripting

Exercise both the real headless runtime and the combined Player configuration:

```text
cargo test --workspace --no-default-features --features scripting
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --release --no-default-features --features scripting --lib --test kernel --test runtime --test drawing --test scripting --test scripting_feasibility --test scripting_utilities --test assets --test script_assets --test sprites_sample
cargo run --example script_host --no-default-features --features scripting -- examples/games/lifecycle
cargo run --example script_host --no-default-features --features scripting -- --ticks 200 examples/games/sprites
cargo run --example script_host --no-default-features --features scripting -- --preload --ticks 4 examples/games/sprites
```

For both sprite drivers, read the `assets [...]` trace and confirm both sheets
complete. A successful three-tick lifecycle run proves no staged-loading exit.
When changing deadlines, retain exact checks at every VM interrupt, rerun the
protected-call/expensive-built-in probes and the distance benchmark below.

## Native plugins and SDK

```text
cargo check --no-default-features --features native-plugins
cargo test --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
cargo test --release --no-default-features --features scripting,native-plugins --lib --test plugins --test manifest
cargo test --release -p protogine-plugin-api
cargo run -p protogine-headergen -- --check
cargo test --test plugins -- --ignored
```

Native execution supports `x86_64-pc-windows-msvc`; tests require `clang` (or
`CLANG`) and the MSVC/Windows SDK. C fixtures compile against only a copied
generated header and platform headers, independently of the engine. The ignored
Player tests also require graphics. They cover native startup/faults and repeated
distance-field captures.

Edit [SDK definitions](../sdk/src/lib.rs), then regenerate with
`cargo run -p protogine-headergen`. The separate tool pins cbindgen 0.29.2;
normal engine builds do not generate the header. Keep the generated extern
prototype's Windows export macro and `.gitattributes` LF rule: drift checking
compares exact bytes, including under `core.autocrlf=true`.

Build and measure the standalone distance example:

```text
pwsh -NoProfile -File tools/build_native_distance.ps1
cargo run --release --example native_benchmark -- target/native-distance-demo/game
```

The [benchmark](../examples/games/native_distance/BENCHMARK.md) records complete
typed-wrapper costs, including packing, FFI copies and decoding. FFI-only timings
are not authoring performance. See [README](../README.md#native-plugins) for the
independently compiled lifecycle example and DLL dependency policy.

## Graphics and Player

Renderer changes require graphics-only tests, the production GPU harness, and
Player captures when a context is available:

```text
cargo check --workspace --all-targets --no-default-features --features graphics
cargo test --workspace --no-default-features --features graphics
cargo clippy --workspace --all-targets --no-default-features --features graphics -- -D warnings
cargo build --release --no-default-features --features graphics --example renderer_harness
powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode cycles
powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode bands
powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode eviction
powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode pressure
powershell -NoProfile -File tools/run_renderer_harness.ps1 -Mode recreate
cargo test --test player_capture -- --ignored
```

Use the [built-in Player capture](../README.md#screenshot-capture) and
[renderer harness](../examples/renderer_harness.rs), which drives the production
`MacroquadRenderer` and `AssetStore`. Do not create a second renderer for testing.
The harness is an executable because the graphics context needs the main thread.
Capture still needs graphics even though it exits unattended.

| Harness mode | Required evidence |
| --- | --- |
| `cycles` | 100 attach/load/retire cycles; bounded lifetime creations, stable identities, cleared retirement |
| `bands` | Bounded per-pass uploads; drawing advances no upload |
| `eviction` | Mid-upload cancellation and slot reuse; queued opaque 1x1 texel survives readback then clears; store teardown releases the staged upload's CPU pin |
| `pressure` | A full pool pinned by a queued frame yields with admission intact |
| `recreate` | Negative control fails at its named lifetime-creation assertion |

The runner supports Windows PowerShell 5.1 and PowerShell 7, enforces a
60-second watchdog per mode, and requires each positive mode's `PASS mode=`
marker. Zero exit alone does not prove the assertions ran.

For physical input or Player shutdown changes, build the release Player and run
the input probe. Sample, physical-input or eviction changes also require the
sprite probe:

```text
cargo build --release --bin protogine-player
pwsh -NoProfile -File tests/player_input.ps1
powershell -NoProfile -File tools/run_sprites_probe.ps1
```

Both require Windows and a working desktop/graphics context. The input probe
requires PowerShell 7; it copies the Player under `target/player-input/`, checks
every arrow/Space/Backspace mapping, held/edge order, Escape/close shutdown once,
and shutdown-fault exit 3. Logs remain beside the copy. `-Player <executable>`
selects another build; inherited capture/window overrides are excluded.

Capture uses neutral input and cannot prove key reactions. The sprite probe
posts real Windows key events and reads the sample's position/animation log:
movement stops at the first solid tile, both walk frames appear, release restores
idle, Left turns, and Space unloads/reloads both images. It adds one position-log
statement to the committed sample and fails if its insertion anchor no longer
matches. This is injected-event evidence, not a manual keyboard playtest.

## Coverage and watchdogs

Keep subprocess watchdogs independent of the subsystem under test:

| Suite | Coverage / constraint |
| --- | --- |
| `bundle` | Discovery and filesystem failure boundaries |
| `scripting`, `scripting_feasibility` | Lifecycle, module policy, scoped calls, JIT, faults, allocation and protected cancellation; runaway probes have a 10-second child watchdog |
| `scripting_utilities` | Value/export rules, rooted I/O, stale/retained values, limits, replacement failure, script-owned save/load |
| `kernel`, `runtime` | Handles, immediate writes, systems, latched limits, fixed-input replay and catch-up edges |
| `drawing` | Owned publication/validation, expired bindings, faults and seeded sample state without graphics |
| `assets` | Rooting, coalescing, staged grants, independent pixels, storage/admission bounds, eviction/cancellation at each stage and worker teardown; adversarial decoders have a 10-second child watchdog because non-preemptible decode plus join can outlast an in-process drain deadline |
| `script_assets` | Canonical wrappers, publication boundaries, budgets/phases, failed jobs, retained terminal status, sprite options/refusals/order, stop/fault/drop; foreign handles and failed wrapper publication use unit harnesses in `src/scripting/assets.rs` |
| `sprites_sample` | Committed bundle: independent image readiness while moving, room cells/border/order, unload/reload, equal state/animation at 30/60/144 FPS after preload; IDs map to paths through the two distinct logged image sizes |
| `rendering` | Validation/admission without a context: `new` allocates nothing, empty `attach` touches no texture, `validate` is the check used by `render` |
| `player_capture` | Copied Player from unrelated cwd; source/PNG replacement without rebuild, repeated seeded PNGs, movement/compositing, loading/loaded sample texels, recoverable decode failure and missing-asset fault |
| `manifest`, `plugins` | Schema/layout/dependencies, startup/rollback/reverse teardown, buffers/aliasing/zero lengths, failure publication, poison, deadlines/limits, expiry and distance parity; native child watchdog is 15 seconds |
| SDK `panic_boundary` | Actual C entry points, ordinary and recursively panicking payload disposal; 10-second child watchdog |

Inspect `cargo tree --no-default-features` and the `assets`, `scripting` and
`graphics` variants when changing feature boundaries. Core needs no decoder,
VM or graphics; assets needs no VM or graphics; scripting and graphics must not
enable each other.

## Phase 0 dependency probe

When changing the feasibility probe itself, run:

```text
cargo build --release --example png_sprite_probe --locked --offline
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode cpu
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu-recreate
pwsh -NoProfile -File tools/run_png_sprite_probe.ps1 -Mode gpu-early-retire
```

It probes dependency APIs and reuses the Player PNG writer; production renderer
evidence comes from the separate harness above. CPU fixtures use Python stdlib
and a 10-second child watchdog; GPU modes use 30 seconds. Negative controls must
fail at their named assertions. Generated files live under
`target/png-sprite-probe/`; durable receipts are linked from the
[Phase 0 record](implementation/PNG_SPRITE_PHASE0.md).
