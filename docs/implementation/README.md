# Implementation records

Accepted decisions, phase contracts, delivery evidence and deferred scope live
here. The [root TODO](../../TODO.md) owns the remaining feature backlog.
The [authoring README](../../README.md)
describes current behavior; [AGENTS](../../AGENTS.md) owns contribution rules and
[DEVELOPMENT](../DEVELOPMENT.md) owns current verification commands.

## Planned

| Plan | Status |
| --- | --- |
| [Engine-owned tilemaps and solid-tile collision](../../TILEMAP_COLLISION_PLAN.md) | T1-T8 accepted with scope clarifications; M1-M4 required. M1 Phases 0-2 complete, Phases 3-4 unstarted |

Active plans live at the repository root and move here when complete. A closed
phase of an active plan is filed under Completed below, as the PNG/sprite Phase 0
record already is, because later phases depend on the contracts it froze.

## Completed

| Record | Delivered scope |
| --- | --- |
| [ADR-001: Scripting and C API](SCRIPTING_C_API_PLAN.md) | Luau host, data/I/O, kernel/input, scripted Player and trusted C batch plugins; Phases 0-5 including 1a, Windows MSVC x64 |
| [ADR-002: PNG assets and sprites](PNG_SPRITE_PLAN.md) | Staged bundle PNG service, Luau handles/eviction, shared renderer and authoring sample; Phases 0-4 |
| [PNG/sprite Phase 0](PNG_SPRITE_PHASE0.md) | Frozen initial contracts and dependency/worker/GPU feasibility receipts supporting ADR-002 |
| [Tilemap/collision Phase 0](TILEMAP_COLLISION_PHASE0.md) | Frozen M1 contracts plus numerical and work/storage feasibility receipts; the rest of that plan is still active |

Current contract summaries incorporate later corrections. Dated records retain
historical versions, commands, test counts and measurements; later records close
or supersede earlier phase gaps. They do not imply tests were rerun during a doc
edit. Paths/commands assume the repository root unless noted; historical commands
may reference retired fixtures such as `examples/games/loading/`, now replaced
by `examples/games/sprites/`.

When changing behavior, update the authoring contract and relevant decision
record together. Keep commands in DEVELOPMENT; record only actual execution
evidence in completion entries. Preserve artifact hashes, negative controls,
platform limits and unresolved decisions when pruning.
