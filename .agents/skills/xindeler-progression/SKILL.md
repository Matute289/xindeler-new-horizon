---
name: xindeler-progression
description: Use when implementing character levels, classes, races, XP curves, skill trees, or equipment requirements — knows the SkillSet/persistence architecture and the progression specs
---

# xindeler-progression

**REQUIRED:** Read the relevant design spec before coding. Invoke `xindeler-dev` for general gameplay-change patterns and `superpowers:test-driven-development` before writing code.

## Specs (read the one that owns your change)

| Topic | Spec |
|---|---|
| Character level (derived, 1–60) | `docs/design/specs/2026-06-10-character-levels-design.md` |
| Classes + races | `docs/design/specs/2026-06-10-classes-races-design.md` |
| Item requirements (class/level/race) | `docs/design/specs/2026-06-10-equipment-restrictions-design.md` |
| Zone/mob levels, XP differential | `docs/design/specs/2026-06-10-world-difficulty-zones-design.md` |
| Program order & dependencies | `docs/design/specs/2026-06-10-rpg-evolution-master-roadmap.md` |

## Architecture facts (verified 2026-06-10)

- **XP/skills:** `SkillSet` in `common/src/comp/skillset/mod.rs` — `skill_groups: HashMap<SkillGroupKind, SkillGroup>` with persisted `earned_exp`. `SkillGroupKind` lives in `skillset/mod.rs:89` (NOT skills.rs). XP is awarded in `handle_exp_gain`, `server/src/events/entity_manipulation.rs:505`, split across General + equipped-weapon pools; reward size is combat-rating-based (~`:1122`).
- **Character level is DERIVED** from `SkillSet::total_earned_exp()` — never persist it, never cache it in a component without a sync story. Curve constants live next to `level_from_total_exp` in `skillset/mod.rs`.
- **Species:** `Danari, Dwarf, Elf, Human, Orc, Draugr` (`common/src/comp/body/humanoid.rs:116`). There is no Undead/Demon species.
- **DB migrations:** refinery SQL files in `server/src/migrations/` (`V<N>__<name>.sql`, sequential). `skill_group_to_db_string` in `server/src/persistence/json_models.rs:71` PANICS on unknown `SkillGroupKind`s — adding a kind without extending both to/from db-string converters corrupts saves.
- **Equip validation path:** `server/src/events/inventory_manip.rs:535/798` → `Inventory::equip/swap` (`common/src/comp/inventory/mod.rs`) → `loadout.rs:384` → `slot.rs:111`. `EquipSlot::can_hold` only sees `ItemKind` — requirement checks must happen at the inventory_manip layer where the full entity is available.
- **Stats temp modifiers reset every tick** (`Stats::reset_temp_modifiers`, `common/src/comp/stats.rs:147`) — they are for buffs. Permanent per-level/per-class scaling needs its own mechanism (see classes spec).
- **Char creation flow:** `voxygen/src/menu/char_selection/` → `ClientGeneral::CreateCharacter` → `server/src/character_creator.rs` (starter whitelist `VALID_STARTER_ITEMS` at `:11`).

## Rules

1. **Server authority.** Every gate (equip, ability, level) is enforced server-side; client UI is advisory (gray-out + tooltip).
2. **Additive changes only** on upstream-owned files — new fields get `#[serde(default)]`, new RON fields optional; keeps upstream merges cheap.
3. **No DB migration without a rollback note** in the PR description, and never edit an already-applied migration file.
4. **Balance numbers** come from the `game-balance-designer` agent's tables, not ad-hoc constants. Tag new tunables with a comment pointing at the owning spec.
5. After implementing, dispatch `ecs-design-reviewer` and run `xindeler-review` before merge.

## Test checklist

- Unit tests in the owning crate (`VELOREN_ASSETS="$(pwd)/assets" cargo test -p veloren-common`).
- If persistence touched: round-trip test via `json_models` converters + fresh-character creation on a scratch DB.
- In-game smoke via `xindeler-run`; session analysis via `xindeler-telemetry`.
