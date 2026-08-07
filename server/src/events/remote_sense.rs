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
    assets::{AssetExt, Ron},
    combat::CombatTuning,
    comp::{
        Alignment, Body, Buffs, Collider, ConcealedUnlessTrueSight, Controller, Density,
        DerivedStats, Energy, Group, Health, Immovable, Inventory, Mass, Object, Ori, Poise, Pos,
        Presence, RemoteSense, SkillSet, SpectatingEntity, Stats, Vel, buff::SenseAnchorKind,
        object::Body as ObjectBody, pet::is_tameable, projectile::ProjectileHitEntities,
        remote_sense::SenseAnchor,
    },
    event::{DeleteEvent, EventBus, ResolveRemoteSenseEvent},
    link::{Is, LinkHandle},
    outcome::Outcome,
    piloting::{Pilot, Piloted, Piloting},
    resources::Time,
    terrain::{TerrainGrid, positions_have_line_of_sight},
    uid::{IdMaps, Uid},
};
use common_i18n::Content;
use rand::RngExt;
use specs::{
    Entities, Entity, Read, ReadExpect, ReadStorage, SystemData, Write, WriteStorage, shred,
};
use std::time::Duration;
use vek::Vec3;

use crate::sys::remote_sense::{
    max_sense_range, scrying_focus_item_def_id, scrying_follow_offset, scrying_resist_roll_chance,
};

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
    /// `scrying`'s resist roll needs the scried target's `combat_rating`
    /// (`server/src/events/banishment.rs` reads the same cache for its own
    /// saving throw, same rationale).
    derived_stats: ReadStorage<'a, DerivedStats>,
    /// `scrying`'s party-bonus check ("is the scried target in my own
    /// group").
    groups: ReadStorage<'a, Group>,
    remote_senses: WriteStorage<'a, RemoteSense>,
    concealed: WriteStorage<'a, ConcealedUnlessTrueSight>,
    controllers: WriteStorage<'a, Controller>,
    is_pilots: WriteStorage<'a, Is<Pilot>>,
    is_piloteds: WriteStorage<'a, Is<Piloted>>,
    spectating_entities: WriteStorage<'a, SpectatingEntity>,
    delete_events: Read<'a, EventBus<DeleteEvent>>,
    /// Drives the same floating "Resisted" indicator every other resisted
    /// magical effect uses when `scrying`'s cast-time roll fails.
    outcomes: Read<'a, EventBus<Outcome>>,
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
                SenseAnchorKind::Tracking => {
                    resolve_tracking(&mut data, &ev, ev.entity, caster_uid, caster_pos);
                },
            }
        }
    }
}

