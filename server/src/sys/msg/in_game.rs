#[cfg(feature = "persistent_world")]
use crate::TerrainPersistence;
use crate::{EditableSettings, Settings, client::Client};
use common::{
    comp::{
        Admin, AdminRole, Body, CanBuild, CharacterClass, ControlEvent, Controller, ForceUpdate,
        Health, Ori, Player, Pos, Presence, PresenceKind, RemoteSense, Scale, SkillSet,
        SpectatingEntity, Vel,
        buff::{BuffChange, BuffKind},
    },
    event::{self, EmitExt},
    event_emitters,
    link::Is,
    mounting::{Rider, VolumeRider},
    resources::{DeltaTime, PlayerPhysicsSetting, PlayerPhysicsSettings},
    slowjob::SlowJobPool,
    terrain::TerrainGrid,
    uid::{IdMaps, Uid},
    vol::ReadVol,
};
use common_ecs::{Job, Origin, Phase, System};
use common_net::msg::{ClientGeneral, ServerGeneral};
use common_state::{AreasContainer, BlockChange, BuildArea};
use core::mem;
use rayon::prelude::*;
use specs::{Entities, Join, LendJoin, Read, ReadExpect, ReadStorage, Write, WriteStorage};
use std::{borrow::Cow, time::Instant};
use tracing::{debug, trace, warn};
use vek::*;

#[cfg(feature = "persistent_world")]
pub type TerrainPersistenceData<'a> = Option<Write<'a, TerrainPersistence>>;
#[cfg(not(feature = "persistent_world"))]
pub type TerrainPersistenceData<'a> = core::marker::PhantomData<&'a mut ()>;

// NOTE: These writes are considered "rare", meaning (currently) that they are
// admin-gated features that players shouldn't normally access, and which we're
// not that concerned about the performance of when two players try to use them
// at once.
//
// In such cases, we're okay putting them behind a mutex and penalizing the
// system if they're actually used concurrently by lots of users.  Please do not
// put less rare writes here, unless you want to serialize the system!
struct RareWrites<'a, 'b> {
    block_changes: &'b mut BlockChange,
    _terrain_persistence: &'b mut TerrainPersistenceData<'a>,
}

event_emitters! {
    struct Events[Emitters] {
        exit_ingame: event::ExitIngameEvent,
        request_site_info: event::RequestSiteInfoEvent,
        update_map_marker: event::UpdateMapMarkerEvent,
        client_disconnect: event::ClientDisconnectEvent,
        set_battle_mode: event::SetBattleModeEvent,
        buff: event::BuffEvent,
    }
}

