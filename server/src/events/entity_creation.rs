use crate::{
    CharacterUpdater, Server, StateExt, client::Client, events::player::handle_exit_ingame,
    persistence::PersistedComponents, pet::tame_pet, presence::RepositionToFreeSpace, sys,
};
use common::{
    CachedSpatialGrid,
    combat::AttackTarget,
    comp::{
        self, Alignment, BehaviorCapability, Body, Collider, Density, Group, Inventory, ItemDrops,
        LightEmitter, Mass, Object, Ori, Pos, ThrownItem, TradingBehavior, Vel, WaypointArea,
        aura::{Aura, AuraKind, AuraTarget},
        body,
        buff::{BuffCategory, BuffChange, BuffData, BuffKind, BuffSource},
        item::MaterialStatManifest,
        ship,
        ship::figuredata::{VOXEL_COLLIDER_MANIFEST, VoxelCollider},
        tool::AbilityMap,
    },
    consts::{AIR_DENSITY, MAX_CAMPFIRE_RANGE},
    event::{
        ArcingEvent, BuffEvent, CreateAuraEntityEvent, CreateFloatingDiskEvent,
        CreateItemDropEvent, CreateNpcEvent, CreateNpcGroupEvent, CreateObjectEvent,
        CreatePoolEvent, CreateShipEvent, CreateSpecialEntityEvent, DeleteEvent, EventBus,
        InitializeCharacterEvent, InitializeSpectatorEvent, NpcBuilder, ShockwaveEvent, ShootEvent,
        SummonBeamPillarsEvent, ThrowEvent, UpdateCharacterDataEvent,
    },
    generation::SpecialEntity,
    mounting::{Mounting, Volume, VolumeMounting, VolumePos},
    outcome::Outcome,
    resources::{Secs, Time},
    terrain::{Block, BlockKind, TerrainGrid},
    uid::{IdMaps, Uid},
    util::Dir,
    vol::IntoFullVolIterator,
};
use common_net::{msg::ServerGeneral, sync::WorldSyncExt};
use specs::{Builder, Entity as EcsEntity, Join, WorldExt};
use std::{sync::Arc, time::Duration};
use vek::{Rgb, Vec3};

use super::group_manip::update_map_markers;

pub fn handle_initialize_character(server: &mut Server, ev: InitializeCharacterEvent) {
    let updater = server.state.ecs().fetch::<CharacterUpdater>();
    let pending_database_action = updater.has_pending_database_action(ev.character_id);
    drop(updater);

    if !pending_database_action {
        let clamped_vds = ev
            .requested_view_distances
            .clamp(server.settings().max_view_distance);
        server
            .state
            .initialize_character_data(ev.entity, ev.character_id, clamped_vds);
        // Correct client if its requested VD is too high.
        if ev.requested_view_distances.terrain != clamped_vds.terrain {
            server.notify_client(
                ev.entity,
                ServerGeneral::SetViewDistance(clamped_vds.terrain),
            );
        }
    } else {
        // A character delete or update was somehow initiated after the login commenced,
        // so kick the client out of "ingame" without saving any data and abort
        // the character loading process.
        handle_exit_ingame(server, ev.entity, true);
    }
}

pub fn handle_initialize_spectator(server: &mut Server, ev: InitializeSpectatorEvent) {
    let clamped_vds = ev.1.clamp(server.settings().max_view_distance);
    server.state.initialize_spectator_data(ev.0, clamped_vds);
    // Correct client if its requested VD is too high.
    if ev.1.terrain != clamped_vds.terrain {
        server.notify_client(ev.0, ServerGeneral::SetViewDistance(clamped_vds.terrain));
    }
    sys::subscription::initialize_region_subscription(server.state.ecs(), ev.0);
}

pub fn handle_loaded_character_data(server: &mut Server, ev: UpdateCharacterDataEvent) {
    let loaded_components = PersistedComponents {
        body: ev.components.0,
        hardcore: ev.components.1,
        character_class: ev.components.2,
        stats: ev.components.3,
        skill_set: ev.components.4,
        inventory: ev.components.5,
        waypoint: ev.components.6,
        pets: ev.components.7,
        active_abilities: ev.components.8,
        map_marker: ev.components.9,
        ethos: ev.components.10,
        background: ev.components.11,
        pact: ev.components.12,
        trigger_slots: ev.components.13,
        spell_mastery: ev.components.14,
    };
    if let Some(marker) = loaded_components.map_marker {
        server.notify_client(
            ev.entity,
            ServerGeneral::MapMarker(comp::MapMarkerUpdate::Owned(comp::MapMarkerChange::Update(
                marker.0,
            ))),
        );
    }

    let result_msg = if let Err(err) = server
        .state
        .update_character_data(ev.entity, loaded_components)
    {
        handle_exit_ingame(server, ev.entity, false); // remove client from in-game state
        ServerGeneral::CharacterDataLoadResult(Err(err))
    } else {
        sys::subscription::initialize_region_subscription(server.state.ecs(), ev.entity);
        // We notify the client with the metadata result from the operation.
        ServerGeneral::CharacterDataLoadResult(Ok(ev.metadata))
    };
    server.notify_client(ev.entity, result_msg);
}

