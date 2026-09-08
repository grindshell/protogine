# M2 contract: simultaneous maps

**Status:** proposed, 2026-09-08, against `7d7c3ad`. **Nothing here is frozen and
nothing is implemented.** This document exists to be argued with before any code
depends on it, which is the one point in this plan where the claims are still
cheap to change. Every number below is a proposal awaiting the Phase 0 probe;
every rule is a proposal awaiting review. Where a decision is genuinely open it
is marked **[OPEN]** with the alternatives, rather than written as settled and
quietly revisited later.

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

Rules, all inherited from how entity handles already behave:

- A handle from another `Kernel` refuses, by `Rc::ptr_eq` on the session.
- A handle to a removed map refuses, by generation, including after its slot has
  been reused by a different map.
- `Kernel::stop` releases every map and invalidates every handle.
- Handles are opaque userdata in Luau, never numbers, so a script cannot forge
  one - the same reason `EntityHandle` is userdata.

The internal identity a component stores is `TileMapId { slot: u32, generation:
u32 }`: `Copy`, no `Rc`, because a component can only exist inside the kernel
that owns it, so the session is implied. The public handle carries the session
and is checked at the API boundary. This is the same split `ImageId` and
`EntityHandle` already make at different layers.

## M2-2. Membership

**A collider belongs to exactly one map, named when it is attached.** Membership
is a separate private component holding a `TileMapId`, inserted and removed
atomically with `TileCollider`.

Separate rather than a field on `TileCollider`, because `src/collision.rs`
depends on `crate::tilemap` and nothing else, and a slot index into a kernel
registry is not a geometry concept. Every predicate there already takes its map
as an argument, so the module needs no change at all. The cost is that "collider
without membership" becomes representable and the kernel must keep the pair in
step - the same obligation it already carries for the `colliders` counter.

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
> map's contents revalidates only its own members. Colliders on other maps are
> neither checked nor detached, and no operation on one map can refuse because
> of a body on another.

Everything else in T6 survives unchanged: replacement, cell edits and collider
attachment still preserve non-overlap or refuse atomically, and a refusal still
leaves the old map, cell, collider and position exactly as they were. M4 will
need a further revision for regions retired under a live body; this one does not
anticipate it.

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
explicit handle, and two are renamed because their meaning changed:

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
| `tile_collider(e)` | `tile_collider(e)`, now returning the map alongside the box |

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
| Maps per session | 32 | Enough for a screen of rooms plus staging; small enough that a linear scan over maps is never a cost worth optimising |
| Aggregate live cells | 524,288 | 1 MiB of cell storage, twice M1's single-map allowance. Deliberately below `MAX_CALLBACK_WORK`, so a session can install its entire map budget inside one callback rather than being forced to spread installs across ticks |
| Per-map dimensions and cells | unchanged: 1..1,024 per axis, at most 262,144 cells | A single map is no larger than M1's |
| Staging | at most one candidate map in flight | Peak storage is the aggregate plus one map: about 1.5 MiB |
| Live colliders | unchanged: 1,024 across all maps | The limit that bounds the fixed pass is global, so it stays global |
| Fixed-pass work | unchanged: 16,777,216 | The derivation is per body against its own map, and per-map dimensions are unchanged, so 1,024 bodies x 9 cells x 1,280 faces = 11,796,480 still fits. The probe must re-verify this with bodies spread across many maps rather than assume the arithmetic transfers |
| Callback work | unchanged: 1,048,576 | Now shared across every map a callback touches |
| Region output | unchanged: 4,096 per call, 262,144 per callback | Now aggregate across maps |

Solid-flag storage is at most 32 x 1,024 flags. Map count and aggregate cells are
both checked before allocation, and admission is charged against the aggregate
before a candidate is built, not after.

## M2-8. Failure classification

Unchanged from M1's split, extended to the new refusals. Catchable through
`pcall`: schema, geometry, bounds, missing or stale map handle, foreign handle,
bad entity handle, wrong phase, illegal placement and illegal transfer. Latching
outside `pcall`: the map count, the aggregate cell budget, the collider limit,
callback tile work and region output.

A new `KernelError::InvalidTileMap` covers stale, foreign and removed handles,
distinct from `NoTileMap`, which M2 retires along with the implicit map.

## Phases

Mirroring M1's shape, because the review pair and the harness conventions are
built around it.

| Phase | Work | Exit evidence |
| --- | --- | --- |
| 0. Contract and feasibility | This document, reviewed and frozen. A probe measuring aggregate storage and per-tick cost at the maximum configuration, and re-deriving the fixed-pass bound with bodies spread across maps | Contract frozen with no open item; probe receipts; every proposed number either confirmed or revised with its measurement |
| 1. Kernel map registry | Slot table, handles, generations, create/replace/remove/info, `stop` release, storage and count accounting | Foreign, stale and reused-slot handles refuse; removal and replacement are map-local; storage accounting agrees with the budget; core configuration |
| 2. Membership, transfer and the fixed pass | Membership component, atomic transfer, per-map sweeping, the M2-R1 revision on every mutation | Two overlapping maps give independent collision; transfer is atomic in both directions; removing one map preserves unrelated maps and bodies; insertion-order independence survives |
| 3. Luau migration | The map-addressed calls, handle refusals, shared budgets | Every refusal reaches Lua unchanged; aggregate ceilings latch; schema battery over the new placement shape |
| 4. Sample and release proof | Migrate the sprite sample to a named map, add a two-map fixture, update docs and checks | Sample behaviour unchanged again; two-map evidence headlessly and in a copied Player; M2 completion recorded |

## Required behavioural evidence

| Area | Cases |
| --- | --- |
| Identity | Foreign handle; handle to a removed map; handle to a reused slot; handle after `stop`; two live maps with distinct handles that are never confused |
| Independence | Two maps with overlapping coordinate ranges and different walls; a body on each; each stops at its own wall and neither sees the other's |
| Lifecycle | Removing a map with members refuses; removing one without members succeeds while unrelated bodies keep moving; replacing one map's contents revalidates only its members |
| Transfer | Between overlapping maps; between disjoint maps, which needs the position; refused for an illegal destination box, an illegal extent against a smaller tile size, and a stale destination handle - each leaving membership, geometry and position untouched |
| Budgets | The 33rd map refuses; the aggregate cell budget refuses before allocation; a refused admission leaves the count and storage unchanged; the fixed pass still fits with bodies spread across the maximum map count |
| Migration | Every renamed call refuses its M1 argument shape rather than guessing |

Required negative controls, in the harness style the earlier phases established:
membership read from position rather than from the component must fail the
overlapping-maps fixture; a generation check removed must fail the reused-slot
fixture; transfer that writes membership before validating placement must fail
the atomicity fixture; and removal that scans all colliders rather than the
map's own must fail the unrelated-bodies fixture.

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
4. **Whether 32 maps is a limit anyone will feel**, and whether the map count
   needs to be separate from the aggregate cell budget at all, given the cell
   budget already bounds storage.
5. **Whether `tile_collider` returning the map alongside the box is the right
   shape**, or whether membership deserves its own read.
