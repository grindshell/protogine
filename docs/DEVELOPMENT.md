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
cargo test --release --no-default-features --features scripting --lib --test kernel --test tilemap --test collision --test runtime --test drawing --test scripting --test scripting_feasibility --test scripting_utilities --test assets --test script_assets --test script_tilemap --test sprites_sample
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
idle, Left turns, Backspace teleports home, and Space unloads/reloads both
images. It adds one position-log statement to the committed sample and fails if
its insertion anchor no longer matches. This is injected-event evidence, not a
manual keyboard playtest.

The log moved when the sample stopped owning its position. Movement now happens
in the fixed pass after update returns, so a log at the end of update would pair
this tick's animation state with last tick's coordinates and label the pair with
the wrong number. It sits at the top of update instead, where all five fields
describe the last completed tick, and the frozen format is
`probe <ticks> <x> <y> <frame> <facing>` with every field whole.
`Get-ProbeStates` now throws on a `probe` line it cannot read rather than
skipping it: whole pixels used to be guaranteed by the script that wrote them and
are now a property of the solver's clamp, so a rounding regression would print a
fraction, and the old filter would have returned an empty or stale sample instead
of reporting anything wrong. Every target the probe drives to is one the
character saturates against, so holding a key longer than intended cannot change
the answer. No crate is reachable by a single key hold from the spawn: the
headless fixture reaches one in three legs, and the two other routes that have
been walked take five and six. So the probe does not exercise the map edit, which
is proven headlessly instead of by timing key presses.

## Coverage and watchdogs

Keep subprocess watchdogs independent of the subsystem under test:

| Suite | Coverage / constraint |
| --- | --- |
| `bundle` | Discovery and filesystem failure boundaries |
| `scripting`, `scripting_feasibility` | Lifecycle, module policy, scoped calls, JIT, faults, allocation and protected cancellation; runaway probes have a 10-second child watchdog |
| `scripting_utilities` | Value/export rules, rooted I/O, stale/retained values, limits, replacement failure, script-owned save/load |
| `kernel`, `runtime` | Handles, immediate writes, systems, latched limits, fixed-input replay and catch-up edges |
| `tilemap` | Checked map schema and storage, row-major IDs, rectangular tiles and negative origins, saturated world-to-cell conversion, region and edit bounds, refused replacement, and map release on stop; runs in the core configuration with no decoder, VM or window |
| `collision` | Colliders and the swept solver: approach directions and flush contact, interior cells missed by corners, offsets, smallest and maximum boxes, multi-tile sweeps, nearest wall, boundary clamping, X-before-Y, teleport/attach/install/edit/clear guards, all-candidate atomicity and insertion-order independence. Face selection is checked against an independent 1/256-pixel integer oracle across the geometry domain, and clamp rounding against four batteries: both map boundaries, an interior face at the domain edge, and ordinary content, where about a third of random draws need the repair. Core configuration; the max-load stress is `#[ignore]`d and runs through its own harness |
| `script_tilemap` | The `ctx.world` map and collider calls through the real runtime: schema refusals and copying, owned snapshots, phase gating, handle expiry and slot reuse, T5/T6 guards reaching Lua unchanged, the shared attempt budget and the three aggregate ceilings, systems faults and zero-tick/catch-up frames. A tick of enormous velocity is swept here as well as in `collision`, because a game sets velocity and never a position, so the script path has its own chance to get round the solver |
| `drawing` | Owned publication/validation, expired bindings, faults and seeded sample state without graphics |
| `assets` | Rooting, coalescing, staged grants, independent pixels, storage/admission bounds, eviction/cancellation at each stage and worker teardown; adversarial decoders have a 10-second child watchdog because non-preemptible decode plus join can outlast an in-process drain deadline |
| `script_assets` | Canonical wrappers, publication boundaries, budgets/phases, failed jobs, retained terminal status, sprite options/refusals/order, stop/fault/drop; foreign handles and failed wrapper publication use unit harnesses in `src/scripting/assets.rs` |
| `sprites_sample` | Committed bundle: independent image readiness while moving, room cells/border/order, unload/reload, equal state/animation at 30/60/144 FPS after preload; IDs map to paths through the two distinct logged image sizes. Since the migration it also pins the engine-resolved movement: 120 pixels per second is asserted to be exactly two per tick, a pushed crate is one edit that changes both the drawn cell and what blocks, walls hold while the art is loading and after it is evicted, and repeated draws and zero-tick frames move nothing. Positions come from `kernel().snapshot()` as well as from the draw commands |
| `rendering` | Validation/admission without a context: `new` allocates nothing, empty `attach` touches no texture, `validate` is the check used by `render` |
| `player_capture` | Copied Player from unrelated cwd; source/PNG replacement without rebuild, repeated seeded PNGs, movement/compositing, loading/loaded sample texels, recoverable decode failure and missing-asset fault |
| `manifest`, `plugins` | Schema/layout/dependencies, startup/rollback/reverse teardown, buffers/aliasing/zero lengths, failure publication, poison, deadlines/limits, expiry and distance parity; native child watchdog is 15 seconds |
| SDK `panic_boundary` | Actual C entry points, ordinary and recursively panicking payload disposal; 10-second child watchdog |

