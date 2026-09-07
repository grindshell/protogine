# Tilemap/collision Phase 0: contracts and feasibility

**Status:** Phase 0 completed 2026-09-07 against baseline `508266c`. This freezes
the M1 contracts of the
[tilemap/collision plan](../../TILEMAP_COLLISION_PLAN.md) and records the
feasibility measurements behind them. Phases 1-4 remain unstarted; M2-M4 remain
required core scope with their own contract gates. Measurements below are
observations from this probe on one machine, not benchmark percentiles or
real-time guarantees.

Accepted decisions T1-T8 are applied here without amendment. Where the plan left
a proposal open, this record states the chosen rule and the evidence for it. Two
accepted decisions are restated because everything below depends on them: the
kernel owns a single `Option<TileMap>` in dense row-major storage outside hecs
(T1), and the per-entity collider is a `hecs` component (T3), which no later
membership rule may change.

## Selected numerical rules

These are the rules Phases 1 and 2 implement. All of them are enforced at the
Rust entry points, not only behind a Lua wrapper.

**N1. Faces are exact; no epsilon exists.** Map origins and tile sizes are
integers and every map edge stays within +/-2^24 world pixels, so every tile face
`origin + index * tile` is an exactly representable f64. Every comparison between
a face and a box edge is therefore exact, and no tolerance is introduced anywhere
in the solver. A fixed epsilon would either open gaps or admit penetration; the
plan forbids both and nothing here needs one.

**N2. World-to-cell conversion is mathematical floor, corrected against exact
integer faces.** Compute `((world - origin) / tile).floor()` as a starting guess,
then correct until `face(index) <= world < face(index + 1)` using the exact
integer faces of N1. Callers clip world coordinates into the map extent before
converting, which keeps the correction one cell wide.

Two parts of that rule carry different weight, and the probe separates them.

The `floor` is load-bearing. Truncation toward zero folds the first cell left of
a negative origin onto the first cell inside it: with `origin -100` and `tile 32`,
world `-101` reads as column 0 instead of -1.

The correction, within today's frozen limits, is insurance rather than a fix, and
the margin is worth stating numerically because a reader has to be able to check
it.

`world - origin` is exact: both are multiples of 2^-28, the ulp at 2^24, and the
difference is at most one map extent of 2^20, needing 48 significand bits against
the 53 available. The quotient's magnitude is then bounded by the cell count of
1,024, **not** by the extent, so its ulp is at most 2^-42 and its rounding error at
most 2^-43. The tightest real input is `next_down(face)`, which sits at least
`2^-29 / tile` and so at least 2^-39 below the face. The margin between the two is
a factor of 2^4 to 2^5, and it is the index bound rather than the coordinate
magnitude that supplies it.

A scan of every legal tile size at both geometry limits, taking each face and its
two representable neighbours, found 0 disagreements between the raw floor and the
exact answer over 26,624 cases, and the randomized oracle battery and
maximum-footprint fixtures needed 0 corrections either. The correction is kept
because it makes the conversion depend on exact integer faces instead of on that
margin surviving every future limit change; a wider geometry domain, a larger map
extent or a larger index space erodes it, and the correction costs one comparison.
Phase 1 keeps it and must re-run the scan if any of those limits move.

Phase 1 obligation: the probe's correction loops are unbounded and terminate only
because callers clip into the map first. Production must saturate the loop at one
cell in each direction and `debug_assert` that it never needed more, rather than
relying on caller discipline - the plan separately forbids scanning toward a
requested index, and an unclipped caller would otherwise walk.

**N3. Overlap is half-open on both axes.** `a_min < b_max && a_max > b_min`. Edge
contact is free, positive-area overlap blocks, outside the installed map is
solid, and a box whose far edge lands exactly on a face does not overlap the cell
beyond it. Degenerate and unrepresentable boxes are rejected by the schema below.

A recorded consequence: penetration must be expressed on the reconstructed *edge*,
not on the position. At a face of 64 with a 32-pixel box, one representable step
of the position is half an ulp of the sum and rounds straight back onto the face;
only a step taken on the edge itself changes the comparison. Phase 2 fixtures must
perturb the edge, and the frozen clamp rule below verifies the edge for the same
reason.

**Edge reconstruction is canonical.** Every predicate that compares a body
against the grid - the sweep, the placement check, `set_position`, cell-edit
validation and install revalidation - must reconstruct the box with the identical
expression `(position + offset) + size`, through one shared helper rather than a
locally reassociated equivalent.