impl Sys {
    #[expect(clippy::too_many_arguments)]
    fn handle_client_in_game_msg(
        emitters: &mut Emitters,
        entity: specs::Entity,
        client: &Client,
        maybe_presence: &mut Option<&mut Presence>,
        terrain: &ReadExpect<'_, TerrainGrid>,
        can_build: &ReadStorage<'_, CanBuild>,
        is_rider: &ReadStorage<'_, Is<Rider>>,
        is_volume_rider: &ReadStorage<'_, Is<VolumeRider>>,
        force_update: Option<&&mut ForceUpdate>,
        skill_set: &mut Option<Cow<'_, SkillSet>>,
        character_class: &mut Option<CharacterClass>,
        healths: &ReadStorage<'_, Health>,
        rare_writes: &parking_lot::Mutex<RareWrites<'_, '_>>,
        position: Option<&mut Pos>,
        spectating_entity: &mut Option<Option<common::uid::Uid>>,
        controller: Option<&mut Controller>,
        settings: &Read<'_, Settings>,
        build_areas: &Read<'_, AreasContainer<BuildArea>>,
        player_physics_setting: Option<&mut PlayerPhysicsSetting>,
        server_physics_forced: bool,
        maybe_admin: &Option<&Admin>,
        maybe_remote_sense: &Option<&RemoteSense>,
        time_for_vd_changes: Instant,
        msg: ClientGeneral,
        player_physics: &mut Option<(Pos, Vel, Ori)>,
    ) -> Result<(), crate::error::Error> {
        let presence = match maybe_presence.as_deref_mut() {
            Some(g) => g,
            None => {
                debug!(?entity, "client is not in_game, ignoring msg");
                trace!(?msg, "ignored msg content");
                return Ok(());
            },
        };
        match msg {
            // Go back to registered state (char selection screen)
            ClientGeneral::ExitInGame => {
                emitters.emit(event::ExitIngameEvent { entity });
                client.send(ServerGeneral::ExitInGameSuccess)?;
                *maybe_presence = None;
            },
            ClientGeneral::SetViewDistance(view_distances) => {
                let clamped_vds = view_distances.clamp(settings.max_view_distance);

                presence
                    .terrain_view_distance
                    .set_target(clamped_vds.terrain, time_for_vd_changes);
                presence
                    .entity_view_distance
                    .set_target(clamped_vds.entity, time_for_vd_changes);

                // Correct client if its requested VD is too high.
                if view_distances.terrain != clamped_vds.terrain {
                    client.send(ServerGeneral::SetViewDistance(clamped_vds.terrain))?;
                }
            },
            ClientGeneral::ControllerInputs(inputs) => {
                if presence.kind.controlling_char()
                    && let Some(controller) = controller
                {
                    controller.inputs.update_with_new(*inputs);
                }
            },
            ClientGeneral::ControlEvent(event) => {
                if presence.kind.controlling_char()
                    && let Some(controller) = controller
                {
                    // Skip respawn if client entity is alive
                    let skip_respawn = matches!(event, ControlEvent::Respawn)
                        && healths.get(entity).is_none_or(|h| !h.is_dead);

                    if !skip_respawn {
                        controller.push_event(event);
                    }
                }
            },
            ClientGeneral::ControlAction(event) => {
                if presence.kind.controlling_char()
                    && let Some(controller) = controller
                {
                    controller.push_action(event);
                }
            },
            ClientGeneral::PlayerPhysics {
                pos,
                vel,
                ori,
                force_counter,
            } => {
                if presence.kind.controlling_char()
                    && force_update
                        .is_none_or(|force_update| force_update.counter() == force_counter)
                    && healths.get(entity).is_none_or(|h| !h.is_dead)
                    && is_rider.get(entity).is_none()
                    && is_volume_rider.get(entity).is_none()
                    && !server_physics_forced
                    && player_physics_setting
                        .as_ref()
                        .is_none_or(|s| !s.server_authoritative_physics_optin())
                {
                    *player_physics = Some((pos, vel, ori));
                }
            },
            ClientGeneral::BreakBlock(pos) => {
                if let Some(comp_can_build) = can_build.get(entity)
                    && comp_can_build.enabled
                {
                    for area in comp_can_build.build_areas.iter() {
                        if let Some(old_block) = build_areas
                                .areas()
                                .get(*area)
                                // TODO: Make this an exclusive check on the upper bound of the AABB
                                // Vek defaults to inclusive which is not optimal
                                .filter(|aabb| aabb.contains_point(pos))
                                .and_then(|_| terrain.get(pos).ok())
                        {
                            let new_block = old_block.into_vacant();
                            // Take the rare writes lock as briefly as possible.
                            let mut guard = rare_writes.lock();
                            let _was_set = guard.block_changes.try_set(pos, new_block).is_some();
                            #[cfg(feature = "persistent_world")]
                            if _was_set
                                && let Some(terrain_persistence) =
                                    guard._terrain_persistence.as_mut()
                            {
                                terrain_persistence.set_block(pos, new_block);
                            }
                        }
                    }
                }
            },
            ClientGeneral::PlaceBlock(pos, new_block) => {
                if let Some(comp_can_build) = can_build.get(entity)
                    && comp_can_build.enabled
                {
                    for area in comp_can_build.build_areas.iter() {
                        if build_areas
                                .areas()
                                .get(*area)
                                // TODO: Make this an exclusive check on the upper bound of the AABB
                                // Vek defaults to inclusive which is not optimal
                                .filter(|aabb| aabb.contains_point(pos))
                                .is_some()
                        {
                            // Take the rare writes lock as briefly as possible.
                            let mut guard = rare_writes.lock();
                            let _was_set = guard.block_changes.try_set(pos, new_block).is_some();
                            #[cfg(feature = "persistent_world")]
                            if _was_set
                                && let Some(terrain_persistence) =
                                    guard._terrain_persistence.as_mut()
                            {
                                terrain_persistence.set_block(pos, new_block);
                            }
                        }
                    }
                }
            },
            ClientGeneral::UnlockSkill(skill) => {
                // FIXME: How do we want to handle the error?  Probably not by swallowing it.
                let _ = skill_set
                    .as_mut()
                    .map(|skill_set| {
                        SkillSet::unlock_skill_cow(skill_set, skill, |skill_set| skill_set.to_mut())
                    })
                    .transpose();
            },
            ClientGeneral::SetFutureLevelsToSecondary(value) => {
                if let Some(character_class) = character_class.as_mut() {
                    character_class.set_future_levels_to_secondary(value);
                }
            },
            ClientGeneral::RequestSiteInfo(id) => {
                emitters.emit(event::RequestSiteInfoEvent { entity, id });
            },
            ClientGeneral::RequestPlayerPhysics {
                server_authoritative,
            } => {
                if let Some(setting) = player_physics_setting {
                    setting.client_optin = server_authoritative;
                }
            },
            ClientGeneral::RequestLossyTerrainCompression {
                lossy_terrain_compression,
            } => {
                presence.lossy_terrain_compression = lossy_terrain_compression;
            },
            ClientGeneral::UpdateMapMarker(update) => {
                emitters.emit(event::UpdateMapMarkerEvent { entity, update });
            },
            ClientGeneral::SpectatePosition(pos) => {
                if let Some(admin) = maybe_admin
                    && admin.0 >= AdminRole::Moderator
                    && presence.kind == PresenceKind::Spectator
                    && let Some(position) = position
                {
                    position.0 = pos;
                }
            },
            ClientGeneral::SpectateEntity(uid) => {
                // This reads the SERVER's own `RemoteSense` storage, never
                // anything the client sent — the client cannot grant itself
                // a viewpoint by lying about its own copy of this component.
                let is_moderator = maybe_admin.is_some_and(|admin| admin.0 >= AdminRole::Moderator);
                if spectate_entity_permitted(is_moderator, uid, *maybe_remote_sense) {
                    *spectating_entity = Some(uid);
                }
            },
            ClientGeneral::SetBattleMode(battle_mode) => {
                emitters.emit(event::SetBattleModeEvent {
                    entity,
                    battle_mode,
                });
            },
            ClientGeneral::CancelRemoteSense => {
                // A voluntary early end of the sender's own remote-sensing
                // link. Only ever removes the sender's own buff — the actual
                // cleanup (clearing `RemoteSense`/`SpectatingEntity`) happens
                // next tick in `server::sys::remote_sense::Sys` once it
                // notices the sustaining buff is gone, exactly like a
                // duration-expiry or a concentration-breaking hit.
                emitters.emit(event::BuffEvent {
                    entity,
                    buff_change: BuffChange::RemoveByKind(BuffKind::RemoteSensing),
                });
            },
            ClientGeneral::RequestCharacterList
            | ClientGeneral::CreateCharacter { .. }
            | ClientGeneral::EditCharacter { .. }
            | ClientGeneral::DeleteCharacter(_)
            | ClientGeneral::Character(_, _)
            | ClientGeneral::Spectate(_)
            | ClientGeneral::TerrainChunkRequest { .. }
            | ClientGeneral::LodZoneRequest { .. }
            | ClientGeneral::ChatMsg(_)
            | ClientGeneral::Command(..)
            | ClientGeneral::Terminate
            | ClientGeneral::RequestPlugins(_) => {
                debug!("Kicking possibly misbehaving client due to invalid client in game request");
                emitters.emit(event::ClientDisconnectEvent(
                    entity,
                    common::comp::DisconnectReason::NetworkError,
                ));
            },
        }
        Ok(())
    }
}

