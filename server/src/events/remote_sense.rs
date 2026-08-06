//! Server-side resolution of [`ResolveRemoteSenseEvent`]: the cast-time step
//! that turns a validated target/position into a written `RemoteSense` link.
//!
//! Everything here is authoritative — the event carries only the client's
//! raw, unchecked cast-time intent (which entity or point it targeted), and
//! every predicate that decides whether that intent is honoured runs in this
//! handler, never on the client. `server/src/sys/remote_sense.rs`'s own
//! per-tick system then re-runs the equivalent check against whatever this
//! handler wrote, every tick, for as long as the link lasts — this handler
//! only ever runs once, at cast time.

use common::{
    comp::{
        Alignment, Body, Buffs, Collider, Density, Energy, Health, Immovable, Inventory, Mass,
        Object, Ori, Poise, Pos, Presence, RemoteSense, SkillSet, Stats, Vel,
        buff::SenseAnchorKind, object::Body as ObjectBody, pet::is_tameable,
        projectile::ProjectileHitEntities, remote_sense::SenseAnchor,
    },
    event::ResolveRemoteSenseEvent,
    resources::Time,
    terrain::{TerrainGrid, positions_have_line_of_sight},
    uid::{IdMaps, Uid},
};
use common_i18n::Content;
use specs::{
    Entities, Entity, Read, ReadExpect, ReadStorage, SystemData, Write, WriteStorage, shred,
};
use std::time::Duration;
use vek::Vec3;

use crate::sys::remote_sense::max_sense_range;

use super::ServerEvent;

/// Outer bound on how long a spawned sensor can exist before its
/// belt-and-braces `Object::DeleteAfter` claims it, independent of the
/// sustaining buff's own duration. The buff ending is the normal, prompt
/// despawn path (`server/src/sys/remote_sense.rs`'s per-tick teardown, which
/// also reaps the sensor directly); this is only a backstop against a bug
/// leaving that teardown from ever running, so it is set comfortably above
/// every remote-sensing spell's shipped duration (a 60s `Concentration` cap,
/// currently, for all of them) rather than threaded through as an exact
/// value.
const SENSOR_MAX_LIFETIME: Duration = Duration::from_secs(300);

#[derive(SystemData)]
pub struct ResolveRemoteSenseEventData<'a> {
    entities: Entities<'a>,
    id_maps: Write<'a, IdMaps>,
    time: Read<'a, Time>,
    terrain: ReadExpect<'a, TerrainGrid>,
    uids: WriteStorage<'a, Uid>,
    positions: WriteStorage<'a, Pos>,
    velocities: WriteStorage<'a, Vel>,
    orientations: WriteStorage<'a, Ori>,
    masses: WriteStorage<'a, Mass>,
    densities: WriteStorage<'a, Density>,
    colliders: WriteStorage<'a, Collider>,
    bodies: WriteStorage<'a, Body>,
    alignments: WriteStorage<'a, Alignment>,
    buffs: WriteStorage<'a, Buffs>,
    healths: WriteStorage<'a, Health>,
    stats: WriteStorage<'a, Stats>,
    energies: WriteStorage<'a, Energy>,
    poises: WriteStorage<'a, Poise>,
    skill_sets: WriteStorage<'a, SkillSet>,
    inventories: WriteStorage<'a, Inventory>,
    immovables: WriteStorage<'a, Immovable>,
    projectile_hit_entities: WriteStorage<'a, ProjectileHitEntities>,
    objects: WriteStorage<'a, Object>,
    presences: ReadStorage<'a, Presence>,
    remote_senses: WriteStorage<'a, RemoteSense>,
}

impl ServerEvent for ResolveRemoteSenseEvent {
    type SystemData<'a> = ResolveRemoteSenseEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        for ev in events {
            if !data.entities.is_alive(ev.entity) {
                continue;
            }
            let Some(caster_uid) = data.uids.get(ev.entity).copied() else {
                continue;
            };
            let Some(caster_pos) = data.positions.get(ev.entity).copied() else {
                continue;
            };