Inspect `cargo tree --no-default-features` and the `assets`, `scripting` and
`graphics` variants when changing feature boundaries. Core needs no decoder,
VM or graphics; assets needs no VM or graphics; scripting and graphics must not
enable each other.

## Mutation controls and stress

These exercise shipped engine code rather than probing a contract ahead of it,
so they are rerun whenever the guards they cover change. Each control removes one
guard from the engine source, runs the single test meant to catch it, and
requires that test to fail at a named assertion; a passing suite alone cannot
tell a live guard from a dead one. A stale anchor is reported as a stale control
rather than as a passing guard, and an edit may name a second file, so a rule
that two files now enforce jointly can be removed from both.

Patches are written to an isolated copy of the tree under `target/`, never to the
working tree, through the shared `tools/control_tree.ps1`. Patching in place is
safe for the author but not for a concurrent reader: any build that overlaps a
run - another session's `cargo test`, an editor checking on save - would silently
compile a deliberately broken source and report a result that was never about the
code under review. Each run fingerprints the engine sources before and after and
fails if either moved, so it proves it wrote nothing rather than asserting it.
Runs may therefore overlap freely and may be run against uncommitted work.

A zero exit is never read as evidence on its own. A filter that selects no test
exits zero, and so does a stale binary cargo decided not to rebuild, so either
could report a live guard as dead or a dead guard as live. Every conclusion is
gated on four separate things: the patched source surviving the run, the crate
actually recompiling, exactly one test executing, and a detecting control's test
reporting failure. Every control's full cargo output is saved under
`target/<harness>/runs/`, passing ones included, because diagnosing a
disagreement usually means comparing against a control that behaved. The
read-back establishes that the patch was still in place when cargo exited, which
is slightly weaker than "cargo compiled it". The copy also stamps every file it writes, because
`Copy-Item` preserves source timestamps and the copy reuses its own `target/`.

Each harness carries one self-test per gate, which it must *refuse*: a source
clobbered after cargo exits, a source backdated so cargo skips the rebuild and
runs the previous binary, a control naming a test that does not exist, and an
edit that compiles and changes nothing the test observes. All four were accepted
as ordinary results at some point, and any of them would have reported every
guard as covered while proving nothing. Each is built on a genuine instance of
what its gate catches rather than a synthetic stand-in, and each fails naming the
gate if one is removed, so the gates are themselves controlled. They run last,
because the backdated one needs a previous build in the copy's target directory.

**One harness run at a time; a second refuses by name.** Isolation from the
working tree is proven by the fingerprint, and the concurrency hazard that used
to sit beside it is now identified and closed. It was never cargo. Each harness
derives its copy's path from its own name, so two concurrent runs of one harness
shared a single patched tree: each wrote its control's patch and each restored
the originals in its own loop, overwriting the other mid-control, while each
cleared the other's saved cargo output at startup. `Enter-ControlLock` in
[control_tree.ps1](../tools/control_tree.ps1) now refuses the second run and
names the process holding the lock, taking over only a lock whose process is
gone. Sharing a build cache between runs is fine; sharing patched sources never
was, and the copy alone only ever protected the working tree.

Both times this happened, every affected conclusion was refused rather than
reported - once during a Phase 3 receipt regeneration, once when the review
session ran the same harness against a run already in flight, which is what
identified it. That is the patch-survival gate doing what it was added for. The
five Phase 2 anomalies fit this mechanism and are not claimed as explained by it;
no second harness was known to be running for those.

The lock guards every conclusion all three harnesses produce, so it has its own
self-tests:

```text
pwsh -NoProfile -File tools/run_control_lock_selftest.ps1
```

Five cases, no cargo, about a second: a clean acquisition releases, a live holder
is refused by name, a lock whose process is gone is taken over, a recycled
process identifier with a different start time is not the same run, and a
malformed lock is taken over rather than crashing the run that finds it. Run it
whenever `control_tree.ps1` changes.

