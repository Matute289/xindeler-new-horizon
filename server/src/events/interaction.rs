use std::{f32::consts::PI, ops::Mul};

use common::{comp::loot_owner::ONWERSHIP_TIMEOUT_FAST, rtsim::DialogueKind};
use common_state::{BlockChange, ScheduledBlockChange};
use specs::{DispatcherBuilder, Join, Read, ReadExpect, ReadStorage, WriteExpect, WriteStorage};
use tracing::error;
use vek::*;

use common::{
    assets::{AssetCombined, AssetHandle, Ron},
    combat,
    comp::{
        self, EnteredAuras, Group, InventoryUpdateEvent, Player,
        agent::{AgentEvent, Sound, SoundKind},
        inventory::slot::EquipSlot,
        item::{MaterialStatManifest, flatten_counted_items},
        loot_owner::LootOwnerKind,
        tool::AbilityMap,
    },
    consts::{MAX_INTERACT_RANGE, MAX_NPCINTERACT_RANGE, SOUND_TRAVEL_DIST_PER_VOLUME},
    event::{
        CommandPetEvent, CreateItemDropEvent, CreateSpriteEvent, DeleteEvent, DialogueEvent,
        DismissSummonEvent, EventBus, MineBlockEvent, NpcInteractEvent, SetLanternEvent,
        SetPetStayEvent, SoundEvent, TamePetEvent, ToggleSpriteLightEvent,
    },
    link::Is,
    mounting::Mount,
    outcome::Outcome,
    resources::ProgramTime,
    terrain::{self, Block, SpriteKind, TerrainGrid},
    uid::{IdMaps, Uid},
    util::Dir,
    vol::ReadVol,
};

use crate::{Server, ServerGeneral, Time, client::Client};

use crate::pet::tame_pet;
use hashbrown::{HashMap, HashSet};
use lazy_static::lazy_static;

use super::{ServerEvent, event_dispatch, mounting::within_mounting_range};

pub(super) fn register_event_systems(builder: &mut DispatcherBuilder) {
    event_dispatch::<SetLanternEvent>(builder, &[]);
    event_dispatch::<NpcInteractEvent>(builder, &[]);
    event_dispatch::<DialogueEvent>(builder, &[]);
    event_dispatch::<SetPetStayEvent>(builder, &[]);
    event_dispatch::<CommandPetEvent>(builder, &[]);
    event_dispatch::<DismissSummonEvent>(builder, &[]);
    event_dispatch::<MineBlockEvent>(builder, &[]);
    event_dispatch::<SoundEvent>(builder, &[]);
    event_dispatch::<CreateSpriteEvent>(builder, &[]);
    event_dispatch::<ToggleSpriteLightEvent>(builder, &[]);
}

impl ServerEvent for SetLanternEvent {
    type SystemData<'a> = (
        WriteStorage<'a, comp::LightEmitter>,
        ReadStorage<'a, comp::Inventory>,
        ReadStorage<'a, comp::Health>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut light_emitters, inventories, healths): Self::SystemData<'_>,
    ) {
        for SetLanternEvent(entity, enable) in events {
            let lantern_exists = light_emitters
                .get(entity)
                .is_some_and(|light| light.strength > 0.0);

            if lantern_exists != enable {
                if !enable {
                    light_emitters.remove(entity);
                }
                // Only enable lantern if entity is alive
                else if healths.get(entity).is_none_or(|h| !h.is_dead) {
                    inventories
                        .get(entity)
                        .and_then(|inventory| inventory.equipped(EquipSlot::Lantern))
                        .map(|item| {
                            if let comp::item::ItemKind::Lantern(l) = &*item.kind() {
                                let _ = light_emitters.insert(entity, comp::LightEmitter {
                                    col: l.color(),
                                    strength: l.strength(),
                                    flicker: l.flicker(),
                                    animated: true,
                                    dir: l.dir,
                                });
                            }
                        });
                }
            }
        }
    }
}

impl ServerEvent for NpcInteractEvent {
    type SystemData<'a> = (
        WriteStorage<'a, comp::Agent>,
        ReadStorage<'a, comp::Pos>,
        ReadStorage<'a, Uid>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut agents, positions, uids): Self::SystemData<'_>,
    ) {
        for NpcInteractEvent(interactor, npc_entity) in events {
            let within_range = {
                positions
                    .get(interactor)
                    .zip(positions.get(npc_entity))
                    .is_some_and(|(interactor_pos, npc_pos)| {
                        interactor_pos.0.distance_squared(npc_pos.0)
                            <= MAX_NPCINTERACT_RANGE.powi(2)
                    })
            };

            if within_range
                && let Some(agent) = agents.get_mut(npc_entity)
                && agent.target.is_none()
                && let Some(interactor_uid) = uids.get(interactor)
            {
                agent.inbox.push_back(AgentEvent::Talk(*interactor_uid));
            }
        }
    }
}

