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
        Alignment, Body, Buffs, Collider, ConcealedUnlessTrueSight, Controller, Density, Energy,
        Health, Immovable, Inventory, Mass, Object, Ori, Poise, Pos, Presence, RemoteSense,
        SkillSet, Stats, Vel, buff::SenseAnchorKind, object::Body as ObjectBody, pet::is_tameable,
        projectile::ProjectileHitEntities, remote_sense::SenseAnchor,
    },
    event::ResolveRemoteSenseEvent,
    link::{Is, LinkHandle},
    piloting::{Pilot, Piloted, Piloting},
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

/// How far in front of the caster's own facing `arcane_eye` conjures its eye.
/// The caster never aims this point themselves the way `clairvoyance`'s
/// `select_pos` does -- the eye is piloted immediately after spawning, so
/// letting the player choose an exact spawn point would be redundant
/// complexity. It always spawns along the caster's own look direction.
const EYE_SPAWN_RANGE: f32 = 9.0;

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
    concealed: WriteStorage<'a, ConcealedUnlessTrueSight>,
    controllers: WriteStorage<'a, Controller>,
    is_pilots: WriteStorage<'a, Is<Pilot>>,
    is_piloteds: WriteStorage<'a, Is<Piloted>>,
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
                SenseAnchorKind::Piloted => {
                    resolve_piloted(&mut data, &ev, ev.entity, caster_uid, caster_pos);
                },
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

/// Builds the "invisible, 30 HP, destructible, `Alignment::Passive` prop"
/// shape shared by every remote-sensing sensor entity -- `resolve_sensor`'s
/// `Sensor` anchor and `resolve_piloted`'s `Piloted` anchor alike: `Uid`,
/// `Pos`, `Vel`, `Ori`, `Mass`/`Density` (from `body`), `Collider::Point`
/// (never an absent `Collider` -- an absent one drops `PhysicsState`
/// entirely, which silently drops the entity from the figure renderer),
/// `Body`, `Alignment::Passive`, `ConcealedUnlessTrueSight` (the
/// True-Sight-gated visibility/targetability marker consumed by
/// `voxygen/src/session/target.rs`, `voxygen/src/scene/figure/mod.rs`, and
/// `voxygen/src/hud/mod.rs`), `Health`/`Stats`/`Energy`/`Poise`/`SkillSet`/
/// `Buffs`/`Inventory`/`Immovable`/`ProjectileHitEntities`, and the
/// belt-and-braces `Object::DeleteAfter`. Callers add whatever is specific to
/// their own anchor kind on top (`resolve_piloted` additionally inserts
/// `Controller` and the `Piloting` link).
fn spawn_sensor_base(
    data: &mut ResolveRemoteSenseEventData,
    body: Body,
    pos: Vec3<f32>,
    name_key: &str,
) -> (Entity, Uid) {
    let sensor = data.entities.create();
    let sensor_uid = data.id_maps.allocate(sensor);

    let _ = data.uids.insert(sensor, sensor_uid);
    let _ = data.positions.insert(sensor, Pos(pos));
    let _ = data.velocities.insert(sensor, Vel(Vec3::zero()));
    let _ = data.orientations.insert(sensor, Ori::default());
    let _ = data.masses.insert(sensor, body.mass());
    let _ = data.densities.insert(sensor, body.density());
    let _ = data.colliders.insert(sensor, Collider::Point);
    let _ = data.bodies.insert(sensor, body);
    // Never hostile, never AI-targeted -- and invisible/untargetable to any
    // observer without True Sight regardless, via `ConcealedUnlessTrueSight`
    // below.
    let _ = data.alignments.insert(sensor, Alignment::Passive);
    let _ = data.concealed.insert(sensor, ConcealedUnlessTrueSight);
    // A one-shot destructible prop, not a creature: 30 HP, no regen buff ever
    // applied. Reaching 0 runs the entity through the standard destroy/delete
    // pipeline, which then invalidates this link on the very next tick of
    // `server/src/sys/remote_sense.rs` (the anchor `Uid` no longer resolves)
    // -- the same duration-expiry/concentration-break teardown, not a second
    // "spell ends" path.
    let _ = data.healths.insert(sensor, Health::new(body));
    let _ = data.stats.insert(
        sensor,
        Stats::new(Content::Key(String::from(name_key)), body),
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

    (sensor, sensor_uid)
}

/// The `SenseAnchorKind::Sensor` predicate and spawn: the cast's target point
/// must be within the caster's own sense range *and* have an unobstructed
/// line of sight from the caster's position. `common/src/states/blink.rs`'s
/// `update.pos.0 = pos` (no range check at all) is the anti-pattern this
/// guards against -- a client-supplied position is never trusted without
/// both checks. On any failure this simply does not spawn a sensor or write
/// `RemoteSense`, the same "no remote view granted" shape as
/// `resolve_existing`.
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

    let body = Body::Object(ObjectBody::RemoteSensor);
    let (_sensor, sensor_uid) = spawn_sensor_base(data, body, target_pos, "name-remote-sensor");

    let _ = data.remote_senses.insert(caster, RemoteSense {
        anchor: SenseAnchor::Sensor(sensor_uid),
        free_look: ev.free_look,
        piloted: ev.piloted,
        caster: caster_uid,
    });
}