/// This system will handle new messages from clients
#[derive(Default)]
pub struct Sys;
impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        (
            ReadExpect<'a, TerrainGrid>,
            ReadExpect<'a, SlowJobPool>,
            ReadExpect<'a, EditableSettings>,
        ),
        (
            Read<'a, IdMaps>,
            Read<'a, DeltaTime>,
            Read<'a, Settings>,
            Read<'a, AreasContainer<BuildArea>>,
        ),
        ReadStorage<'a, CanBuild>,
        WriteStorage<'a, ForceUpdate>,
        ReadStorage<'a, Is<Rider>>,
        ReadStorage<'a, Is<VolumeRider>>,
        WriteStorage<'a, SkillSet>,
        WriteStorage<'a, CharacterClass>,
        ReadStorage<'a, Health>,
        ReadStorage<'a, Body>,
        ReadStorage<'a, Scale>,
        Write<'a, BlockChange>,
        WriteStorage<'a, Pos>,
        WriteStorage<'a, Vel>,
        WriteStorage<'a, Ori>,
        WriteStorage<'a, Presence>,
        WriteStorage<'a, Client>,
        WriteStorage<'a, Controller>,
        WriteStorage<'a, SpectatingEntity>,
        Write<'a, PlayerPhysicsSettings>,
        TerrainPersistenceData<'a>,
        ReadStorage<'a, Player>,
        ReadStorage<'a, Admin>,
        ReadStorage<'a, RemoteSense>,
    );

    const NAME: &'static str = "msg::in_game";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            entities,
            events,
            (terrain, slow_jobs, editable_settings),
            (id_maps, dt, settings, build_areas),
            can_build,
            mut force_updates,
            is_rider,
            is_volume_rider,
            mut skill_sets,
            mut character_classes,
            healths,
            bodies,
            scales,
            mut block_changes,
            mut positions,
            mut velocities,
            mut orientations,
            mut presences,
            mut clients,
            mut controllers,
            mut spectating_entities,
            mut player_physics_settings_,
            mut terrain_persistence,
            players,
            admins,
            remote_senses,
        ): Self::SystemData,
    ) {
        let time_for_vd_changes = Instant::now();

        // NOTE: stdlib mutex is more than good enough on Linux and (probably) Windows,
        // but not Mac.
        let rare_writes = parking_lot::Mutex::new(RareWrites {
            block_changes: &mut block_changes,
            _terrain_persistence: &mut terrain_persistence,
        });

        let player_physics_settings = &*player_physics_settings_;
        let mut deferred_updates = (
            &entities,
            &mut clients,
            (&mut presences).maybe(),
            players.maybe(),
            admins.maybe(),
            remote_senses.maybe(),
            (&skill_sets).maybe(),
            (&character_classes).maybe(),
            (&mut positions).maybe(),
            (&mut velocities).maybe(),
            (&mut orientations).maybe(),
            (&mut controllers).maybe(),
            (&mut force_updates).maybe(),
        )
            .join()
            // NOTE: Required because Specs has very poor work splitting for sparse joins.
            .par_bridge()
            .map_init(
                || events.get_emitters(),
                |emitters, (
                    entity,
                    client,
                    mut maybe_presence,
                    maybe_player,
                    maybe_admin,
                    maybe_remote_sense,
                    skill_set,
                    character_class,
                    ref mut pos,
                    ref mut vel,
                    ref mut ori,
                    ref mut controller,
                    ref mut force_update,
                )| {
                    let old_player_physics_setting = maybe_player.map(|p| {
                        player_physics_settings
                            .settings
                            .get(&p.uuid())
                            .copied()
                            .unwrap_or_default()
                    });
                    let mut new_player_physics_setting = old_player_physics_setting;
                    let is_server_physics_forced = maybe_player.is_none_or(|p| editable_settings.server_physics_force_list.contains_key(&p.uuid()));
                    // If an `ExitInGame` message is received this is set to `None` allowing further
                    // ingame messages to be ignored.
                    let mut clearable_maybe_presence = maybe_presence.as_deref_mut();
                    let mut skill_set = skill_set.map(Cow::Borrowed);
                    let mut character_class_owned = character_class.copied();
                    let mut player_physics = None;
                    let mut spectating_entity = None;
                    let _ = super::try_recv_all(client, 2, |client, msg| {
                        Self::handle_client_in_game_msg(
                            emitters,
                            entity,
                            client,
                            &mut clearable_maybe_presence,
                            &terrain,
                            &can_build,
                            &is_rider,
                            &is_volume_rider,
                            force_update.as_ref(),
                            &mut skill_set,
                            &mut character_class_owned,
                            &healths,
                            &rare_writes,
                            pos.as_deref_mut(),
                            &mut spectating_entity,
                            controller.as_deref_mut(),
                            &settings,
                            &build_areas,
                            new_player_physics_setting.as_mut(),
                            is_server_physics_forced,
                            &maybe_admin,
                            &maybe_remote_sense,
                            time_for_vd_changes,
                            msg,
                            &mut player_physics,
                        )
                    });

                    if let Some((new_pos, new_vel, new_ori)) = player_physics
                        && let Some(old_pos) = pos.as_deref_mut()
                        && let Some(old_vel) = vel.as_deref_mut()
                        && let Some(old_ori) = ori.as_deref_mut()
                    {
                        enum Rejection {
                            TooFar { old: Vec3<f32>, new: Vec3<f32> },
                            TooFast { vel: Vec3<f32> },
                            InsideTerrain,
                        }

                        let rejection = if maybe_admin.is_some() {
                            None
                        } else {
                            // Reminder: review these frequently to ensure they're reasonable
                            const MAX_H_VELOCITY: f32 = 75.0;
                            const MAX_V_VELOCITY: std::ops::Range<f32> = -100.0..80.0;

                            'rejection: {
                                let is_velocity_ok = new_vel.0.xy().magnitude_squared() < MAX_H_VELOCITY.powi(2)
                                    && MAX_V_VELOCITY.contains(&new_vel.0.z);

                                if !is_velocity_ok {
                                    break 'rejection Some(Rejection::TooFast { vel: new_vel.0 });
                                }

                                // How far the player is permitted to stray from the correct position (perhaps due to
                                // latency problems).
                                const POSITION_THRESHOLD: f32 = 16.0;

                                // The position can either be sensible with respect to either the old or the new
                                // velocity such that we don't punish for edge cases after a sudden change
                                let is_position_ok = [old_vel.0, new_vel.0]
                                    .into_iter()
                                    .any(|ref_vel| {
                                        let rpos = new_pos.0 - old_pos.0;
                                        // Determine whether the change in position is broadly consistent with both
                                        // the magnitude and direction of the velocity, with appropriate thresholds.
                                        LineSegment3 {
                                            start: Vec3::zero(),
                                            end: ref_vel * dt.0,
                                        }
                                            .projected_point(rpos)
                                            // + 1.5 accounts for minor changes in position without corresponding
                                            // velocity like block hopping/snapping
                                            .distance_squared(rpos) < (rpos.magnitude() * 0.5 + 1.5 + POSITION_THRESHOLD).powi(2)
                                    });

                                if !is_position_ok {
                                    break 'rejection Some(Rejection::TooFar { old: old_pos.0, new: new_pos.0 });
                                }

                                // Checks that are only relevant if the position changed
                                if new_pos.0 != old_pos.0 {
                                    // Reject updates that would move the entity into terrain
                                    let scale = scales.get(entity).map_or(1.0, |s| s.0);
                                    let min_z = new_pos.0.z as i32;
                                    let height = bodies.get(entity).map_or(0.0, |b| b.height()) * scale;
                                    let head_pos_z = (new_pos.0.z + height) as i32;

                                    if !(min_z..=head_pos_z).any(|z| {
                                        let pos = new_pos.0.as_().with_z(z);

                                        terrain
                                            .get(pos)
                                            .is_ok_and(|block| block.is_fluid())
                                    }) {
                                        break 'rejection Some(Rejection::InsideTerrain);
                                    }
                                }

                                None
                            }
                        };

                        if let Some(rejection) = rejection {
                            // TODO: Log when false positives aren't generated often
                            let alias = maybe_player.map(|p| &p.alias);
                            match rejection {
                                Rejection::TooFar { old, new } => warn!("Rejected physics for player {alias:?} (new position {new:?} is too far from old position {old:?})"),
                                Rejection::TooFast { vel } => warn!("Rejected physics for player {alias:?} (new velocity {vel:?} is too fast)"),
                                Rejection::InsideTerrain => warn!("Rejected physics for player {alias:?}: Inside terrain."),
                            }

                            /*
                            // Perhaps this is overzealous?
                            if let Some(mut setting) = new_player_physics_setting.as_mut() {
                                setting.server_force = true;
                                warn!("Switching player {alias:?} to server-side physics");
                            }
                            */

                            // Reject the change and force the server's view of the physics state
                            force_update.as_mut().map(|fu| fu.update());
                        } else {
                            *old_pos = new_pos;
                            *old_vel = new_vel;
                            *old_ori = new_ori;
                        }
                    }

                    // Ensure deferred view distance changes are applied (if the
                    // requsite time has elapsed).
                    if let Some(presence) = maybe_presence {
                        presence.terrain_view_distance.update(time_for_vd_changes);
                        presence.entity_view_distance.update(time_for_vd_changes);
                    }

                    // Return the possibly modified skill set, and possibly modified server physics
                    // settings.
                    let skill_set_update = skill_set.and_then(|skill_set| match skill_set {
                        Cow::Borrowed(_) => None,
                        Cow::Owned(skill_set) => Some((entity, skill_set)),
                    });
                    // NOTE: Since we pass Option<&mut _> rather than &mut Option<_> to
                    // handle_client_in_game_msg, and the new player was initialized to the same
                    // value as the old setting , we know that either both the new and old setting
                    // are Some, or they are both None.
                    let physics_update = maybe_player.map(|p| p.uuid())
                        .zip(new_player_physics_setting
                             .filter(|_| old_player_physics_setting != new_player_physics_setting));
                     let spectating_entity_update = spectating_entity.map(|e| (entity, e));
                    // Only a real, rarely-toggled preference change, never a per-tick write —
                    // compared against the borrowed pre-message value so an untouched message
                    // batch never produces a spurious deferred write.
                    let character_class_update = character_class_owned
                        .zip(character_class.copied())
                        .filter(|(new, old)| new != old)
                        .map(|(new, _)| (entity, new));
                    (skill_set_update, spectating_entity_update, physics_update, character_class_update)
                },
            )
            // NOTE: Would be nice to combine this with the map_init somehow, but I'm not sure if
            // that's possible.
            .filter(|(x, y, z, w)| x.is_some() || y.is_some() || z.is_some() || w.is_some())
            // NOTE: I feel like we shouldn't actually need to allocate here, but hopefully this
            // doesn't turn out to be important as there shouldn't be that many connected clients.
            // The reason we can't just use unzip is that the two sides might be different lengths.
            .collect::<Vec<_>>();
        let player_physics_settings = &mut *player_physics_settings_;
        // Deferred updates to skillsets and player physics.
        //
        // NOTE: It is an invariant that there is at most one client entry per player
        // uuid; since we joined on clients, it follows that there's just one update
        // per uuid, so the physics update is sound and doesn't depend on evaluation
        // order, even though we're not updating directly by entity or uid (note that
        // for a given entity, we process messages serially).
        deferred_updates.iter_mut().for_each(
            |(
                skill_set_update,
                spectating_entity_update,
                physics_update,
                character_class_update,
            )| {
                if let Some((entity, new_skill_set)) = skill_set_update {
                    // We know this exists, because we already iterated over it with the skillset
                    // lock taken, so we can ignore the error.
                    //
                    // Note that we replace rather than just updating.  This is in order to avoid
                    // dropping here; we'll drop later on a background thread, in case skillsets are
                    // slow to drop.
                    skill_sets
                        .get_mut(*entity)
                        .map(|mut old_skill_set| mem::swap(&mut *old_skill_set, new_skill_set));
                }
                if let &mut Some((entity, spectating_uid)) = spectating_entity_update {
                    if let Some(uid) = spectating_uid
                        && let Some(spectated_entity) = id_maps.uid_entity(uid)
                    {
                        // We know this exists, so can ignore the error.
                        let _ =
                            spectating_entities.insert(entity, SpectatingEntity(spectated_entity));
                    } else {
                        spectating_entities.remove(entity);
                    }
                }
                if let &mut Some((uuid, player_physics_setting)) = physics_update {
                    // We don't necessarily know this exists, but that's fine, because dropping
                    // player physics is a no op.
                    player_physics_settings
                        .settings
                        .insert(uuid, player_physics_setting);
                }
                if let &mut Some((entity, new_character_class)) = character_class_update {
                    // We know this exists, for the same reason as the skillset case above.
                    character_classes
                        .get_mut(entity)
                        .map(|mut cc| *cc = new_character_class);
                }
            },
        );
        // Finally, drop the deferred updates in another thread.
        slow_jobs.spawn("CHUNK_DROP", move || {
            drop(deferred_updates);
        });
    }
}

