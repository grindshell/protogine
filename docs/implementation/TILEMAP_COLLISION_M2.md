# M2 contract: simultaneous maps

**Status:** frozen 2026-09-08 against `7d7c3ad`, after three review rounds.
**Nothing is implemented.** Every rule below is settled and Phase 1 may be built
against it; every *number* remains a proposal for the Phase 0 probe to confirm
or move, and the probe's job is to make some of them wrong. Four decisions stay
marked **[OPEN]** with their alternatives, because none of them blocks the probe
and pretending they were settled is how the last three rounds' findings got
written in the first place.

Frozen means the claims have been read against the M1 *code* rather than against
M1's documents, twice by a second session; it does not mean they are true. The
review found two blocking contradictions, four smaller errors and an impossible
worked example, none of which were in the parts marked uncertain.

Inherits [M1](../../TILEMAP_COLLISION_PLAN.md), whose
[Phase 0 record](TILEMAP_COLLISION_PHASE0.md) owns every numerical and refusal
rule this document does not restate. M1's contracts stand except where a
numbered revision below replaces one.

## What M2 owes

From the [plan's milestone table](../../TILEMAP_COLLISION_PLAN.md#required-core-milestones-and-full-plan-completion):
map identity/session/generation rules, membership and transfer semantics,
per-map coordinates, simulation participation, map-local replacement and
removal, counts and aggregate memory/work limits, migration of M1's
implicit-map APIs, and a recorded T6 revision. Completion needs two maps with
overlapping coordinate ranges and different walls giving independent collision
results; atomic transfer; stale and foreign map references refusing; replacing
or removing one map preserving unrelated maps and entities; and the same
behaviour headlessly and in a copied Player.

What M2 does **not** touch: layers (M3), streaming (M4), and every M1 exclusion -
infinite maps, isometric grids, tile-map rendering commands, cameras,
entity-entity collision, slopes, gravity or rotation. A map is still one finite
orthogonal grid of solid/non-solid tiles.

## M2-1. Map identity

**A map is named by a handle, not by being the only one.** `TileMapHandle`
carries an opaque session marker and a generation-checked slot, exactly as
`EntityHandle` does:

```rust
pub struct TileMapHandle { session: Rc<()>, slot: u32, generation: u32 }
```

Maps live in a private slot table on `Kernel`, not in hecs. T1 put cell storage
outside the ECS so a static tile is never an entity; putting a *map* in hecs
would be a smaller violation of the same idea, but it would make maps appear in
`entities()` and `snapshot()`, which are contracts about game entities. The slot
table reimplements the small part of hecs this needs - free-list reuse and a
generation per slot - and nothing else.

Rules. Three are inherited from how entity handles already behave; the fourth
is new, and **an earlier draft said "all inherited" because it was true when
written and stopped being true three commits later**, when freezing the
slot-reuse policy added a rule with no M1 counterpart. `EntityHandle` is
`{ session: Rc<()>, entity: Entity }` and `validate` is `Rc::ptr_eq` plus
`world.contains` (`src/kernel.rs:191-197`): protogine holds no generation of its
own, the one that exists lives inside hecs' `Entity`, and the reuse order is
hecs' business and unspecified here. So the generation bullet inherits nothing -
which is exactly why M2 had to freeze a policy rather than point at one. This is
the freeze pass's third clause again, in its other form: not a constant whose
referent moved, but a *new rule* falsifying a summary sentence about the old
ones. The finding is the review session's.

- A handle from another `Kernel` refuses, by `Rc::ptr_eq` on the session.
- A handle to a removed map refuses, by generation, including after its slot has
  been reused by a different map.
- `Kernel::stop` releases every map and invalidates every handle - by making the
  session inactive, not by touching generations. **The mechanism matters and an
  earlier draft stated only the outcome.** `require_active` already runs first
  in `validate` (`src/kernel.rs:192`) and in `map` (`:319`), so after `stop`
  every entry point refuses with `Inactive` *before* any slot or generation is
  examined. Generations are not bumped and the table is dropped only to release
  storage, exactly as M1 drops `tilemap` for that reason alone. So the two
  candidate mechanisms a reader might infer - bump every live generation, or
  empty the table - are both wrong, and the distinction is observable: after
  `stop` a handle refuses `Inactive`, where the same handle to a removed map in
  a live session refuses `InvalidTileMap`. Phase 1 asserts both errors by name
  rather than asserting that two different situations both refuse. The question
  is the review session's.
- Handles are opaque userdata in Luau, never numbers, so a script cannot forge
  one - the same reason `EntityHandle` is userdata.

The internal identity a component stores is `TileMapId { slot: u32, generation:
u32 }`: `Copy`, no `Rc`, because a component can only exist inside the kernel
that owns it, so the session is implied. The public handle carries the session
and is checked at the API boundary. This is the same split `ImageId` and
`EntityHandle` already make at different layers.

**Slot reuse is deterministic, and that is a contract clause rather than an
implementation detail.** `create_tilemap` takes the most recently freed slot;
with none free it takes the next unused index. The reason is that the generation
check cannot otherwise be tested. A handle to a removed map and a handle to a
reused slot both refuse, and the obvious fixture - remove a map, present its
handle, assert a refusal - cannot tell those apart, because both hold on a slot
that was never recycled. Distinguishing them needs an *actual* reuse: remove map
A, create map B, require B to land in A's slot, then present A's handle and
require the refusal while B's own handle succeeds at the same slot index. If the
allocator is free to hand B a fresh index, that fixture silently degrades into
the easy case it was written to avoid. So the policy is frozen here and Phase 1
asserts the landing rather than assuming it. The requirement is the review
session's.

**Freezing "most recently freed" also picks the policy that concentrates
generation churn, so the bound is stated rather than left to be inferred.** A
create-and-remove loop hammers one slot's counter where a first-in-first-out
free list would spread it across the whole list, and M4's streaming is exactly
that loop. At `u32` a slot survives 4,294,967,296 reuses, which at one create
and one remove per 60 Hz tick is **828 days of continuous churn**, so it is
unreachable and carries no coverage claim - the same treatment M1 gives
`KernelError::Capacity` and `CollisionError::Unconverged`, which are recorded as
defensive with their arithmetic attached. If M4 ever makes that loop real at a
higher rate, this is the paragraph to revisit.

**The entity side has no equivalent guarantee, and the symmetry is inviting
enough to say so.** hecs owns entity slot reuse, so an M1-style fixture -
despawn, respawn, assert the stale handle refuses - rests on a dependency's
behaviour rather than on a contract of ours. M1 already handles that correctly:
`reused_slots_replace_their_stale_wrapper_without_growing_the_cache`
(`src/scripting/world.rs:535`) asserts `first.slot() == second.slot()` before
relying on the reuse, so it fails loudly if hecs ever stops recycling. The
*pattern* is therefore already in the repository; the *guarantee* is not, and
only maps have one. Nobody should write a new entity fixture believing otherwise.

## M2-2. Membership

**A collider belongs to exactly one map, named when it is attached.** Membership
is a separate private component holding a `TileMapId`, inserted and removed
atomically with `TileCollider`.

Separate rather than a field on `TileCollider`, because `src/collision.rs`
depends on `crate::tilemap` and nothing else, and a slot index into a kernel
registry is not a geometry concept. Every predicate there already takes its map
as an argument, so the module needs no change at all.

**The cost is that "collider without membership" becomes representable, and it
is larger than the `colliders` counter's.** An earlier draft called the two
obligations equivalent; they are not. The counter is one integer with one update
site. Membership is per entity and has to stay in step with `TileCollider` at
four: attach, transfer, `remove_tilemap`, and entity despawn. That is the
specific risk this choice buys, so the control list carries a control for it -
a collider that can exist without membership, or that survives its map's
removal, must fail a named fixture.

**[OPEN]** The alternative is a `map` field inside `TileCollider`, which makes
the invariant structural instead of enforced. It pushes a kernel identity into
the geometry module, which is why it is not the proposal, but a reviewer who
weighs the invariant above the layering should say so now rather than after
Phase 2.

Membership is not inferred from position. Two maps may cover the same world
coordinates; which one a body collides against is a property of the body, and
the only way to change it is M2-5's transfer.

## M2-3. Coordinates and participation

**One shared world coordinate space. Maps carry their own origin and tile size
and may overlap freely.** Entity positions stay finite f64 world pixels, exactly
as M1 leaves them; no position is ever map-relative, and no transform is applied
anywhere.

This is what makes the mandated evidence work: two maps with overlapping ranges
and different walls give independent results because each body consults only its
own map, not because their coordinates are separated.

The consequence to state plainly, because it is a semantic change hiding inside
an unchanged sentence: T3's **"outside the map is solid"** becomes **"outside
*your* map is solid"**. A body cannot walk out of map A into map B even where
they overlap. Movement between maps is only ever M2-5's explicit transfer. The
alternative - a body leaving one map's bounds and being picked up by another -
would need an ordering rule for overlapping maps, would make collision depend on
map creation order, and is not proposed.

**Simulation participation follows membership and nothing else.** A map with no
members costs nothing per tick, because the fixed pass iterates colliders rather
than maps. There is deliberately no per-map "active" flag: M3 has to define how
colliders select participating *layers*, and a second participation mechanism at
map level would have to be reconciled with it. One mechanism, defined once, in
the milestone that needs it.

## M2-4. Lifecycle, and the T6 revision

| Call | Behaviour |
| --- | --- |
| `create_tilemap(desc) -> handle` | Validate and copy a complete description, admit it against the aggregate budgets, install it in a fresh slot |
| `replace_tilemap(handle, desc)` | Replace one map's contents in place, revalidating only *that map's* members; the handle stays valid |
| `remove_tilemap(handle)` | Retire one map; refuses while any collider is a member of it |
| `tilemap_info(handle)` | Owned dimensions, tile size and origin |

**T6 revision (M2-R1), recorded as the plan requires.** M1's T6 reads "Clear the
map only when no colliders remain" and its transition recipe is detach
everything, replace, reposition, reattach. That global restriction becomes
map-local:

> A map may be removed only when no collider is a member of *it*. Replacing a
> map's contents revalidates only its own members. **A cell edit scans only the
> edited map's members.** Colliders on other maps are neither checked nor
> detached, and no operation on one map can refuse because of a body on another.

**The cell-edit clause is the one that has to be written down, and an earlier
draft of this section said the opposite by calling cell edits "unchanged".**
`Kernel::set_tile` (`src/kernel.rs:423`) queries every live collider in the
world and tests each against `overlaps_cell`, which compares a body's world-space
box against a cell rectangle derived from the map it is passed. In M1 that global
query was exactly right, because there was one map and every collider was on it.
Under M2-3 - one shared coordinate space, maps free to overlap - it becomes
wrong in a way that contradicts the sentence three lines above it: a body that is
a member of map B, standing at world coordinates that happen to fall inside a
cell of map A, would make a non-solid-to-solid edit to **map A** refuse. So
"unchanged" was the wrong word for the one entry point whose map argument no
longer implies its collider set.

What does survive unchanged is the guarantee rather than the implementation:
replacement, cell edits and collider attachment still preserve non-overlap or
refuse atomically, and a refusal still leaves the old map, cell, collider and
position exactly as they were.

M4 will need a further revision for regions retired under a live body; this one
does not anticipate it.

Removal is not deferred and there is no reference counting: a map with members
refuses removal, so a handle is invalidated only by an operation that had
nothing depending on it.

## M2-5. Transfer

The plan mandates that transfer **succeeds or refuses atomically**, so it must
be one call that changes membership and position together. Three calls - detach,
teleport, reattach - are not equivalent: a failure at the third leaves the body
detached at a new position, which is a partial outcome the script has to unwind.

The proposal folds it into attachment rather than adding a parallel entry point:

```rust
pub struct ColliderPlacement {
    pub map: TileMapHandle,
    pub collider: TileCollider,
    /// `None` keeps the entity where it is.
    pub position: Option<Position>,
}

fn set_tile_collider(&mut self, entity: &EntityHandle, placement: Option<ColliderPlacement>)
```

- `None` detaches, as today.
- `Some` with no position attaches, replaces or transfers at the current
  position, validated against the named map.
- `Some` with a position does the same *and* moves the body, validated only
  against the destination. This is the only way to reach a map that does not
  overlap the body's current one.

Order of checks, all before anything is written: session and generation on both
handles, then extents against the destination map's tile size - a body legal on
a 32-pixel grid can be illegal on an 8-pixel one, since `MAX_COLLIDER_TILES` is
tile-relative - then placement of the destination box on the destination map. A
refusal leaves membership, geometry and position untouched.

The position here is a teleport under T5 and is never swept, exactly as
`set_position` is. Transfer does not interpolate between maps and there is no
intermediate state in which a body belongs to both or neither.

**Mid-sweep transfer is not a state that exists.** `fixed_update` runs to
completion inside one call with no script interleaving, so a body cannot change
maps while being swept. This was flagged as an open question when M2 was
proposed and it resolves to "unreachable" rather than to a rule.

**[OPEN]** Whether the Luau surface should carry this as one optional-field
table (`{map = m, offset_x = …, width = …, x = …, y = …}`, with `x` and `y`
present together or not at all) or as a separate `transfer_collider` call. The
table keeps one schema and one call; the separate call keeps `set_tile_collider`
exactly as scripts know it and makes the atomic move nameable in an error
message. Leaning to the table, unsure.

## M2-6. API migration

Every M1 map call addressed an implicit current map. All of them gain an
explicit handle or resolve one from the body, and two are renamed because their
meaning changed.

**The scripted calls are not all of them, and an earlier draft's "all of them"
covered only these nine.** The Rust entry points that address the implicit map
are listed after the table; one of them is a T5 semantic change of exactly the
kind M2-3 spells out for T3, and it was missing.

| M1 | M2 |
| --- | --- |
| `set_tilemap(desc)` | `create_tilemap(desc) -> map` and `replace_tilemap(map, desc)` |
| `clear_tilemap()` | `remove_tilemap(map)` |
| `tilemap_info()` | `tilemap_info(map)` |
| `tile(c, r)` | `tile(map, c, r)` |
| `tile_solid(c, r)` | `tile_solid(map, c, r)` |
| `tiles_region(c, r, w, h)` | `tiles_region(map, c, r, w, h)` |
| `set_tile(c, r, id)` | `set_tile(map, c, r, id)` |
| `set_tile_collider(e, options)` | `set_tile_collider(e, placement)` |
| `tile_collider(e)` | `tile_collider(e)`, returning the map alongside the box, because M2-2 makes membership unobservable any other way |

Four Rust entry points also address the implicit map and are not scripted, so
they are not in the table above:

| M1 | M2 |
| --- | --- |
| `set_position(entity, position)` | unchanged signature; resolves the body's **member** map, not a session-wide one - see M2-R2 |
| `tile_face(axis, index)` | `tile_face(map, axis, index)` |
| `tile_at(axis, world)` | `tile_at(map, axis, world)` |
| `tilemap_storage_bytes()` | becomes the aggregate accessor M2-7's budgets and the stress runs read |

**T5 revision (M2-R2).** T5 reads "For a collider, reject an overlapping or
out-of-map destination", and that sentence is identical before and after M2 while
meaning something different - the same trap M2-3 defuses for "outside the map is
solid" and which an earlier draft of this document walked into here:

> A teleport is validated against the map the body is a **member** of, never
> against any other live map. `set_position` remains a teleport, is still never
> swept, and still refuses an overlapping or out-of-map destination; "the map"
> in that rule now means the body's own. A body with no collider is unvalidated
> as before, and a body whose destination is legal on a different map is still
> refused, because changing maps is M2-5's transfer and nothing else.

`set_position` keeps its signature. Adding a map argument would give it two ways
to say which map applies - the argument and the membership - and one of them
would have to lose.

This is a breaking change to every script that uses a map, which the plan
anticipates: "M2 must define their API migration and map-local replacements".
No compatibility shim, and specifically **no implicit "the only map" fallback** -
it would work until a game created a second map and then change the meaning of
existing calls silently, which is the worst available failure mode.

`ctx.world` keeps one table and one attempt budget (T7), and the new calls share
them.

## M2-7. Budgets

**Every number in this section is a proposal for the Phase 0 probe to confirm or
move.** M1's ceilings are explicitly not aggregate multi-map limits.

| Resource | Proposed bound | Reasoning |
| --- | --- | --- |
| Maps per session | 64 | Raised from a proposed 32 by the owner on the Phase 0 probe's evidence, recorded under [OPEN] 4 below. Still small enough that a linear scan over maps is never a cost worth optimising |
| Aggregate live cells | 524,288 | 1 MiB of cell storage, twice M1's single-map allowance, and it fits inside one callback with the arithmetic stated rather than asserted. A Luau install charges exactly once per cell - `src/scripting/world.rs:333` wires `charge_callback_work` into `solid_flags` and `cell_ids`, and `Kernel::set_tilemap` charges only for revalidating existing members, which is why Phase 4's install-cost test lands on 510 + 5 + 1 = 516 rather than the 1,026 a second charge per cell would add. So filling the whole budget costs 524,288 cells plus at most 64 x 1,024 solid flags = **589,824 of 1,048,576**, leaving 44% spare |
| Per-map dimensions and cells | unchanged: 1..1,024 per axis, at most 262,144 cells | A single map is no larger than M1's |
| Staging | at most one candidate map in flight | Peak cell storage is the aggregate plus one map: `(524,288 + 262,144) x 2` = 1,572,864 bytes, exactly 1.5 MiB. Adding the solid flags from the row above - 64 x 1,024 live plus the candidate's 1,024 - makes the peak 1,639,424 bytes, 1.563 MiB. The unqualified "about 1.5 MiB" an earlier draft carried was true of cells and not of the total, in a table where the row above counts flags explicitly |
| Live colliders | unchanged: 1,024 across all maps | The limit that bounds the fixed pass is global, so it stays global |
| Fixed-pass work | unchanged: 16,777,216 | Not a fresh derivation: 1,280 faces x 9 cells x 1,024 bodies = 11,796,480 is what `the_fixed_pass_ceiling_cannot_be_reached_under_the_frozen_limits` (`src/collision.rs:590`) already computes and asserts. The bound is insensitive to how bodies are distributed **by construction**, given four inputs M2 leaves alone: `MAX_LIVE_COLLIDERS` stays global, `MAX_SPAN_CELLS` is unchanged, per-map `columns + rows` is still capped at 1,280, and the pass still iterates colliders rather than maps. See the Phase 0 probe for what is actually at risk |
| Callback work | unchanged: 1,048,576 | Now shared across every map a callback touches |
| Region output | unchanged: 4,096 per call, 262,144 per callback | Now aggregate across maps |

Solid-flag storage is at most 64 x 1,024 flags. Map count and aggregate cells are
both checked before allocation.

**Admission is checked against `live - replaced + candidate`**, where `replaced`
is zero for `create_tilemap` and the outgoing map's cell count for
`replace_tilemap`. The description's info carries `columns` and `rows`, so the
candidate's size is known before its cells are copied and the check happens
before the allocation rather than after it.

That subtraction is the whole rule and an earlier draft omitted it, leaving two
sentences pointing opposite ways: this one read as `live + candidate`, while the
staging row's "peak storage is the aggregate plus one map" only makes sense if
the candidate is *not* counted in the aggregate. Charging `live + candidate`
would mean **a session using the budget it was granted could never replace any
map, including with an identical one** - a game holding 524,288 cells could not
swap a room, and a game holding one full-size map plus **a single cell anywhere
else** could not replace that map at all: replacing it would need
`live <= 524,288 - 262,144`, and `live` already contains the map's own 262,144.
The cliff appears only at full utilisation,
which is the same shape as M1's region-charging cliff: a contract that
contradicts itself exactly for the games that use what it offers. Found by the
review session.

The candidate's own storage is staging, not live, which is why peak storage is
the aggregate plus one map rather than the aggregate. Replacing a map while the
aggregate is full therefore succeeds when the replacement is no larger and
refuses when it is larger, and the evidence table below carries both cases.

## M2-8. Failure classification

Unchanged from M1's split, extended to the new refusals. Catchable through
`pcall`: schema, geometry, bounds, missing or stale map handle, foreign handle,
bad entity handle, wrong phase, illegal placement, illegal transfer, and
**removing a map that still has members** - the map-local successor to M1's
`KernelError::CollidersAttached` (`src/kernel.rs:368`), which is catchable
today and stays so. Latching outside `pcall`: the map count, the aggregate cell
budget, the collider limit, callback tile work and region output.

A new `KernelError::InvalidTileMap` covers stale, foreign and removed handles,
distinct from `NoTileMap`, which M2 retires along with the implicit map.

## Phases

Mirroring M1's shape, because the review pair and the harness conventions are
built around it.

| Phase | Work | Exit evidence |
| --- | --- | --- |
| 0. Contract and feasibility | This document, reviewed and frozen. A probe measuring aggregate storage through the production allocation path, ordinary content against both budget limits, per-tick cost, and the fixed-pass baseline pair below | Contract frozen with no open item; probe receipts; every proposed number either confirmed or revised with its measurement; each mode asserting its own configuration was exercised, not only its result |
| 1. Kernel map registry | Slot table, handles, generations, create/replace/remove/info, `stop` release, storage and count accounting | Foreign, stale and reused-slot handles refuse; removal and replacement are map-local; storage accounting agrees with the budget; core configuration |
| 2. Membership, transfer and the fixed pass | Membership component, atomic transfer, per-map sweeping, M2-R1 on removal, replacement and cell edits, and M2-R2 on `set_position` | Two overlapping maps give independent collision; transfer is atomic in both directions; removing one map preserves unrelated maps and bodies; a cell edit ignores bodies on other maps; a teleport validates against the member map; insertion-order independence survives |
| 3. Luau migration | The map-addressed calls, handle refusals, shared budgets | Every refusal reaches Lua unchanged; aggregate ceilings latch; schema battery over the new placement shape |
| 4. Sample and release proof | Migrate the sprite sample to a named map, add a two-map fixture, update docs and checks | Sample behaviour unchanged again; two-map evidence headlessly and in a copied Player; M2 completion recorded |

**The fixed-pass check is an equality, not "still fits", and it belongs to
Phase 2.** What is at risk is not the bound - which cannot depend on distribution,
for the four structural reasons in the budget table - but the M2 *implementation*
growing a per-map term the M1 one did not have: resolving members by scanning
maps, revalidating on the pass, anything carrying a factor of 64. The check is:

> A fixed pass with 1,024 bodies on **one 128x64 map** charges **exactly** what
> the same 1,024 bodies charge spread across **64 identical 128x64 maps**, from
> the same start positions and velocities relative to each map.

**Both arms must have identical map geometry, and two earlier drafts of this
paragraph did not.** Sweep cost is proportional to the map, not only to the body:
`sweep` enumerates faces until the boundary index clamps, so a smaller map ends
the walk sooner and charges less. "Spread across many maps" under a fixed
aggregate *necessarily means smaller maps*, so comparing 1,024 bodies on one
1,024x256 map against 1,024 bodies spread over the maximum map count compares
11,796,480 against 1,769,472 - a ratio of 0.15, varying geometry and distribution
together. Phase 2 would have
asserted that, watched it fail for an entirely legitimate reason, and someone
would have weakened it back to "still fits" with a failing test as the
justification. Holding geometry constant leaves distribution as the only
difference, which is what the check was always for.

128x64 is not an arbitrary choice: `524,288 / 64 = 8,192` cells, and 128x64 is a
shape achieving that exactly, so 64 copies fill the aggregate to the cell and no
larger shape fits 64 times. The point where the two budget limits bind together
is therefore also the maximum-stress form of the constant-geometry comparison.

**Raising the map count to 64 cost this shape its symmetry, and that is worth
recording rather than rounding past.** At 32 maps the balance point was
`16,384 = 128²`, a square. At 64 it is 8,192 cells, which is not a perfect
square: the largest square whose 64 copies fit is 90x90, leaving 5,888 cells
unused. So the exact shape has to be rectangular. No loss for the comparison -
unequal axes exercise the two sweep legs differently, which is strictly better -
but a reader reaching for a square will try 90x90 and find slack, so the probe
asserts both facts.

The check has a single right answer, fails the moment a per-map cost appears, and
**cannot fail at Phase 0**, where a prototype holding `Vec<TileMap>` and indexing
it has no per-map term to grow. Phase 0 produces the baseline pair as reference
values and says they are a property of the prototype rather than evidence; the
assertion is Phase 2's. The check, the correction and the constant-geometry form
are all the review session's.

**The equality is necessary and not sufficient, so Phase 2 must assert a
predicted total in both arms and keep the equality as the cheaper cross-check.**
An equality can only see a term that *differs between the arms*. A per-body cost
added uniformly - the M2 pass resolving membership once per body, which is a
completely plausible implementation - raises both arms by the same amount, leaves
them equal, and passes. So does every other assertion the Phase 0 probe carried
before this was noticed: the spread is unchanged, the totals stay inside the
arrangement's own worst case, and no amount of varying the battery helps, because
variation does not expose an additive constant. **Only a prediction does.**

The prediction is available in closed form. For a tile-aligned body of
`SPAN_TILES` tiles starting on tile `(column, row)` of a `columns x rows` map
and sweeping to both far boundaries, the leading edge enumerates every face from
`1 + column + SPAN_TILES` up to the last interior one - the boundary face clamps
before a cell is inspected and charges nothing - with `SPAN_TILES` perpendicular
cells at each, and the Y leg does the same from the resolved X:

> `charge(body) = SPAN_TILES * ((columns - 1 - column - SPAN_TILES) + (rows - 1 - row - SPAN_TILES))`

The Phase 0 probe asserts that per body and reproduces its measurement exactly.
**Before it existed, a per-body overhead of up to 609 units each - 54% of a
body's actual average cost of 1,119 - passed every assertion in the mode**,
because the only bound was the arrangement's worst case of 1,769,472 against a
measured 1,145,600. The gap is not argued: adding one unit per body leaves all five
configuration assertions passing, leaves the two arms equal, sits comfortably
under the ceiling, and fails only the prediction. The derivation, the observation
that variation can never catch an additive term, and the closed form are the
review session's.

**"Still fits" is not an acceptable restatement of it anywhere**, and an earlier
draft left that older phrasing in the evidence table where Phase 2's exit gate
reads it. Under the constant-geometry arms above, both charge 1,145,600 against a
16,777,216 ceiling, so "still fits" is satisfied with 93% of the ceiling unused
and goes on being satisfied by a **fourteenfold** per-map term - the exact
threshold is `16,777,216 / 1,145,600 = 14.6`. An accidental unit per map per body
adds 1,024 x 64 = 65,536 and lands at 1,211,136, which is not close to anything.
"Still fits" passes. The equality fails. A small accidental per-map cost is the realistic shape of the mistake, so
the weaker phrasing would have been satisfied by precisely the defect the check
exists to catch, and the smaller the maps the more room it has to hide in.

(An earlier version of this paragraph made the same point against 11,796,480 and
11,829,248, which are the *rejected* arm's figures - a derived example left
pointing at a configuration the paragraph above it had just replaced. Caught by
the second half of the freeze pass, on the same edit that introduced it.)

## Required behavioural evidence

| Area | Cases |
| --- | --- |
| Identity | Foreign handle; handle to a removed map; handle to a reused slot, where the fixture **forces the reuse and asserts the new map landed in the old slot** rather than asserting a refusal that an unrecycled slot would also satisfy; handle after `stop`; two live maps with distinct handles that are never confused |
| Independence | Two maps with overlapping coordinate ranges and different walls; a body on each; each stops at its own wall and neither sees the other's |
| Lifecycle | Removing a map with members refuses; removing one without members succeeds while unrelated bodies keep moving; replacing one map's contents revalidates only its members |
| Transfer | Between overlapping maps; between disjoint maps, which needs the position; refused for an illegal destination box, an illegal extent against a smaller tile size, and a stale destination handle - each leaving membership, geometry and position untouched |
| Budgets | The 65th map refuses; the aggregate cell budget refuses before allocation; a refused admission leaves the count and storage unchanged; replacing a map while the aggregate is full succeeds when the replacement is no larger and refuses when it is larger; and a fixed pass with 1,024 bodies across 64 identical 128x64 maps charges **exactly** what the closed form above predicts, body by body, **and** the same as those bodies charge on one 128x64 map - the prediction because an equality alone cannot see a uniformly added per-body term, the equality because it is the cheaper cross-check; both Phase 2's to assert, and neither is "still fits" |
| Migration | Every renamed call refuses its M1 argument shape rather than guessing; `set_position` on a body validates against its member map and refuses a destination that is legal only on another |
| Membership integrity | A collider and its membership are attached, transferred, removed and despawned together, with no observable state where one exists without the other |

Required negative controls, in the harness style the earlier phases established:

- Membership read from position rather than from the component must fail the
  overlapping-maps fixture.
- A generation check removed must fail the reused-slot fixture.
- Transfer that writes membership before validating placement must fail the
  atomicity fixture.
- Removal that scans all colliders rather than the map's own must fail the
  unrelated-bodies fixture.
- **A cell edit that scans all colliders rather than the edited map's own must
  fail a fixture with a body of map B standing over a cell of map A.** This one
  matters more than its siblings because the mistake is *inherited* rather than
  newly written: `set_tile`'s global query is correct M1 code, so a Phase 2
  implementer reading it has no local reason to touch it.
- **A collider that can exist without membership, or that survives its map's
  removal, must fail a named fixture.** This is the control that pays for M2-2's
  choice of a representable invalid state, and none of the four above covers the
  pair coming apart.

The last two are the review session's.

## What I am least sure of

Flagged deliberately, because on this plan's evidence the parts written with
most confidence are the ones that have needed correcting.

1. **Membership as a separate component versus a field on `TileCollider`**
   (M2-2). The layering argument is real but so is the representable invalid
   state, and I have chosen the one that keeps `collision.rs` pure.
2. **The Luau shape of transfer** (M2-5) - one optional-field table against a
   separate named call.
3. **The aggregate cell budget** (M2-7). 524,288 is chosen so a full install
   fits in one callback, which is a real constraint but not obviously the one
   that should decide it. A game wanting sixteen 128x128 rooms needs 262,144 and
   fits comfortably; a game wanting four full-size maps does not.
4. ~~**Whether 32 maps is a limit anyone will feel.**~~ **Resolved by the owner
   on 2026-09-08: 64.** The probe found the one arrangement where a single limit
   refuses while the other is comfortable - sixty-four 64x64 rooms, an ordinary
   metroidvania, at 262,144 cells and exactly half the aggregate - and the owner
   raised the count on that evidence.

   The count is not redundant and never was: without one, 524,288 maps of a
   single cell would fit the cell budget while costing a slot, a generation and
   three `Vec` headers each. What it constrains is games with many small rooms,
   and 64 moves that boundary from "sixty-four ordinary rooms" to "one hundred
   32x32 rooms", which the probe now carries as the shape the count refuses.

   The cross-over moved with it, and lost a property worth noting.
   `524,288 / 64 = 8,192` cells per map: below that size the count binds first,
   above it the cell budget does. But 8,192 is not a perfect square, where
   16,384 was `128²`, so the balance point is now a rectangle - 128x64 exactly -
   and the largest square whose 64 copies fit is 90x90 with 5,888 cells spare.
   Noticed while designing the Phase 0 measurement, which is an argument for
   designing measurements.

**Resolved, and no longer open: `tile_collider` must return the map.** It was
listed here as a question of shape. It is not - M2-2 makes membership
uninferable from position, so a read that omits it leaves a script with no way
to observe which map a body is on at all. That is not ergonomics, it is whether
the state this document just made authoritative is readable. The argument is the
review session's.

## Phase 0 exit, 2026-09-08

`examples/tilemap_m2_probe.rs` and `tools/run_tilemap_m2_probe.ps1`, built with
`--no-default-features` because prototype geometry needs no decoder, VM or
window.

**Across the whole of M2 so far, `src/` and `tests/` are untouched.** Seventeen
commits from the contract's first draft, and the diff against M1's final state is
four documents, one probe and one runner. Every finding the review produced -
two blocking contradictions, a T5 revision, an admission rule, a fixed-pass check
that would have entered a gate false, a per-body window nothing could see, a
timing claim that weakened under measurement, a free-list policy, a `stop`
mechanism, an inheritance claim a later commit invalidated, and a structural
defect no mechanical clause would find - was in a document or in a probe built
to measure one. That is what "the contract before the code" was meant to buy, and
it is the first time this plan has been able to say it rather than intend it.

Receipts in
[evidence](evidence/tilemap-m2-phase0-probe.txt). **Every proposed number
survived, which is the least interesting outcome available and is reported as
such**: the probe was built to make some of them wrong and did not.

| Mode | Result |
| --- | --- |
| `storage` | 64 maps of 128x64 filling the aggregate exactly measure **1,114,112** bytes live and **1,639,424** at peak with the largest staging candidate, matching the contract's arithmetic to the byte |
| `work` | 1,024 bodies of maximum footprint sweeping a 128x64 map charge **1,145,600** units - 64.7% of that arrangement's own worst case of 1,769,472, and 6.8% of the 16,777,216 fixed-pass ceiling. Every body charges exactly what its geometry predicts, in both arms |
| `shapes` | All six arrangements behave as the contract claims; the cross-over is confirmed at 8,192 cells per map, which 128x64 achieves exactly and no square achieves at all - 90x90 is the largest, with 5,888 cells spare |
| `timing` | p50 **1.783 to 1.934 ms across nine runs**, median 1.794, against a 16.667 ms tick. Reported as a range because a single run understates it by 8.5% and the first figure recorded here was below all nine |

**The storage figure is measured rather than derived, and the distinction was
the reason to build it that way.** The probe allocates through the production
sequence - `Vec::new`, `try_reserve_exact`, `resize` - rather than with
`vec![...]`, whose capacity is exact by construction and would have made the
measurement confirm its own prediction. `try_reserve_exact` is documented as not
deliberately over-allocating without being guaranteed exact; on this allocator it
is exact, and that is now observed instead of assumed.

**One cross-check, and measuring it properly made it weaker.** M1's Phase 2
stress charged 11,386,880 units at a p50 of 16.5 to 17.8 ms. This arrangement
charges 1,145,600, a ratio of 9.94, so its p50 times 9.94 should land in that
range if charged work tracks time.

It roughly does, and the honest version is less tidy than the first draft of this
paragraph. **Nine runs of the timing mode on one machine give p50 between 1.783
and 1.934 ms, a spread of 8.5%**, which is the same order as the movement between
the three arrangements this was measured against. Feeding those through gives
**17.7 to 19.2 ms, median 17.8** - sitting at the top of M1's range rather than
inside it, and above it on a slow run. The single figure first recorded here,
1.7794, was below all nine.

So the claim survives as a claim about order of magnitude and nothing finer: an
arrangement charging a tenth of M1's units takes roughly a tenth of its time.
That is still the first evidence in this plan that the charged work unit tracks
time at all, and the plan has treated the two as unrelated on purpose since Phase
2 recorded that the ceiling bounds cells and not latency - but a derived figure
whose input varies that much between runs on one machine is an observation, and
reading anything finer out of it is reading the noise. The spread is the review
session's finding; they measured 1.8226 where this had recorded 1.7794 and asked
what that did to the sequence.

**The data looks tighter than that claim, and the claim stays loose anyway.**
Anyone who checks will find the unit ratio is 9.94 against an observed time ratio
of 9.2 to 9.9 - agreement within about 8%, visibly better than "order of
magnitude" - and be tempted to write that the charged unit tracks time to within
ten percent. It does not support that. It is one point of comparison across two
milestones, different map shapes, different arrangements and a machine whose own
spread is 8.5%, and the derived figure has already moved 17.7, 18.8, 17.7 on
arrangement changes that had nothing to do with the relationship being tested.
The closeness is a coincidence of this arrangement until something establishes
otherwise, and establishing it would need a deliberate sweep across shapes and
loads rather than a second data point. Recorded here so the temptation arrives
pre-answered; the caution is the review session's.

**The probe's own assertions were watched failing**, because a mode that reports
a plausible number while measuring the wrong arrangement is this plan's most
frequent defect. Eight deliberate breakages, each firing at its own named
assertion and nowhere else: the spread arm put on one map fails *every map must
carry its share of bodies*; bodies given zero travel fail *every body must charge
something*; bodies given identical start rows fail *per-body cost must vary*; one
map short of the aggregate fails *the maximum map count must be built*; a shape
claimed to fit that does not fails *does not behave as the contract claims*; and
one unit charged per body for membership resolution fails *every body must charge
exactly what its geometry predicts* **while passing all five of the others**. The
tree was confirmed byte-identical after each. The sixth exists because the first
five were satisfied by a defect none of them could see.

The probe's start column varies as well as its row, which is presentation rather
than correctness now that the prediction guards the arrangement. It was worth
fixing anyway: holding the column fixed made the X leg charge an identical 952
units for every body, 63% of the average cost invariant across the whole battery,
so a mode advertising "the absolute work an M2-shaped arrangement charges" was
exercising one axis.

**Two more guards close the prediction's residual coupling.** `predicted` reads
the same `start_cell` the battery does, so it cannot see the arrangement changing
underneath both of them - it would predict a degenerate battery correctly and
agree. That is narrow: sharing `start_cell` cannot hide a wrong solver, a
per-body term or a per-map term, all of which still fail. But it is blind to
exactly the defect the column variation just fixed, so a refactor could reinstate
it silently. The total is therefore pinned to a literal, which cannot follow
`start_cell` anywhere - the same convention as
`the_fixed_pass_ceiling_cannot_be_reached_under_the_frozen_limits` pinning
11,796,480 - and the coprimality claim in `start_cell`'s comment is asserted as
1,024 distinct starts rather than left as prose. Both are the review session's.

Ordering them took a breakage to get right, and it is the phase's own lesson
turning up inside its fix. With the literal first, the distinct-pair check never
fired at all: the total moves whenever the spread does, so the literal always
pre-empted the more specific diagnosis, leaving a guard nobody could ever read.
The specific one goes first, and each now has a breakage of its own - `i % 1`
fires the distinct-pair check, `i % 23` changes the total while keeping 1,024
distinct starts and fires the literal.

### What the probe found that the contract did not predict

**The map count bound alone for a game that is not absurd, and the owner raised
it.** `shapes` included sixty-four 64x64 rooms at the proposed 32-map limit:
262,144 cells, exactly half the aggregate, refused by the count with storage half
unused. That is a large but ordinary
metroidvania-shaped game, and it is the only arrangement tested where one limit
refuses while the other is comfortable.

A count is needed either way - without one, 524,288 maps of a single cell would
fit the cell budget while costing a slot, a generation and three `Vec` headers
each - so the question was never whether to have one but where to put it. The
probe took that to the owner rather than settling it, because which games to
constrain is a product judgement and not something a measurement decides.
**Raised to 64 on 2026-09-08.** Solid flags rise to 65,536 bytes total, noise
beside a megabyte of cells, and the shape the count now refuses is a hundred
32x32 rooms rather than sixty-four ordinary ones.

The knock-on was larger than "change a constant", which is the part worth
keeping. The constant-geometry comparison shape is derived from the map count -
it must be the largest shape whose *N* copies fit the aggregate - so raising N
retired 128x128, whose 64 copies overflow at 1,048,576. Every figure downstream
moved with it: the comparison is now 128x64, the arrangement's worst case falls
from 2,359,296 to 1,769,472, the measured total from 1,469,888 to 1,145,600, the
per-body window the prediction closes from 788 units to 609, and the "still fits"
threshold from sevenfold to fourteenfold. The probe and this document were
re-derived together rather than patched, and all six breakages re-run.

## The one failure mode this document has produced

Worth stating because it is the same every time, and because the next person to
edit this file will produce it again.

**Every finding across two review rounds - the review session's and the ones
caught here - was a sentence that was true when it was written and became false
when something beside it changed.** The cell-edit clause was correct M1 prose
that M2-3 falsified. "All of them" covered every map call there was when the
list was made. The probe phrasing was right until the equality replaced it in
one place and not the other. The admission rule and the staging row were each
right about the case their author had in mind. Not one was a reasoning error,
and reading harder would not have found them, because each is only wrong
relative to a sentence somewhere else.

So the freeze got a mechanical pass rather than another reading, and it has two
halves, because the first half alone missed something both sessions ran it over:

1. **Every number is located in every place that restates it, and the
   restatements are checked against each other.** Two disagreed - the admission
   rule against the staging row, and the equality against the evidence table -
   and both are fixed above.
2. **Every derived example is recomputed against the constraint it sits
   inside.** A worked example is not a restatement of anything; it appears once,
   matches nothing, and is wrong relative to a different number. Two failed. One
   illustrated the rejected admission rule with a game that cannot exist, since
   it held 524,289 cells against a 524,288 budget - and the corrected version is
   the stronger argument, because *one* cell elsewhere is enough to make the
   replacement refuse, not 262,145. The other quoted a double-charged install as
   "about 1,027" where the arithmetic gives 1,026.
3. **After a constant moves, scan for the constant itself in every form it
   takes** - the bare number, the shapes derived from it, and the ordinals that
   count it - and not only for the figures derived from it. When the map count
   rose from 32 to 64, the derived values (`2,359,296`, `1,469,888`, `788`) were
   all correctly quarantined, because a scan keyed on retired *values* finds
   them. The survivors were `32` itself and `128x128`, whose values did not
   change: 32 is still a number in this document and 128x128 is still a live
   `shapes` case.

   **The Budgets evidence row contradicted itself inside one table cell** - it
   opened "The 65th map refuses" and closed "across 32 identical 128x128 maps" -
   which is the sharpest form of why this clause is needed. The ordinal had been
   updated and the bare number had not, so a scan keyed on the *new* value lands
   on a correctly-updated fragment sitting immediately beside a stale one and
   reads as confirmation. That row has now kept a retired statement through two
   separate rounds, the first being "still fits", which makes it the one to check
   first.

4. **Every file the phase touched, not only the ones written in prose.** Four
   figures survived two passes over this document because they lived in the
   probe's comments, and nobody had pointed the method at the source. A probe is
   a document with a compiler: its comments carry the same load-bearing prose,
   with the added hazard of sitting beside code that *is* current, so they look
   maintained.
5. **Count repeated figures and phrases inside a section; a spike locates an
   insertion seam.** Editing this file produced a defect none of the clauses
   above could see: inserting a paragraph left the tail of the one it displaced
   spliced on the end, so "That" lost its antecedent, a clause appeared twice
   twenty lines apart differing only in tense, and the section ended on the
   temptation it had just spent a paragraph disowning. Every sentence was
   individually true; the arrangement inverted the point.

   The damage is a judgement - nothing counts that a paragraph ends on the claim
   its own argument disowns - but **the seam is mechanically locatable**, and
   that is how it was found: `8.5%` three times, `17.7` three times and "point
   of comparison" twice inside one 33-line window. A section restating one figure
   three times is either emphasis or a splice, and it costs ten seconds to learn
   which. Across the three commits the count of `8.5%` ran 4, 4, 3, which is the
   shape to look for.

   It is a locator and not a verdict, with legitimate false positives: a table
   row and the prose explaining it will share a figure, which is why lines 582
   and 599 both carry `8.5%` and should. Mechanical detection of the seam, human
   judgement on the damage - which is a narrower and more honest claim than
   either "no pass finds this" or a pretence that reading can be automated.

   All three clauses are the review session's, each from a second occurrence.

**Do both halves again after any edit that changes a number or a rule**, in
preference to re-reading the prose around them. The method, the second half, and
the impossible game are all the review session's; the arithmetic slip in the
example is theirs and travelled into this document verbatim through a message,
which is its own small lesson about quoting a number rather than recomputing it.

A first draft of this paragraph listed how many times each number appears, and
the counts were stale before the edit finished, because adding the paragraph
added occurrences. Recording a count here creates one more restatement to keep
in step, which is the failure mode this section is about - so the method is
written down and the tally deliberately is not.

A first draft of this paragraph listed how many times each number appears, and
the counts were stale before the edit finished, because adding the paragraph
added occurrences. Recording a count here creates one more restatement to keep
in step, which is the failure mode this section is about - so the method is
written down and the tally deliberately is not.
