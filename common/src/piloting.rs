//! The `Piloting` link: `arcane_eye`'s caster (`Pilot`) driving a spawned,
//! flying sensor entity (`Piloted`) — `common/src/mounting.rs`'s `Mounting`
//! and `common/src/tether.rs`'s `Tethered` are the shipped precedents this
//! mirrors (same `Link`/`Role`/`LinkHandle` shape, `common/src/link.rs`).
//!
//! 🔴 **Created only server-side.** Unlike `Tethered` (linkable via
//! `server/src/cmd.rs` admin commands) or `Mounting` (linkable via the
//! client's own `MountEvent`), there is no path that creates a `Piloting`
//! link except `server/src/events/remote_sense.rs`'s `resolve_piloted` —
//! itself only reachable from the server-only `ResolveRemoteSenseEvent`
//! handler. The client has no message that names a foreign entity to drive
//! (see `server/src/sys/msg/in_game.rs`'s own invariant), so it structurally
//! cannot mint one of these for itself.
//!
//! `resolve_piloted` inserts the `Is<Pilot>`/`Is<Piloted>` roles directly via
//! the `WriteStorage`s it already borrows rather than calling
//! `StateExt::link()` (unavailable there — that handler only ever gets
//! `specs::SystemData`, never `&mut State`), the same bypass
//! `common/src/states/telekinetic_grip.rs` already uses for `Tethered`. The
//! `Link::create`/`persist`/`delete` methods below still exist for API-shape
//! parity with `Mounting`/`Tethered` (and are exercised directly by this
//! module's own tests) even though the live spawn path doesn't call
//! `create` — `server/src/sys/remote_sense.rs`'s per-tick teardown removes
//! `Is<Pilot>` by hand when a `Piloted` anchor's link ends, mirroring
//! `telekinetic_grip.rs`'s own explicit `Is<Leader>`/`Is<Follower>` removal.

use crate::{
    comp,
    link::{Is, Link, LinkHandle, Role},
    uid::{IdMaps, Uid},
};
use serde::{Deserialize, Serialize};
use specs::{Entities, Read, ReadStorage, WriteStorage};

#[derive(Serialize, Deserialize, Debug)]
pub struct Pilot;