Every harness wraps its summary filters in `@(...)`, and that is not style. A
`Where-Object` pipeline that matches one item returns that item rather than a
one-element array, and PowerShell answers `.Count` with 1 only for an object
with no `Count` member of its own. A hashtable has one - its number of keys - so
a single matching control reports how many fields that control happens to carry.
The sample harness has a single recorded redundancy and first reported it as
five: a measured, plausible number about the wrong object.

**The hazard is the element type, not the match count**, which is why the fix is
to wrap rather than to reason about how many items a filter can select. The
three older harnesses never misreported, because none of their counts can reach
one *today* - a property of their current contents, not of their code - so they
are wrapped too. `tools/run_sprites_probe.ps1` counts `[pscustomobject]` rows,
which carry no `Count` of their own and so were never at risk; it is wrapped for
uniformity rather than because it was wrong.

Each case also asserts that an acquisition returns exactly one usable path, which
is not type pedantry. The first version of the lock wrote its takeover note to
the output stream, so a takeover returned the note *and* the path; the caller
kept both in one variable, the release matched nothing, and the first takeover
left a lock that every later run took over and never released. A self-test that
only checked the result was truthy passed while that was true.

Capture a note with `-InformationVariable`, never by redirecting the stream and
rebuilding the value by hand. The second reads the note but discards what the
function returned, so the assertion beside it starts checking a string the test
constructed and can no longer fail. That happened here, to the case that asserts
the malformed-lock wording, and it kept its name and its green tick for an hour.

**To verify a commit rather than a working tree, extract it first** - `git
archive HEAD` into a scratch directory and run there. The harness script is a
live file like any other, so two invocations of it taken while tooling is being
edited can run different control lists; the committed state cannot move.

**Extract to a root of 120 characters or fewer.** `cl.exe` is not long-path aware
whatever `LongPathsEnabled` reports, and the longest path it is asked to write is
a Luau object under
`<root>/target/<harness>/tree/target/debug/build/mlua-sys-<hash>/out/luau-build/<hash>-IrValueLocationTracking.o`.
The harness name is in that path, so the limit is per harness: the tail past the
extraction root measures 132 characters for `sprites-controls` and
`tilemap-controls`, 134 for `collision-controls` and **139 for
`script-tilemap-controls`**, which is the binding one. A 120-character root keeps
all four under `MAX_PATH` - and 120 rather than 121 because `MAX_PATH` is 260
*including the terminating null*, so 259 is the longest usable path and the
subtraction is `259 - 139`. Redoing it from 260 gives 121, which puts
`script-tilemap-controls` at exactly one character too many.

Which half of that is measured here matters, because the numbers sit in one
sentence and have different standing. The 132, 134 and 139 tails and the 261
failure below were measured against this tree; the 259 boundary is `MAX_PATH`'s
documented definition, and nobody here has watched 259 succeed where 260 fails.

Shortening the harness directory names would be optimising the wrong leg. The
deepest paths in a built copy are not `cl.exe`'s at all: cargo's incremental
fragments reach a tail of 158, which exceeds the C++ limit under any root that
works, and they are fine because rustc is long-path aware. Only the C++ leg is
fragile.

That margin is thinner than it looks and the near-miss is the useful part. A
Phase 4 extraction into a 133-character directory put one of those objects at 261
characters and `mlua-sys` failed with `fatal error C1083: Cannot open compiler
generated file`; every conclusion in the run was *refused* rather than reported,
which is the gates holding on a class nobody designed them for, but the output
reads like ten simultaneously broken controls. What made it look like an
unreachable case for so long is that the scratch directory this repository's
tooling defaults to is 117 characters, three inside the tightest limit, so
extracting straight into it succeeds and only naming a subdirectory pushes it
over.

Markers must never be a bare `assertion` or `panicked` substring: those match any
failure at all, which would reduce a harness to "something broke" and silently
absorb a control that moved to a different failure site. That rule is not
precautionary. Tightening generic markers in the map harness immediately caught a
wrong guess, and so did the first tightened marker in the collision harness,
which named the assertion text without the word the message actually printed.

### Tilemap map guards

```text
pwsh -NoProfile -File tools/run_tilemap_controls.ps1
pwsh -NoProfile -File tools/run_tilemap_controls.ps1 -Release
```

