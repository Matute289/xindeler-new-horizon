//! Banishment lifecycle: the wall clock every deadline is measured against,
//! and the once-per-tick **park / return / rehydrate** maintenance pass.
//!
//! # Why a `&mut Server` pass and not a `specs` system
//!
//! Rehydration needs
//! [`handle_create_npc`](crate::events::shared::handle_create_npc)'s
//! **returned** `EcsEntity` so the freshly spawned creature can be marked
//! `Banished` in the same statement. A system that emits `CreateNpcEvent`
//! never learns which entity it created, so the whole pass lives here and is
//! called from `Server::tick` right after `handle_events` — late enough that
//! the `DestroyEvent{Banished}` reward block has already read the creature's
//! position, early enough that a banishment raised this tick is parked this
//! tick.
//!
//! # The two freeze paths
//!
//! Every one of the three passes forks on whether the creature is an rtsim
//! actor, because the two are simulated by completely different machinery:
//!
//! | | plain mob | rtsim actor |
//! |---|---|---|
//! | park | strip `Pos`/`Agent`/`Vel`, keep the entity | clear `Actor::presence` + unload-and-delete the entity |
//! | return | restore the components on the parked entity | restore `presence`; rtsim's load loop rebuilds the entity |
//! | rehydrate | respawn a frozen entity from the persisted archetype | nothing — `Banishments::prepare` never queues one |

use common::comp::{self, Agent, Auras, Banished, Buffs, Combo, Energy, EnteredAuras, Health, Pos};
use specs::{Join, WorldExt};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "worldgen")]
use crate::{rtsim::RtSim, state_ext::StateExt, sys::terrain::SpawnEntityData};
#[cfg(feature = "worldgen")]
use common::{
    comp::{Ori, inventory::loadout_builder::LoadoutBuilder},
    event::CreateNpcEvent,
    generation::EntityInfo,
};
#[cfg(feature = "worldgen")]
use rtsim::data::{BanishedCreature, BanishedKind};
#[cfg(feature = "worldgen")]
use specs::Entity as EcsEntity;
#[cfg(feature = "worldgen")] use tracing::warn;

use crate::Server;

/// Wall-clock seconds since the UNIX epoch — the only clock in this engine
/// that means the same thing before and after a server restart. See
/// `comp::Banished`'s doc comment for why `Time` and `TimeOfDay` do not.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One tick of banishment upkeep.
///
/// Runs on every server tick, so every pass is written to cost nothing when
/// nothing is banished: the two joins are over `Banished`'s (empty) storage
/// mask, and the registry lookups iterate an empty map. No allocation happens
/// on the idle path — `Vec::new()` does not allocate, and `mem::take` on an
/// empty `Vec` does not either.
///
/// The pass order is **park → return → rehydrate**, and it is deliberate:
///
/// * Park first, so a creature banished during this tick's `handle_events` is
///   already frozen before anything else looks at it.
/// * Return second, so a record whose deadline is *already* in the past
///   (`/banish 0`, or a deadline that expired while the server was down) is
///   parked and un-parked inside the same tick rather than being visibly frozen
///   for one. Nothing observes the intermediate state: client sync runs later
///   in the tick.
/// * Rehydrate last, so a record loaded from the save file is respawned with
///   the current tick's return pass already done — an overdue one is dropped
///   immediately instead of spending a tick parked.
///
/// There is no "unbanished and re-banished in the same tick" case to worry
/// about: the only writer of `Banished` is `BanishEvent`'s handler, which runs
/// inside `handle_events` (i.e. strictly before this pass) and skips any
/// entity that already carries the component. An entity this pass returns is
/// therefore banishable again at the earliest on the *next* tick, and gets a
/// brand-new record and id when it is.
pub fn maintain(server: &mut Server) {
    let now = now_unix_secs();
    park_newly_banished(server);
    return_due(server, now);
    #[cfg(feature = "worldgen")]
    rehydrate_pending(server, now);
}