            match ev.anchor_kind {
                SenseAnchorKind::Existing => {
                    resolve_existing(&mut data, &ev, ev.entity, caster_uid, caster_pos);
                },
                SenseAnchorKind::Sensor => {
                    resolve_sensor(&mut data, &ev, ev.entity, caster_uid, caster_pos);
                },
                // A piloted eye spawns the same kind of sensor entity plus a
                // pilot link this event does not build -- left unimplemented
                // here rather than guessed at ahead of the spell that first
                // needs it (`arcane_eye`). Matched explicitly (not `_`) so
                // adding a future `SenseAnchorKind` variant fails to compile
                // here instead of silently falling through.
                SenseAnchorKind::Piloted => {},
            }
        }
    }
}

/// The `SenseAnchorKind::Existing` predicate: the target must be a beast the
/// caster owns (pet/summon) or has charmed, alive, and within the caster's
/// own sense range. On any failure this simply does not write `RemoteSense`
/// -- the buff's own concentration/duration then runs its course with no
/// remote view ever granted, exactly like a spell cast at a target that
/// turns out to be invalid.
fn resolve_existing(
    data: &mut ResolveRemoteSenseEventData,
    ev: &ResolveRemoteSenseEvent,
    caster: Entity,
    caster_uid: Uid,
    caster_pos: Pos,
) {
    let Some(target_uid) = ev.target_entity else {
        return;
    };
    let Some(target_entity) = data.id_maps.uid_entity(target_uid) else {
        return;
    };
    let Some(target_body) = data.bodies.get(target_entity) else {
        return;
    };
    let Some(target_pos) = data.positions.get(target_entity).copied() else {
        return;
    };

    let is_beast = is_tameable(target_body);
    let is_owned_or_charmed = data.alignments.get(target_entity)
        == Some(&Alignment::Owned(caster_uid))
        || data
            .buffs
            .get(target_entity)
            .is_some_and(|buffs| buffs.charmed_by(caster_uid));
    let is_alive = data
        .healths
        .get(target_entity)
        .is_some_and(|health| !health.is_dead);
    let max_range = max_sense_range(data.presences.get(caster));
    let is_in_range = target_pos.0.distance_squared(caster_pos.0) <= max_range * max_range;

    if is_beast && is_owned_or_charmed && is_alive && is_in_range {
        let _ = data.remote_senses.insert(caster, RemoteSense {
            anchor: SenseAnchor::Existing(target_uid),
            free_look: ev.free_look,
            piloted: ev.piloted,
            caster: caster_uid,
        });
    }
}