impl ServerEvent for DialogueEvent {
    type SystemData<'a> = (
        ReadStorage<'a, Uid>,
        ReadStorage<'a, comp::Pos>,
        ReadStorage<'a, Client>,
        WriteStorage<'a, comp::Agent>,
        WriteStorage<'a, comp::Inventory>,
        ReadExpect<'a, AbilityMap>,
        ReadExpect<'a, MaterialStatManifest>,
        WriteStorage<'a, comp::InventoryUpdateBuffer>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            uids,
            positions,
            clients,
            mut agents,
            mut inventories,
            ability_map,
            msm,
            mut inventory_update_buffers,
        ): Self::SystemData<'_>,
    ) {
        for DialogueEvent(sender, target, dialogue) in events {
            let within_range = positions
                .get(sender)
                .zip(positions.get(target))
                .is_some_and(|(sender_pos, target_pos)| {
                    sender_pos.0.distance_squared(target_pos.0) <= MAX_NPCINTERACT_RANGE.powi(2)
                });

            if within_range && let Some(sender_uid) = uids.get(sender) {
                // Perform item transfer, if required
                let given_item = match &dialogue.kind {
                    DialogueKind::Start
                    | DialogueKind::End
                    | DialogueKind::Question { .. }
                    | DialogueKind::Marker { .. }
                    | DialogueKind::Ack { .. } => None,
                    DialogueKind::Statement { given_item, .. } => given_item.as_ref(),
                    DialogueKind::Response { response, .. } => response.given_item.as_ref(),
                };
                // If the response requires an item to be given, perform exchange (or exit)
                if let Some((item_def, amount)) = given_item {
                    // Check that the target's inventory has enough space for the item
                    if let Some(target_inv) = inventories.get(target)
                        && target_inv.has_space_for(item_def, *amount)
                        // Check that the sender has enough of the item
                        && let Some(mut sender_inv) = inventories.get_mut(sender)
                        && sender_inv.item_count(item_def) >= *amount as u64
                        // First, remove the item from the sender's inventory
                        && let Some(items) = sender_inv.remove_item_amount(item_def, *amount, &ability_map, &msm)
                        && let Some(mut target_inv) = inventories.get_mut(target)
                    {
                        for item in items {
                            let item_event = InventoryUpdateEvent::Collected(
                                item.frontend_item(&ability_map, &msm),
                            );
                            // Push the items to the target's inventory
                            if target_inv.push(item).is_err() {
                                error!(
                                    "Failed to insert dialogue given item despite target \
                                     inventory claiming to have space, dropping remaining items..."
                                );
                                break;
                            } else if let Some(buf) = inventory_update_buffers.get_mut(target) {
                                buf.push(item_event);
                            }
                        }
                    } else {
                        // TODO: Respond with error message on failure?
                        continue;
                    }
                }

                let dialogue = dialogue.into_validated_unchecked();

                if let Some(agent) = agents.get_mut(target) {
                    agent
                        .inbox
                        .push_back(AgentEvent::Dialogue(*sender_uid, dialogue.clone()));
                }

                if let Some(client) = clients.get(target) {
                    client.send_fallible(ServerGeneral::Dialogue(*sender_uid, dialogue));
                }
            }
        }
    }
}

impl ServerEvent for SetPetStayEvent {
    type SystemData<'a> = (
        WriteStorage<'a, comp::Agent>,
        WriteStorage<'a, comp::CharacterActivity>,
        ReadStorage<'a, comp::Pos>,
        ReadStorage<'a, comp::Alignment>,
        ReadStorage<'a, Is<Mount>>,
        ReadStorage<'a, Uid>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut agents, mut character_activities, positions, alignments, is_mounts, uids): Self::SystemData<'_>,
    ) {
        for SetPetStayEvent(command_giver, pet, stay) in events {
            let is_owner = uids.get(command_giver).is_some_and(|owner_uid| {
                matches!(
                    alignments.get(pet),
                    Some(comp::Alignment::Owned(pet_owner)) if *pet_owner == *owner_uid,
                )
            });

            let current_pet_position = positions.get(pet).copied();
            let stay = stay && current_pet_position.is_some();
            if is_owner
                && within_mounting_range(positions.get(command_giver), positions.get(pet))
                && is_mounts.get(pet).is_none()
            {
                // `is_pet_staying`/`stay_pos` remain the sole drivers of the
                // actual stay-in-place behaviour, exactly as before this
                // command existed. `pet_command` is set here purely so it
                // accurately reflects `Stay` instead of silently staying
                // `Follow` while the pet is, in fact, staying -- it is not
                // read by any Stay/Follow logic. `stay == false` resets it
                // fully to `Follow`, canceling any active `Guard` order:
                // the `V` key is the "back to normal" key.
                let pet_command = if stay {
                    comp::PetCommand::Stay
                } else {
                    comp::PetCommand::Follow
                };
                character_activities.get_mut(pet).map(|mut activity| {
                    activity.is_pet_staying = stay;
                    activity.pet_command = pet_command;
                });
                agents.get_mut(pet).map(|agent| {
                    agent.stay_pos = current_pet_position.filter(|_| stay);
                    agent.pet_command = pet_command;
                });
            }
        }
    }
}

