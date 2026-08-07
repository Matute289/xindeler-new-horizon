//! Server-side resolution of [`ResolveIdentifyEvent`]: the cast-time step
//! that turns a validated Identify cast target into a written
//! [`IdentifyLinks`] entry.
//!
//! Everything here is authoritative — the event carries only the client's
//! raw, unchecked cast-time intent (which entity it targeted), and every
//! predicate that decides whether that intent is honoured runs in this
//! handler, never on the client. `server/src/sys/detection.rs`'s own
//! per-tick system then reads whatever this handler wrote, every tick, for
//! as long as the link lasts — this handler only ever runs once, at cast
//! time, exactly mirroring `server/src/events/remote_sense.rs`'s own split
//! between cast-time resolution and per-tick re-validation.

use common::{
    comp::{Body, PickupItem, Pos, Stats},
    event::ResolveIdentifyEvent,
    resources::Time,
    uid::{IdMaps, Uid},
};
use specs::{Entities, Read, ReadStorage, SystemData, Write, shred};

use crate::sys::detection::IdentifyLinks;

use super::ServerEvent;

/// How close the caster must be to the target, in blocks, for an Identify
/// cast to resolve. A short-range utility spell -- unlike `scrying`'s
/// long-distance sensor, Identify has no ongoing tether to maintain, so a
/// single fixed range (rather than a presence-scaled one like
/// `remote_sense::max_sense_range`) is enough.
const IDENTIFY_RANGE: f32 = 20.0;

#[derive(SystemData)]
pub struct ResolveIdentifyEventData<'a> {
    entities: Entities<'a>,
    id_maps: Read<'a, IdMaps>,
    time: Read<'a, Time>,
    uids: ReadStorage<'a, Uid>,
    positions: ReadStorage<'a, Pos>,
    bodies: ReadStorage<'a, Body>,
    stats: ReadStorage<'a, Stats>,
    pickup_items: ReadStorage<'a, PickupItem>,
    identify_links: Write<'a, IdentifyLinks>,
}

impl ServerEvent for ResolveIdentifyEvent {
    type SystemData<'a> = ResolveIdentifyEventData<'a>;

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
            let Some(target_uid) = ev.target_entity else {
                continue;
            };
            let Some(target_entity) = data.id_maps.uid_entity(target_uid) else {
                continue;
            };
            let Some(target_pos) = data.positions.get(target_entity).copied() else {
                continue;
            };

            let is_in_range =
                target_pos.0.distance_squared(caster_pos.0) <= IDENTIFY_RANGE * IDENTIFY_RANGE;
            if !is_in_range {
                continue;
            }

            // `nondetection` is the same "can't be targeted by divination"
            // claim `server/src/sys/detection.rs`'s `reveals` predicate
            // checks first, before every other sense -- Identify is a
            // single-target divination effect, so it must honour the same
            // guard rather than quietly bypassing it.
            if data
                .stats
                .get(target_entity)
                .is_some_and(|s| s.nondetection)
            {
                continue;
            }

            // A dropped item resolves to `DetectDetail::Item` (the
            // `ItemTooltip` half); anything else with a `Body` resolves to
            // `DetectDetail::Creature` (the new inspect card). Neither ->
            // not a valid Identify target, so the cast simply does nothing,
            // the same "no anchor ever gets written" non-error every other
            // resolve-on-cast handler uses for an invalid target.
            let is_item = data.pickup_items.contains(target_entity);
            let is_creature = !is_item && data.bodies.contains(target_entity);
            if !is_item && !is_creature {
                continue;
            }

            data.identify_links
                .record_cast(caster_uid, target_uid, is_item, data.time.0);
        }
    }
}