This is a correctness requirement rather than a style preference, and the reason
is structural, not statistical. Equality with the blocking face is permitted by
N3 and routinely attained, so the reconstructed edge has **zero** margin by
design: a reassociation that rounds even one ulp high converts a legal flush
contact into an overlap. The aligned case is the common one, not a corner. Three
fixtures in the probe land flush and then assert the box is free - `edge contact
is free: a flush box must not overlap` at face 64, `maximum footprint: the
clamped box must be free` reconstructing to exactly 16777216.0 at the geometry
limit, and `sample fence stop` where the shipped sample's 640 + 32 is exactly the
fence face 672. Each of those breaks under a one-ulp-high reconstruction.

The residual gap N5 reports is a rounding statistic and says nothing about this
margin: it is a maximum, while what matters here is the minimum, which is zero.
No divergence between associations was observed over 39,939 domain-edge cases, so
this remains hardening rather than a demonstrated bug - but the justification does
not depend on any measured number and survives those numbers changing.

**N4. Sweeps enumerate faces, never pixels or endpoints.** For positive travel,
start at the first face at or after the leading edge and iterate while
`face < destination_edge`. The destination edge is
`(start + travel + offset) + size`, the edge the committed position actually
reconstructs, not `leading_edge + travel`; using the latter lets a rounding
difference skip the last face. A face already touching the leading edge is
included when moving into it, which yields zero displacement rather than a push.
Reaching the map's own boundary index clamps there because outside is solid, so a
huge finite velocity costs work proportional to the map, not to the velocity.
The whole perpendicular span is enumerated on every candidate face. Negative
travel is the mirror, entering the column or row below each face.

**N5. Clamped position: aim, bound, repair, and fall back.** This is the
outward-rounding rule the plan required Phase 0 to settle.

```text
ideal    = (face - size) - offset          # mirror direction: face - offset
position = clamp(ideal, start, target)
repeat at most 4 times while (position + offset) + size > face:
    overshoot = ((position + offset) + size) - face
    position  = min(position - overshoot, next_down(position))
    position  = max(position, start)
if the box still crosses the face: position = start
```

The candidate is verified by reconstructing the box edge exactly as the committed
position will, then repaired by the measured overshoot. Correcting by the
overshoot rather than by ulps of the position matters: when a large offset cancels
against a large face the needed correction is many billions of position ulps wide,
while a correction in the box's own magnitude converges immediately. The
`next_down` branch exists only because a correction smaller than the position's own
resolution cannot move it; it guarantees progress.

The fallback to `start` is safe because `start` always satisfies the
postcondition, and the argument is stronger than "the invariant says so". The
blocking face is chosen as the first face at or after the leading edge, so
`(start + offset) + size = lead <= face` holds by construction: the loop condition
is already false at `start`, and the fallback can therefore only return a position
that passes the same check the loop is testing. That holds identically on both
axes, but the second axis needs one extra step. The X sweep stops before the first
solid face, having checked every crossed column against the full row span of the
*original* Y, and moving along X only vacates cells on the trailing side without
entering any new row. The body at (resolved X, original Y) therefore occupies a
subset of the cells it already occupied plus cells the sweep just checked and
found clear, so the Y sweep starts from a free box exactly as the X sweep did.

Falling back is conservative, deterministic, and was not reached by any observed
case. Phase 2 obligation: because that fallback is only correct given a free
start, an exhausted repair must be treated as a loud invariant violation and fault
the systems pass, not silently return `start` and increment a counter as the probe
does. A future path that reached it with an embedded body would otherwise publish
an overlapping position.

Measured over 889,145 adjacent-f64 cases spanning the whole geometry domain:
35,190 cases (4.0%) needed repair, the most any case needed was 2 steps, no case
fell back, and the largest residual gap was 3.725290298461914e-9 pixels (2^-28),
which is 2^-20 of the smallest collider extent the schema admits. Without the
repair the same rule penetrates: at the +2^24 boundary with offset -4096 and
extent 4095.6666666666665 the naive subtraction reconstructs an edge of
16777216.000000004.