/// Whether a `ClientGeneral::SpectateEntity(requested)` message should be
/// honoured.
///
/// Moderators may spectate anything, and stopping (`requested == None`) is
/// always allowed. Everyone else may spectate ONLY the entity their own
/// active `RemoteSense` link is anchored to — `remote_sense` here must always
/// be read from the SERVER's own component storage for the sending entity,
/// never anything the client claims about itself, or this check is a
/// wall-hack.
///
/// Pulled out as its own pure function (rather than left inline in the
/// message match) so this security property is directly unit-testable
/// without needing a full ECS/network fixture.
fn spectate_entity_permitted(
    is_moderator: bool,
    requested: Option<Uid>,
    remote_sense: Option<&RemoteSense>,
) -> bool {
    is_moderator
        || requested.is_none()
        || requested.is_some_and(|target| remote_sense.is_some_and(|rs| rs.anchor_uid() == target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::comp::remote_sense::SenseAnchor;
    use std::num::NonZeroU64;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn remote_sense_anchored_to(target: Uid) -> RemoteSense {
        RemoteSense {
            anchor: SenseAnchor::Sensor(target),
            free_look: true,
            piloted: false,
            caster: uid(1),
        }
    }

    #[test]
    fn moderator_may_spectate_anything() {
        assert!(spectate_entity_permitted(true, Some(uid(99)), None));
    }

    #[test]
    fn stopping_is_always_allowed() {
        assert!(spectate_entity_permitted(false, None, None));
        let rs = remote_sense_anchored_to(uid(5));
        assert!(spectate_entity_permitted(false, None, Some(&rs)));
    }

    #[test]
    fn non_moderator_with_no_remote_sense_is_refused() {
        assert!(!spectate_entity_permitted(false, Some(uid(5)), None));
    }

    #[test]
    fn non_moderator_targeting_an_entity_their_remote_sense_does_not_name_is_refused() {
        // The link is anchored to entity 5, but the client is asking to
        // spectate entity 6: this is exactly what a hacked client spamming
        // `SpectateEntity` with an arbitrary `Uid` would send, and it must be
        // refused even though a (mismatched) `RemoteSense` link exists.
        let rs = remote_sense_anchored_to(uid(5));
        assert!(!spectate_entity_permitted(false, Some(uid(6)), Some(&rs)));
    }

    #[test]
    fn non_moderator_with_a_matching_remote_sense_is_accepted() {
        let rs = remote_sense_anchored_to(uid(5));
        assert!(spectate_entity_permitted(false, Some(uid(5)), Some(&rs)));
    }
}