Eighteen controls over `src/tilemap.rs` and `src/kernel.rs`. Run both profiles:
several fail at different sites once `debug_assert` is compiled out, and each
marker names the exact assertion for its profile. Three are labelled crash
controls, where removing the guard panics inside the library before a test
assertion is reached; that is weaker evidence than a test catching the mistake,
so it is labelled rather than hidden. Two are expected to *pass*, recording that
mathematical floor and the exact-face correction are redundant by design, and
that `Kernel::set_tile`'s read of the previous ID now refuses out-of-bounds
coordinates before `TileMap::set_tile` is reached.

### Collision guards

```text
pwsh -NoProfile -File tools/run_collision_controls.ps1
pwsh -NoProfile -File tools/run_collision_controls.ps1 -Release
```

Twenty-eight controls over `src/collision.rs` and `src/kernel.rs`, covering the
four the tilemap plan names for this phase - endpoint-only movement, four-corner
overlap, Y-before-X and a partial integration commit - plus the numerical rules,
the work accounting and every T5/T6 placement guard. One is expected to *pass*
and says what makes it redundant: the body sort cannot change a result, because
bodies never affect one another and no refusal names an entity, so it buys
internal reproducibility rather than behaviour. A second differs by profile - the
`overlaps_cell` bounds guard is a crash control in debug, where removing it
overflows `index + 1`, and a recorded redundancy in release, where the wrap still
answers correctly - which is why both profiles are run.

Treat a recorded redundancy as a question about a missing test, not a settled
fact. `check_placement`'s extent check was recorded as redundant with saturation
plus "outside the map is solid" until the test its own comment described got
written; it turned out to be the only thing refusing a NaN edge, and writing that
case then exposed a second gap in the same predicate.

### World binding guards

```text
pwsh -NoProfile -File tools/run_script_tilemap_controls.ps1
pwsh -NoProfile -File tools/run_script_tilemap_controls.ps1 -Release
```

Thirty-six controls over `src/scripting/tilemap.rs`, `src/scripting/world.rs`,
`src/scripting/utilities.rs` and `src/kernel.rs`: thirty-two that must be
detected with their rule removed and four recorded redundancies, covering the
description and collider schemas, arguments refused rather than narrowed, phase
gating, the shared attempt budget, and each of the three aggregate ceilings both
enforced and latching outside `pcall`. Four self-tests bring the file to forty
entries. All of them behave identically in both profiles, which is unlike the
other two harnesses because nothing here depends on a `debug_assert`. The runner
prints the three counts separately rather than one total: a single number is what
let an earlier draft of this section say "thirty-six controls, three of them
redundancies" and be wrong twice in one sentence.

Eight of the thirty-two cover one rule each at four entry points, twice over:
every call that charges tile work must budget against what the callback has left
rather than the whole ceiling, and must charge what it performed before refusing.
Controlling the shared helper alone proved only that the helper matters -
reverting any single call site left the suite green - so each site has its own
assertion and its own control.

Those four redundancy entries cover three rules; the third carries two because it
is dead in two different places. The `is_finite` test inside `whole` is redundant
because `f64::fract` is NaN for infinity as well as for NaN, so the `fract` test
beside it already refuses every non-finite value, and `dense`'s early exit is
redundant with the index range check beside it because keys are unique.

The remaining two are one rule in a shared helper, which is why it needs three
targets rather than one. `plain`'s
unknown-name branch is dead for both schemas here - every map and collider field
is required, so an unknown name either makes the table too large, which the count
refuses, or displaces a required field, which the field read refuses - and
load-bearing for sprite options, whose six fields are optional, so
`{width = 8, bogus = 1}` is two keys under the count limit with nothing else to
refuse it. The same edit therefore runs three times: against `script_assets`,
where it must be detected, and against this phase's two schemas, where it must
not. Run against one suite it would have reported a confirmed redundancy either
way, which is the general point: a redundancy control on a shared helper has to
be wider than one on a private function.

Two deadline observations, one covered and one not.
`conversion-skips-the-deadline` witnesses the check inside `dense`, and it works
only because its test can observe a kernel that outlived a failed callback:
through `GameRuntime` a latched deadline stops the session either way, so "the
copy stopped before publishing" and "the copy finished and installed a map" look
identical from outside. The fixture therefore lives beside the bindings and
asserts its own premise - that the copy really did outlast the budget - so a
machine fast enough to finish inside it fails loudly instead of passing without
testing anything. `region_table`'s check has no control and cannot have one: it
runs after the kernel call, on a read that mutates nothing, so a deadline
expiring during output conversion leaves no state that differs from one expiring
after it. It is kept because the contract requires the observation, and recorded
as uncovered rather than left looking covered.

