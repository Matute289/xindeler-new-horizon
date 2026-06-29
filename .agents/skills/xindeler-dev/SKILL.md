---
name: xindeler-dev
description: Use when implementing new gameplay mechanics, ECS components, systems, abilities, NPC behaviors, or admin commands — guides where to make changes and what to check
---

# xindeler-dev

**REQUIRED:** Before writing any code, invoke `superpowers:test-driven-development`.

## Step 0: Identify the Change Type

Before touching any file, classify the work:

| Change type | Primary file(s) | Registration |
|------------|-----------------|-------------|
| New ECS component | `common/src/comp/<name>.rs` | `common/state/src/state.rs` |
| New shared system (client+server) | `common/systems/src/<name>.rs` | `common/systems/src/lib.rs` |
| New server-only system | `server/src/sys/<name>.rs` | `server/src/sys/mod.rs` |
| New combat ability | `common/src/comp/ability.rs` | + state in `character_state.rs` |
| NPC behavior / AI | `server/agent/src/action_nodes.rs` | — (hot-reloadable dylib) |
| NPC combat AI | `server/agent/src/attack.rs` | — |
| New admin command | `common/src/cmd.rs` (enum) | + handler in `server/src/cmd.rs` |
| New item / recipe | `assets/common/items/` (RON) | `assets/common/recipe_book_manifest.ron` |
| New buff/debuff | `common/src/comp/buff.rs` | — |
| New resource | `common/src/resources.rs` | `common/state/src/state.rs` |

## Step 1: Read the Existing Pattern

Always find a similar existing implementation and read it before writing new code.

- New component? Read `common/src/comp/poise.rs` (clean, small example).
- New system? Read `common/systems/src/stats.rs` (minimal shared system).
- New server system? Read `server/src/sys/waypoint.rs` (small, focused).
- New ability? Read an existing ability in `common/src/comp/ability.rs` — find the `BasicMelee` variant as a starting point.
- New NPC behavior? Read a small node in `server/agent/src/action_nodes.rs`.

## Step 2: Write Tests First

Invoke `superpowers:test-driven-development` now.

Tests for ECS go in `<crate>/src/<module>.rs` as `#[cfg(test)]` blocks, or in `<crate>/tests/`. Run with:
```bash
VELOREN_ASSETS="$(pwd)/assets" cargo test -p veloren-<crate> -- --nocapture
```

For a specific test:
```bash
VELOREN_ASSETS="$(pwd)/assets" cargo test -p veloren-common test_my_component -- --nocapture
```

## Step 3: Implement

### Adding a New ECS Component

1. Create `common/src/comp/<name>.rs` with the component struct.
2. Derive `Component` from specs: `use specs::Component;`
3. Export from `common/src/comp/mod.rs`: `pub mod <name>;` and re-export key types.
4. Register in `common/state/src/state.rs` in the `register_components` function:
   ```rust
   world.register::<comp::MyComponent>();
   ```

### Adding a New Shared System

1. Create `common/systems/src/<name>.rs`.
2. Implement `specs::System` with a `SystemData` type alias.
3. Register in `common/systems/src/lib.rs`:
   ```rust
   pub fn add_local_systems(dispatch_builder: &mut DispatcherBuilder) {
       dispatch::<my_sys::Sys>(dispatch_builder, &[/* dependencies */]);
   }
   ```

### Adding a New Server-Only System

1. Create `server/src/sys/<name>.rs`.
2. Implement `specs::System`.
3. Add to `server/src/sys/mod.rs` in the server dispatcher setup.

### Adding a New Admin Command

1. Add variant to the `ServerChatCommand` or `ChatCommand` enum in `common/src/cmd.rs`.
2. Add the handler function in `server/src/cmd.rs` — follow the pattern of existing handlers.
3. The command will auto-appear in `/help` output.

## Step 4: Verify Incrementally

After each logical unit of code (not after every line), run:
```bash
source "$HOME/.cargo/env"
cargo check -p veloren-<affected-crate>
```

This is fast (~5-10s) and catches most errors before a full build.

## Step 5: Test In-Game

Use the `xindeler-run` skill to launch the game and test the feature manually.

Useful in-game commands for testing:
- `/give_item <item_path>` — spawn an item
- `/tp <x> <y> <z>` — teleport for positioning
- `/spawn <entity>` — spawn an NPC
- `/sudo <player> <command>` — run command as another entity

## Key Reference Files by Area

| Area | File | Size |
|------|------|------|
| Combat abilities | `common/src/comp/ability.rs` | 142KB |
| Character state machine | `common/src/comp/character_state.rs` | 65KB |
| Buff system | `common/src/comp/buff.rs` | 57KB |
| Inventory/loadout | `common/src/comp/inventory/loadout_builder.rs` | 61KB |
| NPC combat AI | `server/agent/src/attack.rs` | 361KB |
| NPC behavior tree | `server/agent/src/action_nodes.rs` | 103KB |
| Physics system | `common/systems/src/phys/mod.rs` | — |
| Server state ext | `server/src/state_ext.rs` | 56KB |
| Admin commands | `server/src/cmd.rs` | 217KB |
