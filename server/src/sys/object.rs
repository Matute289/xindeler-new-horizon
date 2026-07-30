use common::{
    CachedSpatialGrid, Damage, DamageKind, GroupTarget,
    combat::{Attack, AttackDamage},
    comp::{Body, Object, Ori, Pos, Teleporting, Vel, beam, object},
    consts::TELEPORTER_RADIUS,
    event::{ChangeBodyEvent, DeleteEvent, EmitExt, EventBus},
    event_emitters,
    outcome::Outcome,
    resources::{DeltaTime, Secs, Time},
    states::basic_summon::BeamPillarIndicatorSpecifier,
    terrain::{Block, TerrainGrid},
    uid::IdMaps,
    vol::ReadVol,
};
use common_ecs::{Job, Origin, Phase, System};
use hashbrown::HashMap;
use specs::{Entities, Join, LazyUpdate, LendJoin, Read, ReadExpect, ReadStorage, WriteStorage};
use vek::{QuadraticBezier3, Vec2, Vec3};

event_emitters! {
    struct Events[Emitters] {
        delete: DeleteEvent,
        change_body: ChangeBodyEvent,
    }
}

/// This system is responsible for handling misc object behaviours
#[derive(Default)]
pub struct Sys;
impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        Read<'a, DeltaTime>,
        Read<'a, Time>,
        Read<'a, EventBus<Outcome>>,
        Read<'a, CachedSpatialGrid>,
        ReadStorage<'a, Pos>,
        ReadStorage<'a, Vel>,
        WriteStorage<'a, Object>,
        ReadStorage<'a, Body>,
        ReadStorage<'a, Teleporting>,
        ReadStorage<'a, beam::Beam>,
        Read<'a, LazyUpdate>,
        ReadExpect<'a, TerrainGrid>,
        Read<'a, IdMaps>,
        ReadStorage<'a, Ori>,
    );

    const NAME: &'static str = "object";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            entities,
            events,
            dt,
            time,
            outcome_bus,
            spatial_grid,
            positions,
            velocities,
            mut objects,
            bodies,
            teleporting,
            beams,
            updater,
            terrain,
            id_maps,
            orientations,
        ): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();

        // Objects
        for (entity, pos, vel, object, body) in (
            &entities,
            &positions,
            velocities.maybe(),
            &mut objects,
            bodies.maybe(),
        )
            .join()
        {
            match object {
                Object::DeleteAfter {
                    spawned_at,
                    timeout,
                } => {
                    if (time.0 - spawned_at.0).max(0.0) > timeout.as_secs_f64() {
                        emitters.emit(DeleteEvent(entity));
                    }
                },
                Object::Portal { .. } => {
                    let is_active = spatial_grid
                        .0
                        .in_circle_aabr(pos.0.xy(), TELEPORTER_RADIUS)
                        .any(|entity| {
                            (&positions, &teleporting)
                                .lend_join()
                                .get(entity, &entities)
                                .is_some_and(|(teleporter_pos, _)| {
                                    pos.0.distance_squared(teleporter_pos.0)
                                        <= TELEPORTER_RADIUS.powi(2)
                                })
                        });

                    if body.is_some_and(|body| {
                        (*body == Body::Object(object::Body::PortalActive)) != is_active
                    }) {
                        emitters.emit(ChangeBodyEvent {
                            entity,
                            new_body: Body::Object(if is_active {
                                outcome_bus.emit_now(Outcome::PortalActivated { pos: pos.0 });
                                object::Body::PortalActive
                            } else {
                                object::Body::Portal
                            }),
                            permanent_change: None,
                        });
                    }
                },
                Object::BeamPillar {
                    spawned_at,
                    buildup_duration,
                    attack_duration,
                    beam_duration,
                    radius,
                    height,
                    damage,
                    damage_effect,
                    dodgeable,
                    tick_rate,
                    specifier,
                    indicator_specifier,
                } => {
                    match indicator_specifier {
                        BeamPillarIndicatorSpecifier::FirePillar => {
                            outcome_bus.emit_now(Outcome::FirePillarIndicator {
                                pos: pos.0,
                                radius: *radius,
                            })
                        },
                    }

                    let age = (time.0 - spawned_at.0).max(0.0);
                    let buildup = buildup_duration.as_secs_f64();
                    let attack = attack_duration.as_secs_f64();

                    if age > buildup + attack {
                        emitters.emit(DeleteEvent(entity));
                    } else if age > buildup && !beams.contains(entity) {
                        let mut attack_damage = AttackDamage::new(
                            Damage {
                                kind: DamageKind::Energy,
                                value: *damage,
                            },
                            Some(GroupTarget::OutOfGroup),
                            rand::random(),
                        );
                        if let Some(combat_effect) = damage_effect {
                            attack_damage = attack_damage.with_effect(combat_effect.clone());
                        }

                        updater.insert(entity, beam::Beam {
                            attack: Attack::new(None).with_damage(attack_damage),
                            dodgeable: *dodgeable,
                            start_radius: *radius,
                            end_radius: *radius,
                            range: *height,
                            duration: Secs(beam_duration.as_secs_f64()),
                            tick_dur: Secs(1.0 / *tick_rate as f64),
                            hit_entities: Vec::new(),
                            hit_durations: HashMap::new(),
                            specifier: *specifier,
                            bezier: QuadraticBezier3 {
                                start: pos.0,
                                ctrl: pos.0,
                                end: pos.0,
                            },
                        });
                    }
                },
                Object::Crux { pid_controller, .. } => {
                    if let Some(vel) = vel
                        && let Some(pid_controller) = pid_controller
                        && let Some(accel) = body.and_then(|body| {
                            body.fly_thrust()
                                .map(|fly_thrust| fly_thrust / body.mass().0)
                        })
                    {
                        pid_controller.add_measurement(time.0, pos.0.z);
                        let dir = pid_controller.calc_err();
                        pid_controller.limit_integral_windup(|z| *z = z.clamp(-1.0, 1.0));

                        updater
                            .insert(entity, Vel((vel.0.z + dir * accel * dt.0) * Vec3::unit_z()));
                    }
                },
                Object::FloatingDisk {
                    owner,
                    spawned_at,
                    timeout,
                    follow_distance,
                    hover_height,
                    max_owner_distance,
                } => {
                    // D1: duration expired.
                    if (time.0 - spawned_at.0).max(0.0) > timeout.as_secs_f64() {
                        emitters.emit(DeleteEvent(entity));
                        continue;
                    }
                    // D2: the caster no longer resolves (disconnected, died and
                    // despawned, etc).
                    let Some(owner_entity) = id_maps.uid_entity(*owner) else {
                        emitters.emit(DeleteEvent(entity));
                        continue;
                    };
                    let Some(owner_pos) = positions.get(owner_entity) else {
                        emitters.emit(DeleteEvent(entity));
                        continue;
                    };
                    // D3: the caster moved out of range.
                    if owner_pos.0.distance_squared(pos.0) > max_owner_distance.powi(2) {
                        emitters.emit(DeleteEvent(entity));
                        continue;
                    }

                    const MAX_DROP: f32 = 32.0;
                    const FOLLOW_GAIN: f32 = 3.0;
                    const MAX_SPEED: f32 = 8.0;

                    // Ground clearance beneath the disk itself, not the caster.
                    let dist_to_ground = terrain
                        .ray(pos.0, pos.0 - Vec3::unit_z() * MAX_DROP)
                        .until(Block::is_solid)
                        .cast()
                        .0;
                    let ground_z = pos.0.z - dist_to_ground;
                    let target_z = floating_disk_target_z(ground_z, *hover_height, owner_pos.0.z);

                    let owner_facing =
                        orientations
                            .get(owner_entity)
                            .map_or(Vec2::unit_y(), |ori| {
                                ori.look_vec()
                                    .xy()
                                    .try_normalized()
                                    .unwrap_or(Vec2::unit_y())
                            });
                    let target_xy = owner_pos.0.xy() - owner_facing * *follow_distance;
                    let target = Vec3::new(target_xy.x, target_xy.y, target_z);

                    let mut new_vel = (target - pos.0) * FOLLOW_GAIN;
                    let speed = new_vel.magnitude();
                    if speed > MAX_SPEED {
                        new_vel *= MAX_SPEED / speed;
                    }
                    updater.insert(entity, Vel(new_vel));
                },
            }
        }
    }
}

