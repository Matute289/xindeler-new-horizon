//! Banishment lifecycle: the wall clock every deadline is measured against,
//! and the once-per-tick **park / return / rehydrate** maintenance pass.
//!
//! # Why a `&mut Server` pass and not a `specs` system
//!
//! Rehydration needs `handle_create_npc`'s
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

use common::{
    comp::{self, Agent, Auras, Banished, Buffs, Combo, Energy, EnteredAuras, Health, Pos},
    link::Is,
    mounting::{Mount, Rider, VolumeRider},
};
use common_base::prof_span;
use common_net::sync::WorldSyncExt;
use specs::{Entity as EcsEntity, Join, WorldExt};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "worldgen")]
use crate::{rtsim::RtSim, state_ext::StateExt, sys::terrain::SpawnEntityData};
#[cfg(feature = "worldgen")]
use common::{
    comp::{Ori, inventory::loadout_builder::LoadoutBuilder},
    event::CreateNpcEvent,
    generation::EntityInfo,
    terrain::CoordinateConversions,
};
#[cfg(feature = "worldgen")]
use rtsim::data::{BanishedCreature, BanishedKind};
use tracing::warn;

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

/// Deadline for an admin-forced banishment: a plain offset in seconds, so
/// `/banish 10` is testable by hand. Kept here rather than in `cmd.rs` so the
/// clock arithmetic has exactly one home.
pub fn admin_return_deadline(now_unix_secs: u64, secs: u64) -> u64 {
    now_unix_secs.saturating_add(secs)
}

