---
name: xindeler-abilities
description: Use when creating spells, combat abilities, buffs, auras, or new magic schools — guides the RON ability pipeline, CharacterState implementation, and the magic spec
---

# xindeler-abilities

**REQUIRED:** Read `docs/design/specs/2026-06-13-magic-system-v2-design.md`, then the
owning mechanic spec (`2026-06-24-spell-riders-engine-design.md`,
`2026-06-25-combat-resolution-design.md`, or the relevant school spec). Invoke
`xindeler-dev` and `superpowers:test-driven-development` before coding.

## The ability pipeline (end to end)

```
RON asset (assets/common/abilities/<school>/<spell>.ron)
  → ability_set_manifest.ron (maps AbilitySpec::Tool(ToolKind)/Custom(String) → AbilitySet)
  → AbilityMap resource (common/src/comp/inventory/item/tool.rs)
  → CharacterAbility variant (common/src/comp/ability.rs)
  → CharacterState (common/src/states/<state>.rs)
  → voxygen FX (scene/particle.rs, audio/sfx/mod.rs, outcomes)
```

A new *spell* using existing machinery = RON file + manifest entry + (optional) skill gate.
A new *mechanic* = new CharacterAbility variant + new state file + registration in
`character_state` handling + FX wiring. Inspect the current primitives and deferred-rider
inventory before writing Rust—many effects are already expressible.

## Current architecture

- Existing variants cover: nukes, beams, AoE, auras (`BasicAura`/`StaticAura`), self-buffs,
  heals, `BasicSummon`, `Blink`, `Transform` (polymorph), `SpriteSummon` (walls/terrain),
  `Explosion`. Projectiles already apply status (`ProjectileConstructor.buff`,
  `common/src/comp/projectile.rs:141`).
- **Cooldowns exist:** `AbilityCooldowns` stores per-ability readiness;
  `AbilityMeta.cooldown` is gated in `common/src/states/utils.rs`. Reuse it—
  `charge_duration` remains wind-up, not cooldown.
- Implemented rider primitives include `Terrified`, `Charmed`, `Anchored`, `Asleep`,
  `Blinded`, `DifficultTerrain`, and `Antimagic`. Check `BuffKind` and
  `docs/design/specs/2026-06-25-content-combat-mapping.md` before adding another condition.
- Ability slots: guard/primary/secondary/auxiliary + `movement` (`MovementAbility::Species`).
  Auxiliary sets key on equipped tools (`AuxiliaryKey = (Option<ToolKind>, Option<ToolKind>)`)
  — weaponless class abilities use `AbilityPool` + `AuxiliaryAbility::Innate`; do not
  add a second innate-slot mechanism.
- Skill-gating: `AbilityKind::Simple(Option<Skill>, T)` — gate spells on class-tree skills.
- `asset_tweak` feature exists (`common/assets/Cargo.toml:29`) — use it for live balance
  iteration instead of recompiling.

## Steps for a new spell (content-only)

1. Pick the school + class from the spec's spell table; confirm the `CharacterAbility`
   variant it maps to.
2. Copy the closest existing RON under `assets/common/abilities/` (e.g. staff fireball for
   a nuke), adjust numbers per the balance table.
3. Register in `assets/common/abilities/ability_set_manifest.ron` under the right
   `AbilitySpec`, with its `Skill` gate.
4. `VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-common` — asset-loading tests
   (e.g. `AbilityMap` load) must pass; a typo'd RON fails here, not at runtime.
5. In-game check via `xindeler-run` (hot-reloading picks up RON edits in dev).

## Steps for a new mechanic (Rust)

1. New variant in `common/src/comp/ability.rs` + state in `common/src/states/<name>.rs`
   (4-stage pattern: buildup → action → recover, see the spec's `GroundAoe` sketch).
2. Wire FX: outcome or particle mapping (shockwave pattern at
   `voxygen/src/scene/particle.rs:3604` is the reference).
3. Exhaustive matches: let `cargo check --workspace --all-targets` find every site; never
   add wildcard arms.
4. Unit-test state transitions; then content steps above.

## Rules

- Server authority on all effects; FX are client-side only.
- New BuffKinds need: variant, stacking/decay rules, icon asset, i18n string (`.ftl`).
- Balance numbers from `game-balance-designer` tables; spell names/flavor from
  `lore-writer` (canon check against `docs/design/lore/`).