/// A `floating_disk`'s target hover altitude: ground clearance at the disk's
/// own position, clamped so it can never carry a rider higher than the
/// caster could already stand (`owner_z + 2.0`) or noticeably lower
/// (`owner_z - 1.0`). Without this clamp a follow-the-caster platform is a
/// level-geometry skeleton key — ride it over a dungeon's gating wall or up
/// a cliff the caster could not otherwise reach.
fn floating_disk_target_z(ground_z: f32, hover_height: f32, owner_z: f32) -> f32 {
    (ground_z + hover_height).clamp(owner_z - 1.0, owner_z + 2.0)
}

#[cfg(test)]
mod floating_disk_tests {
    use super::floating_disk_target_z;

    #[test]
    fn hovers_at_ground_plus_hover_height_within_bounds() {
        assert_eq!(floating_disk_target_z(10.0, 1.0, 10.0), 11.0);
    }

    #[test]
    fn clamped_to_owner_minus_one_over_a_deep_pit() {
        // Ground far below (a chasm) must not pull the disk down past
        // owner_z - 1.0.
        assert_eq!(floating_disk_target_z(-50.0, 1.0, 10.0), 9.0);
    }

    #[test]
    fn clamped_to_owner_plus_two_below_a_tall_ledge() {
        // Ground far above (standing at the base of a tall wall/cliff) must
        // not let the disk climb past owner_z + 2.0 — the exploit this
        // clamp exists to close.
        assert_eq!(floating_disk_target_z(100.0, 1.0, 10.0), 12.0);
    }

    #[test]
    fn clamp_bounds_are_exactly_owner_minus_one_and_plus_two() {
        assert_eq!(floating_disk_target_z(f32::MIN, 0.0, 10.0), 9.0);
        assert_eq!(floating_disk_target_z(f32::MAX, 0.0, 10.0), 12.0);
    }
}
