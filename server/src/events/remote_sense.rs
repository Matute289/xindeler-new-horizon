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
        Alignment, Body, Buffs, Health, Pos, Presence, RemoteSense, buff::SenseAnchorKind,
        pet::is_tameable, remote_sense::SenseAnchor,
    },
    event::ResolveRemoteSenseEvent,
    uid::{IdMaps, Uid},
};
use specs::{Entities, Entity, Read, ReadStorage, SystemData, WriteStorage, shred};

use crate::sys::remote_sense::max_sense_range;

use super::ServerEvent;

#[derive(SystemData)]
pub struct ResolveRemoteSenseEventData<'a> {
    entities: Entities<'a>,
    id_maps: Read<'a, IdMaps>,
    uids: ReadStorage<'a, Uid>,
    positions: ReadStorage<'a, Pos>,
    bodies: ReadStorage<'a, Body>,
    alignments: ReadStorage<'a, Alignment>,
    buffs: ReadStorage<'a, Buffs>,
    healths: ReadStorage<'a, Health>,
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
                // A sensor/eye anchor spawns and owns a brand-new server
                // entity rather than resolving one that already exists, so it
                // needs its own spawn-and-validate path -- left unimplemented
                // here rather than guessed at ahead of the spell that first
                // needs it. Matched explicitly (not `_`) so adding a future
                // `SenseAnchorKind` variant fails to compile here instead of
                // silently falling through.
                SenseAnchorKind::Sensor | SenseAnchorKind::Piloted => {},
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::comp::{
        Buffs,
        buff::{Buff, BuffData, BuffKind, BuffSource},
        quadruped_medium,
    };
    use specs::{Builder, WorldExt};
    use std::num::NonZeroU64;
    use vek::Vec3;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Body>();
        world.register::<Alignment>();
        world.register::<Buffs>();
        world.register::<Health>();
        world.register::<Presence>();
        world.register::<Uid>();
        world.register::<RemoteSense>();
        world.insert(IdMaps::default());
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
}
