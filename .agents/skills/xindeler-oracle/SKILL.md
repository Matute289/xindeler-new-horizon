---
name: xindeler-oracle
description: Use when implementing ORACLE world-director features — world events, story arcs, monster ecosystem, climate/astronomy, narrative, player legacy — knows the world-state/event-engine architecture and the ORACLE spec
---

# xindeler-oracle

**REQUIRED:** Read `docs/design/specs/2026-06-10-project-oracle-design.md` (the GDD/TDD)
**and `docs/design/specs/2026-06-24-oracle-design-addendum.md`** (gap-closure: politics/diplomacy, macro-economy +
ORACLE↔AURORA seam, quest-opportunity generator, perception/intake pipeline, causal-graph + history
API, religion/culture spread, event-scale, distributed track, revised 8↔11 phase map) and identify the
phase your change belongs to. Invoke `xindeler-dev` and `superpowers:test-driven-development` before
coding. For sim internals, dispatch the `sim-systems-engineer` agent (implement) and **`sim-design-reviewer`** (design/diff review).

## Division of labor (do not blur it)

**ORACLE decides WHAT happens in the world** (events, arcs, ecosystem shifts, climate);
**AURORA decides how inhabitants react** (`docs/design/specs/2026-06-10-project-aurora-design.md`).
ORACLE publishes typed world facts; AURORA consumes them. If you're writing per-NPC
reaction logic, you're in the wrong spec — use `xindeler-aurora`.

## Architecture anchors

| Concern | Where |
|---|---|
| World state it observes/extends | `rtsim/src/data/mod.rs` (Nature, Sites, Factions, Reports, TimeOfDay…) |
| Rules/tick model to follow | `rtsim/src/rule/` (architect.rs is the reference "director" rule) |
| Event lifecycle (Proposed→Validated→Active→Resolved→Consequences) | per spec §Event Engine — implement as data, not code branches |
| Monster populations & respawn | `rtsim/src/rule/architect.rs` (extend toward ecosystem model) |
| Weather/climate hooks | `server/src/weather/` (verify current state before extending) |
| Calendar/time | `common/src/calendar.rs`, TimeOfDay in rtsim Data |
| Sky/astronomy rendering | voxygen sky pipeline (`voxygen/src/render/`, `voxygen/src/scene/`) |
| Telemetry feed for observation | fork's `telemetry!` macro + `common/frontend/` sinks |

## Non-negotiable constraints (from the spec)

1. **LLM proposes, rules validate, sim executes.** No LLM output reaches game state
   without passing the validation layer (preconditions, anti-chaos invariants).
2. **Anti-chaos invariants are code, not guidelines:** faction-dominance caps, economic
   circuit breakers, event-density caps, narrative kill-switch admin command. New event
   types must declare which invariants bound them.
3. **Every event is auditable:** chronicle entry with cause chain; telemetry event on each
   lifecycle transition. If you can't explain an event from logs, it doesn't merge.
4. **Dynamic dungeons:** terrain is seed-deterministic — follow the spec's chosen
   mechanism (dormant-site activation / instanced pocket planes first); never mutate
   generated chunks ad hoc.
5. **Server restart:** define catch-up behavior for any time-dependent system
   (accelerated offline sim per spec §Time).

## Verification

- Event-injection harness via admin commands (`common/src/cmd.rs` + `server/src/cmd.rs`)
  for every new event type — manual triggering must always work.
- Headless soak with event telemetry dashboards; chronicle audit after each soak.
- Dispatch `rust-perf-reviewer` for tick-path changes; `game-balance-designer` for
  ecosystem/economy parameters.