/// Banishes `entity` for `secs` seconds, recording it durably. Returns the new
/// record's id, or `None` if the entity has no position/body or is already
/// banished. Shared by the `/banish` admin command and available to any future
/// caller that wants a banishment without an ability behind it.
///
/// Deliberately reuses the exact rtsim-vs-plain-mob fork and `Owned` →
/// `Tame` alignment normalisation the spell path
/// (`server/src/events/banishment.rs`) uses, so `/banish` exercises the real
/// code paths rather than a simplified one. Unlike the spell path there is no
/// saving throw: an admin-forced banishment always succeeds.
#[cfg(feature = "worldgen")]
pub fn banish_entity(
    server: &mut Server,
    entity: EcsEntity,
    secs: u64,
) -> Option<comp::BanishmentId> {
    let ecs = server.state.ecs();
    if ecs.read_storage::<Banished>().contains(entity) {
        return None;
    }
    let pos = ecs.read_storage::<Pos>().get(entity)?.0;
    let body = *ecs.read_storage::<comp::Body>().get(entity)?;
    let alignment = match ecs.read_storage::<comp::Alignment>().get(entity).copied() {
        Some(comp::Alignment::Owned(_)) => comp::Alignment::Tame,
        Some(alignment) => alignment,
        None => comp::Alignment::Wild,
    };
    let creature_kind = ecs
        .read_storage::<comp::Stats>()
        .get(entity)
        .and_then(|stats| stats.creature_kind);
    let scale = ecs
        .read_storage::<comp::Scale>()
        .get(entity)
        .map_or(1.0, |s| s.0);
    let returns_at_unix_secs = admin_return_deadline(now_unix_secs(), secs);

    // Same rtsim-vs-plain-mob fork as the spell path (N38B21-F), so `/banish`
    // exercises the real code paths rather than a simplified one.
    let kind = match ecs
        .read_storage::<common::rtsim::ActorId>()
        .get(entity)
        .copied()
    {
        Some(actor) => BanishedKind::RtsimActor(actor),
        None => BanishedKind::Freestanding {
            body,
            alignment,
            creature_kind,
            scale,
        },
    };

    let id = ecs
        .read_resource::<RtSim>()
        .with_banishments(|banishments| {
            banishments.insert(BanishedCreature {
                kind,
                return_pos: pos,
                return_chunk: pos.xy().as_::<i32>().wpos_to_cpos(),
                returns_at_unix_secs,
            })
        });

    let _ = server
        .state
        .ecs_mut()
        .write_storage::<Banished>()
        .insert(entity, Banished {
            id,
            return_pos: pos,
            returns_at_unix_secs,
        });
    Some(id)
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
/// about: the only writers of `Banished` are `BanishEvent`'s handler and the
/// admin-only `banish_entity` (`/banish`, N38B21-J) — both run inside
/// `handle_events` (i.e. strictly before this pass) and both skip any entity
/// that already carries the component. An entity this pass returns is
/// therefore banishable again at the earliest on the *next* tick, and gets a
/// brand-new record and id when it is.
pub fn maintain(server: &mut Server) {
    // Its own span: the tick's coarse timers would otherwise fold this pass
    // into the event-handling bucket, and "free when nothing is banished" is a
    // claim that should be checkable in Tracy rather than only asserted here.
    prof_span!("banishment::maintain");
    let now = now_unix_secs();
    park_newly_banished(server);
    return_due(server, now);
    #[cfg(feature = "worldgen")]
    rehydrate_pending(server, now);
}

/// Freeze. Two branches:
///
/// * **Plain mob** — strip `Pos` and `Vel`, and *only* those two. `Pos` is what
///   takes the entity out of physics (`common/systems/src/phys/mod.rs`), out of
///   client entity-sync and therefore out of rendering
///   (`server/src/sys/entity_sync.rs`), out of every `CachedSpatialGrid` target
///   query, and — together with `Vel` — out of the AI, because
///   `server/src/sys/agent/mod.rs` joins on `&positions` and `&velocities`.
///   This ECS has no generic "disabled entity" marker to reuse — absence of the
///   component a system joins on *is* how it expresses "not simulated", and
///   `common/src/region.rs` documents a removed `Pos` as an anticipated state.
///
///   🟢 **`Agent` is deliberately left in place.** Removing it would stop
///   nothing that removing `Pos`/`Vel` has not already stopped, but it *would*
///   throw away everything `SpawnEntityData` authored on that agent —
///   `patrol_origin`, merchant/guard `Behavior` and trade site, `no_flee`,
///   `idle_wander_factor`, `aggro_range_multiplier` — because the return pass
///   could then only rebuild a generic `Agent::from_body`. The creature would
///   come back subtly wrong: a merchant that no longer trades, a guard that
///   flees, a beast that wanders off the spawn it used to patrol. Keeping the
///   component is both cheaper and higher fidelity.
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
    let mut players = Vec::new();

    let plain = {
        let ecs = server.state.ecs();
        let entities = ecs.entities();
        let banished = ecs.read_storage::<Banished>();
        // Only entities that still have a `Pos` are candidates: a mob parked
        // on an earlier tick has none, which is what keeps this pass O(newly
        // banished) rather than O(banished).
        let positions = ecs.read_storage::<Pos>();
        // 🔴 Defence in depth: a player must never be parked. The spell cannot
        // reach one (`Body::Humanoid` is unconditionally
        // `CreatureKind::Humanoid`, which is never in the banishable set), but
        // a player carries an `ActorId`, so a future ability-free caller such
        // as `/banish` (N38B21-J) would otherwise fall into the rtsim branch
        // below and *delete a logged-in player's entity* outside the
        // disconnect path — something rtsim itself explicitly refuses to do.
        // `Presence` is the same marker the engine's own NPC-unload sweep uses
        // to mean "this is a client, not a mob".
        let presences = ecs.read_storage::<comp::Presence>();
        #[cfg(feature = "worldgen")]
        let actor_ids = ecs.read_storage::<common::rtsim::ActorId>();

        let mut plain = Vec::new();
        for (entity, _, _) in (&entities, &banished, &positions).join() {
            if presences.contains(entity) {
                players.push(entity);
                continue;
            }
            #[cfg(feature = "worldgen")]
            if let Some(actor) = actor_ids.get(entity).copied() {
                rtsim_actors.push((entity, actor));
                continue;
            }
            plain.push(entity);
        }
        plain
    };

    // Refuse the banishment outright rather than merely skipping the park:
    // leaving the marker on would make the return pass later "reset" a live
    // player's buffs and health.
    for entity in players {
        warn!("Refusing to banish a player entity; dropping the banishment");
        let ecs = server.state.ecs();
        let id = ecs.write_storage::<Banished>().remove(entity).map(|b| b.id);
        #[cfg(feature = "worldgen")]
        if let Some(id) = id {
            ecs.read_resource::<RtSim>()
                .with_banishments(|banishments| {
                    banishments.remove(id);
                });
        }
        let _ = id;
    }

    if !plain.is_empty() {
        // Break any mounting link first: `Mounting::persist`
        // (`common/src/mounting.rs`) checks liveness, health, body and mass —
        // but never `Pos`. A parked *mount* therefore keeps its rider linked
        // and `common/systems/src/mount.rs` stops repositioning that rider,
        // freezing it in place for the whole banishment; a parked *rider* is
        // instead handed a fresh `Pos` by that same system every tick, which
        // this pass strips again the next tick — a `CreateEntity` /
        // `DeleteEntity` pair to every nearby client, every tick, for days.
        for entity in &plain {
            unlink_mounts(server, *entity);
        }

        let ecs = server.state.ecs();
        let mut positions = ecs.write_storage::<Pos>();
        let mut velocities = ecs.write_storage::<comp::Vel>();
        for entity in &plain {
            positions.remove(*entity);
            velocities.remove(*entity);
        }
        drop((positions, velocities));

        // 🔴 Clear the creature's transient state **here**, not only on
        // return. Neither the buff system (`common/systems/src/buff.rs`) nor
        // the stat system (`common/systems/src/stats.rs`) joins on `Pos`, so a
        // parked creature keeps ticking damage-over-time buffs and keeps being
        // checked for death. A banished creature that was burning would
        // therefore die in limbo, and a death is a `RemovalInfo::killed()`
        // `DestroyEvent`, which *deletes the entity* — leaving its registry
        // record orphaned forever, the creature gone for good, and its chunk
        // permanently one spawn short.
        for entity in &plain {
            reset_transient_state(ecs, *entity);
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
            // The actor is stranded either way — its `presence` is already
            // cleared and its `mode` already `Simulated`. Strip the components
            // too so this join stops re-selecting the same entity every tick,
            // which would otherwise re-clear `presence` and re-run
            // `hook_rtsim_entity_unload` forever, the latter logging
            // "Unloaded already unloaded entity" on every single tick.
            warn!(
                ?e,
                "Failed to unload a banished rtsim actor; parking it instead"
            );
            let ecs = server.state.ecs();
            ecs.write_storage::<Pos>().remove(entity);
            ecs.write_storage::<comp::Vel>().remove(entity);
        }
    }
}