pub fn handle_create_npc(server: &mut Server, ev: CreateNpcEvent) -> EcsEntity {
    // Read before `create_npc` below takes an exclusive borrow of the ecs
    // `World` for the rest of the builder chain.
    let time = *server.state.ecs().read_resource::<Time>();
    // Destruct the builder to ensure all fields are exhaustive
    let NpcBuilder {
        stats,
        skill_set,
        health,
        poise,
        inventory,
        body,
        mut agent,
        alignment,
        ethos,
        scale,
        anchor,
        loot,
        pets,
        rtsim_actor,
        projectile,
        heads,
        death_effects,
        rider_effects,
        rider,
        incorporeal,
        phantom_illusion,
        delete_after,
        oracle_event_id,
        chain_summon_cost,
    } = ev.npc;
    let entity = server
        .state
        .create_npc(
            ev.pos, ev.ori, stats, skill_set, health, poise, inventory, body, scale,
        )
        .maybe_with(heads)
        .maybe_with(death_effects)
        .maybe_with(rider_effects);
    // Overrides the `body.collider()` `create_npc` already inserted --
    // `WriteStorage::insert` replaces rather than panics on a second `.with`
    // for the same component, the same pattern `alignment` below relies on.
    let entity = if incorporeal {
        entity.with(Collider::Point)
    } else {
        entity
    };

    let entity = if phantom_illusion {
        entity.with(comp::PhantomIllusion)
    } else {
        entity
    };

    let entity = entity.maybe_with(delete_after.map(|timeout| Object::DeleteAfter {
        spawned_at: time,
        timeout,
    }));

    // Attribute this entity to the ORACLE `DmEvent` that spawned it, if any.
    // Cloned rather than moved: `oracle_event_id` is still needed below to
    // propagate onto recursively-created rider/pet entities.
    let entity =
        entity.maybe_with(
            oracle_event_id
                .clone()
                .map(|event_id| crate::oracle::OracleSpawned {
                    event_id,
                    spawned_at: time,
                }),
        );

    if let Some(agent) = &mut agent
        && let Alignment::Owned(_) = &alignment
    {
        agent.behavior.allow(BehaviorCapability::TRADE);
        agent.behavior.trading_behavior = TradingBehavior::AcceptFood;
    }

    let entity = entity.with(alignment);

    // BL-33: NPCs carry a moral alignment (humanoids get one seeded from their
    // AI alignment; non-agents get None). Feeds AURORA.
    let entity = if let Some(ethos) = ethos {
        entity.with(ethos)
    } else {
        entity
    };

    let entity = if let Some(agent) = agent {
        entity.with(agent)
    } else {
        entity
    };

    let entity = if let Some(drop_items) = loot.to_items() {
        entity.with(ItemDrops(drop_items))
    } else {
        entity
    };

    let entity = if let Some(home_chunk) = anchor {
        entity.with(home_chunk)
    } else {
        entity
    };

    // Rtsim entity added to IdMaps below.
    let entity = if let Some(rtsim_actor) = rtsim_actor {
        entity.with(rtsim_actor).with(RepositionToFreeSpace {
            needs_ground: false,
            modify_waypoints: true,
        })
    } else {
        entity
    };

    let entity = if let Some(projectile) = projectile {
        entity.with(projectile)
    } else {
        entity
    };

    let new_entity = entity.build();

    if let Some(rtsim_actor) = rtsim_actor {
        server
            .state()
            .ecs()
            .write_resource::<IdMaps>()
            .add_rtsim(rtsim_actor, new_entity);
    }

    // N27-O: the server-authoritative Cadena point-pool gate. Runs BEFORE
    // group registration, and — if refused — deletes `new_entity` and
    // returns immediately, before the group/anchor code below ever treats
    // it as live. `chain_summon_cost` is `None` for every summon that isn't
    // a Cadena fiend (see its doc comment on `NpcBuilder`), so this is a
    // no-op for every other caller of `handle_create_npc` (admin `/spawn`,
    // riders, tamed pets, world/rtsim/ORACLE spawns, every pre-existing
    // non-Cadena `BasicSummon` ability).
    //
    // `CharacterAbility::requirements_paid`'s `BasicSummon` arm already
    // verified the whole cast's batch cost was affordable before this
    // character state was ever entered (the "not activatable client-side"
    // half of the acceptance bar) — this re-check, per creature as it
    // actually spawns, is the true last-line authority a modified or
    // desynced client cannot bypass.
    if let comp::Alignment::Owned(owner_uid) = alignment
        && let Some(cost) = chain_summon_cost
    {
        let ecs = server.state.ecs();
        let Some(owner) = ecs.entity_from_uid(owner_uid) else {
            // No resolvable owner at all -- never charge or anchor an
            // orphaned summon; let it despawn on the very next unload sweep
            // like any other unowned entity.
            let _ = server.state.delete_entity_recorded(new_entity);
            return new_entity;
        };
        let new_uid = *ecs
            .read_storage::<Uid>()
            .get(new_entity)
            .expect("create_entity_synced always assigns a Uid");
        if charge_chain_summon(ecs, owner, new_uid, cost) {
            // Mirrors `server/src/pet.rs`'s `tame_pet` anchor: without it, a
            // Cadena fiend standing outside its owner's own load radius
            // would despawn on the next chunk-unload sweep even while its
            // owner is still connected. Scoped to `chain_summon_cost.is_some()`
            // rather than every `Alignment::Owned` NPC on purpose -- see
            // this block's doc comment.
            let _ = ecs
                .write_storage()
                .insert(new_entity, comp::Anchor::Entity(owner));
        } else {
            tracing::warn!(
                ?owner_uid,
                cost,
                "Refusing a Cadena summon: would exceed the owner's chain pool"
            );
            let _ = server.state.delete_entity_recorded(new_entity);
            return new_entity;
        }
    }

    // Add to group system if a pet
    if let comp::Alignment::Owned(owner_uid) = alignment {
        let state = server.state();
        let uids = state.ecs().read_storage::<Uid>();
        let clients = state.ecs().read_storage::<Client>();
        let mut group_manager = state.ecs().write_resource::<comp::group::GroupManager>();
        if let Some(owner) = state.ecs().entity_from_uid(owner_uid) {
            let map_markers = state.ecs().read_storage::<comp::MapMarker>();
            group_manager.new_pet(
                new_entity,
                owner,
                &mut state.ecs().write_storage(),
                &state.ecs().entities(),
                &state.ecs().read_storage(),
                &uids,
                &mut |entity, group_change| {
                    group_change
                        .try_map_ref(|e| uids.get(*e).copied())
                        .zip(clients.get(entity))
                        .map(|(g, c)| {
                            // Might be unnecessary, but maybe pets can somehow have map
                            // markers in the future
                            update_map_markers(&map_markers, &uids, c, &group_change);
                            c.send_fallible(ServerGeneral::GroupUpdate(g));
                        });
                },
            );
        }
    } else if let Some(group) = alignment.group() {
        let _ = server.state.ecs().write_storage().insert(new_entity, group);
    }

    if let Some(mut rider) = rider {
        // Riders are created via a nested `NpcBuilder` that does not inherit
        // the parent's ORACLE attribution automatically. Propagate it
        // explicitly so a mounted rider is still counted against the live
        // ceiling and shows up under the same event id.
        rider.oracle_event_id = oracle_event_id.clone();
        let rider_entity = handle_create_npc(server, CreateNpcEvent {
            pos: ev.pos,
            ori: Ori::default(),
            npc: *rider,
        });
        let uids = server.state().ecs().read_storage::<Uid>();
        let link = Mounting {
            mount: *uids.get(new_entity).expect("We just created this entity"),
            rider: *uids.get(rider_entity).expect("We just created this entity"),
        };
        drop(uids);
        server
            .state
            .link(link)
            .expect("We just created these entities");
    }

    for (mut pet, offset) in pets {
        // Same propagation as the rider above: a pet spawned by an ORACLE
        // trigger must stay tagged and countable, not slip past the ceiling
        // untagged.
        pet.oracle_event_id = oracle_event_id.clone();
        let pet_entity = handle_create_npc(server, CreateNpcEvent {
            pos: comp::Pos(ev.pos.0 + offset),
            ori: Ori::from_unnormalized_vec(offset).unwrap_or_default(),
            npc: pet,
        });

        tame_pet(server.state.ecs(), pet_entity, new_entity);
    }

    new_entity
}