**N6. Free-flight integration stays exact at the sample's speed.**
`FIXED_DT` is `0x3f91111111111111` and `120.0 * FIXED_DT` is
`0x4000000000000000`, exactly 2.0. Accumulating that step 30 times from 448.0 and
256.0 gives exactly (508.0, 316.0), and 60 steps give exactly 120.0 of travel, so
the sample's exact-equality assertions and its 30/60/144 FPS replay comparison
survive migration. The exactness belongs to 120, not to the integration: 130 px/s
drifts -2.842170943040401e-14 over the same 30 ticks. This was verified, not
assumed.

**N7. Axis order is a contract.** X resolves fully, then Y resolves from the
resolved X. A single solid diagonal neighbour resolves to (32, 0) under X-first
and (0, 32) under Y-first, so the order is observable and pinned by fixture, not
left to floating-point ties or ECS iteration order.

## Frozen M1 schema and API

All calls live beneath `ctx.world`, share its existing 4,096 attempts per
callback, and are counted before conversion. Only init and update may mutate;
draw and shutdown may read. Standalone `ScriptHost` still omits `ctx.world`.

| Call | Contract |
| --- | --- |
| `set_tilemap(desc)` | Validate and copy a complete description, then atomically install or replace the map. Every existing collider is revalidated against the candidate before the swap. No return value. |
| `clear_tilemap()` | Remove the map only when no colliders exist; succeeds when already absent. |
| `tilemap_info()` | Owned `{columns, rows, tile_width, tile_height, origin_x, origin_y}`, or nil with no map. |
| `tile(column, row)` | Numeric tile ID; requires a map and in-bounds exact integer indices. |
| `tile_solid(column, row)` | Boolean; requires a map, and returns true outside its bounds. The only call accepting indices outside the grid. |
| `tiles_region(column, row, columns, rows)` | Owned flat row-major array of IDs from a wholly in-bounds positive rectangle, at most 4,096 cells. |
| `set_tile(column, row, id)` | Immediate single-cell change, refused when it would overlap a collider. |
| `set_tile_collider(entity, options)` | Attach or replace the collider; nil removes it. Attaching requires an installed map. |
| `tile_collider(entity)` | Owned `{offset_x, offset_y, width, height}` or nil; validates the entity even with no collider attached. |

`desc` carries exactly the six info fields plus `solids` and `cells`; all eight
are required and nothing else is accepted.

- `columns`, `rows`: exact integers in `1..=1024`, product at most 262,144,
  checked before any allocation.
- `tile_width`, `tile_height`: exact integers in `1..=1024`. Square tiles are not
  required.
- `origin_x`, `origin_y`: exact integers; `origin` and `origin + count * tile`
  must both lie within +/-2^24 on their axis.
- `solids`: a dense plain array of `1..=1024` booleans, element `i` defining tile
  ID `i`. ID 0 is implicitly non-solid, so a map defines at most 1,025 distinct
  IDs. u16 is the cell storage width, never the usable ID space.
- `cells`: a dense plain array of exactly `columns * rows` integer IDs in
  `0..=#solids`, row-major, read as `cells[row * columns + column + 1]` from
  one-based Lua while tile coordinates stay zero-based.

Collider options carry exactly `offset_x`, `offset_y`, `width`, `height`.

- Offsets are finite with magnitude at most 4,096 pixels.
- Each extent is at least 1/256 pixel and at most
  `min(8 * tile on that axis, 4,096)` pixels, so a box spans at most nine cells
  per axis. The tile-relative half of that bound is what caps the perpendicular
  span a sweep enumerates and cannot be replaced by the pixel cap alone.
- The resulting box must lie inside the installed map with no positive-area solid
  overlap, which also keeps its edges inside +/-2^24.

No defaults, numeric strings, truthy substitutes, metatables, unexpected fields,
holes or hash keys are accepted anywhere. Key inspection is raw and bounded by
field count and stops at the first unexpected key; a bounded array scan proves
both the required indices and the absence of extra keys, because `#` alone does
not prove density. Primitives are copied and caller tables are never retained.
Column and row arguments must be exact signed 32-bit integers before range
checks, and there is no implicit pixel-to-index coercion.

## Movement, mutation and refusal semantics

Per successful tick, in this order:

1. Input and asset publication precede the Luau update, unchanged. Accepted world
   and map writes are visible to later calls in the same callback.
