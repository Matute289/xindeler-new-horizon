use crate::{persistence::character_updater, sys::SysScheduler};
use common::{
    comp::{
        ActiveAbilities, Alignment, Background, Body, CharacterClass, Ethos, Inventory, MapMarker,
        Pact, Presence, PresenceKind, SkillSet, SpellMastery, Stats, TriggerSlots, Waypoint,
        ability::AbilityPool,
        pet::{Pet, is_tameable},
    },
    uid::Uid,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Join, LendJoin, ReadStorage, Write, WriteExpect};
use tracing::error;

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        ReadStorage<'a, Alignment>,
        ReadStorage<'a, Body>,
        ReadStorage<'a, Presence>,
        ReadStorage<'a, SkillSet>,
        ReadStorage<'a, Inventory>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Waypoint>,
        ReadStorage<'a, MapMarker>,
        ReadStorage<'a, Pet>,
        ReadStorage<'a, Stats>,
        ReadStorage<'a, ActiveAbilities>,
        // Xindeler: persisted `Innate` hotbar slots are stored by pool key, so
        // the writer needs each character's pool to translate them.
        ReadStorage<'a, AbilityPool>,
        ReadStorage<'a, CharacterClass>,
        ReadStorage<'a, Ethos>,
        ReadStorage<'a, Background>,
        ReadStorage<'a, Pact>,
        ReadStorage<'a, TriggerSlots>,
        ReadStorage<'a, SpellMastery>,
        WriteExpect<'a, character_updater::CharacterUpdater>,
        Write<'a, SysScheduler<Self>>,
    );

    const NAME: &'static str = "persistence";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            alignments,
            bodies,
            presences,
            player_skill_set,
            player_inventories,
            uids,
            player_waypoints,
            map_markers,
            pets,
            stats,
            active_abilities,
            ability_pools,
            character_classes,
            ethoses,
            backgrounds,
            pacts,
            trigger_slots,
            spell_masteries,
            mut updater,
            mut scheduler,
        ): Self::SystemData,
    ) {
        if scheduler.should_run() {
            updater.batch_update(
                (
                    &presences,
                    &player_skill_set,
                    &player_inventories,
                    &uids,
                    player_waypoints.maybe(),
                    &active_abilities,
                    ability_pools.maybe(),
                    map_markers.maybe(),
                    character_classes.maybe(),
                    ethoses.maybe(),
                    backgrounds.maybe(),
                    pacts.maybe(),
                    trigger_slots.maybe(),
                    spell_masteries.maybe(),
                )
                    .join()
                    .filter_map(
                        |(
                            presence,
                            skill_set,
                            inventory,
                            player_uid,
                            waypoint,
                            active_abilities,
                            ability_pool,
                            map_marker,
                            character_class,
                            ethos,
                            background,
                            pact,
                            trigger_slots,
                            spell_mastery,
                        )| match presence.kind {
                            PresenceKind::LoadingCharacter(_char_id) => {
                                error!(
                                    "Unexpected state when persisting characters! Some of the \
                                     components required above should only be present after a \
                                     character is loaded!"
                                );
                                None
                            },
                            PresenceKind::Character(id) => {
                                let pets = (&alignments, &bodies, &stats, &pets)
                                    .join()
                                    .filter_map(|(alignment, body, stats, pet)| match alignment {
                                        // Don't try to persist non-tameable pets (likely spawned
                                        // using /spawn) since there isn't any code to handle
                                        // persisting them
                                        Alignment::Owned(pet_owner)
                                            if pet_owner == player_uid && is_tameable(body) =>
                                        {
                                            Some(((*pet).clone(), *body, stats.clone()))
                                        },
                                        _ => None,
                                    })
                                    .collect();

                                Some((
                                    id,
                                    skill_set.clone(),
                                    inventory.clone(),
                                    pets,
                                    waypoint.cloned(),
                                    active_abilities.clone(),
                                    ability_pool.cloned().unwrap_or_default(),
                                    map_marker.cloned(),
                                    character_class.copied().unwrap_or_default(),
                                    ethos.copied().unwrap_or_default(),
                                    background.cloned().unwrap_or_default(),
                                    pact.copied().unwrap_or_default(),
                                    trigger_slots.cloned().unwrap_or_default(),
                                    spell_mastery.copied().unwrap_or_default(),
                                ))
                            },
                            PresenceKind::Spectator | PresenceKind::Possessor => None,
                        },
                    ),
            );
        }
    }
}