impl ServerEvent for CommandPetEvent {
    type SystemData<'a> = (
        WriteStorage<'a, comp::Agent>,
        WriteStorage<'a, comp::CharacterActivity>,
        ReadStorage<'a, comp::Pos>,
        ReadStorage<'a, comp::Alignment>,
        ReadStorage<'a, Is<Mount>>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Group>,
        ReadStorage<'a, Player>,
        ReadStorage<'a, EnteredAuras>,
        Read<'a, IdMaps>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            mut agents,
            mut character_activities,
            positions,
            alignments,
            is_mounts,
            uids,
            groups,
            players,
            entered_auras,
            id_maps,
        ): Self::SystemData<'_>,
    ) {
        for CommandPetEvent(command_giver, pet, command) in events {
            let is_owner = uids.get(command_giver).is_some_and(|owner_uid| {
                matches!(
                    alignments.get(pet),
                    Some(comp::Alignment::Owned(pet_owner)) if *pet_owner == *owner_uid,
                )
            });

            if !is_owner
                || !within_mounting_range(positions.get(command_giver), positions.get(pet))
                || is_mounts.get(pet).is_some()
            {
                continue;
            }

            // `Attack` is refused when the designated target is the owner
            // themselves, is in the owner's group, or is someone the owner
            // could not legally attack directly -- without these checks,
            // commanding a pet to attack becomes a PvP-bypass exploit.
            if let comp::PetCommand::Attack(target_uid) = command {
                let Some(target_entity) = id_maps.uid_entity(target_uid) else {
                    continue;
                };
                let same_group = groups
                    .get(command_giver)
                    .is_some_and(|giver_group| Some(giver_group) == groups.get(target_entity));
                let legal_target = target_entity != command_giver
                    && !same_group
                    && combat::permit_pvp(
                        &alignments,
                        &players,
                        &entered_auras,
                        &id_maps,
                        Some(command_giver),
                        target_entity,
                    );
                if !legal_target {
                    continue;
                }
            }

            character_activities
                .get_mut(pet)
                .map(|mut activity| activity.pet_command = command);
            agents.get_mut(pet).map(|agent| agent.pet_command = command);
        }
    }
}

/// N27-O: a player-issued dismiss of one of their own Cadena
/// (`PactBoon::Chain`) summons. Mirrors `SetPetStayEvent`'s ownership +
/// mounting-range check, then does nothing further itself -- it routes
/// through `DeleteEvent`, the SAME funnel death and lifetime expiry already
/// use, so `server::events::entity_manipulation::handle_delete` frees the
/// point-pool charge from exactly one place regardless of which exit route
/// ended the summon's life. No direct `Summons` access here on purpose.
impl ServerEvent for DismissSummonEvent {
    type SystemData<'a> = (
        ReadStorage<'a, comp::Alignment>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, comp::Pos>,
        ReadExpect<'a, EventBus<DeleteEvent>>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (alignments, uids, positions, delete_events): Self::SystemData<'_>,
    ) {
        let mut delete_emitter = delete_events.emitter();
        for DismissSummonEvent(command_giver, summon) in events {
            let is_owner = uids.get(command_giver).is_some_and(|owner_uid| {
                matches!(
                    alignments.get(summon),
                    Some(comp::Alignment::Owned(summon_owner)) if *summon_owner == *owner_uid,
                )
            });
            if is_owner
                && within_mounting_range(positions.get(command_giver), positions.get(summon))
            {
                delete_emitter.emit(DeleteEvent(summon));
            }
        }
    }
}

lazy_static! {
    static ref RESOURCE_EXPERIENCE_MANIFEST: AssetHandle<Ron<HashMap<String, u32>>> =
        Ron::load_expect_combined_static("server.manifests.resource_experience_manifest");
}