2. After the callback returns, candidates are collected in stable entity-ID order
   (`Kernel::entities`' existing hecs-slot ordering). An entity without a
   collider keeps `position + velocity * FIXED_DT` exactly.
3. Each collider sweeps X across every crossed face and every overlapped row,
   then Y from the resolved X across every column now overlapped. Zero
   displacement on an axis performs no sweep and visits no cells.
4. Every candidate and the whole-pass work budget are validated before any
   position is committed. A failure preserves the post-callback state of every
   entity and does not roll back earlier successful script writes; the runtime
   then uses its existing `systems` fault path and does not count the tick.
5. Draw and the next update read committed positions. Velocity keeps its
   requested value even when blocked, so holding a direction keeps pressing the
   wall.

Bodies never block each other and never inherit image dimensions. There is no
pending edit queue, automatic depenetration, contact callback, collision event
history or implicit velocity reset.

`set_position` remains an immediate teleport (T5). For a collider it is refused
when the destination box would leave the map or overlap a solid tile; the
teleport path is never swept and the body is never depenetrated. Teleporting
across a wall to a free destination is intentional.

T6 transitions for M1: map replacement, cell edits and collider
attachment/resize each preserve non-overlap or refuse atomically, and the map may
be cleared only when no colliders remain. Installation validates every existing
collider against the candidate map, including its dimensions, extent limits and
boundary containment, before the swap. A cell edit validates against every
potentially overlapping body before changing the ID; an edit to a non-solid ID
can never introduce overlap and skips that check. Attaching, resizing and
teleporting check every covered cell rather than a sweep. Failed calls preserve
the old map, cell, collider and position, and reads never observe a partial map.
The M1 transition recipe is therefore: detach colliders, replace the map,
position entities, reattach. That global restriction is an M1 contract and must
be revised for M2-M4.

Lifecycle: map calls always address the current map, and every owned read is a
snapshot rather than a live alias, so a retained table cannot observe a later
edit. Existing entity handles supply collider identity, including their stop,
fault and slot-reuse refusals. Despawn releases collider capacity. Stop makes map
and collider access inactive and releases the map and candidate storage; a normal
shutdown may read them before stop, while a fault invokes no shutdown callback at
all. No collision work runs from `draw`, from asset advance or drain, or from a
refused lifecycle call.

Failure classification:

| Condition | Behavior |
| --- | --- |
| Schema, geometry, bounds, missing map, bad handle, wrong phase, overlapping placement | Recoverable call error; nothing published |
| World-attempt, aggregate tile-work, region-output and live-collider budget exhaustion | Latched outside `pcall`, as the existing entity and world budgets already are |
| Fixed-system budget or invariant failure | Atomic systems fault; no entity moves |
| Allocation refusal before publication | Recoverable inside a callback; a candidate-buffer allocation failure inside the systems pass is a systems fault |

Errors stay bounded and retain no caller tables, map-sized diagnostics or
allocations proportional to rejected text. No kernel, hecs or `RefCell` borrow
survives VM conversion, VM allocation, deadline checking or user code. The host
deadline is checked before and after kernel operations and between conversion
batches of at most 256 work units. Map installation prepares an owned candidate
and a bounded collider-placement snapshot, releases the kernel borrow, validates
in batches, and swaps last; the callback is synchronous and validation runs no
user code, so nothing can mutate the kernel in between. Fixed systems run outside
the callback deadline under their own deterministic work cap.

## Bounds, storage and work, as measured

| Resource | Frozen bound | Measured |
| --- | --- | --- |
| Map dimensions | Each `1..=1024`; product at most 262,144, checked before allocation | 1,024 x 256 maximises columns plus rows and is the stress shape |
| Tile dimensions | Each integer `1..=1024` world pixels | - |
| Geometry domain | Integer origin; every map and collider box edge within +/-2^24 | A 1,024 x 256 map of 1,024-pixel tiles placed to end exactly on +2^24 clamps a maximum body exactly at the boundary |
| Collider extent | `1/256 .. min(8 tiles, 4,096 px)` per axis; at most nine cells per axis | - |
| Collider offset | Finite, magnitude at most 4,096 pixels | - |
| Live colliders | 1,024, inside the existing 16,384 entity limit | - |
| Map storage | u16 cells, at most 512 KiB, plus 1,024 solid flags | 524,288 + 1,024 bytes |
| Replacement peak | One old map plus one candidate | 1,050,624 bytes |
| Integration scratch | One reusable candidate buffer for 16,384 entities, reserved only once a map or collider exists | `(hecs::Entity, Position)` is 24 bytes, so 393,216 bytes; a session with neither keeps today's allocation-free integration. **Superseded by Phase 2**, which sizes the buffer against the collider limit instead: only a swept body needs its result remembered, because a free-flight entity recomputes `position + velocity * FIXED_DT` bit-identically in the commit pass, so 1,024 entries of 72 bytes is 73,728. Atomicity is unchanged; the reservation still waits for a collider to exist |
| Worst single body | Analytic ceiling `9 * (columns + rows)` = 11,520 units | 11,120 units, at 1-pixel tiles with an 8-tile body |
| Fixed-pass tile work | 16,777,216 units per tick, no per-entity reset | Long-sweep stress: 11,386,880 units, 67.9% of the ceiling |
| Callback tile work | 1,048,576 units | Largest map install: 346,112 units, leaving room for 685 further solid edits under 1,024 colliders |
| Region output | 4,096 IDs per call, 262,144 per callback, charged before allocation | 64 maximum-size calls per callback |
| World calls | The existing 4,096 attempts per callback, shared | - |

Work units are visited cells and body checks. A sweep charges one unit per cell
it inspects on a candidate face. A placement check charges one unit per covered
cell, at most 81, and belongs to the callback that attached, resized, teleported
or edited, never to the fixed pass. That split is a fact rather than a
convention: every placement check originates from `set_tile_collider`,
`set_position`, `set_tile` or `set_tilemap`, all of which run inside a callback.
A map install charges its copied and validated `cells` and `solids` elements plus
one placement check per live collider.

A cell edit that makes a cell solid charges one unit plus one per live collider.
That figure is exact rather than an upper bound, because each collider-versus-cell
test is constant time and M1 keeps no collider index. An edit that leaves the cell
non-solid, or that replaces one solid ID with another, cannot introduce overlap
and charges one unit. As the plan requires, a rejected request still consumes the
work already performed: the collider scan happens before the refusal, so a refused
`set_tile` is charged for it.

Caveat on the fixed-pass figure below: 11,386,880 units is the sweep alone. If
Phase 2 adds any per-candidate re-check after the sweep, even a defensive one,
that adds up to 81 units per collider, or 82,944 across the collider limit. The
measured number must be restated if that happens rather than carried forward.

The fixed-pass ceiling was recounted against the frozen collider extent, as the
plan required. The mandated long-sweep configuration - the 1,024 x 256 map, 1,024
maximum-footprint colliders each sweeping the full width and height, and the full
16,384-entity population - costs 11,386,880 units and completes rather than
faulting. The plan's estimate of about 11,520 units per body is an upper bound the
measurement stays under, because a body that reaches the far boundary arrives
flush and its second axis then spans eight cells rather than nine.

**Wall-clock caveat, recorded as a Phase 2 obligation rather than a ceiling
change.** That same long-sweep pass took p50 20.6 ms, p95 24.3 ms and max 25.8 ms
over fifteen repetitions in this release build, against a 16.67 ms tick period;
repeated runs moved the tail between roughly 24 and 34 ms with an unchanged p50.
The deterministic work ceiling is therefore not a frame-rate promise. The concrete
degradation is checkable: `MAX_FRAME_TICKS` is 5, so a frame that runs full
catch-up at this load performs about 100 ms of fixed-pass work, which is roughly a
10 FPS floor with `overloads` incrementing on every frame that drops ticks. The
sparse short-motion arrangement of the same population costs 5,057 units and
89 us, four orders of magnitude cheaper, which is what ordinary content resembles.
The probe
solver is unoptimised prototype code with bounds-checked indexing and per-axis
dispatch, so Phase 2 should re-measure its own implementation before anyone
treats 20 ms as the production cost. No ceiling is changed on this evidence.

## Captured sprite expectations and migration decisions

The sample's current behavior, captured before any change:

| Property | Current value |
| --- | --- |
| Room | 30 x 17 tiles at 32 screen pixels, from `room.luau`'s legend and rows |
| Spawn | (448, 256), tile (14, 8) |
| Speed | 2 pixels per tick, applied X then Y in `main.luau` |
| Solver | Four integer pixel corners with a `size - 1` extent, private `x`/`y` |
| Holding Right | Settles at x = 640 against the fence at column 21 |
| 30 Right then 30 Down | (508, 316) |
| Animation | Driven by held movement intent, including walking against a wall |
| Live probe | Logs position, frame and facing at the end of update, after Luau movement |

Migration decisions frozen here:

- The migrated velocity is 120 pixels per second, which is exactly 2.0 per tick
  (N6), so every asserted position stays a whole pixel and the exact-equality
  fixtures survive.
- The collider is a zero-offset 32 x 32 box. Holding Right still settles at
  x = 640: the fence's left face is 672 and 640 + 32 lands exactly on it, so the
  old corner test and the half-open box agree at that wall. Verified through the
  frozen sweep on the real room, along with the (508, 316) walk and the spawn
  box being free.
- `room.luau` stays the authored source. Its legend becomes tile IDs 1..5 with a
  matching `solids` array, converted once in init; ID 0 stays unused, which also
  exercises a non-solid ID other than 0. Its malformed-row and unknown-symbol
  diagnostics are preserved.
- The immutable ID palette the script authored stays script-side. The
  pre-tileset placeholder path keeps drawing a rectangle per solid tile from that
  palette rather than spending one `tile_solid` call per cell. It is not a second
  mutable collision grid.
- Backspace keeps calling `set_position` unconditionally and documents its
  reliance on the spawn cell staying free, rather than handling a refusal. Init
  attaches the collider at the spawn, which is an engine-checked placement, and
  the sample never edits its own spawn cell. A refusal would therefore be an
  engine or authoring bug and should fault loudly instead of being swallowed.
- The update-time tile-edit fixture lives in a separate headless fixture bundle,
  not in the shipped sample, so the sample keeps its current controls and the
  fixture can edit cells under the body freely. The plan's "a stale Luau grid must
  fail the tile-edit fixture" negative control lives with that bundle, since it
  has to perturb the same fixture.
- The live probe's log statement moves to the top of `update`, before `ticks` is
  incremented, and reports the committed kernel position together with the
  animation state the previous tick left. Every field then describes the state
  after completed tick N, and simulation never moves into draw to preserve a log
  anchor. `Get-ProbeStates` must be widened to parse a decimal coordinate and
  compare numerically: it currently matches integers only and silently discards
  any line it cannot parse, so a fractional clamped position would leave the
  harness reading a stale sample instead of failing. Positions stay integral in
  this sample, but the harness must fail rather than skip if that ever changes.
- Phase 4 keeps a separate headless assertion of post-step kernel position, since
  the probe log is a pre-system read by construction.

## Probe and observations

The [Phase 0 probe](../../examples/tilemap_probe.rs) is prototype geometry and
work counting, not the production kernel; Phases 1 and 2 own `src/tilemap.rs` and
`src/collision.rs`. It depends only on `std`, `hecs` for the candidate size, and
the existing `protogine::kernel` constants, so it builds in the core
configuration. Its integer oracle restates the same sweep in exact 1/256-pixel
arithmetic phrased on box edges rather than position plus offset, and shares only
the fixture grid, so agreement is evidence rather than a tautology.

That oracle's scope is narrow and the record should not be read as claiming more:
its grids are at most 12 x 9 cells with tiles of 1..8 pixels and origins within
+/-64, so it never visits the geometry limit where the interesting rounding lives.
Its arithmetic could reach the limit - every intermediate stays under 2^32 - and
widening it is cheap work Phase 2 should do. The domain edge is instead covered by
the separate maximum-footprint fixtures here, and by an independent cross-check the
review session ran outside this repository: an exact-rational oracle over 39,939
cases at +/-2^24 with tiles 1..1,024, offsets +/-4,096, maximum-footprint bodies
and travel up to 1e7 found 0 logic mismatches, 0 penetration on the reconstructed
edge, a worst gap of 2^-29 and at most 1 repair step. That result is recorded as
reviewer-supplied evidence; its harness is not committed here, so it is not
reproducible from this repository and does not substitute for widening the
committed oracle.

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

The runner enforces an independent 30-second child watchdog per mode, 60 seconds
for `work`, and requires each positive mode's own `TILEMAP PASS mode=` marker
because a zero exit does not prove the assertions ran. It was exercised under
both PowerShell 7 and Windows PowerShell 5.1.

Positive fixtures covered: exact and fractional coordinates, non-square maps and
tiles, negative origins, ID 0 and a non-solid ID above 0, all four approach
directions, exact touching, moving away and tangent spans, local offsets, the
smallest and largest legal boxes, an interior solid cell that clears every
corner, travel across many tiles into a one-cell wall, the nearer of two walls,
enormous finite velocities stopping at the map boundary, the X-before-Y corner,
wall sliding, and the shipped sample's own fence and replay expectations.
Counted coverage from the receipt: 26,624 face-adjacent index cases, 16,270
randomized cases agreeing with the integer oracle at the small-grid scope noted
above, and 889,145 clamp cases spanning the whole domain.

Negative controls, each failing at its named assertion under the watchdog:

| Mode | Replaced rule | Failing assertion and observed value |
| --- | --- | --- |
| `control-endpoint` | Sweeping becomes an endpoint-only test | `high-speed wall stop`: tunnelled to x=384.0 instead of stopping at 288.0 |
| `control-corners` | Perpendicular spans keep only their endpoints | `interior solid cell`: a 3x3 box over a solid centre read as free |
| `control-truncate` | Cell indexing truncates toward zero | `negative origin cell index`: world -101.0 read as column 0, not -1 |
| `control-yfirst` | Axis order becomes Y before X | `x-before-y corner`: resolved to (0.0, 32.0) instead of (32.0, 0.0) |
| `control-naive-clamp` | The clamp trusts its subtraction without repair | `clamped box on the free side`: edge 16777216.000000004 past the +2^24 boundary |

Saved receipts: [probe](evidence/tilemap-phase0-probe.txt) and
[negative controls](evidence/tilemap-phase0-controls.txt).

Observed on Windows MSVC x64, rustc 1.95.0 (59807616e), release profile, with OS
scheduling uncontrolled. Timings are single-machine observations, not throughput
promises. Nothing here establishes runtime publication, Luau binding behavior,
Player rendering or live-input behavior; those are the Phase 1-4 gates.

## Check results and handoff

Passed:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo build --release --example tilemap_probe --no-default-features
pwsh -NoProfile -File tools/run_tilemap_probe.ps1 -Mode <each of the seven modes>
powershell -NoProfile -File tools/run_tilemap_probe.ps1 -Mode numeric
powershell -NoProfile -File tools/run_tilemap_probe.ps1 -Mode control-naive-clamp
```

`cargo test --workspace` reported 170 passing and 5 ignored across the library
and integration targets, and `--no-default-features` reported 20 passing, both
unchanged by this phase. Changed paths, all additions:

```text
examples/tilemap_probe.rs
tools/run_tilemap_probe.ps1
docs/implementation/TILEMAP_COLLISION_PHASE0.md
docs/implementation/evidence/tilemap-phase0-probe.txt
docs/implementation/evidence/tilemap-phase0-controls.txt
```

plus status and pointer edits to `TILEMAP_COLLISION_PLAN.md`, `TODO.md`,
`AGENTS.md`, `docs/DEVELOPMENT.md` and `docs/implementation/README.md`. No engine
source was modified and no production API was added. Graphics, native, capture
and live-input gates were not rerun because no code they cover changed.

The three Phase 0 stop gates are closed. Overlap semantics are defined in N1-N3
and the schema; transition semantics are defined by the T6 section above; budget
behavior is defined by the failure table and the measured bounds. The adjacent-f64
rule is settled by N5 with its measurement, the free-flight product is verified
rather than assumed, and the max-load long-sweep count is recomputed against the
fixed-pass ceiling and fits.

The next unmet gate is Phase 1: checked map types, install, replace, clear, info,
regions and cell edits in `src/tilemap.rs` with dependency-free exports and owned
inspection, tested with no default features.

Phase 1 delivers those mutations before any collider exists, so its exit gate is
not judged against the collider guards in the T6 section above; extending every
Phase 1 mutation to enforce collider invariants is explicitly Phase 2's work. What
Phase 1 does owe from this record is the schema, the N1-N4 geometry rules, the
saturated cell-index correction and the canonical edge-reconstruction helper.

Obligations carried forward:

- Phase 1: saturate the cell-index correction and `debug_assert` its bound rather
  than relying on callers clipping first; route every body-versus-grid comparison
  through one edge-reconstruction helper.
- Phase 2 **(discharged 2026-09-07)**: make an exhausted clamp repair a systems
  fault rather than a silent fallback; re-measure the 20 ms worst case against
  the production solver before anyone reads it as a cost; restate the fixed-pass
  figure if a post-sweep re-check is added; widen the committed integer oracle to
  the geometry limit. No post-sweep re-check was added, so the measured
  fixed-pass figure stands. See the plan's Phase 2 exit for what each discharge
  actually produced, including the split of the clamp fault into `Embedded` and
  `Unconverged` and the re-measured timings.
- Phase 4: apply the `Get-ProbeStates` and probe-log changes specified in the
  migration section before touching the live probe.
