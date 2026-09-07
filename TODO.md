# Remaining features

Current backlog, reconciled with the implementation records on 2026-09-07.
Feature checkboxes track unimplemented scope; verification gaps are listed
separately below. Tilemaps/collision has an active plan with accepted directions
and staged core delivery; the other groups have no committed order or design.
Completed work and historical evidence live in [implementation records](docs/implementation/README.md).

## Active feature: tilemaps and collision

- [ ] **Engine-owned tilemaps and solid-tile collision.** Move authoritative
  grid state and collision-aware movement into the shared kernel, with Luau
  authoring APIs and a migrated sprite sample. The overall feature stays open
  until all four required milestones below are delivered. See the
  [tilemap/collision plan](TILEMAP_COLLISION_PLAN.md); T1-T8 are accepted with
  scope clarifications, and no engine behavior is implemented yet.

- [ ] **M1: Single-map tilemaps and solid-tile collision.** The next delivery:
  one kernel map, optional hecs colliders, swept movement and sample migration.
  Phases 0 and 1 are complete: the
  [Phase 0 record](docs/implementation/TILEMAP_COLLISION_PHASE0.md) freezes the
  API/schema, numerical and refusal contracts with feasibility receipts, and the
  kernel now owns a checked map with bounded reads and cell edits. Phase 2
  (colliders and fixed systems) is the next unmet gate; no collision behavior
  exists yet.
- [ ] **M2: Simultaneous maps.** Independent live maps, entity membership,
  transfers and map-local lifecycle, including the required T6 revision.
- [ ] **M3: Independent layers.** Separate tile layers with explicit editing,
  visual order, collision participation and lifecycle.
- [ ] **M4: Streaming.** Incremental region loading/retirement with bounded
  residency/work and defined behavior for colliders, unavailable data and edits.

M2-M4 are required core features, not optional deferred extensions. Their
detailed contracts and limits must be frozen before their implementation. M1
completion does not complete the feature or archive its plan.

## Core product work

- [ ] **Audio through Kira 0.12.4.** Shared runtime service and game-facing APIs;
  keep the kernel usable without an audio device.
- [ ] **Editor and tile-map authoring.** Authoring UI, inspection, map editing
  and playtesting through the same runtime/content contracts as the Player.
- [ ] **Export and packaging tooling.** Assemble runnable game distributions;
  archive bundle formats remain a separate design choice. Loose source/PNG/DLL
  bundles already run when copied beside the Player.
- [ ] **Player writable-data directory selection.** Give shipped scripts an
  explicit data root. Rooted filesystem/data utilities already exist; the
  current Player grants bundle reads only.

Sources: [project boundaries](AGENTS.md#architecture-boundaries),
[scripting deferred scope](docs/implementation/SCRIPTING_C_API_PLAN.md#compatibility-and-verification),
[Player persistence limitation](README.md#script-data-and-filesystem-utilities).

## Deferred rendering and asset extensions

These were excluded from the PNG/sprite milestone; listing them is not a
commitment to implement every extension.

- [ ] Cameras and world-to-screen transforms.
- [ ] Sprite rotation and pivots.
- [ ] Engine animation components; frame selection currently belongs to Luau.
- [ ] Asset hot reload.
- [ ] Texture packing/atlasing, with a new resource-lifetime contract.
- [ ] Image formats beyond PNG.
- [ ] Direct pixel-buffer access.
- [ ] GPU-only eviction; logical image unload already exists.
- [ ] A dedicated script loading-cancellation API; unload already cancels the
  corresponding pending job.

Sources: [PNG/sprite scope](docs/implementation/PNG_SPRITE_PLAN.md#outcome-and-scope)
and [asset extensions](docs/implementation/PNG_SPRITE_PLAN.md#image-access-eviction-and-cancellation-boundaries).

## Deferred scripting and native extensions

These require separate lifecycle, scheduling or ABI decisions before work.

- [ ] Entity-attached scripts and their lifecycle/execution order.
- [ ] Asynchronous script/native work.
- [ ] Custom plugin components and explicit ECS query/view APIs.
- [ ] Script/native hot reload beyond a complete runtime restart.
- [ ] Embedding the whole engine through an external API.
- [ ] Zero-copy native buffers, if end-to-end measurements justify them.
- [ ] Engine mutation command buffers and their ordering/visibility rules.

Source: [scripting/native deferred scope](docs/implementation/SCRIPTING_C_API_PLAN.md#compatibility-and-verification).
Bytecode distribution and automatic DLL discovery are unselected alternatives
in [D5](docs/implementation/SCRIPTING_C_API_PLAN.md#accepted-decisions), not agreed backlog deliverables.

## Separate verification follow-ups

These are coverage/platform gaps, not missing implementations of the features above.

- [ ] Verify additional native targets; ABI 1 is currently verified only on
  Windows MSVC x64.
- [ ] Deterministically exercise native scratch-allocation refusal.
- [ ] Add command and rendered-pixel evidence for fractional sprite tint alpha.
- [ ] Establish rendering evidence on additional graphics stacks; cross-GPU
  pixel equality is not currently established.

Sources: [native verification limits](docs/implementation/SCRIPTING_C_API_PLAN.md#post-completion-review-contract-and-coverage-clarifications)
and [PNG/sprite standing gaps](docs/implementation/PNG_SPRITE_PLAN.md#unresolved-gaps-1).
The sample's fixed 960x600 layout and driver memory-reclamation limits remain
documented constraints, not promises of responsive layout or driver guarantees.

## Completed scope and deliberate boundaries

The Luau/data/filesystem/kernel/input/Player/native-plugin milestone and the
staged PNG/sprite/shared-renderer milestone are complete. Older phase entries
can describe gaps subsequently closed; use their current status summaries.

Save schemas, slots, save timing, restoration and migrations belong to games.
Automatic ECS/VM serialization and engine-managed saves are not pending engine
features. RPG Maker/Godot parity and general-purpose physics are not implied.