/// Wipes everything that makes a creature "mid-fight", leaving the pristine
/// state the design calls for on return.
///
/// Applied at **park** as well as at return, because most of these systems do
/// not join on `Pos` and therefore keep running on a parked entity: buffs
/// (`common/systems/src/buff.rs`) would tick damage-over-time and kill it in
/// limbo, and `CharacterState`/`Controller`/`Stance` would otherwise be frozen
/// mid-ability and resume days later on the tick it returns.
///
/// 🔴 Reset, do **not** remove. Every component here is inserted
/// unconditionally by `StateExt::create_npc`, and the buff and aura systems
/// *join* on `Buffs`/`EnteredAuras` rather than treating them as optional — a
/// creature missing `Buffs` could never be buffed or debuffed again, and one
/// missing `EnteredAuras` would be invisible to every aura in the game,
/// including the spell that banished it.
fn reset_transient_state(ecs: &specs::World, entity: EcsEntity) {
    if let Some(mut health) = ecs.write_storage::<Health>().get_mut(entity) {
        // `revive` rather than `set_fraction(1.0)`: it also clears `is_dead`
        // and restores death protection, which is what "fully reset" means —
        // and a creature parked at 0 HP would otherwise be killed outright by
        // `stats.rs`'s `Pos`-free death check.
        health.revive();
        health.clear_absorb();
    }
    if let Some(mut energy) = ecs.write_storage::<Energy>().get_mut(entity) {
        energy.refresh();
    }
    let _ = ecs
        .write_storage::<Buffs>()
        .insert(entity, Buffs::default());
    let _ = ecs
        .write_storage::<Auras>()
        .insert(entity, Auras::default());
    let _ = ecs
        .write_storage::<EnteredAuras>()
        .insert(entity, EnteredAuras::default());
    let _ = ecs
        .write_storage::<Combo>()
        .insert(entity, Combo::default());
    let _ = ecs
        .write_storage::<comp::CharacterState>()
        .insert(entity, comp::CharacterState::default());
    let _ = ecs
        .write_storage::<comp::Controller>()
        .insert(entity, comp::Controller::default());
    let _ = ecs
        .write_storage::<comp::Stance>()
        .insert(entity, comp::Stance::default());

    // The `Agent` itself survives the trip — that is the point of not removing
    // it — but the *fight* it was in must not. Only the combat-scratch fields
    // are cleared; everything `SpawnEntityData` authored (`patrol_origin`,
    // `behavior`, `psyche`) is left exactly as it was, which is the whole
    // reason the component is kept in the first place.
    if let Some(agent) = ecs.write_storage::<Agent>().get_mut(entity) {
        agent.target = None;
        agent.inbox.clear();
        agent.sounds_heard.clear();
    }
}