impl ServerEvent for MineBlockEvent {
    type SystemData<'a> = (
        WriteExpect<'a, BlockChange>,
        ReadExpect<'a, TerrainGrid>,
        ReadExpect<'a, MaterialStatManifest>,
        ReadExpect<'a, AbilityMap>,
        ReadExpect<'a, EventBus<CreateItemDropEvent>>,
        ReadExpect<'a, EventBus<SoundEvent>>,
        ReadExpect<'a, EventBus<Outcome>>,
        ReadExpect<'a, ProgramTime>,
        ReadExpect<'a, Time>,
        WriteStorage<'a, comp::SkillSet>,
        ReadStorage<'a, Uid>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            mut block_change,
            terrain,
            msm,
            ability_map,
            create_item_drop_events,
            sound_events,
            outcomes,
            program_time,
            time,
            mut skill_sets,
            uids,
        ): Self::SystemData<'_>,
    ) {
        use rand::RngExt;
        let mut rng = rand::rng();
        let mut create_item_drop_emitter = create_item_drop_events.emitter();
        let mut sound_event_emitter = sound_events.emitter();
        let mut outcome_emitter = outcomes.emitter();
        for ev in events {
            if block_change.can_set_block(ev.pos) {
                let block = terrain.get(ev.pos).ok().copied();
                if let Some(mut block) =
                    block.filter(|b| b.mine_tool().is_some_and(|t| Some(t) == ev.tool))
                {
                    // Attempt to increase the resource's damage
                    let damage = if let Ok(damage) = block.get_attr::<terrain::sprite::Damage>() {
                        let updated_damage = damage.0.saturating_add(1);
                        block
                            .set_attr(terrain::sprite::Damage(updated_damage))
                            .expect(
                                "We just read the Damage attribute from the block, writing should \
                                 be possible too",
                            );

                        Some(updated_damage)
                    } else {
                        None
                    };

                    let sprite = block.get_sprite();

                    // Maximum damage has reached, destroy the block
                    let is_broken = damage
                        .and_then(|damage| Some((sprite?.required_mine_damage(), damage)))
                        .is_some_and(|(required_damage, damage)| {
                            required_damage.is_none_or(|required| damage >= required)
                        });

                    // Stage changes happen in damage interval of `mine_drop_intevral`
                    let stage_changed = damage
                        .and_then(|damage| Some((sprite?.mine_drop_interval(), damage)))
                        .is_some_and(|(interval, damage)| damage % interval == 0);

                    let sprite_cfg = terrain.sprite_cfg_at(ev.pos);
                    if (stage_changed || is_broken)
                        && let Some(items) = comp::Item::try_reclaim_from_block(block, sprite_cfg)
                    {
                        let mut items: Vec<_> =
                            flatten_counted_items(&items, &ability_map, &msm).collect();
                        let maybe_uid = uids.get(ev.entity).copied();

                        if let Some(mut skillset) = skill_sets.get_mut(ev.entity) {
                            use common::comp::skills::{MiningSkill, SKILL_MODIFIERS, Skill};

                            if is_broken
                                && let (Some(tool), Some(uid), exp_reward @ 1..) = (
                                    ev.tool,
                                    maybe_uid,
                                    items
                                        .iter()
                                        .filter_map(|item| {
                                            item.item_definition_id().itemdef_id().and_then(|id| {
                                                RESOURCE_EXPERIENCE_MANIFEST
                                                    .read()
                                                    .0
                                                    .get(id)
                                                    .copied()
                                            })
                                        })
                                        .sum(),
                                )
                            {
                                let skill_group = comp::SkillGroupKind::Weapon(tool);
                                if let Some(level_outcome) =
                                    skillset.add_experience(skill_group, exp_reward)
                                {
                                    outcome_emitter.emit(Outcome::SkillPointGain {
                                        uid,
                                        skill_tree: skill_group,
                                        total_points: level_outcome,
                                    });
                                }
                                outcome_emitter.emit(Outcome::ExpChange {
                                    uid,
                                    exp: exp_reward,
                                    xp_pools: HashSet::from([skill_group]),
                                });
                            }

                            let stage_ore_chance = || {
                                let chance_mod = f64::from(SKILL_MODIFIERS.mining_tree.ore_gain);
                                let skill_level = skillset
                                    .skill_level(Skill::Pick(MiningSkill::OreGain))
                                    .unwrap_or(0);

                                chance_mod * f64::from(skill_level)
                            };
                            let stage_gem_chance = || {
                                let chance_mod = f64::from(SKILL_MODIFIERS.mining_tree.gem_gain);
                                let skill_level = skillset
                                    .skill_level(Skill::Pick(MiningSkill::GemGain))
                                    .unwrap_or(0);

                                chance_mod * f64::from(skill_level)
                            };

                            // If the resource hasn't been fully broken, only drop certain resources
                            // with a chance
                            if !is_broken {
                                items.retain(|item| {
                                    rng.random_bool(
                                        0.5 + item
                                            .item_definition_id()
                                            .itemdef_id()
                                            .map(|id| {
                                                if id.contains("mineral.ore.") {
                                                    stage_ore_chance()
                                                } else if id.contains("mineral.gem.") {
                                                    stage_gem_chance()
                                                } else {
                                                    0.0
                                                }
                                            })
                                            .unwrap_or(0.0),
                                    )
                                });
                            }
                        }
                        for item in items {
                            let loot_owner = maybe_uid.map(LootOwnerKind::Player).map(|owner| {
                                comp::LootOwner::new(owner, false, ONWERSHIP_TIMEOUT_FAST)
                            });
                            create_item_drop_emitter.emit(CreateItemDropEvent {
                                pos: comp::Pos(ev.pos.map(|e| e as f32) + Vec3::broadcast(0.5)),
                                vel: comp::Vel(
                                    Vec2::unit_x()
                                        .rotated_z(rng.random::<f32>() * PI * 2.0)
                                        .mul(4.0)
                                        .with_z(rng.random_range(5.0..10.0)),
                                ),
                                ori: comp::Ori::from(Dir::random_2d(&mut rng)),
                                item: comp::PickupItem::new(item, *program_time, false),
                                loot_owner,
                            });
                        }
                    }

                    if damage.is_some() && !is_broken {
                        block_change.set(ev.pos, block);
                    } else {
                        block_change.set(ev.pos, block.into_vacant());
                    }
                    outcome_emitter.emit(if is_broken {
                        Outcome::BreakBlock {
                            pos: ev.pos,
                            tool: ev.tool,
                            color: block.get_color(),
                        }
                    } else {
                        Outcome::DamagedBlock {
                            pos: ev.pos,
                            stage_changed,
                            tool: ev.tool,
                        }
                    });

                    // Emit mining sound
                    sound_event_emitter.emit(SoundEvent {
                        sound: Sound::new(SoundKind::Mine, ev.pos.as_(), 20.0, time.0),
                    });
                }
            }
        }
    }
}

