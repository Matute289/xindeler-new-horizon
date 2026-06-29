---
name: game-architecture
description: Use when deciding where game content, data, code, or documentation should live; when adding or tuning game content (lore, items, abilities, dialogue, balance numbers, NPCs, loot, sites); or when judging whether something belongs in engine code vs a data file vs the design repo. Triggers on "hardcode", "where should this go", new asset/config, or coupling between code and design docs.
---

# Game Architecture (data-driven + clean)

## Overview

We are building a **game**, so the default is **data-driven design**: the Rust code is a
*generic engine*; all game **content** lives in **data**, loaded at runtime. Keep the layers
clean — content → data → loaders → systems → presentation — with dependencies pointing one
way, and **never couple shipped engine code to the private design repo.**

## The core rule — data-driven game development

Game **content** — lore, items, recipes, abilities, dialogue, balance numbers, NPC/site/loot
definitions — lives in **data files** (RON under `assets/`), **not** hardcoded in Rust. The
code loads and interprets that data. This lets writers and designers iterate **without
recompiling**, and is exactly how this engine already works (RON configs + the `Asset`/`Ron`
loader — see `common/src/recipe.rs`, `common/src/comp/spell.rs`).

- Adding content? → a data file in `assets/`, never a Rust string/struct literal.
- Tuning a number? → data, not a `const` in code.
- *"Generic engine, domain-specific data"* (Jason Gregory, *Game Engine Architecture*).

## Where things go

| Thing | Home | Why |
|---|---|---|
| Game content (the actual lore / items / dialogue) | `assets/` (RON) — and its markdown **source** in the private design repo | content ≠ code |
| Code that loads/interprets data | engine crates, idiomatic next to its type (`recipe.rs`, `spell.rs`) | generic engine |
| Design docs (specs, plans, lore canon markdown) | the **private** design repo only | not shipped; may hold copyright-mapping notes |
| Machine-readable index of canon | a generated `assets/*.ron` data file | bridges private docs → game, by id |

## Clean-architecture rules (layering)

- **Dependencies point inward/downward:** presentation → systems → domain types → data. Never up.
- **Never couple shipped/public engine code to the private design repo.** The engine may depend
  only on shipped `assets/` data — never on a `docs/…` design-repo path. (A loader whose test
  reached into the private lore tree was a layer violation; the fix is to depend on the generated
  `assets/` data, or to not ship the loader until a system consumes it.)
- **YAGNI:** add a loader/abstraction only when a system actually consumes it. Don't ship unused
  infrastructure in a shared crate.
- **Externalize, don't hardcode:** if a designer or writer would ever want to change it, it's data.

## Red flags — STOP and reconsider

- A line of lore/dialogue, an item stat, or a balance number written as a **Rust literal** → make it data.
- **Shipped engine code referencing `docs/…`** (the private design repo) → coupling violation.
- A new "manager"/loader/index in `common` that **nothing calls yet** → premature; defer it.
- A `match`/`if` in code that encodes *which content exists* (rather than loading it) → data table.

## Sources

Jason Gregory, *Game Engine Architecture* (data-driven design — separate generic engine from
domain data); [data-driven design in game dev](https://dev.to/methodox/data-driven-design-leveraging-lessons-from-game-development-in-everyday-software-5512);
[clean architecture in games](https://betterprogramming.pub/clean-architecture-in-game-development-e57542a96e5e),
[cleangamearchitecture.com](https://cleangamearchitecture.com/architecture-model/).

For the automated coupling check, see `game-architecture-reviewer` (subagent) and
`scripts/check-no-design-repo-coupling.sh`.