/// Breaks any mounting link the banished entity takes part in, in whichever
/// direction. Mirrors `events::mounting`'s own unmount: only the *rider* side
/// is removed, and `State::maintain_links` drops the matching `Is<Mount>` on
/// the next tick.
fn unlink_mounts(server: &mut Server, entity: EcsEntity) {
    let rider = {
        let ecs = server.state.ecs();
        // If the banished entity is itself a mount, its rider is the one that
        // has to be freed.
        ecs.read_storage::<Is<Mount>>()
            .get(entity)
            .map(|is_mount| is_mount.rider)
            .and_then(|rider_uid| ecs.entity_from_uid(rider_uid))
    };
    let ecs = server.state.ecs();
    for entity in [Some(entity), rider].into_iter().flatten() {
        ecs.write_storage::<Is<Rider>>().remove(entity);
        ecs.write_storage::<Is<VolumeRider>>().remove(entity);
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
            for (id, actor) in &due_actors {
                // Restoring `presence` is the whole return: rtsim's load loop
                // re-materialises the entity the next time its chunk is
                // loaded, rebuilt from the actor's `EntityConfig` — i.e. fully
                // reset, for free.
                if !rtsim.set_actor_presence(*actor, true) {
                    warn!(
                        ?id,
                        "Banished rtsim actor no longer exists; dropping its record"
                    );
                }
            }
            // One borrow of the registry for the whole batch rather than one
            // per actor.
            rtsim.with_banishments(|banishments| {
                for (id, _) in &due_actors {
                    banishments.remove(*id);
                }
            });
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
        for (entity, record) in &due {
            let entity = *entity;
            // Anything that could have accumulated while parked. Almost always
            // a no-op — park already did this — but it costs nothing and keeps
            // "a returned creature is pristine" true by construction rather
            // than by an argument about which systems skip a `Pos`-less
            // entity.
            reset_transient_state(ecs, entity);

            let _ = ecs
                .write_storage::<Pos>()
                .insert(entity, Pos(record.return_pos));
            let _ = ecs
                .write_storage::<comp::Vel>()
                .insert(entity, comp::Vel::zero());
            ecs.write_storage::<Banished>().remove(entity);
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
    let (body, alignment, creature_kind, scale) = freestanding_archetype(record)?;

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

/// Whether this record describes a plain world mob (and therefore needs a
/// rehydrated entity) rather than an rtsim actor.
///
/// Exhaustive on purpose, mirroring `BanishedCreature::is_freestanding`: a
/// future `BanishedKind` variant must make this decision explicitly instead of
/// silently defaulting to "never rehydrated", which is a duplicate-creature
/// bug waiting to happen in the other direction.
#[cfg(feature = "worldgen")]
fn freestanding_archetype(
    record: &BanishedCreature,
) -> Option<(comp::Body, comp::Alignment, Option<comp::CreatureKind>, f32)> {
    match record.kind {
        BanishedKind::Freestanding {
            body,
            alignment,
            creature_kind,
            scale,
        } => Some((body, alignment, creature_kind, scale)),
        BanishedKind::RtsimActor(_) => None,
    }
}

/// Re-create a frozen entity for every persisted **`Freestanding`** record in
/// the rehydration queue. In practice that means every such record right after
/// a server start, since `Banishments::prepare` is the only thing that fills
/// the queue. A record whose deadline already passed while the server was down
/// comes back immediately.
///
/// ⚠️ **Known gap.** `Banishments`' own doc comment also claims the queue holds
/// "any [record] whose parked entity was lost mid-session", but nothing
/// detects that — there is no reconciliation sweep comparing live `Banished`
/// entities against the registry, so a parked entity destroyed by some other
/// route (an admin `/kill`, a future sweep) leaves its record orphaned and its
/// chunk permanently one spawn short. The two routes that actually made this
/// reachable are closed — a parked creature no longer dies of a lingering
/// damage-over-time buff (see `reset_transient_state`), and a rehydrated one
/// is no longer culled the tick it is created — so this is a durability gap,
/// not a live bug. Closing it needs a way to enumerate the registry's records,
/// which `Banishments` does not currently expose.
///
/// `RtsimActor` records never reach here — `prepare` does not queue them,
/// because rtsim re-materialises those itself once `return_due` restores their
/// `presence`. The `rehydration_entity_info` fork is a second, belt-and-braces
/// guard against a future caller of `queue_rehydration` that forgets.
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
            // The same anchor worldgen's own chunk supplement gives every mob
            // it spawns, using the chunk key the record already persists.
            // Without it the returned creature would be culled by a different
            // rule than its chunk-mates.
            npc: npc.with_anchor(comp::Anchor::Chunk(record.return_chunk)),
        });

        if record.returns_at_unix_secs <= now_unix_secs {
            // The deadline passed while the server was down: it is already
            // back, so it is spawned live and the record is forgotten. If its
            // chunk happens to be unloaded, `Server::tick`'s NPC-cleanup sweep
            // culls it later this tick — which is the ordinary fate of any
            // wild mob in an unloaded chunk, and correct: with the record
            // gone, worldgen no longer suppresses a spawn there.
            server
                .state
                .ecs()
                .read_resource::<RtSim>()
                .with_banishments(|banishments| {
                    banishments.remove(id);
                });
            continue;
        }

        // 🔴 Park in the same statement, **not** on the next tick's
        // `park_newly_banished`. `Server::tick` runs its "remove NPCs outside
        // every player's view distance" sweep later in this same tick, and
        // that sweep joins on `&Pos` and deletes anything whose chunk is not
        // loaded — which, at server start, is every chunk. Leaving the
        // rehydrated creature holding a `Pos` for one tick therefore deletes
        // it immediately and orphans its record forever (the rehydration
        // queue has already been drained above), making the whole
        // survives-a-restart guarantee inert.
        let ecs = server.state.ecs();
        match ecs.write_storage::<Banished>().insert(entity, Banished {
            id,
            return_pos: record.return_pos,
            returns_at_unix_secs: record.returns_at_unix_secs,
        }) {
            Ok(_) => {
                ecs.write_storage::<Pos>().remove(entity);
                ecs.write_storage::<comp::Vel>().remove(entity);
            },
            Err(e) => {
                // The entity died between creation and here. Re-queue rather
                // than drop: the record is still owed a return, and the next
                // tick builds a fresh entity for it. This cannot spin — a
                // successful insert leaves the queue empty.
                warn!(
                    ?e,
                    ?id,
                    "Failed to re-park a rehydrated banished creature; re-queueing it"
                );
                ecs.read_resource::<RtSim>()
                    .with_banishments(|banishments| {
                        banishments.queue_rehydration(id);
                    });
            },
        }
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

    /// `/banish <secs>` has to be able to produce a deadline seconds away, not
    /// only the authored 24–168 hour window, or the feature cannot be smoke
    /// tested by hand at all.
    #[test]
    fn an_admin_deadline_can_be_seconds_away() {
        let now = now_unix_secs();
        assert_eq!(admin_return_deadline(now, 10), now + 10);
        assert_eq!(admin_return_deadline(now, 0), now);
    }
}