impl ServerEvent for SoundEvent {
    type SystemData<'a> = (
        ReadExpect<'a, EventBus<Outcome>>,
        WriteStorage<'a, comp::Agent>,
        ReadStorage<'a, comp::Pos>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (outcomes, mut agents, positions): Self::SystemData<'_>,
    ) {
        let mut outcome_emitter = outcomes.emitter();
        for SoundEvent { sound } in events {
            // TODO: Reduce the complexity of this problem by using spatial partitioning
            // system
            for (agent, agent_pos) in (&mut agents, &positions).join() {
                // TODO: Use pathfinding for more dropoff around obstacles
                let agent_dist_sqrd = agent_pos.0.distance_squared(sound.pos);
                let sound_travel_dist_sqrd = (sound.vol * SOUND_TRAVEL_DIST_PER_VOLUME).powi(2);

                let vol_dropoff = agent_dist_sqrd / sound_travel_dist_sqrd * sound.vol;
                let propagated_sound = sound.with_new_vol(sound.vol - vol_dropoff);

                let can_hear_sound = propagated_sound.vol > 0.00;
                let should_hear_sound = agent_dist_sqrd < agent.psyche.listen_dist.powi(2);

                if can_hear_sound && should_hear_sound {
                    agent
                        .inbox
                        .push_back(AgentEvent::ServerSound(propagated_sound));
                }
            }

            // Attempt to turn this sound into an outcome to be received by frontends.
            if let Some(outcome) = match sound.kind {
                SoundKind::Utterance(kind, body) => Some(Outcome::Utterance {
                    kind,
                    pos: sound.pos,
                    body,
                }),
                _ => None,
            } {
                outcome_emitter.emit(outcome);
            }
        }
    }
}

