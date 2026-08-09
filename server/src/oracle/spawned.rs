//! `OracleSpawned` — server-only attribution marker for entities created by
//! a PROJECT ORACLE `DmEvent` trigger (`/oracle_trigger`, and later the ops
//! console's HTTP path).
//!
//! **Never net-synced.** Unlike `common::comp::PhantomIllusion` (which *is*
//! synced because voxygen tints phantasms client-side), which entities an
//! operator manually spawned is metagame information: the client must never
//! learn it. This type derives neither `Serialize` nor `Deserialize`, is not
//! listed in `common/net/src/synced_components.rs`, and must never be — a
//! future session reaching for those derives "for completeness" would leak
//! that information and should stop and re-read this doc instead.
//!
//! **Never persisted.** No SQLite row, no `PersistedComponents` entry, no
//! rtsim save entry, no migration. This is correct, not merely convenient:
//! ORACLE-spawned entities do not survive a chunk unload (they are never
//! `.with_anchor(...)`, so the cleanup sweep in `Server::tick` culls them the
//! moment their chunk unloads), let alone a server restart. The live count
//! this module derives is *supposed* to reset to zero on restart, because
//! the entities it would have counted are genuinely gone.
//!
//! `spawned_at` exists so every entity created by a single trigger fire
//! shares one timestamp, which is what makes "undo just the last fire"
//! (a filter by max `spawned_at`) expressible without inventing a separate
//! fire-id.

use common::resources::Time;
use specs::{Component, DenseVecStorage, Join, World, WorldExt};
use std::sync::Arc;

/// Marks an entity as having been spawned by a PROJECT ORACLE `DmEvent`
/// trigger. Server-only: never `common/src/comp/`, never net-synced, never
/// persisted. See the module doc for why.
#[derive(Clone, Debug)]
pub struct OracleSpawned {
    /// The `<id>` of the `.dmevent.ron`/`.dmevent.json` that spawned this
    /// entity — the same id `/oracle_trigger <id>` takes. `Arc<str>` so a
    /// single trigger's spawns (up to `common_oracle`'s `SPAWN_COUNT` clamp)
    /// share one allocation instead of one `String` each.
    pub event_id: Arc<str>,
    /// Server-authoritative `Time`, stamped by `handle_create_npc` from the
    /// resource it already reads for `delete_after`. Shared by every entity
    /// from one trigger fire — see the module doc.
    pub spawned_at: Time,
}

impl Component for OracleSpawned {
    // `DenseVecStorage`: ORACLE entities are a small minority of all live
    // entities (ceiling in the low hundreds against a world of thousands),
    // so the sparse storage is the right trade. Contrast `PhantomIllusion`'s
    // `NullStorage`, which is only available because that marker is a ZST —
    // this one carries data.
    type Storage = DenseVecStorage<Self>;
}

/// Live count of ORACLE-attributed entities, derived on demand — **never**
/// maintained as a running counter. Entity deletion in this codebase is not
/// funnelled through one place (many `delete_entity_recorded` call sites,
/// several deliberate `delete_entity_common` bypasses, plus the
/// chunk-unload cull), so a counter would silently drift and fail *closed*
/// — refusing legitimate triggers for a world that is actually empty, with
/// no way for the operator to tell. The ECS already maintains component
/// storage membership against entity deletion, correctly, for free — use
/// it.
///
/// Joined against `Entities` (not the storage alone) so a slot belonging to
/// an entity deleted earlier this tick, before `World::maintain` runs, can
/// never be counted.
pub fn live_count(ecs: &World) -> usize {
    (&ecs.entities(), &ecs.read_storage::<OracleSpawned>())
        .join()
        .count()
}

/// Same as [`live_count`], restricted to entities tagged with one `DmEvent`
/// id — for a per-event ceiling and for a future per-event status
/// breakdown.
pub fn live_count_for(ecs: &World, event_id: &str) -> usize {
    (&ecs.entities(), &ecs.read_storage::<OracleSpawned>())
        .join()
        .filter(|(_, spawned)| &*spawned.event_id == event_id)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use specs::Builder;

    fn setup_world() -> World {
        let mut world = World::new();
        world.register::<OracleSpawned>();
        world
    }

    fn tag(world: &mut World, event_id: &str, at: f64) -> specs::Entity {
        world
            .create_entity()
            .with(OracleSpawned {
                event_id: Arc::from(event_id),
                spawned_at: Time(at),
            })
            .build()
    }

    #[test]
    fn live_count_counts_only_tagged_entities() {
        let mut world = setup_world();
        tag(&mut world, "mist_bound", 0.0);
        tag(&mut world, "mist_bound", 0.0);
        tag(&mut world, "wraith_choir", 1.0);
        // Untagged entities must not be counted.
        world.create_entity().build();
        world.create_entity().build();

        assert_eq!(live_count(&world), 3);
        assert_eq!(live_count_for(&world, "mist_bound"), 2);
        assert_eq!(live_count_for(&world, "wraith_choir"), 1);
        assert_eq!(live_count_for(&world, "no_such_event"), 0);
    }

    #[test]
    fn live_count_drops_after_deletion_and_maintain() {
        let mut world = setup_world();
        tag(&mut world, "mist_bound", 0.0);
        let doomed = tag(&mut world, "mist_bound", 0.0);
        tag(&mut world, "mist_bound", 0.0);
        assert_eq!(live_count(&world), 3);

        // Pins the "derived, never maintained" invariant: the count must
        // track real deletions without any manual decrement anywhere.
        world.delete_entity(doomed).expect("entity is alive");
        world.maintain();

        assert_eq!(live_count(&world), 2);
        assert_eq!(live_count_for(&world, "mist_bound"), 2);
    }
}