### Sample migration guards

```text
pwsh -NoProfile -File tools/run_sprites_controls.ps1
pwsh -NoProfile -File tools/run_sprites_controls.ps1 -Release
```

Nine controls over `examples/games/sprites/main.luau` and `src/runtime.rs`:
seven that must be detected and two recorded redundancies, plus four self-tests,
in both profiles. They cover the two the tilemap plan names for this phase - a
stale Luau grid must fail the tile-edit fixture, and collision in draw must fail
the repeated-draw/zero-tick fixture - plus the rest of what the migration
asserts: the character is stopped by the engine rather than by the script, the
placeholder room follows the same edit the tileset room does, and the game breaks
a cell only when it identified that cell and only after a push was actually
refused.

**Most of these rules live in Luau, and that changes one gate.** The sample is
data the test bundle loads at run time, so patching it makes cargo rebuild
nothing: the existing binary reads the copy's current file and behaves
differently, which is what the control wants. The rebuild gate therefore applies
only to controls that edit a `.rs` source, where a binary built from different
code is a reachable way to report a live guard as dead; a file read at run time
has no compiled copy to go stale.

What rules out the opposite mistake - a run where the tests never read the
patched bundle at all - is not the patch-survival read-back, which only
establishes that the file on disk was still patched when cargo exited. It is the
other six Luau controls in the same run: a bundle nobody read would make every
one of them fail to detect, loudly, so the run cannot come back quietly wrong in
the one place it would matter, which is a redundancy that is supposed to pass.

The two redundancies are the sample's bounds check before `ctx.world.tile` and
the pressed-axis half of its cell-ahead rule. The room's border is solid, so the
character can never stand in it and the cell one tile from its centre is always
inside the grid; the check stays because `tile` refuses an outside index rather
than answering it, and a sample should not hand an engine call an argument it has
not checked.

The second needs its arithmetic stated, because the obvious way to "close" it
builds a control that cannot fail. `ahead` takes the cell one tile from the box's
centre rather than one step past its leading edge, which guards two different
things and so has one control per half.
`ahead-perpendicular-uncentred` is detected: the character presses the crate
while straddling rows 9 and 10 with the crate in the lower one, so the top edge
alone names an empty cell. `ahead-pressed-axis-uncentred` passes, and cannot do
otherwise. The rule fires only when that axis did not move, meaning the box is
flush against a face; the clamp's ideal is `(face - size) - offset` moving
forward and `face - offset` moving back, and for this collider's zero offset,
32-pixel extent and 32-pixel tiles both are exact, so the committed coordinate
*is* the face - and a coordinate exactly on a face floors to the same cell under
both forms of the cell-ahead rule, in every direction. Pressing a crate from the
west or the north is reachable in this room and both walks were run; both pass
with the centring removed. Witnessing that half needs a non-zero collider offset
or a non-integer extent, not a different approach.

### Collision max-load stress

```text
pwsh -NoProfile -File tools/run_collision_stress.ps1
```

The configuration the tilemap plan requires as evidence: the 1,024 x 256 map,
1,024 maximum-footprint colliders and the full 16,384-entity population, in both
a long-sweep and a sparse short-motion arrangement. It builds release, applies an
independent 300-second child watchdog because a hang is not a slow pass, and
requires both arrangements to report a measurement line, since a zero exit does
not prove they ran. The long-sweep arrangement must complete inside the
fixed-pass ceiling rather than fault: a fault there would mean the ceiling is
wrong, not that the workload is unreasonable.

## Phase 0 feasibility probes

These probe contracts before the production code exists. They are not the
subsystems they precede, and they are rerun only when the probe itself changes.

### PNG and sprites

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

### Tilemaps and collision

```text
cargo build --release --example tilemap_probe --no-default-features
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode numeric
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode work
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-endpoint
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-corners
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-truncate
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-yfirst
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-naive-clamp
```

It builds without default features because the geometry it prototypes needs no
decoder, VM or window. `numeric` settles the adjacent-f64 clamp rule, cell
indexing, sweep fixtures and the sample's expectations; `work` recounts the
worst-case storage and tile work against the plan's ceilings and reports release
p50/p95/max for the mandated stress loads. Each `control-*` mode replaces one
frozen rule with the mistake it prevents and must fail at its named assertion,
not merely exit nonzero. Watchdogs are 30 seconds per mode and 60 for `work`.
Generated files live under `target/tilemap-probe/`; durable receipts are linked
from the [Phase 0 record](implementation/TILEMAP_COLLISION_PHASE0.md).