/// N27-O: the server-authoritative half of the Cadena point-pool gate.
/// Returns `true` (and has charged `new_entity_uid` against `owner`'s
/// `Summons` ledger) if `owner`'s pool has room for `cost`; `false` (having
/// charged nothing) otherwise, in which case the caller must delete the
/// entity it already built. Takes `&specs::World` rather than `&mut Server`
/// so it is unit-testable without constructing a full `Server` -- mirrors
/// `release_chain_summon_charge` in `entity_manipulation`.
fn charge_chain_summon(
    ecs: &specs::World,
    owner: EcsEntity,
    new_entity_uid: Uid,
    cost: u16,
) -> bool {
    let pool = {
        let pacts = ecs.read_storage::<comp::Pact>();
        let skill_sets = ecs.read_storage::<comp::SkillSet>();
        pacts
            .get(owner)
            .zip(skill_sets.get(owner))
            .map_or(0, |(pact, skill_set)| pact.chain_summon_pool(skill_set))
    };
    let mut summons_storage = ecs.write_storage::<comp::Summons>();
    let spent = summons_storage.get(owner).map_or(0, comp::Summons::spent);
    if spent.saturating_add(cost) > pool {
        return false;
    }
    summons_storage
        .entry(owner)
        .expect("owner entity was just resolved live")
        .or_insert_with(comp::Summons::default)
        .charge(new_entity_uid, cost);
    true
}