impl Role for Pilot {
    type Link = Piloting;
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Piloted;

impl Role for Piloted {
    type Link = Piloting;
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Piloting {
    pub pilot: Uid,
    pub piloted: Uid,
}

#[derive(Debug)]
pub enum PilotingError {
    NoSuchEntity,
    NotPilotable,
}

impl Link for Piloting {
    type CreateData<'a> = (
        Read<'a, IdMaps>,
        WriteStorage<'a, Is<Pilot>>,
        WriteStorage<'a, Is<Piloted>>,
    );
    type DeleteData<'a> = (
        Read<'a, IdMaps>,
        WriteStorage<'a, Is<Pilot>>,
        WriteStorage<'a, Is<Piloted>>,
    );
    type Error = PilotingError;
    type PersistData<'a> = (
        Read<'a, IdMaps>,
        Entities<'a>,
        ReadStorage<'a, comp::Health>,
        ReadStorage<'a, Is<Pilot>>,
        ReadStorage<'a, Is<Piloted>>,
    );

    fn create(
        this: &LinkHandle<Self>,
        (id_maps, is_pilots, is_piloteds): &mut Self::CreateData<'_>,
    ) -> Result<(), Self::Error> {
        let entity = |uid: Uid| id_maps.uid_entity(uid);

        if this.pilot == this.piloted {
            // Forbid self-piloting
            Err(PilotingError::NotPilotable)
        } else if let Some((pilot, piloted)) = entity(this.pilot).zip(entity(this.piloted)) {
            if !is_pilots.contains(pilot) && !is_piloteds.contains(piloted) {
                let _ = is_pilots.insert(pilot, this.make_role());
                let _ = is_piloteds.insert(piloted, this.make_role());
                Ok(())
            } else {
                Err(PilotingError::NotPilotable)
            }
        } else {
            Err(PilotingError::NoSuchEntity)
        }
    }

    fn persist(
        this: &LinkHandle<Self>,
        (id_maps, entities, healths, is_pilots, is_piloteds): &mut Self::PersistData<'_>,
    ) -> bool {
        let entity = |uid: Uid| id_maps.uid_entity(uid);

        if let Some((pilot, piloted)) = entity(this.pilot).zip(entity(this.piloted)) {
            let is_alive = |entity| {
                entities.is_alive(entity) && healths.get(entity).is_none_or(|h| !h.is_dead)
            };

            is_alive(pilot)
                && entities.is_alive(piloted)
                && is_pilots.get(pilot).is_some()
                && is_piloteds.get(piloted).is_some()
        } else {
            false
        }
    }

    fn delete(
        this: &LinkHandle<Self>,
        (id_maps, is_pilots, is_piloteds): &mut Self::DeleteData<'_>,
    ) {
        let entity = |uid: Uid| id_maps.uid_entity(uid);

        let pilot = entity(this.pilot);
        let piloted = entity(this.piloted);

        pilot.map(|pilot| is_pilots.remove(pilot));
        piloted.map(|piloted| is_piloteds.remove(piloted));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specs::{Builder, SystemData, WorldExt};
    use std::num::NonZeroU64;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Is<Pilot>>();
        world.register::<Is<Piloted>>();
        world.register::<comp::Health>();
        world.insert(IdMaps::default());
        world
    }

    #[test]
    fn create_wires_both_roles() {
        let mut world = setup_world();
        let pilot_uid = uid(1);
        let piloted_uid = uid(2);
        let pilot = world.create_entity().build();
        let piloted = world.create_entity().build();
        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(pilot_uid, pilot);
            id_maps.add_entity(piloted_uid, piloted);
        }

        let handle = LinkHandle::from_link(Piloting {
            pilot: pilot_uid,
            piloted: piloted_uid,
        });
        let mut data = <Piloting as Link>::CreateData::fetch(&world);
        let result = Piloting::create(&handle, &mut data);
        drop(data);
        world.maintain();

        assert!(result.is_ok());
        assert!(world.read_storage::<Is<Pilot>>().get(pilot).is_some());
        assert!(world.read_storage::<Is<Piloted>>().get(piloted).is_some());
    }

    #[test]
    fn self_piloting_is_rejected() {
        let mut world = setup_world();
        let same_uid = uid(1);
        let entity = world.create_entity().build();
        world
            .write_resource::<IdMaps>()
            .add_entity(same_uid, entity);

        let handle = LinkHandle::from_link(Piloting {
            pilot: same_uid,
            piloted: same_uid,
        });
        let mut data = <Piloting as Link>::CreateData::fetch(&world);
        let result = Piloting::create(&handle, &mut data);

        assert!(matches!(result, Err(PilotingError::NotPilotable)));
    }

    #[test]
    fn persist_fails_once_the_eye_is_gone() {
        let mut world = setup_world();
        let pilot_uid = uid(1);
        let piloted_uid = uid(2);
        let pilot = world.create_entity().build();
        let piloted = world.create_entity().build();
        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(pilot_uid, pilot);
            id_maps.add_entity(piloted_uid, piloted);
        }

        let handle = LinkHandle::from_link(Piloting {
            pilot: pilot_uid,
            piloted: piloted_uid,
        });
        {
            let mut data = <Piloting as Link>::CreateData::fetch(&world);
            Piloting::create(&handle, &mut data).unwrap();
        }
        world.maintain();

        {
            let mut persist_data = <Piloting as Link>::PersistData::fetch(&world);
            assert!(Piloting::persist(&handle, &mut persist_data));
        }

        // The eye is destroyed (e.g. its Health hit 0 and the destroy
        // pipeline reaped it) -- the link must stop persisting.
        world.delete_entity(piloted).unwrap();
        world.maintain();

        let mut persist_data = <Piloting as Link>::PersistData::fetch(&world);
        assert!(!Piloting::persist(&handle, &mut persist_data));
    }
}
