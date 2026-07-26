//! Server-only authority system for active remote-sensing links.
//!
//! Each tick, for every entity whose `RemoteSense` component names an active
//! link, this system re-resolves the anchor via `IdMaps`, re-validates it
//! (alive, still holding its sustaining buff, within the caster's own view
//! distance), and writes or clears the shipped `SpectatingEntity` component
//! accordingly — which is what actually drives the existing spectate-sync and
//! camera path. On failure it also cancels the sustaining buff and, for a
//! spawned sensor/eye, deletes the anchor entity as a belt-and-braces reaper.
//!
//! This is deliberately registered ONLY in `server/src/sys/mod.rs`, never in
//! `common/systems/src/lib.rs::add_local_systems`: the client must not be able
//! to grant itself a viewpoint, and it structurally cannot, because the code
//! that grants one does not live in a crate the client dispatches.

use common::{
    comp::{
        Buffs, Health, Pos, Presence, RemoteSense, SpectatingEntity,
        buff::{BuffChange, BuffKind},
    },
    event::{BuffEvent, DeleteEvent, EmitExt},
    event_emitters,
    terrain::TerrainChunkSize,
    uid::IdMaps,
    vol::RectVolSize,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Entity, Join, Read, ReadStorage, WriteStorage};

event_emitters! {
    struct Events[Emitters] {
        buff: BuffEvent,
        delete: DeleteEvent,
    }
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        Read<'a, IdMaps>,
        ReadStorage<'a, Pos>,
        ReadStorage<'a, Buffs>,
        ReadStorage<'a, Health>,
        ReadStorage<'a, Presence>,
        WriteStorage<'a, RemoteSense>,
        WriteStorage<'a, SpectatingEntity>,
    );

    const NAME: &'static str = "remote_sense";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            entities,
            events,
            id_maps,
            positions,
            buffs,
            healths,
            presences,
            mut remote_senses,
            mut spectating_entities,
        ): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();

        // Two passes: the first only reads `remote_senses` to decide what's
        // still valid (join requires it be borrowed immutably), the second
        // applies the (rare) removals. Never more than a handful of active
        // links, so the extra allocation is negligible.
        let mut to_end: Vec<Entity> = Vec::new();

        for (caster, remote_sense, caster_pos) in (&entities, &remote_senses, &positions).join() {
            let anchor_entity = id_maps.uid_entity(remote_sense.anchor_uid());

            let is_valid = !healths.get(caster).is_some_and(|h| h.is_dead)
                && buffs
                    .get(caster)
                    .is_some_and(|b| b.contains(BuffKind::RemoteSensing))
                && anchor_entity.is_some_and(|anchor_entity| {
                    positions.get(anchor_entity).is_some_and(|anchor_pos| {
                        let max_range = presences.get(caster).map_or(0.0, |presence| {
                            presence.entity_view_distance.current() as f32
                                * TerrainChunkSize::RECT_SIZE.reduce_max() as f32
                        });
                        anchor_pos.0.distance_squared(caster_pos.0) <= max_range * max_range
                    })
                });

            if is_valid {
                if let Some(anchor_entity) = anchor_entity
                    && spectating_entities.get(caster).map(|s| s.0) != Some(anchor_entity)
                {
                    let _ = spectating_entities.insert(caster, SpectatingEntity(anchor_entity));
                }
            } else {
                to_end.push(caster);
            }
        }

        for caster in to_end {
            spectating_entities.remove(caster);

            if let Some(remote_sense) = remote_senses.remove(caster)
                && remote_sense.anchor.is_spawned_sensor()
                && let Some(anchor_entity) = id_maps.uid_entity(remote_sense.anchor_uid())
            {
                emitters.emit(DeleteEvent(anchor_entity));
            }

            emitters.emit(BuffEvent {
                entity: caster,
                buff_change: BuffChange::RemoveByKind(BuffKind::RemoteSensing),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{
            Body,
            buff::{Buff, BuffData, BuffSource, DestInfo},
            humanoid,
            remote_sense::SenseAnchor,
        },
        resources::Time,
        uid::Uid,
    };
    use specs::{Builder, WorldExt};
    use std::num::NonZeroU64;
    use vek::Vec3;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn remote_sensing_buff() -> Buff {
        Buff::new(
            BuffKind::RemoteSensing,
            BuffData::new(0.0, None),
            Vec::new(),
            BuffSource::World,
            Time(0.0),
            DestInfo::default(),
            None,
            None,
        )
    }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Buffs>();
        world.register::<Health>();
        world.register::<Presence>();
        world.register::<RemoteSense>();
        world.register::<SpectatingEntity>();
        world.insert(IdMaps::default());
        // `common_ecs::run_now`'s `Job<T>` wrapper records CPU-time metrics
        // per system and expects this resource to already exist.
        world.insert(common_ecs::SysMetrics::default());
        world
    }

    /// The row's core security/liveness property: a link stays honoured while
    /// its sustaining buff is active, and revoking that buff (whether by
    /// duration expiry, a concentration-breaking hit, or the player's own
    /// voluntary cancel) clears `SpectatingEntity` (and `RemoteSense` itself)
    /// on the very next run of this system — never lingering.
    #[test]
    fn revoking_the_sustaining_buff_clears_spectating_entity_next_tick() {
        let mut world = setup_world();

        let caster_uid = uid(1);
        let anchor_uid = uid(2);

        // Anchor and caster share a position: this test is about the buff
        // lifecycle, not the view-distance range clamp, so keep distance out
        // of the way by making it trivially zero.
        let anchor = world.create_entity().with(Pos(Vec3::zero())).build();
        let caster = world.create_entity().with(Pos(Vec3::zero())).build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(anchor_uid, anchor);
        }

        let mut buffs = Buffs::default();
        buffs.insert(remote_sensing_buff(), Time(0.0));
        world
            .write_storage::<Buffs>()
            .insert(caster, buffs)
            .unwrap();
        world
            .write_storage::<RemoteSense>()
            .insert(caster, RemoteSense {
                anchor: SenseAnchor::Existing(anchor_uid),
                free_look: false,
                piloted: false,
                caster: caster_uid,
            })
            .unwrap();

        common_ecs::run_now::<Sys>(&world);
        assert_eq!(
            world
                .read_storage::<SpectatingEntity>()
                .get(caster)
                .map(|s| s.0),
            Some(anchor),
            "a live link with its buff still present must drive SpectatingEntity"
        );
        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_some(),
            "the link itself must still be intact"
        );

        // Revoke the sustaining buff -- this is what a concentration-breaking
        // hit, a duration expiry, or the player's cancel key all do.
        world
            .write_storage::<Buffs>()
            .get_mut(caster)
            .unwrap()
            .remove_kind(BuffKind::RemoteSensing);

        common_ecs::run_now::<Sys>(&world);
        assert!(
            world
                .read_storage::<SpectatingEntity>()
                .get(caster)
                .is_none(),
            "revoking the buff must clear SpectatingEntity on the next tick"
        );
        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "revoking the buff must also clear the RemoteSense link itself"
        );
    }

    #[test]
    fn a_dead_caster_ends_the_link_even_with_the_buff_still_present() {
        let mut world = setup_world();

        let caster_uid = uid(1);
        let anchor_uid = uid(2);
        let anchor = world.create_entity().with(Pos(Vec3::zero())).build();
        let body = Body::Humanoid(humanoid::Body::random());
        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Health::new(body))
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(anchor_uid, anchor);
        }

        let mut buffs = Buffs::default();
        buffs.insert(remote_sensing_buff(), Time(0.0));
        world
            .write_storage::<Buffs>()
            .insert(caster, buffs)
            .unwrap();
        world
            .write_storage::<RemoteSense>()
            .insert(caster, RemoteSense {
                anchor: SenseAnchor::Existing(anchor_uid),
                free_look: false,
                piloted: false,
                caster: caster_uid,
            })
            .unwrap();
        world
            .write_storage::<Health>()
            .get_mut(caster)
            .unwrap()
            .is_dead = true;

        common_ecs::run_now::<Sys>(&world);
        assert!(
            world
                .read_storage::<SpectatingEntity>()
                .get(caster)
                .is_none()
        );
        assert!(world.read_storage::<RemoteSense>().get(caster).is_none());
    }
}