pub fn handle_create_npc_group(server: &mut Server, ev: CreateNpcGroupEvent) {
    let mut npcs = ev
        .npcs
        .into_iter()
        .map(|ev| handle_create_npc(server, ev))
        .collect::<Vec<_>>()
        .into_iter();
    let Some(leader) = npcs.next() else {
        return;
    };

    let ecs = server.state().ecs();
    let entities = ecs.entities();
    let uids = ecs.read_storage::<Uid>();
    let alignments = ecs.read_storage::<Alignment>();
    let mut groups = ecs.write_storage::<Group>();
    let mut group_manager = ecs.write_resource::<comp::group::GroupManager>();

    if groups.get(leader).is_some() {
        return;
    }

    for entity in npcs {
        group_manager.add_group_member(
            leader,
            entity,
            &entities,
            &mut groups,
            &alignments,
            &uids,
            |_, _| {},
        );
    }
}

pub fn handle_create_ship(server: &mut Server, ev: CreateShipEvent) {
    let collider = ev.ship.make_collider();
    let voxel_colliders_manifest = VOXEL_COLLIDER_MANIFEST.read();

    // TODO: Find better solution for this, maybe something like a serverside block
    // of interests.
    let (mut steering, mut _seats) = {
        let mut steering = Vec::new();
        let mut seats = Vec::new();

        for (pos, block) in collider
            .get_vol(&voxel_colliders_manifest)
            .iter()
            .flat_map(|voxel_collider| voxel_collider.volume().full_vol_iter())
        {
            match (block.is_controller(), block.is_mountable()) {
                (true, true) => steering.push((pos, *block)),
                (false, true) => seats.push((pos, *block)),
                _ => {},
            }
        }
        (steering.into_iter(), seats.into_iter())
    };

    let mut entity = server
        .state
        .create_ship(ev.pos, ev.ori, ev.ship, |_| collider);
    /*
    if let Some(mut agent) = agent {
        let (kp, ki, kd) = pid_coefficients(&Body::Ship(ship));
        fn pure_z(sp: Vec3<f32>, pv: Vec3<f32>) -> f32 { (sp - pv).z }
        agent =
            agent.with_position_pid_controller(PidController::new(kp, ki, kd, pos.0, 0.0, pure_z));
        entity = entity.with(agent);
    }
    */
    if let Some(rtsim_vehicle) = ev.rtsim_actor {
        entity = entity.with(rtsim_vehicle);
    }
    let entity = entity.build();

    if let Some(rtsim_actor) = ev.rtsim_actor {
        server
            .state()
            .ecs()
            .write_resource::<IdMaps>()
            .add_rtsim(rtsim_actor, entity);
    }

    if let Some(driver) = ev.driver {
        let npc_entity = handle_create_npc(server, CreateNpcEvent {
            pos: ev.pos,
            ori: ev.ori,
            npc: driver,
        });

        let uids = server.state.ecs().read_storage::<Uid>();
        let (rider_uid, mount_uid) = uids
            .get(npc_entity)
            .copied()
            .zip(uids.get(entity).copied())
            .expect("Couldn't get Uid from newly created ship and npc");
        drop(uids);

        if let Some((steering_pos, steering_block)) = steering.next() {
            server
                .state
                .link(VolumeMounting {
                    pos: VolumePos {
                        kind: Volume::Entity(mount_uid),
                        pos: steering_pos,
                    },
                    block: steering_block,
                    rider: rider_uid,
                })
                .expect("Failed to link driver to ship");
        } else {
            server
                .state
                .link(Mounting {
                    mount: mount_uid,
                    rider: rider_uid,
                })
                .expect("Failed to link driver to ship");
        }
    }

    /*
    for passenger in ev.passengers {
        let npc_entity = handle_create_npc(server, CreateNpcEvent {
            pos: Pos(ev.pos.0 + Vec3::unit_z() * 5.0),
            ori: ev.ori,
            npc: passenger,
            rider: None,
        });
        if let Some((rider_pos, rider_block)) = seats.next() {
            let uids = server.state.ecs().read_storage::<Uid>();
            let (rider_uid, mount_uid) = uids
                .get(npc_entity)
                .copied()
                .zip(uids.get(entity).copied())
                .expect("Couldn't get Uid from newly created ship and npc");
            drop(uids);

            server
                .state
                .link(VolumeMounting {
                    pos: VolumePos {
                        kind: Volume::Entity(mount_uid),
                        pos: rider_pos,
                    },
                    block: rider_block,
                    rider: rider_uid,
                })
                .expect("Failed to link passanger to ship");
        }
    }
    */
}

