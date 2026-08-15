//! Server-only authority system keeping a Warlock's talisman bond and
//! everything it projects in agreement.
//!
//! A bond is two pieces of state on two different entities: the Warlock's
//! `Pact::talisman_bearer`, and the bearer's `PactTalisman` ward plus the
//! recall key in their `AbilityPool`. Anything that ends one side must end the
//! other, and there are more ways to end one than there are places to hook:
//! the Warlock dies, the Warlock logs out (taking their `Pact` out of the
//! world with them), the pact is severed, the boon is changed, the bearer logs
//! out, the bearer is removed.
//!
//! Rather than a hook per route -- which is how one gets missed -- this
//! re-derives validity each tick from
//! [`common::comp::pact::bond_is_intact`], the single pure rule, and repairs
//! whichever side disagrees. Every exit route is therefore covered by
//! construction, including ones nobody enumerated.
//!
//! Registered ONLY in `server/src/sys/mod.rs`, never in
//! `common/systems/src/lib.rs::add_local_systems`: the client must not be able
//! to grant itself a ward or a teleport.

use common::{
    comp::{
        ActiveAbilities, Buffs, Health, Player, TriggerSlots,
        ability::AbilityPool,
        buff::{BuffChange, BuffKind, BuffSource},
        pact::{Pact, bond_is_intact},
    },
    event::{BuffEvent, EmitExt},
    event_emitters,
    uid::{IdMaps, Uid},
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Entity, Join, Read, ReadStorage, WriteStorage};

use crate::pact::set_talisman_pool_key;

event_emitters! {
    struct Events[Emitters] {
        buff: BuffEvent,
    }
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        Read<'a, IdMaps>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Player>,
        ReadStorage<'a, Buffs>,
        ReadStorage<'a, Health>,
        WriteStorage<'a, Pact>,
        WriteStorage<'a, AbilityPool>,
        WriteStorage<'a, ActiveAbilities>,
        WriteStorage<'a, TriggerSlots>,
    );

    const NAME: &'static str = "pact_talisman";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            entities,
            events,
            id_maps,
            uids,
            players,
            buffs,
            healths,
            mut pacts,
            mut pools,
            mut actives,
            mut triggers,
        ): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();

        // Pass 1 -- a ward with nothing sustaining it.
        //
        // Restricted to players: a bearer is always a player in this version,
        // so this walks a handful of entities and does one O(1) `EnumMap`
        // lookup on each, rather than scanning everything that can hold a
        // buff.
        let mut orphaned: Vec<Entity> = Vec::new();
        for (entity, _, entity_buffs, uid) in (&entities, &players, &buffs, &uids).join() {
            let Some(source) =
                entity_buffs
                    .iter_kind(BuffKind::PactTalisman)
                    .find_map(|(_, buff)| match buff.source {
                        BuffSource::Character { by, .. } => Some(by),
                        _ => None,
                    })
            else {
                continue;
            };
            // A Warlock who logged out has no entity and therefore no `Pact`,
            // which `bond_is_intact` reads as "not intact" -- the logout route
            // needs no separate handling.
            let warlock = id_maps.uid_entity(source);
            let warlock_alive = warlock.is_some_and(|w| healths.get(w).is_none_or(|h| !h.is_dead));
            if !bond_is_intact(warlock.and_then(|w| pacts.get(w)), warlock_alive, *uid) {
                orphaned.push(entity);
            }
        }
        for entity in orphaned {
            emitters.emit(BuffEvent {
                entity,
                buff_change: BuffChange::RemoveByKind(BuffKind::PactTalisman),
            });
            set_talisman_pool_key(&mut pools, &mut actives, &mut triggers, entity, false);
        }

        // Pass 2 -- a record naming a bearer who is gone (logged out, removed,
        // or already stripped by pass 1). Left alone, the Warlock could never
        // bond anyone again without first releasing a bond to a bearer who no
        // longer exists.
        let mut dangling: Vec<Entity> = Vec::new();
        for (entity, pact) in (&entities, &pacts).join() {
            let Some(bearer_uid) = pact.talisman_bearer else {
                continue;
            };
            let bearer_still_warded = id_maps
                .uid_entity(bearer_uid)
                .and_then(|bearer| buffs.get(bearer))
                .is_some_and(|b| b.contains(BuffKind::PactTalisman));
            if !bearer_still_warded {
                dangling.push(entity);
            }
        }
        for entity in dangling {
            if let Some(mut pact) = pacts.get_mut(entity) {
                pact.talisman_bearer = None;
            }
        }
    }
}
