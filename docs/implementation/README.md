# Implementation plans

This directory contains accepted and completed implementation plans. Each plan
records its status, design decisions, phase contracts, verification evidence,
and deferred work.

Commands and plain-text repository paths in these plans assume the repository
root unless noted otherwise.

## In progress

No plan is currently in progress.

## Completed

- [PNG_SPRITE_PLAN.md](PNG_SPRITE_PLAN.md) — Completed staged bundle-local PNG
  loading, sprite drawing, headless asset support, shared rendering, and
  script-controlled eviction. [Phase 0](PNG_SPRITE_PHASE0.md) froze the contracts
  with CPU/GPU evidence; Phase 1 delivered the headless asset service, Phase 2
  its Luau and runtime integration, Phase 3 the shared renderer and Player
  integration, and Phase 4 the authoring sample and delivery evidence.
- [SCRIPTING_C_API_PLAN.md](SCRIPTING_C_API_PLAN.md) — Completed Luau scripting and
  native C plugin milestone for Windows MSVC x64: Phases 0–5, including Phase 1a,
  with one post-completion amendment.