/// The `SenseAnchorKind::Piloted` spawn: `arcane_eye`. Unlike `resolve_sensor`
/// this has no client-supplied point at all -- the caster doesn't aim the
/// eye's spawn point, only where it flies afterward. The spawn point is
/// computed here, a fixed `EYE_SPAWN_RANGE` in front of the caster's own
/// facing, and still server-validated for line of sight so the eye never
/// spawns inside a wall (the same fail-closed shape as `resolve_sensor`: any
/// failure simply grants no link).
fn resolve_piloted(
    data: &mut ResolveRemoteSenseEventData,
    ev: &ResolveRemoteSenseEvent,
    caster: Entity,
    caster_uid: Uid,
    caster_pos: Pos,
) {
    let caster_look = data
        .orientations
        .get(caster)
        .map_or(Vec3::unit_y(), |ori| ori.look_dir().to_vec());
    let candidate_pos = caster_pos.0 + caster_look * EYE_SPAWN_RANGE;

    if !positions_have_line_of_sight(&data.terrain, caster_pos.0, candidate_pos) {
        return;
    }

    let body = Body::Object(ObjectBody::ArcaneEye);
    let (sensor, sensor_uid) = spawn_sensor_base(data, body, candidate_pos, "name-arcane-eye");
    // Drivable, unlike the static `Sensor` anchor: `common/systems/src/pilot.rs`
    // reads/writes this every tick once the `Piloting` link below exists.
    let _ = data.controllers.insert(sensor, Controller::default());

    // Wire the `Piloting` link by hand: this handler only ever gets
    // `specs::SystemData`, never `&mut State`, so `StateExt::link()` isn't
    // reachable here -- this mirrors
    // `common/src/states/telekinetic_grip.rs`'s identical bypass for
    // `Tethered`. Only ever created here, server-side.
    let handle = LinkHandle::from_link(Piloting {
        pilot: caster_uid,
        piloted: sensor_uid,
    });
    let _ = data.is_pilots.insert(caster, handle.make_role::<Pilot>());
    let _ = data
        .is_piloteds
        .insert(sensor, handle.make_role::<Piloted>());

    let _ = data.remote_senses.insert(caster, RemoteSense {
        anchor: SenseAnchor::Piloted(sensor_uid),
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
        world.register::<ConcealedUnlessTrueSight>();
        world.register::<Controller>();
        world.register::<Is<Pilot>>();
        world.register::<Is<Piloted>>();
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
        assert!(
            world
                .read_storage::<ConcealedUnlessTrueSight>()
                .get(sensor)
                .is_some(),
            "the sensor must be marked ConcealedUnlessTrueSight, or it renders for everyone"
        );
    }

    fn piloted_event(caster: Entity) -> ResolveRemoteSenseEvent {
        ResolveRemoteSenseEvent {
            entity: caster,
            target_entity: None,
            target_pos: None,
            anchor_kind: SenseAnchorKind::Piloted,
            free_look: false,
            piloted: true,
        }
    }

    #[test]
    fn piloted_blocked_line_of_sight_grants_no_link_and_spawns_nothing() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let caster = world
            .create_entity()
            .with(Pos(Vec3::new(4.0, 4.0, 5.0)))
            .with(Ori::from_unnormalized_vec(Vec3::new(1.0, 0.0, 0.0)).unwrap())
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);

        // A solid block directly between the caster and where the eye would
        // spawn (9 blocks along its own look direction, +x).
        world
            .write_resource::<TerrainGrid>()
            .set(
                Vec3::new(9, 4, 5),
                Block::new(BlockKind::Rock, Rgb::new(128, 128, 128)),
            )
            .unwrap();

        let entities_before = world.entities().join().count();

        dispatch(&world, piloted_event(caster));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "a blocked spawn point must not grant a link"
        );
        assert!(
            world.read_storage::<Is<Pilot>>().get(caster).is_none(),
            "a blocked spawn point must not wire the Piloting link either"
        );
        assert_eq!(
            world.entities().join().count(),
            entities_before,
            "a blocked spawn point must not spawn an eye entity"
        );
    }

    #[test]
    fn piloted_clear_los_spawns_a_drivable_eye_and_wires_the_piloting_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let caster_pos = Pos(Vec3::new(4.0, 4.0, 5.0));
        let caster = world
            .create_entity()
            .with(caster_pos)
            .with(Ori::from_unnormalized_vec(Vec3::new(1.0, 0.0, 0.0)).unwrap())
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);

        dispatch(&world, piloted_event(caster));

        let remote_sense = world
            .read_storage::<RemoteSense>()
            .get(caster)
            .copied()
            .expect("an unobstructed spawn point must grant a link");
        let SenseAnchor::Piloted(eye_uid) = remote_sense.anchor else {
            panic!("expected a Piloted anchor, got {:?}", remote_sense.anchor);
        };
        assert!(!remote_sense.free_look, "arcane_eye is not free-look");

        let eye = world
            .read_resource::<IdMaps>()
            .uid_entity(eye_uid)
            .expect("the eye's Uid must resolve to a live entity");

        assert_eq!(
            world.read_storage::<Pos>().get(eye).map(|p| p.0),
            Some(caster_pos.0 + Vec3::new(EYE_SPAWN_RANGE, 0.0, 0.0)),
            "the eye spawns EYE_SPAWN_RANGE along the caster's own look direction"
        );
        assert!(
            matches!(
                world.read_storage::<Collider>().get(eye),
                Some(Collider::Point)
            ),
            "never an absent Collider"
        );
        assert_eq!(
            world.read_storage::<Body>().get(eye),
            Some(&Body::Object(ObjectBody::ArcaneEye)),
            "the eye's own dedicated object body variant"
        );
        assert!(
            world
                .read_storage::<ConcealedUnlessTrueSight>()
                .get(eye)
                .is_some(),
            "the eye must be True-Sight-gated, same as the static sensor"
        );
        assert!(
            world.read_storage::<Controller>().get(eye).is_some(),
            "the eye must carry a Controller -- it is drivable, unlike the static sensor"
        );
        assert!(
            world.read_storage::<Is<Pilot>>().get(caster).is_some(),
            "the caster must be wired as the Pilot half of the Piloting link"
        );
        assert!(
            world.read_storage::<Is<Piloted>>().get(eye).is_some(),
            "the eye must be wired as the Piloted half of the Piloting link"
        );
    }
}