impl ServerEvent for CreateSpriteEvent {
    type SystemData<'a> = (
        WriteExpect<'a, BlockChange>,
        WriteExpect<'a, ScheduledBlockChange>,
        ReadExpect<'a, TerrainGrid>,
        ReadExpect<'a, Time>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut block_change, mut scheduled_block_change, terrain, time): Self::SystemData<'_>,
    ) {
        for ev in events {
            if block_change.can_set_block(ev.pos) {
                let block = terrain.get(ev.pos).ok().copied();
                if block.is_some_and(|b| (*b).is_fluid()) {
                    let old_block = block.unwrap_or_else(|| Block::air(SpriteKind::Empty));
                    let new_block = old_block.with_sprite(ev.sprite);
                    block_change.set(ev.pos, new_block);
                    // Remove sprite after del_timeout and offset if specified
                    if let Some((timeout, del_offset)) = ev.del_timeout {
                        use rand::RngExt;
                        let mut rng = rand::rng();
                        let offset = rng.random_range(0.0..del_offset);
                        let current_time: f64 = time.0;
                        let replace_time = current_time + (timeout + offset) as f64;
                        if old_block != new_block {
                            scheduled_block_change.set(ev.pos, old_block, replace_time);
                            scheduled_block_change.outcome_set(ev.pos, new_block, replace_time);
                        }
                    }
                }
            }
        }
    }
}

impl ServerEvent for ToggleSpriteLightEvent {
    type SystemData<'a> = (
        WriteExpect<'a, BlockChange>,
        ReadExpect<'a, TerrainGrid>,
        ReadStorage<'a, comp::Pos>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut block_change, terrain, positions): Self::SystemData<'_>,
    ) {
        for ev in events.into_iter() {
            if let Some(entity_pos) = positions.get(ev.entity)
                && entity_pos.0.distance_squared(ev.pos.as_()) < MAX_INTERACT_RANGE.powi(2)
                && block_change.can_set_block(ev.pos)
                && let Some(new_block) = terrain
                    .get(ev.pos)
                    .ok()
                    .and_then(|block| block.with_toggle_light(ev.enable))
            {
                block_change.set(ev.pos, new_block);
                // TODO: Emit outcome
            }
        }
    }
}

pub fn handle_tame_pet(server: &mut Server, ev: TamePetEvent) {
    // TODO: Raise outcome to send to clients to play sound/render an indicator
    // showing taming success?
    tame_pet(server.state.ecs(), ev.pet_entity, ev.owner_entity);
}

#[cfg(test)]
mod pet_command_tests {
    use super::*;
    use common::{
        comp::{PetCommand, body::humanoid},
        resources::BattleMode,
        uuid::Uuid,
    };
    use specs::{Builder, Entity as EcsEntity, World, WorldExt};

    /// Registers every component/resource type `SetPetStayEvent` and
    /// `CommandPetEvent`'s handlers read or write.
    fn mock_world() -> World {
        let mut world = World::new();
        world.insert(IdMaps::new());
        world.register::<comp::Agent>();
        world.register::<comp::CharacterActivity>();
        world.register::<comp::Pos>();
        world.register::<comp::Alignment>();
        world.register::<Is<Mount>>();
        world.register::<Uid>();
        world.register::<Group>();
        world.register::<Player>();
        world.register::<EnteredAuras>();
        world
    }

    /// Spawns an entity at the origin (so every spawned pair is trivially
    /// within mounting range of each other) and allocates it a `Uid`.
    fn spawn(world: &mut World) -> (EcsEntity, Uid) {
        let entity = world.create_entity().with(comp::Pos(Vec3::zero())).build();
        let uid = {
            let mut uids = world.write_component::<Uid>();
            let mut id_maps = world.write_resource::<IdMaps>();
            let uid = id_maps.allocate(entity);
            uids.insert(entity, uid)
                .expect("fresh entity, insert must succeed");
            uid
        };
        (entity, uid)
    }

    /// Spawns a pet owned by `owner`, with a fresh `Agent` and default
    /// `CharacterActivity` (both start at `PetCommand::Follow`, matching
    /// `Agent::from_body` and `CharacterActivity::default`).
    fn spawn_owned_pet(world: &mut World, owner: Uid) -> EcsEntity {
        let (pet, _) = spawn(world);
        world
            .write_component::<comp::Alignment>()
            .insert(pet, comp::Alignment::Owned(owner))
            .expect("fresh entity, insert must succeed");
        world
            .write_component::<comp::Agent>()
            .insert(
                pet,
                comp::Agent::from_body(&comp::Body::Humanoid(humanoid::Body::random())),
            )
            .expect("fresh entity, insert must succeed");
        world
            .write_component::<comp::CharacterActivity>()
            .insert(pet, comp::CharacterActivity::default())
            .expect("fresh entity, insert must succeed");
        pet
    }

    fn pet_command_on_agent(world: &World, pet: EcsEntity) -> PetCommand {
        world
            .read_component::<comp::Agent>()
            .get(pet)
            .unwrap()
            .pet_command
    }

