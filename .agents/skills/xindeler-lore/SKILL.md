---
name: xindeler-lore
description: Use when writing lore content, naming deities/places/NPCs/items, or surfacing lore in game assets — enforces canon from docs/design/lore and the original-IP rules
---

# xindeler-lore

**REQUIRED:** Read `docs/design/lore/00-cosmology.md`,
`docs/design/lore/70-style-guide.md`, and the relevant canon leaves before writing.
Canon lives in `docs/design/lore/`; the machine-readable index is
`assets/lore/index.ron`. Treat older cosmology specs as history when they conflict with
the current canon leaves or denylist.

## IP rules (hard requirements)

1. **Everything original.** No names, deities, places, or phrases from WotC/D&D,
   Critical Role/Exandria, or any published setting.
2. Lovecraft-INSPIRED is fine; Lovecraft names are not (no Cthulhu/Azathoth/etc. even
   where public domain — we maintain our own outer-gods canon).
3. The spec carries a forbidden-names denylist; check new names against it AND grep
   `docs/design/lore/` for collisions before introducing them.

## Canon workflow

1. Before writing: read `docs/design/lore/00-cosmology.md` and the section your content touches
   (pantheon/fiends/outer-gods/planes/history).
2. New canon entities (deity, fiend, plane, era, major NPC) require: a `docs/design/lore/` entry,
   an `assets/lore/index.ron` id, and a naming-style check against
   `docs/design/lore/70-style-guide.md` (per-culture phonology).
3. Reference existing entities by their index id in code/data, never by retyping names.
4. Existing Veloren content (Dagon/Dagonite, Mindflayer, Cultists, vampire castles,
   chapel sites) is **retconned in**, not replaced — the history spec maps them to eras.

## In-game delivery vectors (verified)

| Vector | Where | Notes |
|---|---|---|
| i18n strings | `assets/voxygen/i18n/<lang>/*.ftl` | Fluent format, NOT RON. Keep en complete; other langs can lag |
| NPC dialogue | `rtsim/src/rule/npc_ai/dialogue.rs` | Topic-based; personality-toned |
| Site/dungeon naming | `world/src/site/` + name gen | Use per-culture phonology tables |
| Readable books/scrolls | **GAP** — no `ItemKind` for readables yet | Spec Phase 2 adds it; don't fake it with tooltips |
| Religions/orgs | AURORA spec consumes pantheon ids | `docs/design/specs/2026-06-10-project-aurora-design.md` |
| World events flavor | ORACLE spec consumes lore arcs | `docs/design/specs/2026-06-10-project-oracle-design.md` |

## Quality bar

- Every lore text names: who wrote it in-world (voice), which era, and which canon ids it
  references — drift is caught by the canon-lint described in the spec.
- Use the `lore-writer` agent for bulk drafting; human-curate everything before commit.
- Tone: grounded high fantasy; cosmic-horror content implies, never explains.
