---
name: xindeler-aurora
description: Use when implementing AURORA social-simulation features — NPC relationships, memories, families, organizations, economy, dynamic quests — knows the rtsim architecture and the AURORA spec
---

# xindeler-aurora

**REQUIRED:** Read `docs/design/specs/2026-06-10-project-aurora-design.md` (the
GDD/TDD) and locate which AURORA phase your change belongs to. Invoke `xindeler-dev` for
ECS patterns and `superpowers:test-driven-development` before coding. For rtsim-heavy
work, dispatch the `sim-systems-engineer` agent.

## rtsim map (where AURORA lives)

| Concern | Where |
|---|---|
| NPC state (personality Big Five, sentiments, home, faction, job) | `rtsim/src/data/npc.rs`, `common/src/rtsim.rs` (Profession at `:485`) |
| Relationships/opinions | `rtsim/src/data/sentiment.rs` (extend per spec: typed edges) |
| Factions → Organizations | `rtsim/src/data/faction.rs` (spec defines the Organization supersede path) |
| Quests (Escort/Slay/Courier + new templates) | `rtsim/src/data/quest.rs` |
| Dialogue | `common/src/rtsim.rs` (DialogueKind), `rtsim/src/rule/npc_ai/dialogue.rs` |
| Population director | `rtsim/src/rule/architect.rs` |
| Behavior composition | `rtsim/src/ai/` Action combinators + `rtsim/src/rule/npc_ai/` |
| Persistence | `rtsim/src/data/mod.rs` (`Data::write_to`/`from_reader`) |
| Server bridge (load NPCs into ECS near players) | `server/src/rtsim/` |
| Site economy | `world/src/site/economy/` |

## Non-negotiable constraints (from the spec)

1. **No LLM calls in the tick path.** LLM output is generated async/batched (dialogue
   color, charters, flavor) with template fallbacks; tick decisions are utility-AI/rules.
2. **Per-NPC memory budget** — respect the spec's persisted-bytes budget per NPC; every
   new persisted field must state its cost at 10k NPCs in the PR description.
3. **Save compatibility:** every new rtsim `Data` field gets `#[serde(default)]` and a
   load test against a pre-change save file. Breaking the rtsim save format requires a
   versioned migration (spec §persistence).
4. **Simulation LOD:** full simulation only near players; far NPCs run statistical
   updates. New systems must define both modes or justify why one suffices.
5. **Determinism where possible:** seedable RNG via the NPC/world seed; soak tests diff
   two runs.
6. ORACLE integration: AURORA *reacts to* world facts published by ORACLE; it never
   creates world-level events itself (wars, disasters). If your feature wants one,
   it belongs in the ORACLE spec.

## Verification

- Unit + sim tests in `rtsim` crate; long-run soak via headless `veloren-server-cli` —
  watch rtsim tick time in telemetry (`xindeler-telemetry` skill).
- Dispatch `rust-perf-reviewer` on any change inside the rtsim tick.