/// If `caster` already holds an active `RemoteSense` link, synchronously
/// tears it down so a new one can be wired over it without orphaning the
/// old one. Recasting `arcane_eye` (or any of the other remote-sensing
/// spells) while already piloting/sensing through a prior
/// anchor must not leave that anchor's spawned entity -- and the caster's
/// `Is<Pilot>` link, if it was `Piloted` -- lingering, driven by nothing
/// that can ever revalidate it, until `SENSOR_MAX_LIFETIME` finally reaps it.
///
/// Mirrors `server/src/sys/remote_sense.rs`'s own per-tick `to_end` teardown
/// (remove the link, remove `Is<Pilot>` for a `Piloted` anchor, reap a
/// spawned sensor/eye) -- with one deliberate omission: the sustaining
/// `BuffKind::RemoteSensing` buff is *not* force-removed here.
///
/// The new cast that triggered this teardown already emitted its own
/// `BuffEvent::Add` for the very same buff kind, in the same tick, from
/// `common/src/states/self_buff.rs`. For a same-spell recast, `Buffs::insert`
/// dedupes on identical `data`/`cat_ids`/`source` and refreshes that buff in
/// place -- no leak. For the rarer cross-spell case (e.g. `beast_sense` then
/// `arcane_eye`), the two buffs' `data` differ, so `Buffs::insert` leaves a
/// second, harmless, inert entry that expires on its own key-scoped timer
/// (`common/systems/src/buff.rs`'s expiry pass removes only the specific
/// expired buff's key, never the whole kind) -- it grants no anchor of its
/// own (that lives on the single-valued `RemoteSense` component, which this
/// function does clear/rewrite) and contributes nothing beyond a redundant,
/// already-present `MovementSpeed(0.0)`.
///
/// Explicitly emitting `BuffChange::RemoveByKind(BuffKind::RemoteSensing)`
/// here instead would be actively wrong, not just redundant:
/// `event_dispatch::<ResolveRemoteSenseEvent>`'s registration in
/// `server/src/events/entity_manipulation.rs` explicitly depends on
/// `event_sys_name::<BuffEvent>()`, so `EventHandler<BuffEvent>` has
/// *already drained and applied* this tick's buff events (including the new
/// cast's own `Add`) by the time this handler runs -- a compiler-checked
/// constraint, not an insertion-order coincidence (see that dependency's own
/// comment for why it had to become an explicit edge). A `RemoveByKind`
/// emitted from here would sit in the queue and only apply *next* tick --
/// by which point it would delete the brand new buff, not the stale one,
/// silently killing the very link this teardown is meant to make room for.
///
/// Callers must invoke this only *after* the new anchor's own validation has
/// succeeded, and *before* writing any of the new anchor's own components or
/// links -- otherwise, in the double-`Piloted` case, this would remove the
/// caster's brand new `Is<Pilot>` moments after it was wired.
fn teardown_existing_anchor(data: &mut ResolveRemoteSenseEventData, caster: Entity) {
    let Some(existing) = data.remote_senses.remove(caster) else {
        return;
    };

    if matches!(existing.anchor, SenseAnchor::Piloted(_)) {
        data.is_pilots.remove(caster);
    }

    // `remote_sense::Sys` (the per-tick re-validator, `server/src/sys/
    // remote_sense.rs`) runs earlier in the same tick's `state.tick()` pass,
    // before this cast's own event is even processed here in
    // `handle_events()` -- so it will already have reasserted
    // `SpectatingEntity` at the *old* anchor once more this tick. Clearing
    // it here (rather than leaving it to self-heal on `remote_sense::Sys`'s
    // next run) avoids a one-tick window where a deleted entity's `Uid`
    // would otherwise sit in `SpectatingEntity`, and keeps this function's
    // teardown a true mirror of the per-tick `to_end` cleanup it's already
    // documented above as mirroring.
    data.spectating_entities.remove(caster);

    if existing.anchor.is_spawned_sensor()
        && let Some(old_anchor_entity) = data.id_maps.uid_entity(existing.anchor.uid())
    {
        data.delete_events.emit_now(DeleteEvent(old_anchor_entity));
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
        teardown_existing_anchor(data, caster);
        let _ = data.remote_senses.insert(caster, RemoteSense {
            anchor: SenseAnchor::Existing(target_uid),
            free_look: ev.free_look,
            piloted: ev.piloted,
            caster: caster_uid,
            // Meaningless for `Existing` (never driven), forwarded anyway
            // for a single uniform `RemoteSense` construction shape across
            // all three `resolve_*` functions.
            flight_speed: ev.flight_speed,
        });
    }
}

/// Builds the "invisible, 30 HP, destructible, `Alignment::Passive` prop"
/// shape shared by every remote-sensing sensor entity -- `resolve_sensor`'s
/// `Sensor` anchor, `resolve_piloted`'s `Piloted` anchor, and
/// `resolve_tracking`'s `Tracking` anchor alike: `Uid`, `Pos`, `Vel`, `Ori`,
/// `Mass`/`Density` (from `body`), `Collider::Point` (never an absent
/// `Collider` -- an absent one drops `PhysicsState` entirely, which silently
/// drops the entity from the figure renderer), `Body`, `Alignment::Passive`,
/// `ConcealedUnlessTrueSight` (the True-Sight-gated visibility/targetability
/// marker consumed by `voxygen/src/session/target.rs`,
/// `voxygen/src/scene/figure/mod.rs`, and `voxygen/src/hud/mod.rs`),
/// `Health`/`Stats`/`Energy`/`Poise`/`SkillSet`/`Buffs`/`Inventory`/
/// `Immovable`/`ProjectileHitEntities`, and the belt-and-braces
/// `Object::DeleteAfter`. Callers add whatever is specific to their own
/// anchor kind on top (`resolve_piloted` additionally inserts `Controller`
/// and the `Piloting` link; `resolve_tracking`'s own per-tick `Pos` upkeep
/// lives in `server/src/sys/remote_sense.rs`, not here).
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

    teardown_existing_anchor(data, caster);

    let body = Body::Object(ObjectBody::RemoteSensor);
    let (_sensor, sensor_uid) = spawn_sensor_base(data, body, target_pos, "name-remote-sensor");

    let _ = data.remote_senses.insert(caster, RemoteSense {
        anchor: SenseAnchor::Sensor(sensor_uid),
        free_look: ev.free_look,
        piloted: ev.piloted,
        caster: caster_uid,
        // Meaningless for `Sensor` (never driven), same uniform-shape
        // rationale as `resolve_existing`.
        flight_speed: ev.flight_speed,
    });
}