pub fn handle_shoot(server: &mut Server, ev: ShootEvent) {
    let state = server.state_mut();

    let pos = ev.pos.0;

    let vel = *ev.dir * ev.speed + ev.source_vel.map_or(Vec3::zero(), |v| v.0);

    // Add an outcome
    state
        .ecs()
        .read_resource::<EventBus<Outcome>>()
        .emit_now(Outcome::ProjectileShot {
            pos,
            body: ev.body,
            vel,
        });

    if let Some(owner) = ev.entity {
        state
            .ecs()
            .read_resource::<EventBus<BuffEvent>>()
            .emit_now(BuffEvent {
                entity: owner,
                buff_change: BuffChange::RemoveByCategory {
                    all_required: vec![BuffCategory::WeaponCoating],
                    any_required: Vec::new(),
                    none_required: Vec::new(),
                },
            });
    }

    state
        .create_projectile(Pos(pos), Vel(vel), ev.body, ev.projectile)
        .maybe_with(ev.light)
        .maybe_with(ev.object)
        .maybe_with(ev.marker)
        .build();
}

pub fn handle_throw(server: &mut Server, ev: ThrowEvent) {
    let state = server.state_mut();

    let thrown_item = state
        .ecs()
        .write_storage::<Inventory>()
        .get_mut(ev.entity)
        .and_then(|mut inv| {
            if let Some(thrown_item) = inv.equipped(ev.equip_slot) {
                let ability_map = state.ecs().read_resource::<AbilityMap>();
                let msm = state.ecs().read_resource::<MaterialStatManifest>();
                let time = state.ecs().read_resource::<Time>();

                // If stackable, try to remove the throwable from inv stacks before
                // removing the equipped one to avoid having to reequip after each throw
                if let Some(inv_slot) = inv.get_slot_of_item(thrown_item)
                    && thrown_item.is_stackable()
                {
                    inv.take(inv_slot, &ability_map, &msm)
                } else {
                    inv.replace_loadout_item(ev.equip_slot, None, *time)
                }
            } else {
                None
            }
        })
        .map(|mut thrown_item| {
            thrown_item.put_in_world();
            ThrownItem(thrown_item)
        });

    if let Some(thrown_item) = thrown_item {
        let body = Body::Item(body::item::Body::from(&thrown_item));

        let pos = ev.pos.0;

        let vel = *ev.dir * ev.speed
            + state
                .ecs()
                .read_storage::<Vel>()
                .get(ev.entity)
                .map_or(Vec3::zero(), |v| v.0);

        // Add an outcome
        state
            .ecs()
            .read_resource::<EventBus<Outcome>>()
            .emit_now(Outcome::ProjectileShot { pos, body, vel });

        state
            .create_projectile(Pos(pos), Vel(vel), body, ev.projectile)
            .with(thrown_item)
            .maybe_with(ev.light)
            .maybe_with(ev.object)
            .build();
    }
}

pub fn handle_shockwave(server: &mut Server, ev: ShockwaveEvent) {
    let state = server.state_mut();
    state
        .create_shockwave(ev.properties, ev.pos, ev.ori)
        .build();
}

pub fn handle_arc(server: &mut Server, ev: ArcingEvent) {
    let state = server.state_mut();
    state
        .create_arcing(ev.arc, ev.target, ev.owner, ev.pos)
        .build();
}

pub fn handle_create_pool(server: &mut Server, ev: CreatePoolEvent) {
    let state = server.state_mut();
    //Pool entities must inherit the xy orientation of their spawner to maintain
    // visual consistency
    let flat_ori =
        comp::Ori::from_unnormalized_vec(ev.ori.look_vec().xy().with_z(0.0)).unwrap_or_default();
    let pos = comp::Pos(ev.pos.0 + vek::Vec3::unit_z() * 0.05);
    state
        .create_pool(ev.properties, ev.owner, pos, flat_ori)
        .build();
}