/// Freeze. Two branches:
///
/// * **Plain mob** — strip the components every simulating system joins on.
///   `Pos` is what takes the entity out of physics
///   (`common/systems/src/phys/mod.rs`), out of client entity-sync and
///   therefore out of rendering (`server/src/sys/entity_sync.rs`), and out of
///   every `CachedSpatialGrid` target query; `Agent` and `Vel` are what stop
///   the AI (`server/src/sys/agent/mod.rs` joins on all three). This ECS has no
///   generic "disabled entity" marker to reuse — absence of the component a
///   system joins on *is* how it expresses "not simulated", and
///   `common/src/region.rs` documents a removed `Pos` as an anticipated state.
/// * **rtsim actor** — clear `Actor::presence` and delete the entity, the exact
///   pair the engine's own chunk-unload sweep uses (`server/src/lib.rs`). The
///   `Actor` stays in `data.actors`, and every simulation path skips it because
///   `presence` is `None`.
///
///   🔴 Component-parking an rtsim actor instead would strand it: both
///   rtsim's sync loop and the chunk-unload sweep join on `&Pos`, so its
///   `mode` would sit at `SimulationMode::Loaded` forever — rtsim would stop
///   simulating it *and* never re-materialise it.
fn park_newly_banished(server: &mut Server) {
    #[cfg(feature = "worldgen")]
    let mut rtsim_actors: Vec<(EcsEntity, common::rtsim::ActorId)> = Vec::new();

    let plain = {
        let ecs = server.state.ecs();
        let entities = ecs.entities();
        let banished = ecs.read_storage::<Banished>();
        // Only entities that still have a `Pos` are candidates: a mob parked
        // on an earlier tick has none, which is what keeps this pass O(newly
        // banished) rather than O(banished).
        let positions = ecs.read_storage::<Pos>();
        #[cfg(feature = "worldgen")]
        let actor_ids = ecs.read_storage::<common::rtsim::ActorId>();

        let mut plain = Vec::new();
        for (entity, _, _) in (&entities, &banished, &positions).join() {
            #[cfg(feature = "worldgen")]
            if let Some(actor) = actor_ids.get(entity).copied() {
                rtsim_actors.push((entity, actor));
                continue;
            }
            plain.push(entity);
        }
        plain
    };

    if !plain.is_empty() {
        let ecs = server.state.ecs();
        let mut positions = ecs.write_storage::<Pos>();
        let mut agents = ecs.write_storage::<Agent>();
        let mut velocities = ecs.write_storage::<comp::Vel>();
        for entity in plain {
            positions.remove(entity);
            agents.remove(entity);
            velocities.remove(entity);
        }
    }

    #[cfg(feature = "worldgen")]
    for (entity, actor) in rtsim_actors {
        {
            let mut rtsim = server.state.ecs().write_resource::<RtSim>();
            rtsim.set_actor_presence(actor, false);
            rtsim.hook_rtsim_entity_unload(actor);
        }
        if let Err(e) = server.state.delete_entity_recorded(entity) {
            warn!(?e, "Failed to unload a banished rtsim actor");
        }
    }
}