/// `reset_transient_state` needs only a `&specs::World`, so the load-bearing
/// half of park/return is testable without a live `Server`.
#[cfg(test)]
mod limbo_reset_tests {
    use super::*;
    use common::{
        comp::{
            Body, BuffData, BuffKind, BuffSource, Stance, UtteranceKind,
            agent::{Sound, SoundKind, Target},
            bird_large,
            buff::{Buff, BuffCategory, DestInfo},
        },
        resources::{Secs, Time},
    };
    use specs::Builder;
    use vek::Vec3;

    /// Somewhere distinctive, so "the authored value survived" cannot pass by
    /// coincidence against a default.
    const PATROL_ORIGIN: Vec3<f32> = Vec3::new(1234.0, 5678.0, 90.0);

    fn phoenix_body() -> Body {
        Body::BirdLarge(bird_large::Body {
            species: bird_large::Species::Phoenix,
            body_type: bird_large::BodyType::Female,
        })
    }

    /// A parked creature, at 2% health with a burning debuff still on it —
    /// exactly the state a mid-combat banishment leaves behind.
    fn parked_creature() -> (specs::World, EcsEntity) {
        let mut world = specs::World::new();
        world.register::<Health>();
        world.register::<Energy>();
        world.register::<Buffs>();
        world.register::<Auras>();
        world.register::<EnteredAuras>();
        world.register::<Combo>();
        world.register::<comp::CharacterState>();
        world.register::<comp::Controller>();
        world.register::<Stance>();
        world.register::<Agent>();

        let body = phoenix_body();
        let mut health = Health::new(body);
        health.set_fraction(0.02);

        let mut buffs = Buffs::default();
        buffs.insert(
            Buff::new(
                BuffKind::Burning,
                BuffData::new(10.0, Some(Secs(60.0))),
                Vec::<BuffCategory>::new(),
                BuffSource::World,
                Time(0.0),
                DestInfo {
                    stats: None,
                    mass: None,
                },
                None,
                None,
            ),
            Time(0.0),
        );

        let mut combo = Combo::default();
        combo.change_by(7, 0.0);

        // The banisher, so the parked creature can be holding a real aggro
        // target rather than a synthetic one.
        let caster = world.create_entity().build();
        // Authored state that must survive the whole banishment, alongside the
        // combat scratch that must not.
        let mut agent = Agent::from_body(&body).with_patrol_origin(PATROL_ORIGIN);
        agent.target = Some(Target::new(caster, true, 0.0, true, None));
        agent.sounds_heard.push(Sound::new(
            SoundKind::Utterance(UtteranceKind::Angry, body),
            Vec3::zero(),
            1.0,
            0.0,
        ));

        let entity = world
            .create_entity()
            .with(health)
            .with(Energy::new(body))
            .with(buffs)
            .with(Auras::default())
            .with(EnteredAuras::default())
            .with(combo)
            .with(comp::CharacterState::default())
            .with(comp::Controller::default())
            .with(Stance::default())
            .with(agent)
            .build();
        (world, entity)
    }

