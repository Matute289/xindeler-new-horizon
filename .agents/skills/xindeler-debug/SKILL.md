---
name: xindeler-debug
description: Use when investigating any bug or unexpected game behavior — provides ECS-aware debugging workflow before proposing fixes
---

# xindeler-debug

**REQUIRED:** Invoke `superpowers:systematic-debugging` before proposing any fix.

## Step 1: Reproduce the Bug

Identify the minimal sequence to reproduce. Use admin commands:

```bash
# Launch server with admin access
cargo server
# In another terminal, connect with client
cargo run --bin veloren-voxygen
```

In-game reproduction commands:
- `/tp <x> <y> <z>` — teleport to specific coordinates
- `/give_item <item>` — e.g. `/give_item common.items.weapons.sword.starter`
- `/spawn <entity>` — e.g. `/spawn common.entity.npc.humanoid.villager`
- `/time <hour>` — set time of day (e.g. `/time 12` for noon)
- `/weather_zone <kind> [radius] [time]` — change weather in a zone
- `/debug_column` — show terrain column debug info at cursor
- `/sudo <player> <cmd>` — act as another entity

## Step 2: Classify the Bug in ECS Terms

Ask: where does the incorrect behavior originate?

**Component data is wrong:**
- The stored data on an entity is incorrect.
- Look in `common/src/comp/` for the relevant component.
- Check where that component is written (grep for `WriteStorage<CompName>`).

**System isn't running / running in wrong order:**
- Check the dispatcher setup in `common/systems/src/lib.rs` (shared) or `server/src/sys/mod.rs` (server-only).
- System dependencies declared via `dispatch::<Sys>(builder, &[deps...])` determine order.

**Event not emitted / not handled:**
- Event types defined in `common/src/event.rs`.
- Handlers in `server/src/events/` (server-side) or client-side in `client/src/`.
- Check `EventBus<EventType>` usage.

**Network message not sent / received:**
- Message types in `common/net/src/msg/`.
- Server sends in `server/src/sys/msg/` systems.
- Client receives in `client/src/` handlers.

**Physics / position issue:**
- Physics system: `common/systems/src/phys/mod.rs`
- Collision detection: `common/systems/src/phys/collision.rs`
- Visual gizmos for debugging positions: `common/src/comp/gizmos.rs`

## Step 3: Instrument the Code

Add temporary tracing spans to narrow down the issue:

```rust
// At the top of the relevant file:
use tracing::{debug, warn, error};

// Inside the suspect code:
debug!(?component_value, entity = ?entity, "Checking component state");
warn!("Unexpected state reached: {:?}", value);
```

Run the server/client and watch logs:
- Server logs: `userdata/server/logs/`
- Client logs: `userdata/client/logs/`
- Or pipe to terminal: `cargo server 2>&1 | grep -i "your_debug_message"`

## Step 4: Performance Bugs — Use Tracy

If the bug is a performance regression (lag, frame drops):

```bash
# Server performance
cargo tracy-server

# Client/rendering performance
cargo tracy-voxygen
```

Connect Tracy profiler (download from https://github.com/wolfpld/tracy) to the running process to see per-system timing.

## Step 5: Visual Physics Bugs

For position/collision/movement bugs that are hard to trace via logs:

1. Add a gizmo to the suspect entity in the relevant system:
   ```rust
   // WriteStorage<comp::Gizmos> in SystemData
   gizmos.insert(entity, comp::Gizmos::default()).ok();
   ```
2. Run with `/debug_column` in-game to see terrain debug info.

## Common Bug Patterns in the Xindeler Engine

| Symptom | Likely cause | Where to look |
|---------|-------------|---------------|
| NPC freezes / stops acting | Agent state machine stuck | `server/agent/src/action_nodes.rs` |
| Ability doesn't activate | Character state transition fails | `common/src/comp/character_state.rs` |
| Damage not applied | Melee/projectile system miss | `common/systems/src/melee.rs` or `projectile.rs` |
| Entity desync client/server | Entity sync system | `server/src/sys/entity_sync.rs` |
| Chunk not loading | Chunk generator queue | `server/src/chunk_generator.rs` |
| Item not saving | Persistence system | `server/src/persistence/` |
| Buff not applying | Buff system data | `common/systems/src/buff.rs` |

## Step 6: Fix → Verify → Clean Up

After finding the root cause:
1. Remove all temporary `debug!` / `warn!` instrumentation.
2. Write a regression test if possible.
3. Run `cargo check -p veloren-<affected-crate>` to verify it compiles.
4. Use the `xindeler-run` skill to do a full manual test.
5. Use the `xindeler-review` skill before committing.