/// The `SenseAnchorKind::Sensor` predicate and spawn: the cast's target point
/// must be within the caster's own sense range *and* have an unobstructed
/// line of sight from the caster's position. `common/src/states/blink.rs`'s
/// `update.pos.0 = pos` (no range check at all) is the anti-pattern this
/// guards against -- a client-supplied position is never trusted without
/// both checks. On any failure this simply does not spawn a sensor or write
/// `RemoteSense`, the same "no remote view granted" shape as
/// `resolve_existing`.
///
/// The spawned entity mirrors `Object::Crux`'s shape (the shipped precedent
/// for a small, `Health`-bearing, attackable object prop) rather than
/// `NpcBuilder`/`CreateNpcEvent`: that path defers entity creation to a later
/// serial event and only returns the new `Uid` once it runs, but this
/// `SenseAnchor::Sensor(uid)` must be known and written into `RemoteSense`
/// synchronously, in this same call. Built with the entities/storages this
/// `SystemData` already borrows instead.
fn resolve_sensor(
    data: &mut ResolveRemoteSenseEventData,
    ev: &ResolveRemoteSenseEvent,
    caster: Entity,
    caster_uid: Uid,
    caster_pos: Pos,
) {
    let Some(target_pos) = ev.target_pos else {
        return;
    };

    let max_range = max_sense_range(data.presences.get(caster));
    let is_in_range = target_pos.distance_squared(caster_pos.0) <= max_range * max_range;
    let has_los = positions_have_line_of_sight(&data.terrain, caster_pos.0, target_pos);
    if !is_in_range || !has_los {
        return;
    }

    let sensor = data.entities.create();
    let sensor_uid = data.id_maps.allocate(sensor);
    let body = Body::Object(ObjectBody::RemoteSensor);

    let _ = data.uids.insert(sensor, sensor_uid);
    let _ = data.positions.insert(sensor, Pos(target_pos));
    let _ = data.velocities.insert(sensor, Vel(Vec3::zero()));
    let _ = data.orientations.insert(sensor, Ori::default());
    let _ = data.masses.insert(sensor, body.mass());
    let _ = data.densities.insert(sensor, body.density());
    // Never an absent `Collider`: that drops `PhysicsState` entirely, which
    // silently drops the entity from the figure renderer.
    let _ = data.colliders.insert(sensor, Collider::Point);
    let _ = data.bodies.insert(sensor, body);
    // Never hostile, never AI-targeted -- see `ConcealedUnlessTrueSight` on
    // the entity's own concealment for why it stays safe from a player
    // without True Sight regardless.
    let _ = data.alignments.insert(sensor, Alignment::Passive);
    // A one-shot destructible prop, not a creature: 30 HP, no regen buff ever
    // applied. Reaching 0 runs the entity through the standard destroy/delete
    // pipeline, which then invalidates this link on the very next tick of
    // `server/src/sys/remote_sense.rs` (the anchor `Uid` no longer resolves)
    // -- the same duration-expiry/concentration-break teardown, not a second
    // "spell ends" path.
    let _ = data.healths.insert(sensor, Health::new(body));
    let _ = data.stats.insert(
        sensor,
        Stats::new(Content::Key(String::from("name-remote-sensor")), body),
    );
    let _ = data.energies.insert(sensor, Energy::new(body));
    let _ = data.poises.insert(sensor, Poise::new(body));
    let _ = data.skill_sets.insert(sensor, SkillSet::default());
    let _ = data.buffs.insert(sensor, Buffs::default());
    let _ = data.inventories.insert(sensor, Inventory::with_empty());
    let _ = data.immovables.insert(sensor, Immovable);
    let _ = data
        .projectile_hit_entities
        .insert(sensor, ProjectileHitEntities::default());
    // Belt-and-braces reaper alongside the buff-end teardown -- see
    // `SENSOR_MAX_LIFETIME`'s own doc comment.
    let _ = data.objects.insert(sensor, Object::DeleteAfter {
        spawned_at: *data.time,
        timeout: SENSOR_MAX_LIFETIME,
    });

    let _ = data.remote_senses.insert(caster, RemoteSense {
        anchor: SenseAnchor::Sensor(sensor_uid),
        free_look: ev.free_look,
        piloted: ev.piloted,
        caster: caster_uid,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        ViewDistances,
        comp::{
            Buffs, PresenceKind,
            buff::{Buff, BuffData, BuffKind, BuffSource},
            quadruped_medium,
        },
        terrain::{Block, BlockKind, MapSizeLg, SpriteKind, TerrainChunk, TerrainChunkMeta},
        vol::WriteVol,
    };
    use specs::{Builder, Join, WorldExt};
    use std::{num::NonZeroU64, sync::Arc};
    use vek::{Rgb, Vec2, Vec3};

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    /// A single-chunk, all-air terrain grid covering world positions roughly
    /// `[0, 32)` on x/y -- large enough to keep every test's caster/target
    /// pair inside one chunk, so nothing here exercises cross-chunk sampling.
    fn empty_terrain() -> TerrainGrid {
        let air = Block::air(SpriteKind::Empty);
        let chunk = TerrainChunk::new(0, air, air, TerrainChunkMeta::void());
        let mut terrain = TerrainGrid::new(
            MapSizeLg::new(Vec2::new(1, 1)).unwrap(),
            Arc::new(chunk.clone()),
        )
        .unwrap();
        terrain.insert(Vec2::new(0, 0), Arc::new(chunk));
        terrain
    }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Vel>();
        world.register::<Ori>();
        world.register::<Mass>();
        world.register::<Density>();
        world.register::<Collider>();
        world.register::<Body>();
        world.register::<Alignment>();
        world.register::<Buffs>();
        world.register::<Health>();
        world.register::<Stats>();
        world.register::<Energy>();
        world.register::<Poise>();
        world.register::<SkillSet>();
        world.register::<Inventory>();
        world.register::<Immovable>();
        world.register::<ProjectileHitEntities>();
        world.register::<Object>();
        world.register::<Presence>();
        world.register::<Uid>();
        world.register::<RemoteSense>();
        world.insert(IdMaps::default());
        world.insert(Time(0.0));
        world.insert(empty_terrain());
        world
    }

    // A fixed, known-tameable species -- `Body::random()` can land on one of
    // `is_tameable`'s excluded species (Catoblepas/Mammoth/Elephant/
    // Hirdrasil), which would make these tests flaky.
    fn tameable_body() -> Body {
        Body::QuadrupedMedium(quadruped_medium::Body {
            species: quadruped_medium::Species::Wolf,
            body_type: quadruped_medium::BodyType::Female,
        })
    }

    fn event(caster: Entity, target: Uid) -> ResolveRemoteSenseEvent {
        ResolveRemoteSenseEvent {
            entity: caster,
            target_entity: Some(target),
            target_pos: None,
            anchor_kind: SenseAnchorKind::Existing,
            free_look: false,
            piloted: false,
        }
    }

    fn dispatch(world: &specs::World, ev: ResolveRemoteSenseEvent) {
        let data = ResolveRemoteSenseEventData::fetch(world);
        <ResolveRemoteSenseEvent as ServerEvent>::handle(vec![ev].into_iter(), data);
    }

    #[test]
    fn owned_pet_in_range_gets_a_remote_sense_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);

        // No `Presence` on the caster: `max_sense_range` then defaults to
        // 0.0, which is still satisfied here because both entities share the
        // same position -- keeps this test about the ownership/aliveness
        // predicate rather than the range clamp (covered separately by
        // `server/src/sys/remote_sense.rs`'s own tests).
        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();
        let target = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(2).unwrap()))
            .with(tameable_body())
            .with(Alignment::Owned(caster_uid))
            .with(Health::new(tameable_body()))
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(target_uid, target);
        }

        dispatch(&world, event(caster, target_uid));

        assert_eq!(
            world
                .read_storage::<RemoteSense>()
                .get(caster)
                .map(|rs| rs.anchor),
            Some(SenseAnchor::Existing(target_uid)),
            "an owned, alive, in-range beast must grant the link"
        );
    }

    #[test]
    fn unowned_uncharmed_target_gets_no_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);

        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();
        // Not owned by the caster, not charmed -- a wild animal.
        let target = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(2).unwrap()))
            .with(tameable_body())
            .with(Alignment::Wild)
            .with(Health::new(tameable_body()))
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(target_uid, target);
        }

        dispatch(&world, event(caster, target_uid));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "a target the caster neither owns nor has charmed must not grant a link"
        );
    }

    #[test]
    fn charmed_but_unowned_target_still_gets_a_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);

        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();

        let mut buffs = Buffs::default();
        buffs.insert(
            Buff::new(
                BuffKind::Charmed,
                BuffData::new(0.0, None),
                Vec::new(),
                BuffSource::Character {
                    by: caster_uid,
                    tool_kind: None,
                },
                common::resources::Time(0.0),
                common::comp::buff::DestInfo::default(),
                None,
                None,
                None,
            ),
            common::resources::Time(0.0),
        );

        let target = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Uid(NonZeroU64::new(2).unwrap()))
            .with(tameable_body())
            .with(Alignment::Wild)
            .with(Health::new(tameable_body()))
            .with(buffs)
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(target_uid, target);
        }

        dispatch(&world, event(caster, target_uid));

        assert_eq!(
            world
                .read_storage::<RemoteSense>()
                .get(caster)
                .map(|rs| rs.anchor),
            Some(SenseAnchor::Existing(target_uid)),
            "a creature charmed by the caster (even if not owned) must grant the link"
        );
    }

    fn sensor_event(caster: Entity, target_pos: Vec3<f32>) -> ResolveRemoteSenseEvent {
        ResolveRemoteSenseEvent {
            entity: caster,
            target_entity: None,
            target_pos: Some(target_pos),
            anchor_kind: SenseAnchorKind::Sensor,
            free_look: true,
            piloted: false,
        }
    }

    #[test]
    fn sensor_out_of_range_target_grants_no_link_and_spawns_nothing() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let caster = world
            .create_entity()
            .with(Pos(Vec3::new(4.0, 4.0, 5.0)))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);

        let entities_before = world.entities().join().count();

        // No `Presence` on the caster: `max_sense_range` defaults to 0.0, so
        // any nonzero distance is out of range -- mirrors the `Existing`
        // anchor's own out-of-range shape, just via the `Sensor` predicate.
        dispatch(&world, sensor_event(caster, Vec3::new(14.0, 4.0, 5.0)));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "an out-of-range target must not grant a link"
        );
        assert_eq!(
            world.entities().join().count(),
            entities_before,
            "an out-of-range target must not spawn a sensor entity"
        );
    }

    #[test]
    fn sensor_blocked_line_of_sight_grants_no_link_and_spawns_nothing() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let caster = world
            .create_entity()
            .with(Pos(Vec3::new(4.0, 4.0, 5.0)))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);

        // A solid block directly between caster and target.
        world
            .write_resource::<TerrainGrid>()
            .set(
                Vec3::new(9, 4, 5),
                Block::new(BlockKind::Rock, Rgb::new(128, 128, 128)),
            )
            .unwrap();

        let entities_before = world.entities().join().count();

        dispatch(&world, sensor_event(caster, Vec3::new(14.0, 4.0, 5.0)));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "a target with an obstructed line of sight must not grant a link"
        );
        assert_eq!(
            world.entities().join().count(),
            entities_before,
            "a blocked line of sight must not spawn a sensor entity"
        );
    }

    #[test]
    fn sensor_within_range_and_clear_los_spawns_a_healthy_incorporeal_sensor() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let caster = world
            .create_entity()
            .with(Pos(Vec3::new(4.0, 4.0, 5.0)))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);

        let target_pos = Vec3::new(14.0, 4.0, 5.0);
        dispatch(&world, sensor_event(caster, target_pos));

        let remote_sense = world
            .read_storage::<RemoteSense>()
            .get(caster)
            .copied()
            .expect("an in-range, unobstructed target must grant a link");
        let SenseAnchor::Sensor(sensor_uid) = remote_sense.anchor else {
            panic!("expected a Sensor anchor, got {:?}", remote_sense.anchor);
        };
        assert_eq!(remote_sense.caster, caster_uid);
        assert!(remote_sense.free_look, "clairvoyance's sensor is free-look");

        let sensor = world
            .read_resource::<IdMaps>()
            .uid_entity(sensor_uid)
            .expect("the sensor's Uid must resolve to a live entity");

        assert_eq!(
            world.read_storage::<Pos>().get(sensor).map(|p| p.0),
            Some(target_pos),
            "the sensor spawns exactly at the validated target position"
        );
        assert!(
            matches!(
                world.read_storage::<Collider>().get(sensor),
                Some(Collider::Point)
            ),
            "never an absent Collider -- an absent one silently drops the figure render"
        );
        assert_eq!(
            world.read_storage::<Alignment>().get(sensor),
            Some(&Alignment::Passive),
            "never hostile, never AI-targeted"
        );
        let health = world
            .read_storage::<Health>()
            .get(sensor)
            .map(|h| h.maximum())
            .expect("the sensor must carry Health so it is destructible");
        assert_eq!(
            health, 30.0,
            "a one-shot 30 HP pool, per the spell's design"
        );
        assert_eq!(
            world.read_storage::<Body>().get(sensor),
            Some(&Body::Object(ObjectBody::RemoteSensor)),
            "the sensor's own dedicated object body variant"
        );
    }
}