pub fn handle_create_special_entity(server: &mut Server, ev: CreateSpecialEntityEvent) {
    let time = server.state.get_time();

    match ev.entity {
        SpecialEntity::Waypoint => {
            server
                .state
                .create_object(Pos(ev.pos), comp::object::Body::CampfireLit)
                .with(LightEmitter {
                    col: Rgb::new(1.0, 0.3, 0.1),
                    strength: 5.0,
                    flicker: 1.0,
                    animated: true,
                    dir: None,
                })
                .with(WaypointArea::default())
                .with(comp::Immovable)
                .with(comp::EnteredAuras::default())
                .with(comp::Auras::new(vec![
                    Aura::new(
                        AuraKind::Buff {
                            kind: BuffKind::RestingHeal,
                            data: BuffData::new(0.02, Some(Secs(1.0))),
                            category: None,
                            source: BuffSource::World,
                            pool_split: None,
                        },
                        MAX_CAMPFIRE_RANGE,
                        None,
                        AuraTarget::All,
                        Time(time),
                        None,
                    ),
                    Aura::new(
                        AuraKind::Buff {
                            kind: BuffKind::Burning,
                            data: BuffData::new(2.0, Some(Secs(10.0))),
                            category: None,
                            source: BuffSource::World,
                            pool_split: None,
                        },
                        0.7,
                        None,
                        AuraTarget::All,
                        Time(time),
                        None,
                    ),
                ]))
                .build();
        },
        SpecialEntity::Teleporter(portal) => {
            server
                .state
                .create_teleporter(comp::Pos(ev.pos), portal)
                .build();
        },
        SpecialEntity::ArenaTotem { range } => {
            server
                .state
                .create_object(Pos(ev.pos), comp::object::Body::GnarlingTotemGreen)
                .with(comp::Immovable)
                .with(comp::EnteredAuras::default())
                .with(comp::Auras::new(vec![
                    Aura::new(
                        AuraKind::FriendlyFire,
                        range,
                        None,
                        AuraTarget::All,
                        Time(time),
                        None,
                    ),
                    Aura::new(
                        AuraKind::ForcePvP,
                        range,
                        None,
                        AuraTarget::All,
                        Time(time),
                        None,
                    ),
                ]))
                .build();
        },
    }
}

pub fn handle_create_item_drop(server: &mut Server, ev: CreateItemDropEvent) {
    server
        .state
        .create_item_drop(ev.pos, ev.ori, ev.vel, ev.item, ev.loot_owner);
}

pub fn handle_create_object(
    server: &mut Server,
    CreateObjectEvent {
        pos,
        vel,
        body,
        object,
        item,
        light_emitter,
        stats,
    }: CreateObjectEvent,
) {
    match object {
        Some(
            object @ Object::Crux {
                owner,
                scale,
                range,
                strength,
                duration,
                ..
            },
        ) => {
            let state = server.state_mut();
            let time = *state.ecs().read_resource::<Time>();

            // HACK: Spawn slightly damaged so that the health bar is visible and players
            // are aware it is a killable entity
            let mut health = comp::Health::new(Body::Object(body));
            health.set_fraction(0.99996);

            let crux = state
                .create_object(pos, body)
                .with(object)
                .maybe_with(light_emitter)
                .maybe_with(stats)
                .with(comp::Scale(scale))
                .with(health)
                .with(comp::Energy::new(Body::Object(body)))
                .with(comp::Poise::new(Body::Object(body)))
                .with(comp::SkillSet::default())
                .with(comp::Buffs::default())
                .with(comp::Inventory::with_empty())
                .with(comp::Immovable)
                .with(comp::Auras::new(vec![Aura::new(
                    AuraKind::Buff {
                        kind: BuffKind::Heatstroke,
                        data: BuffData {
                            strength,
                            duration: Some(duration),
                            delay: None,
                            secondary_duration: None,
                            misc_data: None,
                        },
                        category: None,
                        source: BuffSource::World,
                        pool_split: None,
                    },
                    range,
                    None,
                    AuraTarget::NotGroupOf(owner),
                    time,
                    None,
                )]))
                .with(comp::projectile::ProjectileHitEntities::default())
                .build();

            if let Some(owner) = state.ecs().read_resource::<IdMaps>().uid_entity(owner) {
                let mut group_manager = state.ecs().write_resource::<comp::group::GroupManager>();
                group_manager.new_pet(
                    crux,
                    owner,
                    &mut state.ecs().write_storage(),
                    &state.ecs().entities(),
                    &state.ecs().read_storage(),
                    &state.ecs().read_storage::<Uid>(),
                    &mut |_, _| {},
                );
            }
        },
        _ => {
            server
                .state
                .create_object(pos, body)
                .with(vel)
                .maybe_with(object)
                .maybe_with(item)
                .maybe_with(light_emitter)
                .maybe_with(stats)
                .build();
        },
    }
}

