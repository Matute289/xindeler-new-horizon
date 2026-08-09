---
name: xindeler-worldgen
description: Use when iterating on world generation, terrain, sites, rtsim civilization simulation, or procedural content — knows the generation pipeline and iteration workflow
---

# xindeler-worldgen

## Generation Pipeline (in order)

Changes cascade downstream — a change to `sim/` affects everything below it.

```
world/src/sim/       ← WorldSim: erosion, rivers, rainfall, biomes, caves
      ↓
world/src/civ/       ← WorldCiv: settlements, trade routes, factions
      ↓
world/src/site/      ← Individual sites: towns, dungeons, castles, bridges
      ↓
world/src/layer/     ← Detail layer: trees, grass, sprites, surface features
      ↓
world/src/column.rs  ← Per-column terrain (most terrain detail lives here)
      ↓
rtsim/               ← Long-running simulation: NPC migration, economy, conflicts
```

## Iteration Workflow

```bash
# 1. Make your change
# 2. Fast compile check (5-10s):
source "$HOME/.cargo/env"
cargo check -p xindeler-world    # for world/
cargo check -p xindeler-rtsim    # for rtsim/

# 3. Start the server (worldgen runs on startup):
cargo server

# 4. Connect with the client and fly to the affected area:
cargo run --bin xindeler-voxygen
# In game: use airship or /tp <x> <y> <z>

# 5. Observe, take note, kill server (Ctrl+C), tweak, repeat
```

## Key Files by Area

| Area | File | Notes |
|------|------|-------|
| High-level world params | `world/src/config.rs` | Struct `WorldConfig`, loaded from assets |
| Terrain column generation | `world/src/column.rs` | most biome/terrain logic |
| World simulation | `world/src/sim/mod.rs` | Erosion, rivers, altitude |
| Civilization | `world/src/civ/mod.rs` | Settlement placement, trade |
| Site plot types | `world/src/site/plot/` | All site types: towns, dungeons, castles, bridges, etc. |
| Sprite/tree placement | `world/src/layer/` | Surface detail (scatter.rs, tree.rs, shrub.rs, etc.) |
| Rtsim core | `rtsim/src/lib.rs` | Event/Rule system design |
| Rtsim NPC data | `rtsim/src/data/npc.rs` | NPC state |
| Rtsim AI decisions | `rtsim/src/ai/` | NPC behavior |

## World-Gen Math Profile

World generation uses heavy floating-point math with large numbers. To avoid overflow panics in dev mode (which has overflow checks on), use the `no_overflow` profile for iteration:

```bash
cargo run --bin xindeler-server-cli --profile no_overflow
```

Or to build world specifically with the profile:
```bash
cargo build -p xindeler-world --profile no_overflow
```

## Adjusting Parameters

Most world-gen parameters are either:

1. **In RON config files** (fast to change, no recompile):
   - `assets/world/` — look for `.ron` files with world parameters
   - Edit → restart server → observe

2. **In Rust consts/structs** (requires recompile):
   - `world/src/config.rs` — `WorldConfig`
   - `world/src/sim/mod.rs` — simulation constants
   - After change: `cargo check -p xindeler-world` → restart server

## Visualization Tools

```bash
# Recipe dependency graph (requires graphviz):
cargo dot-recipes | dot -Tsvg > recipes.svg && open recipes.svg

# Skill dependency graph:
cargo dot-skills | dot -Tsvg > skills.svg && open skills.svg

# Airship route maps (run server with feature, exports map images during worldgen):
cargo run -p xindeler-server-cli --bin xindeler-server-cli --features xindeler-world/airship_maps

# Chunk compression benchmark:
cargo run --manifest-path world/Cargo.toml --features=bin_compression --example chunk_compression_benchmarks
```

## rtsim — Real-Time World Simulation

rtsim is **not ECS** — it's a rule/event-driven system simulating thousands of NPCs at coarse granularity (no combat, no physics). It runs in `rtsim/src/`.

**Core concepts:**
- `RtState` — the world state (tables of NPCs, sites, factions)
- `Event` — something that happened (NPC arrived, battle concluded, trade completed)
- `Rule` — reacts to events and updates state
- Data tables in `rtsim/src/data/` — NPCs, sites, factions are rows in tables

**Iteration workflow for rtsim:**
```bash
cargo check -p xindeler-rtsim
cargo server   # rtsim runs on the server
# watch server logs for rtsim output:
cargo server 2>&1 | grep -i rtsim
```

## Testing World Gen Changes

There are no automated tests for visual/aesthetic world-gen output — judgment is required. However:

```bash
# Run world-gen unit tests:
VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-world -- --nocapture

# Run rtsim tests:
VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-rtsim -- --nocapture
```