/// Thaw: the creature comes back exactly where it left, fully reset. The
/// design explicitly does not preserve buffs, mid-fight health or aggro — by
/// the time it returns it is, narratively, no longer in that fight.
///
/// Two sources, because the two park branches leave different traces: a parked
/// plain mob is an ECS entity carrying `Banished`, whereas a parked rtsim
/// actor has **no ECS entity at all** — only its record and its
/// `presence = None`. A `join` on `Banished` can never find the latter.
fn return_due(server: &mut Server, now_unix_secs: u64) {
    // --- rtsim actors: restore `presence`, rtsim reloads them itself --------
    #[cfg(feature = "worldgen")]
    {
        let due_actors = server
            .state
            .ecs()
            .read_resource::<RtSim>()
            .with_banishments(|banishments| {
                banishments
                    .due_rtsim_actors(now_unix_secs)
                    .collect::<Vec<_>>()
            });
        if !due_actors.is_empty() {
            let rtsim = server.state.ecs().read_resource::<RtSim>();
            for (id, actor) in due_actors {
                // Restoring `presence` is the whole return: rtsim's load loop
                // re-materialises the entity the next time its chunk is
                // loaded, rebuilt from the actor's `EntityConfig` — i.e. fully
                // reset, for free.
                if !rtsim.set_actor_presence(actor, true) {
                    warn!(
                        ?id,
                        "Banished rtsim actor no longer exists; dropping its record"
                    );
                }
                rtsim.with_banishments(|banishments| {
                    banishments.remove(id);
                });
            }
        }
    }

    // --- plain mobs: un-park the entity we kept -----------------------------
    let due = {
        let ecs = server.state.ecs();
        let entities = ecs.entities();
        let banished = ecs.read_storage::<Banished>();
        #[cfg(feature = "worldgen")]
        let actor_ids = ecs.read_storage::<common::rtsim::ActorId>();

        // A parked rtsim actor has no ECS entity at all, so anything reachable
        // here that still carries an `ActorId` is a park whose
        // `delete_entity_recorded` failed. Un-parking it through this branch
        // would insert a `Pos` on an actor whose `presence` is `None` — the
        // exact stranding the rtsim branch exists to avoid — so it is left to
        // `due_rtsim_actors` above, which has already restored its `presence`.
        #[cfg(feature = "worldgen")]
        let is_plain_mob = |entity| !actor_ids.contains(entity);
        #[cfg(not(feature = "worldgen"))]
        let is_plain_mob = |_entity| true;

        (&entities, &banished)
            .join()
            .filter(|(entity, banished)| is_plain_mob(*entity) && banished.is_due(now_unix_secs))
            .map(|(entity, banished)| (entity, *banished))
            .collect::<Vec<_>>()
    };

    if due.is_empty() {
        return;
    }

    {
        let ecs = server.state.ecs();
        let bodies = ecs.read_storage::<comp::Body>();
        let mut positions = ecs.write_storage::<Pos>();
        let mut velocities = ecs.write_storage::<comp::Vel>();
        let mut agents = ecs.write_storage::<Agent>();
        let mut healths = ecs.write_storage::<Health>();
        let mut energies = ecs.write_storage::<Energy>();
        let mut buffs = ecs.write_storage::<Buffs>();
        let mut auras = ecs.write_storage::<Auras>();
        let mut entered_auras = ecs.write_storage::<EnteredAuras>();
        let mut combos = ecs.write_storage::<Combo>();
        let mut banished = ecs.write_storage::<Banished>();

        for (entity, record) in &due {
            let entity = *entity;
            let _ = positions.insert(entity, Pos(record.return_pos));
            let _ = velocities.insert(entity, comp::Vel::zero());
            // ⚠️ A creature that was authored with `has_agency: false` comes
            // back with agency, because the record does not remember that it
            // had none. Unreachable through the spell — every banishable
            // `CreatureKind` is a living, acting creature — but reachable
            // through `/banish` (N38B21-J) on a hand-picked target.
            if let Some(body) = bodies.get(entity) {
                let _ = agents.insert(entity, Agent::from_body(body));
            }
            if let Some(mut health) = healths.get_mut(entity) {
                // `revive` rather than `set_fraction(1.0)`: it also clears
                // `is_dead` and restores death protection, which is what
                // "fully reset" means.
                health.revive();
                health.clear_absorb();
            }
            if let Some(mut energy) = energies.get_mut(entity) {
                energy.refresh();
            }
            // 🔴 Reset, do **not** remove. Every one of these is inserted
            // unconditionally by `StateExt::create_npc`, and the buff and aura
            // systems join on them (`common/systems/src/buff.rs`,
            // `common/systems/src/aura.rs`) rather than treating them as
            // optional — a returned creature missing `Buffs` could never be
            // buffed or debuffed again, and one missing `EnteredAuras` would
            // be invisible to every aura in the game, including this spell.
            let _ = buffs.insert(entity, Buffs::default());
            let _ = auras.insert(entity, Auras::default());
            let _ = entered_auras.insert(entity, EnteredAuras::default());
            let _ = combos.insert(entity, Combo::default());
            banished.remove(entity);
        }
    }

    #[cfg(feature = "worldgen")]
    {
        server
            .state
            .ecs()
            .read_resource::<RtSim>()
            .with_banishments(|banishments| {
                for (_, record) in &due {
                    banishments.remove(record.id);
                }
            });
    }
}