/// Spawns a `floating_disk`: a `Body::Ship(ship::Body::Volume)` prop with a
/// procedurally-built flat-disk collider, driven each tick by
/// `Object::FloatingDisk`'s arm in `server/src/sys/object.rs`. Built directly
/// rather than through `create_ship`/`create_object` — neither can express
/// this shape, and `create_ship` adds a Controller/Inventory/CharacterState/
/// Energy/Stats set this prop deliberately does not carry (no nameplate, no
/// health bar, never persisted).
pub fn handle_create_floating_disk(server: &mut Server, ev: CreateFloatingDiskEvent) {
    let state = server.state_mut();
    let time = *state.ecs().read_resource::<Time>();

    // One disk per caster: recasting dismisses any disk this caster already
    // has (also papers over the disabled voxel-voxel collision gap between
    // two disks).
    let existing: Vec<EcsEntity> = {
        let entities = state.ecs().entities();
        let objects = state.ecs().read_storage::<Object>();
        (&entities, &objects)
            .join()
            .filter_map(|(entity, object)| match object {
                Object::FloatingDisk { owner, .. } if *owner == ev.owner => Some(entity),
                _ => None,
            })
            .collect()
    };
    if !existing.is_empty() {
        let delete_events = state.ecs().read_resource::<EventBus<DeleteEvent>>();
        for entity in existing {
            delete_events.emit_now(DeleteEvent(entity));
        }
    }

    let radius = ev.radius;
    let sz = Vec3::new(5u32, 5, 1);
    let half = sz.map(|e| e as i32) / 2;
    let collider = Collider::Volume(Arc::new(VoxelCollider::from_fn(sz, move |rpos| {
        let offset = (rpos.xy() - half.xy()).map(|e| e as f32);
        if offset.magnitude_squared() <= radius * radius {
            Block::new(BlockKind::Misc, Rgb::new(130, 110, 80))
        } else {
            Block::empty()
        }
    })));

    state
        .ecs_mut()
        .create_entity_synced()
        .with(Pos(ev.pos.0 + Vec3::unit_z() * ev.hover_height))
        .with(Vel(Vec3::zero()))
        .with(Ori::default())
        .with(Mass(40.0))
        .with(Density(AIR_DENSITY))
        .with(collider)
        .with(Body::Ship(ship::Body::Volume))
        .with(Object::FloatingDisk {
            owner: ev.owner,
            spawned_at: time,
            timeout: ev.timeout,
            follow_distance: ev.follow_distance,
            hover_height: ev.hover_height,
            max_owner_distance: ev.max_owner_distance,
        })
        .with(Alignment::Owned(ev.owner))
        .build();
}

pub fn handle_create_aura_entity(server: &mut Server, ev: CreateAuraEntityEvent) {
    let time = *server.state.ecs().read_resource::<Time>();
    let mut entity = server
        .state
        .ecs_mut()
        .create_entity_synced()
        .with(ev.pos)
        .with(comp::Vel(Vec3::zero()))
        .with(comp::Ori::default())
        .with(ev.auras)
        .with(comp::Alignment::Owned(ev.creator_uid));

    // If a duration is specified, create a projectile component for the entity
    if let Some(dur) = ev.duration {
        let object = comp::Object::DeleteAfter {
            spawned_at: time,
            timeout: Duration::from_secs_f64(dur.0),
        };
        entity = entity.with(object);
    }
    entity.build();
}

pub fn handle_summon_beam_pillars(server: &mut Server, ev: SummonBeamPillarsEvent) {
    let ecs = server.state().ecs();

    let Some((&Pos(center), &summoner_alignment)) = ecs
        .read_storage::<Pos>()
        .get(ev.summoner)
        .zip(ecs.read_storage::<Alignment>().get(ev.summoner))
    else {
        return;
    };

    let summon_pillar = |server: &mut Server, pos: Vec3<f32>, spawned_at| {
        let integer_pos = pos.map(|x| x as i32);
        let ground_height = server
            .state()
            .ecs()
            .read_resource::<TerrainGrid>()
            .find_ground(integer_pos)
            .z as f32;

        // If the distance from the attempted spawn position and the nearest valid
        // position is too far, avoid spawning the fire pillar to prevent
        // ability usage in a cave from spawning pillars on the surface or other
        // edge cases
        if (ground_height - pos.z).abs() <= 16.0 {
            let ecs = server.state_mut().ecs_mut();

            let pillar = ecs
                .create_entity_synced()
                .with(Pos(pos.with_z(ground_height)))
                .with(Ori::from(Dir::up()))
                .with(comp::Object::BeamPillar {
                    spawned_at,
                    buildup_duration: ev.buildup_duration,
                    attack_duration: ev.attack_duration,
                    beam_duration: ev.beam_duration,
                    radius: ev.radius,
                    height: ev.height,
                    damage: ev.damage,
                    damage_effect: ev.damage_effect.clone(),
                    dodgeable: ev.dodgeable,
                    tick_rate: ev.tick_rate,
                    specifier: ev.specifier,
                    indicator_specifier: ev.indicator_specifier,
                })
                .build();

            let mut group_manager = ecs.write_resource::<comp::group::GroupManager>();
            group_manager.new_pet(
                pillar,
                ev.summoner,
                &mut ecs.write_storage(),
                &ecs.entities(),
                &ecs.read_storage(),
                &ecs.read_storage::<Uid>(),
                &mut |_, _| {},
            );
        }
    };

    let spawned_at = *ecs.read_resource::<Time>();
    match ev.target {
        AttackTarget::AllInRange(range) => {
            let enemy_positions = ecs
                .read_resource::<CachedSpatialGrid>()
                .0
                .in_circle_aabr(center.xy(), range)
                .filter(|entity| {
                    ecs.read_storage::<Alignment>()
                        .get(*entity)
                        .is_some_and(|alignment| summoner_alignment.hostile_towards(*alignment))
                })
                .filter(|entity| {
                    ecs.read_storage::<comp::Group>()
                        .get(ev.summoner)
                        .is_none_or(|summoner_group| {
                            ecs.read_storage::<comp::Group>()
                                .get(*entity)
                                .is_none_or(|entity_group| summoner_group != entity_group)
                        })
                })
                .filter_map(|nearby_enemy| {
                    ecs.read_storage::<Pos>()
                        .get(nearby_enemy)
                        .map(|Pos(pos)| *pos)
                })
                .collect::<Vec<_>>();

            for enemy_pos in enemy_positions.into_iter() {
                summon_pillar(server, enemy_pos, spawned_at);
            }
        },
        AttackTarget::Pos(pos) => {
            summon_pillar(server, pos, spawned_at);
        },
        AttackTarget::Entity(entity) => {
            let pos = ecs.read_storage::<Pos>().get(entity).map(|pos| pos.0);
            if let Some(pos) = pos {
                summon_pillar(server, pos, spawned_at);
            }
        },
    }
}