    /// 🔴 The regression this pins is a silent, permanent data loss, not a
    /// cosmetic one. Neither `common/systems/src/buff.rs` nor
    /// `common/systems/src/stats.rs` joins on `Pos`, so a parked creature keeps
    /// taking damage-over-time and keeps being checked for death. Dying in
    /// limbo raises a `RemovalInfo::killed()` `DestroyEvent`, which **deletes
    /// the entity** — the creature never returns and its registry record is
    /// orphaned forever, permanently suppressing one worldgen spawn in its
    /// chunk. Clearing the buffs and topping the health at park time is what
    /// makes that unreachable.
    #[test]
    fn parking_clears_the_damage_over_time_that_would_kill_a_creature_in_limbo() {
        let (world, entity) = parked_creature();
        assert!(
            !world
                .read_storage::<Buffs>()
                .get(entity)
                .unwrap()
                .buffs
                .is_empty(),
            "test setup: the creature must start out burning"
        );

        reset_transient_state(&world, entity);

        assert!(
            world
                .read_storage::<Buffs>()
                .get(entity)
                .unwrap()
                .buffs
                .is_empty(),
            "a parked creature must not keep ticking a damage-over-time buff"
        );
        let healths = world.read_storage::<Health>();
        let health = healths.get(entity).unwrap();
        assert_eq!(health.fraction(), 1.0, "it must not be left near death");
        assert!(!health.is_dead);
    }

