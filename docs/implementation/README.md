# Implementation plans

This directory contains accepted and completed implementation plans. Each plan
records its status, design decisions, phase contracts, verification evidence,
and deferred work.

Commands and plain-text repository paths in these plans assume the repository
root unless noted otherwise.

## In progress

- [PNG_SPRITE_PLAN.md](PNG_SPRITE_PLAN.md) — Staged bundle-local PNG loading,
  sprite drawing, headless asset support, shared rendering, and script-controlled
  eviction. P1-P8 accepted; [Phase 0](PNG_SPRITE_PHASE0.md) complete with contracts
  and CPU/GPU evidence. Phase 1 delivered the headless asset service and Phase 2
  its Luau and runtime integration. Phases 3-4 have not started.

## Completed

- [SCRIPTING_C_API_PLAN.md](SCRIPTING_C_API_PLAN.md) — Completed Luau scripting and
  native C plugin milestone for Windows MSVC x64: Phases 0–5, including Phase 1a.