/// The `SenseAnchorKind::Piloted` spawn: `arcane_eye`. Unlike `resolve_sensor`
/// this has no client-supplied point at all -- the caster doesn't aim the
/// eye's spawn point, only where it flies afterward. The spawn point is
/// computed here, a fixed `ev.spawn_range` (RON-authored, see
/// `comp::buff::MiscBuffData::RemoteSense::spawn_range`) in front of the
/// caster's own facing, and still server-validated for line of sight so the
/// eye never spawns inside a wall (the same fail-closed shape as
/// `resolve_sensor`: any failure simply grants no link).
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
    let candidate_pos = caster_pos.0 + caster_look * ev.spawn_range;

    if !positions_have_line_of_sight(&data.terrain, caster_pos.0, candidate_pos) {
        return;
    }

    teardown_existing_anchor(data, caster);

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
        // The one anchor kind this actually matters for -- read every tick
        // by `common/systems/src/pilot.rs`.
        flight_speed: ev.flight_speed,
    });
}

/// The `SenseAnchorKind::Tracking` predicate, resist roll, and spawn:
/// `scrying`. The target must be alive and within the caster's own sense
/// range (the same range predicate `resolve_existing` uses, and no line of
/// sight requirement -- like `resolve_existing`, unlike `resolve_sensor`,
/// this targets a specific creature rather than an aimed point the caster
/// must be able to see). Unlike every other anchor kind here, forming the
/// link is also gated on a resisted-magic-effect roll
/// (`scrying_resist_roll_chance`, the same formula this system's own
/// per-tick re-roll uses) -- a failed roll behaves like any other
/// unresolved predicate (no link granted) but additionally raises the
/// standard `Outcome::Resisted` feedback, since here (unlike a structural
/// failure such as being out of range) the target's own resistance is what
/// stopped the link.
fn resolve_tracking(
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
    let Some(target_pos) = data.positions.get(target_entity).copied() else {
        return;
    };
    let is_alive = data
        .healths
        .get(target_entity)
        .is_some_and(|health| !health.is_dead);
    let max_range = max_sense_range(data.presences.get(caster));
    let is_in_range = target_pos.0.distance_squared(caster_pos.0) <= max_range * max_range;
    if !is_alive || !is_in_range {
        return;
    }
    let Some(caster_stats) = data.stats.get(caster) else {
        return;
    };

    let tuning = Ron::<CombatTuning>::load_expect("common.combat_tuning").read();
    let same_group = data
        .groups
        .get(caster)
        .map(|g| Some(g) == data.groups.get(target_entity))
        .unwrap_or(false);
    let holds_focus_item = data.inventories.get(caster).is_some_and(|inv| {
        inv.get_slot_of_item_by_def_id(&scrying_focus_item_def_id())
            .is_some()
    });
    let chance = scrying_resist_roll_chance(
        caster_stats,
        data.stats.get(target_entity),
        data.bodies.get(target_entity),
        data.derived_stats.get(target_entity),
        same_group,
        holds_focus_item,
        &tuning.0,
    );
    let mut rng = rand::rng();
    if rng.random::<f32>() >= chance {
        data.outcomes.emitter().emit(Outcome::Resisted {
            pos: target_pos.0,
            target: target_uid,
        });
        return;
    }

    teardown_existing_anchor(data, caster);

    let spawn_pos = target_pos.0 + scrying_follow_offset(data.orientations.get(target_entity));
    let body = Body::Object(ObjectBody::RemoteSensor);
    let (_sensor, sensor_uid) = spawn_sensor_base(data, body, spawn_pos, "name-remote-sensor");

    let _ = data.remote_senses.insert(caster, RemoteSense {
        anchor: SenseAnchor::Tracking {
            sensor: sensor_uid,
            target: target_uid,
            next_resist_roll: data.time.0 + f64::from(tuning.0.scrying_reroll_secs),
        },
        free_look: ev.free_look,
        piloted: ev.piloted,
        caster: caster_uid,
        // Meaningless for `Tracking` (never driven), same uniform-shape
        // rationale as `resolve_existing`/`resolve_sensor`.
        flight_speed: ev.flight_speed,
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
        world.register::<DerivedStats>();
        world.register::<Group>();
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
        world.register::<SpectatingEntity>();
        world.insert(IdMaps::default());
        world.insert(Time(0.0));
        world.insert(empty_terrain());
        world.insert(EventBus::<DeleteEvent>::default());
        world.insert(EventBus::<Outcome>::default());
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

    /// `arcane_eye`'s own shipped values (the old `EYE_SPAWN_RANGE`/
    /// `EYE_FLIGHT_SPEED` consts, before Fix 6 moved them into
    /// `MiscBuffData::RemoteSense`'s RON-authored fields) -- kept here so
    /// every test event below carries a realistic, non-zero value without
    /// each one re-deriving it.
    const TEST_SPAWN_RANGE: f32 = 9.0;
    const TEST_FLIGHT_SPEED: f32 = 6.0;

    fn event(caster: Entity, target: Uid) -> ResolveRemoteSenseEvent {
        ResolveRemoteSenseEvent {
            entity: caster,
            target_entity: Some(target),
            target_pos: None,
            anchor_kind: SenseAnchorKind::Existing,
            free_look: false,
            piloted: false,
            spawn_range: TEST_SPAWN_RANGE,
            flight_speed: TEST_FLIGHT_SPEED,
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
            spawn_range: TEST_SPAWN_RANGE,
            flight_speed: TEST_FLIGHT_SPEED,
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
            spawn_range: TEST_SPAWN_RANGE,
            flight_speed: TEST_FLIGHT_SPEED,
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
            Some(caster_pos.0 + Vec3::new(TEST_SPAWN_RANGE, 0.0, 0.0)),
            "the eye spawns ev.spawn_range along the caster's own look direction"
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

    /// The core regression this row's fix is about: casting `arcane_eye`
    /// (or any of its siblings) while the caster already holds an active
    /// `RemoteSense` link must not silently overwrite the caster's
    /// component slot and orphan the old anchor. After a recast, exactly
    /// one live anchor must exist, and the old one must be torn down: its
    /// `Is<Pilot>` replaced (never doubled up), and a `DeleteEvent` queued
    /// for its spawned entity (mirroring the per-tick `to_end` reaper).
    ///
    /// 🟡 **Scope note**: this only proves the ECS-teardown mechanics in
    /// isolation -- it calls `ResolveRemoteSenseEvent`'s handler directly,
    /// with no `Buffs` ever populated and `EventHandler<BuffEvent>` never
    /// run, so it does **not** exercise the same-tick buff-ordering claim
    /// `teardown_existing_anchor`'s own doc comment leans on (that
    /// `EventHandler<BuffEvent>` has already applied this tick's own
    /// `BuffEvent::Add` by the time this handler runs, via the
    /// `event_dispatch::<ResolveRemoteSenseEvent>` → `BuffEvent` edge in
    /// `server/src/events/entity_manipulation.rs`). That ordering was
    /// verified by hand against the real dispatcher wiring during review,
    /// not by an automated test -- a real integration test would need to
    /// build and run the actual `BuffEvent`+`ResolveRemoteSenseEvent`
    /// two-system dispatcher pair together and assert a same-spell recast
    /// leaves exactly one live `RemoteSensing` buff entry. Left as a
    /// follow-up rather than blocking this fix on writing that harness.
    #[test]
    fn recasting_piloted_while_already_piloting_tears_down_the_old_eye() {
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

        // First cast: grants a link and spawns the first eye.
        dispatch(&world, piloted_event(caster));
        let first_eye_uid = match world
            .read_storage::<RemoteSense>()
            .get(caster)
            .copied()
            .expect("the first cast must grant a link")
            .anchor
        {
            SenseAnchor::Piloted(eye_uid) => eye_uid,
            other => panic!("expected a Piloted anchor, got {other:?}"),
        };
        let first_eye = world
            .read_resource::<IdMaps>()
            .uid_entity(first_eye_uid)
            .expect("the first eye's Uid must resolve to a live entity");
        assert!(
            world.entities().is_alive(first_eye),
            "the first eye must be alive right after the first cast"
        );

        // Simulates what `remote_sense::Sys`'s per-tick re-validation would
        // have written by the time a recast reaches this handler: the
        // caster's `SpectatingEntity` pointing at the first (still-live) eye.
        world
            .write_storage::<SpectatingEntity>()
            .insert(caster, SpectatingEntity(first_eye))
            .unwrap();

        let entities_before_recast = world.entities().join().count();

        // Recast without the first link ever ending -- the double-tap
        // scenario the bug is about. This must tear the old eye down
        // rather than silently overwriting the caster's `RemoteSense` slot.
        dispatch(&world, piloted_event(caster));

        // Exactly one live anchor after the recast: the caster's
        // `RemoteSense` now names a brand new eye, not the old one.
        let remote_sense = world
            .read_storage::<RemoteSense>()
            .get(caster)
            .copied()
            .expect("the recast must grant a fresh link");
        let second_eye_uid = match remote_sense.anchor {
            SenseAnchor::Piloted(eye_uid) => eye_uid,
            other => panic!("expected a Piloted anchor, got {other:?}"),
        };
        assert_ne!(
            second_eye_uid, first_eye_uid,
            "the recast must anchor to a brand new eye, not the stale one"
        );

        // Exactly one new eye entity was spawned by the recast.
        assert_eq!(
            world.entities().join().count(),
            entities_before_recast + 1,
            "the recast must spawn exactly one new eye entity"
        );

        // The old eye is queued for teardown via exactly one `DeleteEvent`
        // -- this handler has no `&mut State`, so it cannot delete the
        // entity outright, only queue it the same way
        // `server/src/sys/remote_sense.rs`'s own per-tick reaper does.
        let deleted: Vec<_> = world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            deleted.len(),
            1,
            "the recast must queue exactly one DeleteEvent, for the old eye"
        );
        assert_eq!(
            deleted[0].0, first_eye,
            "the queued DeleteEvent must target the old eye, not the new one"
        );

        // The caster ends up with exactly one `Is<Pilot>` -- the new one --
        // never two, and never a dangling reference to the old eye.
        let is_pilots = world.read_storage::<Is<Pilot>>();
        let is_pilot = is_pilots
            .get(caster)
            .expect("the caster must still be wired as a Pilot after the recast");
        assert_eq!(
            is_pilot.piloted, second_eye_uid,
            "the caster's Is<Pilot> must point at the new eye, not the old one"
        );
        drop(is_pilots);

        // The new eye has its own `Is<Piloted>` half of the link.
        let second_eye = world
            .read_resource::<IdMaps>()
            .uid_entity(second_eye_uid)
            .expect("the second eye's Uid must resolve to a live entity");
        assert!(
            world
                .read_storage::<Is<Piloted>>()
                .get(second_eye)
                .is_some(),
            "the new eye must be wired as the Piloted half of the Piloting link"
        );

        // The stale `SpectatingEntity` pointing at the now-deleted first eye
        // must not survive the recast -- left alone it would name a dead
        // entity for one tick until `remote_sense::Sys` next self-heals it.
        assert!(
            world
                .read_storage::<SpectatingEntity>()
                .get(caster)
                .is_none(),
            "teardown_existing_anchor must clear the stale SpectatingEntity, not leave it \
             pointing at the deleted first eye"
        );
    }

    fn tracking_event(caster: Entity, target: Uid) -> ResolveRemoteSenseEvent {
        ResolveRemoteSenseEvent {
            entity: caster,
            target_entity: Some(target),
            target_pos: None,
            anchor_kind: SenseAnchorKind::Tracking,
            free_look: true,
            piloted: false,
            spawn_range: TEST_SPAWN_RANGE,
            flight_speed: TEST_FLIGHT_SPEED,
        }
    }

    /// A caster's own `Stats` is required for `resolve_tracking` to even
    /// attempt the resist roll (a caster somehow missing `Stats` fails
    /// closed, the same "no remote view granted" shape as every other
    /// unresolved predicate here).
    fn caster_with_stats(world: &mut specs::World, uid_val: Uid, pos: Vec3<f32>) -> Entity {
        let caster = world
            .create_entity()
            .with(Pos(pos))
            .with(uid_val)
            .with(Stats::new(
                common_i18n::Content::Plain(String::new()),
                tameable_body(),
            ))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .build();
        world.write_resource::<IdMaps>().add_entity(uid_val, caster);
        caster
    }

    #[test]
    fn tracking_out_of_range_target_grants_no_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);
        // No Presence on this caster (built by hand, not via
        // `caster_with_stats`) -- `max_sense_range` defaults to 0.0, so any
        // nonzero distance is out of range, the same shape
        // `sensor_out_of_range_target_grants_no_link_and_spawns_nothing`
        // already proves for the `Sensor` anchor.
        let caster = world
            .create_entity()
            .with(Pos(Vec3::new(4.0, 4.0, 5.0)))
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .with(Stats::new(
                common_i18n::Content::Plain(String::new()),
                tameable_body(),
            ))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(caster_uid, caster);
        let target = world
            .create_entity()
            .with(Pos(Vec3::new(400.0, 4.0, 5.0)))
            .with(Health::new(tameable_body()))
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(target_uid, target);

        let entities_before = world.entities().join().count();

        dispatch(&world, tracking_event(caster, target_uid));

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
    fn tracking_dead_target_grants_no_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);
        let caster = caster_with_stats(&mut world, caster_uid, Vec3::new(4.0, 4.0, 5.0));
        let mut target_health = Health::new(tameable_body());
        target_health.is_dead = true;
        let target = world
            .create_entity()
            .with(Pos(Vec3::new(6.0, 4.0, 5.0)))
            .with(target_health)
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(target_uid, target);

        dispatch(&world, tracking_event(caster, target_uid));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "a dead target must not grant a link"
        );
    }

    /// The cast-time counterpart of
    /// `server/src/sys/remote_sense.rs`'s own
    /// `an_outclassed_scrying_link_is_resisted_and_ends`: a caster hopelessly
    /// outclassed by the target's `combat_rating` hard-fails
    /// `scrying_success_chance`'s `outclassed_wall` to exactly `0.0`, so the
    /// very first cast must be resisted -- no link, no sensor spawned, and
    /// the standard `Outcome::Resisted` feedback fires (this is the target's
    /// own resistance stopping the link, not a structural failure like
    /// range).
    #[test]
    fn tracking_outclassed_caster_is_resisted_and_grants_no_link() {
        let mut world = setup_world();
        let caster_uid = uid(1);
        let target_uid = uid(2);
        let caster = caster_with_stats(&mut world, caster_uid, Vec3::new(4.0, 4.0, 5.0));
        let target = world
            .create_entity()
            .with(Pos(Vec3::new(6.0, 4.0, 5.0)))
            .with(Health::new(tameable_body()))
            .with(DerivedStats {
                combat_rating: 100.0,
                ..Default::default()
            })
            .build();
        world
            .write_resource::<IdMaps>()
            .add_entity(target_uid, target);

        let entities_before = world.entities().join().count();

        dispatch(&world, tracking_event(caster, target_uid));

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "a hard-failed resist roll must not grant a link"
        );
        assert_eq!(
            world.entities().join().count(),
            entities_before,
            "a resisted cast must not spawn a sensor entity"
        );
        let resisted: Vec<_> = world
            .read_resource::<EventBus<Outcome>>()
            .recv_all()
            .filter(|o| matches!(o, Outcome::Resisted { target, .. } if *target == target_uid))
            .collect();
        assert_eq!(
            resisted.len(),
            1,
            "a resisted cast-time roll must emit exactly one Outcome::Resisted"
        );
    }
}