/// The spawn description a persisted `Freestanding` record turns back into,
/// or `None` for an rtsim actor.
///
/// Deliberately routed through `EntityInfo` rather than hand-building an
/// `NpcBuilder`: `SpawnEntityData::from_entity_info` is the one place that
/// resolves a body into a full, playable NPC (health, poise, skill set,
/// inventory, agent, ethos), and it is the same path worldgen's chunk
/// supplement and `/spawn` both take. Constructing the builder directly would
/// silently produce a creature with **no `Health` component at all** —
/// `NpcBuilder::new` defaults `health` to `None` and `StateExt::create_npc`
/// only `maybe_with`s it — i.e. an invulnerable phoenix.
///
/// What the record cannot restore is the creature's authored loadout and loot
/// table: `BanishedKind::Freestanding` stores an archetype, not a snapshot of
/// the entity, so a creature rehydrated after a **server restart** comes back
/// with its body's default gear and drops nothing when later killed. Within a
/// single session this does not apply — the parked entity keeps its own
/// `ItemDrops` (see N38B21-G), which is what the smoke test's K8 exercises.
#[cfg(feature = "worldgen")]
fn rehydration_entity_info(record: &BanishedCreature) -> Option<EntityInfo> {
    let BanishedKind::Freestanding {
        body,
        alignment,
        creature_kind,
        scale,
    } = record.kind
    else {
        return None;
    };

    let mut info = EntityInfo::at(record.return_pos)
        .with_body(body)
        .with_alignment(alignment)
        .with_scale(scale)
        .with_automatic_name()
        .with_loadout(LoadoutBuilder::from_default(&body));
    if let Some(creature_kind) = creature_kind {
        info = info.with_creature_kind(creature_kind);
    }
    Some(info)
}