/// N27-O: `charge_chain_summon`, the server-authoritative gate/charge
/// `handle_create_npc` applies per creature as it actually spawns.
#[cfg(test)]
mod charge_chain_summon_tests {
    use super::*;
    use common::{
        comp::{Pact, PactBoon},
        skillset_builder::SkillSetBuilder,
    };
    use specs::{Builder, World, WorldExt};

    fn mock_world() -> World {
        let mut world = World::new();
        world.register::<comp::Pact>();
        world.register::<comp::SkillSet>();
        world.register::<comp::Summons>();
        world
    }

    fn chain_warlock(world: &mut World, level: u16) -> EcsEntity {
        let mut skill_set = SkillSetBuilder::default().build();
        skill_set.set_level(level);
        world
            .create_entity()
            .with(Pact {
                boon: Some(PactBoon::Chain),
                ..Pact::default()
            })
            .with(skill_set)
            .build()
    }

    fn uid(n: u64) -> Uid { Uid::from(core::num::NonZeroU64::new(n).unwrap()) }

    /// A level-1 Chain Warlock's pool is 2 (`chain_pool(1, 0)`); a
    /// same-cost second creature must fit exactly, and a third must not.
    #[test]
    fn a_fresh_warlocks_pool_admits_exactly_two_one_point_summons() {
        let mut world = mock_world();
        let owner = chain_warlock(&mut world, 1);
        let ecs = &world;

        assert!(charge_chain_summon(ecs, owner, uid(1), 1));
        assert!(charge_chain_summon(ecs, owner, uid(2), 1));
        assert!(
            !charge_chain_summon(ecs, owner, uid(3), 1),
            "a third point must not fit a pool of 2 with 2 already spent"
        );

        let summons = world.read_component::<comp::Summons>();
        assert_eq!(summons.get(owner).unwrap().spent(), 2);
    }

    /// The single-creature case: a cost that alone exceeds the whole pool
    /// is refused outright and charges nothing.
    #[test]
    fn a_single_summon_costing_more_than_the_whole_pool_is_refused() {
        let mut world = mock_world();
        let owner = chain_warlock(&mut world, 1); // pool == 2

        assert!(!charge_chain_summon(&world, owner, uid(1), 3));

        let summons = world.read_component::<comp::Summons>();
        assert_eq!(
            summons.get(owner).map(comp::Summons::spent).unwrap_or(0),
            0,
            "a refused charge must leave the ledger untouched"
        );
    }

    /// A character with no `Pact` at all (never a Warlock) has a pool of 0
    /// -- refuse rather than panic or default to unlimited.
    #[test]
    fn no_pact_component_means_a_pool_of_zero() {
        let mut world = mock_world();
        let owner = world
            .create_entity()
            .with(SkillSetBuilder::default().build())
            .build();

        assert!(!charge_chain_summon(&world, owner, uid(1), 1));
    }

    /// A Warlock with a pact, but a boon OTHER than Chain, also has a pool
    /// of 0 -- e.g. casting a Conjuration spell must never be charged
    /// against a pool that doesn't apply to it. Covered end-to-end (which
    /// abilities even reach this function) by `pact_chain_summon` on
    /// `SummonInfo::Npc`; this pins the pool side of that guarantee.
    #[test]
    fn a_non_chain_boon_also_means_a_pool_of_zero() {
        let mut world = mock_world();
        let mut skill_set = SkillSetBuilder::default().build();
        skill_set.set_level(60);
        let owner = world
            .create_entity()
            .with(Pact {
                boon: Some(PactBoon::Tome),
                ..Pact::default()
            })
            .with(skill_set)
            .build();

        assert!(!charge_chain_summon(&world, owner, uid(1), 1));
    }
}