    /// The components are *reset*, never removed: `StateExt::create_npc`
    /// inserts all of them unconditionally and the buff and aura systems join
    /// on `Buffs`/`EnteredAuras` non-optionally, so a creature that came back
    /// missing one could never be buffed, debuffed, or touched by any aura
    /// again — including the spell that banished it.
    #[test]
    fn the_reset_replaces_every_component_rather_than_removing_it() {
        let (world, entity) = parked_creature();
        reset_transient_state(&world, entity);

        assert!(world.read_storage::<Buffs>().get(entity).is_some());
        assert!(world.read_storage::<Auras>().get(entity).is_some());
        assert!(world.read_storage::<EnteredAuras>().get(entity).is_some());
        assert!(world.read_storage::<Combo>().get(entity).is_some());
        assert_eq!(
            world.read_storage::<Combo>().get(entity).unwrap().counter(),
            0,
            "combo must not survive the trip"
        );
    }

    /// The `Agent` is never removed, so the return pass never has to rebuild a
    /// generic one — which is exactly what preserves the authored behaviour a
    /// rebuilt agent would lose. This test is the guard on both halves of that
    /// trade: the authored fields must survive, and the combat scratch must
    /// not.
    #[test]
    fn the_agent_keeps_its_authored_behaviour_but_loses_the_fight_it_was_in() {
        let (world, entity) = parked_creature();
        {
            let agents = world.read_storage::<Agent>();
            let agent = agents.get(entity).unwrap();
            assert!(agent.target.is_some(), "test setup: it must start aggroed");
            assert!(!agent.sounds_heard.is_empty(), "test setup");
        }

        reset_transient_state(&world, entity);

        let agents = world.read_storage::<Agent>();
        let agent = agents
            .get(entity)
            .expect("the agent must survive parking, not be rebuilt on return");
        assert_eq!(
            agent.patrol_origin,
            Some(PATROL_ORIGIN),
            "the authored patrol origin is the whole reason the component is kept"
        );
        assert!(
            agent.target.is_none(),
            "a creature gone for days must not come back still aggroed on its banisher"
        );
        assert!(agent.inbox.is_empty());
        assert!(agent.sounds_heard.is_empty(), "stale by days");
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