    fn pet_command_on_activity(world: &World, pet: EcsEntity) -> PetCommand {
        world
            .read_component::<comp::CharacterActivity>()
            .get(pet)
            .unwrap()
            .pet_command
    }

    fn dispatch_command_pet(world: &World, giver: EcsEntity, pet: EcsEntity, command: PetCommand) {
        let data = world.system_data::<<CommandPetEvent as ServerEvent>::SystemData<'_>>();
        CommandPetEvent::handle(vec![CommandPetEvent(giver, pet, command)].into_iter(), data);
    }

    /// Locks in today's `V`-key path: `SetPetStayEvent` must keep driving
    /// `is_pet_staying` and `Agent::stay_pos` exactly as before -- the
    /// actual stay-in-place behaviour is unaffected by this change. It also
    /// now sets `pet_command` to accurately reflect `Stay`/`Follow` (purely
    /// descriptive; no Guard/Attack node reads `Stay`, so this does not
    /// change any AI behaviour), and resets it fully to `Follow` on
    /// "un-stay", canceling any active `Guard` order.
    #[test]
    fn set_pet_stay_event_drives_is_pet_staying_and_reflects_into_pet_command() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);

        {
            let data = world.system_data::<<SetPetStayEvent as ServerEvent>::SystemData<'_>>();
            SetPetStayEvent::handle(vec![SetPetStayEvent(owner, pet, true)].into_iter(), data);
        }

        assert!(
            world
                .read_component::<comp::CharacterActivity>()
                .get(pet)
                .unwrap()
                .is_pet_staying
        );
        assert_eq!(
            world
                .read_component::<comp::Agent>()
                .get(pet)
                .unwrap()
                .stay_pos,
            Some(comp::Pos(Vec3::zero()))
        );
        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Stay);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Stay);

        // Pressing V again ("un-stay") resets pet_command fully to Follow.
        {
            let data = world.system_data::<<SetPetStayEvent as ServerEvent>::SystemData<'_>>();
            SetPetStayEvent::handle(vec![SetPetStayEvent(owner, pet, false)].into_iter(), data);
        }
        assert!(
            !world
                .read_component::<comp::CharacterActivity>()
                .get(pet)
                .unwrap()
                .is_pet_staying
        );
        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Follow);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Follow);
    }

    /// Positive control: a legal target (not the owner, not in the owner's
    /// group, and no PvP conflict since neither side is even a `Player`)
    /// must actually apply the command. Without this, the refusal tests
    /// below could all be passing vacuously because of an unrelated bug
    /// (e.g. the `is_owner`/mounting-range gate rejecting everything).
    #[test]
    fn attack_is_applied_against_a_legal_target() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);
        let (_target, target_uid) = spawn(&mut world);

        dispatch_command_pet(&world, owner, pet, PetCommand::Attack(target_uid));

        assert_eq!(
            pet_command_on_agent(&world, pet),
            PetCommand::Attack(target_uid)
        );
        assert_eq!(
            pet_command_on_activity(&world, pet),
            PetCommand::Attack(target_uid)
        );
    }

    /// `Guard` is not attack-legality-gated at all, and must reach both
    /// `Agent` (read by the behaviour tree) and `CharacterActivity`
    /// (net-synced).
    #[test]
    fn guard_command_reaches_agent_and_character_activity() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);

        dispatch_command_pet(&world, owner, pet, PetCommand::Guard);

        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Guard);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Guard);
    }

    /// The highest-risk case: commanding a pet to attack its own owner must
    /// be refused, or "attack that one" would let a pet be used to bypass
    /// self-harm/PvP protections.
    #[test]
    fn attack_is_refused_when_target_is_the_owner() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);

        dispatch_command_pet(&world, owner, pet, PetCommand::Attack(owner_uid));

        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Follow);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Follow);
    }

    /// Commanding a pet to attack a member of the owner's own group must be
    /// refused (a group is presumptively friendly/cooperating).
    #[test]
    fn attack_is_refused_when_target_is_in_owners_group() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);
        let (target, target_uid) = spawn(&mut world);

        let shared_group = common::comp::group::NPC;
        {
            let mut groups = world.write_component::<Group>();
            groups.insert(owner, shared_group).unwrap();
            groups.insert(target, shared_group).unwrap();
        }

        dispatch_command_pet(&world, owner, pet, PetCommand::Attack(target_uid));

        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Follow);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Follow);
    }

    /// Commanding a pet to attack a player the owner could not legally
    /// attack directly (opposing `BattleMode`) must be refused -- otherwise
    /// "attack that one" is a PvP-bypass exploit routed through a pet.
    #[test]
    fn attack_is_refused_when_pvp_is_not_permitted() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let pet = spawn_owned_pet(&mut world, owner_uid);
        let (target, target_uid) = spawn(&mut world);

        {
            let mut players = world.write_component::<Player>();
            players
                .insert(
                    owner,
                    Player::new("owner".into(), BattleMode::PvE, Uuid::nil(), None),
                )
                .unwrap();
            players
                .insert(
                    target,
                    Player::new("target".into(), BattleMode::PvP, Uuid::nil(), None),
                )
                .unwrap();
        }

        dispatch_command_pet(&world, owner, pet, PetCommand::Attack(target_uid));

        assert_eq!(pet_command_on_agent(&world, pet), PetCommand::Follow);
        assert_eq!(pet_command_on_activity(&world, pet), PetCommand::Follow);
    }
}