/// Re-create a frozen entity for every persisted **`Freestanding`** record
/// that has no live one: every such record right after a server start, plus
/// any whose parked entity was lost mid-session. A record whose deadline
/// already passed while the server was down comes back immediately.
///
/// `RtsimActor` records never reach here — `Banishments::prepare` does not
/// queue them, because rtsim re-materialises those itself once `return_due`
/// restores their `presence`. The `rehydration_entity_info` fork is a second,
/// belt-and-braces guard against a future caller of `queue_rehydration` that
/// forgets.
#[cfg(feature = "worldgen")]
fn rehydrate_pending(server: &mut Server, now_unix_secs: u64) {
    let pending = server
        .state
        .ecs()
        .read_resource::<RtSim>()
        .with_banishments(|banishments| banishments.take_pending_rehydration());
    if pending.is_empty() {
        return;
    }

    for id in pending {
        let Some(record) = server
            .state
            .ecs()
            .read_resource::<RtSim>()
            .with_banishments(|banishments| banishments.get(id).cloned())
        else {
            continue;
        };
        let Some(entity_info) = rehydration_entity_info(&record) else {
            continue;
        };

        let Ok(npc_data) = SpawnEntityData::from_entity_info(entity_info).into_npc_data_inner()
        else {
            // Unreachable: `special_entity` is never set above.
            warn!(?id, "A banished creature rehydrated into a special entity");
            continue;
        };
        let (npc, pos) = npc_data.to_npc_builder();
        let entity = crate::events::shared::handle_create_npc(server, CreateNpcEvent {
            pos,
            ori: Ori::default(),
            npc,
        });

        if record.returns_at_unix_secs <= now_unix_secs {
            // The deadline passed while the server was down: it is already
            // back, so it is spawned live and the record is forgotten.
            server
                .state
                .ecs()
                .read_resource::<RtSim>()
                .with_banishments(|banishments| {
                    banishments.remove(id);
                });
        } else if let Err(e) =
            server
                .state
                .ecs()
                .write_storage::<Banished>()
                .insert(entity, Banished {
                    id,
                    return_pos: record.return_pos,
                    returns_at_unix_secs: record.returns_at_unix_secs,
                })
        {
            warn!(?e, ?id, "Failed to re-park a rehydrated banished creature");
        }
        // Stripping `Pos`/`Agent`/`Vel` happens on the next tick's
        // `park_newly_banished`. One tick of visibility is harmless: at server
        // start no client has been sent the entity yet, and mid-session the
        // creature was already gone from every client.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole design hangs on a monotonic real-time clock that survives a
    /// restart; if this ever returns 0 the registry silently returns every
    /// banished creature at once.
    #[test]
    fn the_wall_clock_is_a_plausible_unix_timestamp() {
        // 2023-01-01T00:00:00Z — any real clock is well past this.
        assert!(now_unix_secs() > 1_672_531_200);
    }

    #[test]
    fn the_wall_clock_is_monotonic_across_calls() {
        let a = now_unix_secs();
        let b = now_unix_secs();
        assert!(b >= a);
    }
}

/// The one piece of the maintenance pass that is testable without a live
/// `Server`: turning a persisted record back into the spawn description the
/// engine's ordinary NPC pipeline understands.
#[cfg(all(test, feature = "worldgen"))]
mod rehydration_tests {
    use super::*;
    use common::comp::{Alignment, Body, CreatureKind, bird_large};
    use rtsim::data::{BanishedCreature, BanishedKind};
    use vek::{Vec2, Vec3};

    fn phoenix_body() -> Body {
        Body::BirdLarge(bird_large::Body {
            species: bird_large::Species::Phoenix,
            body_type: bird_large::BodyType::Female,
        })
    }

    fn freestanding(creature_kind: Option<CreatureKind>) -> BanishedCreature {
        BanishedCreature {
            kind: BanishedKind::Freestanding {
                body: phoenix_body(),
                alignment: Alignment::Enemy,
                creature_kind,
                scale: 1.5,
            },
            return_pos: Vec3::new(512.0, 640.0, 90.0),
            return_chunk: Vec2::new(1, 1),
            returns_at_unix_secs: 1_700_000_000,
        }
    }

    /// The `Freestanding` record is the *only* memory of a plain world mob, so
    /// every field it carries has to reach the respawn description verbatim —
    /// a creature that came back differently scaled, differently aligned or
    /// differently classified would not be the creature that was banished.
    #[test]
    fn a_freestanding_record_rehydrates_its_whole_archetype() {
        let record = freestanding(Some(CreatureKind::Fiend));
        let info = rehydration_entity_info(&record).expect("freestanding records rehydrate");

        assert_eq!(info.pos, record.return_pos);
        assert_eq!(info.body, phoenix_body());
        assert_eq!(info.alignment, Alignment::Enemy);
        assert_eq!(info.scale, 1.5);
        // The authored `EntityConfig::creature_type` override must win over
        // the phoenix body's own `Celestial` default, or a reskinned mob comes
        // back as the wrong kind — and stops being banishable.
        assert_eq!(info.creature_kind, Some(CreatureKind::Fiend));
    }

    /// With no override the body's own kind is used, which is exactly what
    /// `Stats::new` seeds anyway — so leaving it unset is the correct way to
    /// express "no override", not a lost field.
    #[test]
    fn a_record_without_a_creature_kind_override_defers_to_the_body() {
        let info = rehydration_entity_info(&freestanding(None)).expect("freestanding rehydrates");
        assert_eq!(info.creature_kind, None);
    }

    /// rtsim re-materialises its own actors through its load loop once the
    /// return pass restores `presence`. Spawning one here too is the worst bug
    /// available in this row — it duplicates the creature — so the fork is
    /// pinned by a test rather than merely documented.
    #[test]
    fn an_rtsim_actor_record_never_produces_a_fresh_entity() {
        let mut ids = slotmap::SlotMap::<common::rtsim::ActorId, ()>::default();
        let record = BanishedCreature {
            kind: BanishedKind::RtsimActor(ids.insert(())),
            return_pos: Vec3::new(512.0, 640.0, 90.0),
            return_chunk: Vec2::new(1, 1),
            returns_at_unix_secs: 1_700_000_000,
        };
        assert!(rehydration_entity_info(&record).is_none());
    }
}