/// N27-O: `DismissSummonEvent`'s ownership/range check and its hand-off to
/// `DeleteEvent`. What happens once `DeleteEvent` is emitted (the actual
/// point-pool release) is `entity_manipulation::handle_delete`'s job and is
/// covered separately -- this module only proves dismiss reaches that
/// funnel exactly when it should, and never for the wrong caller.
#[cfg(test)]
mod dismiss_summon_tests {
    use super::*;
    use specs::{Builder, Entity as EcsEntity, World, WorldExt};

    fn mock_world() -> World {
        let mut world = World::new();
        world.insert(IdMaps::new());
        world.insert(EventBus::<DeleteEvent>::default());
        world.register::<comp::Pos>();
        world.register::<comp::Alignment>();
        world.register::<Uid>();
        world
    }

    fn spawn(world: &mut World, pos: Vec3<f32>) -> (EcsEntity, Uid) {
        let entity = world.create_entity().with(comp::Pos(pos)).build();
        let uid = {
            let mut uids = world.write_component::<Uid>();
            let mut id_maps = world.write_resource::<IdMaps>();
            let uid = id_maps.allocate(entity);
            uids.insert(entity, uid)
                .expect("fresh entity, insert must succeed");
            uid
        };
        (entity, uid)
    }

    fn spawn_owned_summon(world: &mut World, owner: Uid, pos: Vec3<f32>) -> EcsEntity {
        let (summon, _) = spawn(world, pos);
        world
            .write_component::<comp::Alignment>()
            .insert(summon, comp::Alignment::Owned(owner))
            .expect("fresh entity, insert must succeed");
        summon
    }

    fn dispatch_dismiss(world: &World, giver: EcsEntity, summon: EcsEntity) {
        let data = world.system_data::<<DismissSummonEvent as ServerEvent>::SystemData<'_>>();
        DismissSummonEvent::handle(vec![DismissSummonEvent(giver, summon)].into_iter(), data);
    }

    fn pending_deletes(world: &World) -> Vec<EcsEntity> {
        world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .map(|DeleteEvent(entity)| entity)
            .collect()
    }

    #[test]
    fn owner_dismissing_their_own_summon_emits_delete() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world, Vec3::zero());
        let summon = spawn_owned_summon(&mut world, owner_uid, Vec3::zero());

        dispatch_dismiss(&world, owner, summon);

        assert_eq!(pending_deletes(&world), vec![summon]);
    }

    #[test]
    fn dismissing_someone_elses_summon_is_refused() {
        let mut world = mock_world();
        let (_owner, owner_uid) = spawn(&mut world, Vec3::zero());
        let (impostor, _) = spawn(&mut world, Vec3::zero());
        let summon = spawn_owned_summon(&mut world, owner_uid, Vec3::zero());

        dispatch_dismiss(&world, impostor, summon);

        assert!(pending_deletes(&world).is_empty());
    }

    #[test]
    fn dismissing_a_non_summon_target_is_refused() {
        let mut world = mock_world();
        let (owner, _owner_uid) = spawn(&mut world, Vec3::zero());
        // No `Alignment` at all -- not owned by anyone, let alone `owner`.
        let (not_a_summon, _) = spawn(&mut world, Vec3::zero());

        dispatch_dismiss(&world, owner, not_a_summon);

        assert!(pending_deletes(&world).is_empty());
    }

    #[test]
    fn dismissing_a_summon_out_of_range_is_refused() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world, Vec3::zero());
        let far_away = Vec3::new(1000.0, 1000.0, 1000.0);
        let summon = spawn_owned_summon(&mut world, owner_uid, far_away);

        dispatch_dismiss(&world, owner, summon);

        assert!(pending_deletes(&world).is_empty());
    }
}
