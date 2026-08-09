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
//!
//! Known narrow interaction: this system unconditionally reasserts
//! `SpectatingEntity` from a still-valid `RemoteSense` every tick, so a
//! moderator who manually toggles their own debug spectate elsewhere while
//! *also* holding an active link will see it overwritten back to the link's
//! anchor on the next tick. Harmless (both are the same moderator's own
//! choices), but worth knowing if this ever needs to change.

use common::{
    assets::{AssetExt, Ron},
    combat::{self, CombatTuning, SaveCasterInfo, SaveTargetInfo},
    comp::{
        Body, Buffs, DerivedStats, Group, Health, Inventory, Ori, Pos, Presence, RemoteSense,
        SpectatingEntity, Stats,
        body::MagicResistTier,
        buff::{BuffChange, BuffKind},
        item::ItemDefinitionIdOwned,
        remote_sense::SenseAnchor,
    },
    event::{BuffEvent, DeleteEvent, EmitExt, EventBus},
    event_emitters,
    link::Is,
    outcome::Outcome,
    piloting::Pilot,
    resources::Time,
    terrain::TerrainChunkSize,
    uid::{IdMaps, Uid},
    vol::RectVolSize,
};
use common_ecs::{Job, Origin, Phase, System};
use rand::RngExt;
use specs::{Entities, Entity, Join, Read, ReadStorage, WriteStorage};
use vek::{Vec2, Vec3};

event_emitters! {
    struct Events[Emitters] {
        buff: BuffEvent,
        delete: DeleteEvent,
    }
}

/// The honest range ceiling for any remote-sensing link: a caster with no
/// `Presence` (and so no `entity_view_distance`) can never validate one
/// unless the anchor shares its exact position. Shared by this system's
/// per-tick re-validation and by the cast-time resolve handler
/// (`server/src/events/remote_sense.rs`) so the two never drift apart into a
/// second range constant.
pub(crate) fn max_sense_range(presence: Option<&Presence>) -> f32 {
    presence.map_or(0.0, |presence| {
        presence.entity_view_distance.current() as f32
            * TerrainChunkSize::RECT_SIZE.reduce_max() as f32
    })
}

/// `common.items.utility.scrying_crystal`'s definition id -- the divination
/// focus `scrying`'s resist roll checks for via
/// `Inventory::get_slot_of_item_by_def_id`, the same "does the caster hold
/// item X" idiom `AbilityRequirements.item`/`AbilityReqItem`
/// (`common/src/comp/ability.rs`) already use for consumables. This is a
/// direct inventory-presence check, not routed through `AbilityReqItem`:
/// holding the crystal grants a bonus, it never gates casting `scrying`
/// itself.
pub(crate) fn scrying_focus_item_def_id() -> ItemDefinitionIdOwned {
    ItemDefinitionIdOwned::Simple(String::from("common.items.utility.scrying_crystal"))
}

/// Where `scrying`'s sensor sits relative to the scried target this tick:
/// `behind_dist` behind their horizontal facing and `above_dist` above them
/// (both forwarded from the cast's own `MiscBuffData::RemoteSense` via the
/// active link's `SenseAnchor::Tracking`), recomputed from `target_ori` every
/// call so the sensor swings around with the target rather than clipping
/// through them as they turn. Pitch is deliberately ignored (only the
/// horizontal component of `look_dir` is used) so the sensor never dives
/// underground or rockets skyward when the target merely looks up or down;
/// when `target_ori` is absent, or the target is looking close enough to
/// straight up/down that the horizontal component is degenerate, this falls
/// back to a fixed south-behind offset rather than producing a zero vector.
pub(crate) fn scrying_follow_offset(
    target_ori: Option<&Ori>,
    behind_dist: f32,
    above_dist: f32,
) -> Vec3<f32> {
    const DEGENERATE_EPS: f32 = 1e-6;
    let horizontal_behind = target_ori
        .map(|ori| ori.look_dir().to_vec())
        .and_then(|look| {
            let flat = Vec2::new(look.x, look.y);
            (flat.magnitude_squared() > DEGENERATE_EPS).then(|| -flat.normalized())
        })
        .unwrap_or(Vec2::new(0.0, -1.0));
    Vec3::new(
        horizontal_behind.x * behind_dist,
        horizontal_behind.y * behind_dist,
        above_dist,
    )
}

/// The scrying resist roll for one link: the caster's own `magic_accuracy`
/// plus the additive party/focus-item bonuses (see
/// `combat::scrying_success_chance`'s own doc comment), gathered from
/// already-shipped components exactly like `server/src/events/banishment.rs`
/// gathers the same shapes for its own saving throw. Shared by the cast-time
/// roll (`server/src/events/remote_sense.rs`) and this system's periodic
/// re-roll so both are provably the same mechanism, not two.
pub(crate) fn scrying_resist_roll_chance(
    caster_stats: &Stats,
    target_stats: Option<&Stats>,
    target_body: Option<&Body>,
    target_derived: Option<&DerivedStats>,
    same_group: bool,
    caster_holds_focus_item: bool,
    tuning: &CombatTuning,
) -> f32 {
    let target_info = SaveTargetInfo {
        stats_magic_evasion: target_stats.map_or(0.0, |s| s.magic_evasion),
        crowd_control_resistance: target_stats.map_or(0.0, |s| s.crowd_control_resistance),
        stats_magic_resistance: target_stats.map_or(0.0, |s| s.magic_resistance),
        magic_resist_tier: target_body.map_or(MagicResistTier::None, Body::magic_resist_tier),
        combat_rating: target_derived.map_or(0.0, |d| d.combat_rating),
    };
    combat::scrying_success_chance(
        &SaveCasterInfo {
            magic_accuracy: caster_stats.magic_accuracy,
        },
        &target_info,
        same_group,
        caster_holds_focus_item,
        tuning,
    )
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        Read<'a, IdMaps>,
        Read<'a, Time>,
        Read<'a, EventBus<Outcome>>,
        WriteStorage<'a, Pos>,
        ReadStorage<'a, Ori>,
        ReadStorage<'a, Buffs>,
        ReadStorage<'a, Health>,
        ReadStorage<'a, Presence>,
        ReadStorage<'a, Stats>,
        ReadStorage<'a, DerivedStats>,
        ReadStorage<'a, Group>,
        ReadStorage<'a, Inventory>,
        ReadStorage<'a, Body>,
        WriteStorage<'a, RemoteSense>,
        WriteStorage<'a, SpectatingEntity>,
        WriteStorage<'a, Is<Pilot>>,
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
            time,
            outcomes,
            mut positions,
            orientations,
            buffs,
            healths,
            presences,
            stats,
            derived_stats,
            groups,
            inventories,
            bodies,
            mut remote_senses,
            mut spectating_entities,
            mut is_pilots,
        ): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();
        let mut outcome_emitter = outcomes.emitter();

        // --- `Tracking` (`scrying`) upkeep: follow the target, then re-roll
        // the resist check on its own cadence. Runs before the generic
        // validity pass below so that pass's range check reads each
        // sensor's freshly-updated `Pos`, not last tick's.
        //
        // The link's own state (`caster`, sensor/target `Uid`,
        // `next_resist_roll`) is copied out of `remote_senses` up front so
        // this borrows it only immutably and briefly -- `positions` (needed
        // both to read the target and to write the sensor) and, later,
        // `remote_senses` itself (to advance `next_resist_roll`) are then
        // free to be borrowed again without conflict.
        struct TrackingLink {
            caster: Entity,
            sensor: Uid,
            target: Uid,
            next_resist_roll: f64,
            behind_dist: f32,
            above_dist: f32,
        }
        let tracking_links: Vec<TrackingLink> = (&entities, &remote_senses)
            .join()
            .filter_map(|(caster, rs)| match rs.anchor {
                SenseAnchor::Tracking {
                    sensor,
                    target,
                    next_resist_roll,
                    behind_dist,
                    above_dist,
                } => Some(TrackingLink {
                    caster,
                    sensor,
                    target,
                    next_resist_roll,
                    behind_dist,
                    above_dist,
                }),
                _ => None,
            })
            .collect();

        let mut target_lost: Vec<Entity> = Vec::new();
        let mut resisted: Vec<Entity> = Vec::new();

        if !tracking_links.is_empty() {
            let tuning = Ron::<CombatTuning>::load_expect("common.combat_tuning").read();
            let mut rng = rand::rng();
            let focus_item_def_id = scrying_focus_item_def_id();

            for link in &tracking_links {
                // The sensor itself missing is not handled here: the
                // generic validity pass below already ends any link whose
                // `anchor_uid()` no longer resolves, Tracking included.
                let Some(sensor_entity) = id_maps.uid_entity(link.sensor) else {
                    continue;
                };
                let Some(target_entity) = id_maps.uid_entity(link.target) else {
                    target_lost.push(link.caster);
                    continue;
                };
                let target_alive = !healths.get(target_entity).is_some_and(|h| h.is_dead);
                let Some(target_pos) = positions.get(target_entity).copied() else {
                    target_lost.push(link.caster);
                    continue;
                };
                if !target_alive {
                    target_lost.push(link.caster);
                    continue;
                }

                let offset = scrying_follow_offset(
                    orientations.get(target_entity),
                    link.behind_dist,
                    link.above_dist,
                );
                let new_sensor_pos = target_pos.0 + offset;
                let _ = positions.insert(sensor_entity, Pos(new_sensor_pos));

                if time.0 < link.next_resist_roll {
                    continue;
                }

                let Some(caster_stats) = stats.get(link.caster) else {
                    // No Stats on the caster (should not happen for a
                    // caster that formed this link) -- fail closed rather
                    // than leave an un-rollable link running forever.
                    resisted.push(link.caster);
                    continue;
                };
                let same_group = groups
                    .get(link.caster)
                    .map(|g| Some(g) == groups.get(target_entity))
                    .unwrap_or(false);
                let holds_focus_item = inventories.get(link.caster).is_some_and(|inv| {
                    inv.get_slot_of_item_by_def_id(&focus_item_def_id).is_some()
                });
                let chance = scrying_resist_roll_chance(
                    caster_stats,
                    stats.get(target_entity),
                    bodies.get(target_entity),
                    derived_stats.get(target_entity),
                    same_group,
                    holds_focus_item,
                    &tuning.0,
                );

                if rng.random::<f32>() >= chance {
                    resisted.push(link.caster);
                    outcome_emitter.emit(Outcome::Resisted {
                        pos: new_sensor_pos,
                        target: link.target,
                    });
                } else if let Some(mut remote_sense) = remote_senses.get_mut(link.caster)
                    && let SenseAnchor::Tracking {
                        next_resist_roll, ..
                    } = &mut remote_sense.anchor
                {
                    *next_resist_roll = time.0 + f64::from(tuning.0.scrying_reroll_secs);
                }
            }
        }

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
                        // Correct as long as only entities with a
                        // `Presence` (i.e. players) ever hold a
                        // `RemoteSense`; revisit this if a non-player
                        // caster is ever introduced.
                        let max_range = max_sense_range(presences.get(caster));
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

        // Merge in the Tracking-specific endings (target gone/dead, or the
        // periodic resist re-roll failed) collected above, deduped against
        // whatever the generic pass already caught (e.g. a resisted link
        // whose sensor also just fell out of range this same tick).
        for caster in target_lost.into_iter().chain(resisted) {
            if !to_end.contains(&caster) {
                to_end.push(caster);
            }
        }

        for caster in to_end {
            spectating_entities.remove(caster);

            if let Some(remote_sense) = remote_senses.remove(caster) {
                // A `Piloted` anchor also carries a `Piloting` link, wired by
                // hand (not via `StateExt::link()`, see
                // `common/src/piloting.rs`'s own doc comment) -- so it must be
                // torn down by hand too, the same way
                // `common/src/states/telekinetic_grip.rs` explicitly removes
                // `Is<Leader>`/`Is<Follower>` when its own hand-wired
                // `Tethered` link ends. The eye's own `Is<Piloted>` needs no
                // matching removal: it is cleared automatically when the
                // `DeleteEvent` below removes the entity outright.
                if matches!(remote_sense.anchor, SenseAnchor::Piloted(_)) {
                    is_pilots.remove(caster);
                }

                if remote_sense.anchor.is_spawned_sensor()
                    && let Some(anchor_entity) = id_maps.uid_entity(remote_sense.anchor_uid())
                {
                    emitters.emit(DeleteEvent(anchor_entity));
                }
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
        ViewDistances,
        comp::{
            Body, PresenceKind,
            buff::{Buff, BuffData, BuffSource, DestInfo},
            humanoid,
            remote_sense::SenseAnchor,
        },
        event::EventBus,
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
            None,
        )
    }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Ori>();
        world.register::<Buffs>();
        world.register::<Health>();
        world.register::<Presence>();
        world.register::<Stats>();
        world.register::<DerivedStats>();
        world.register::<Group>();
        world.register::<Inventory>();
        world.register::<Body>();
        world.register::<RemoteSense>();
        world.register::<SpectatingEntity>();
        world.register::<Is<Pilot>>();
        world.insert(IdMaps::default());
        world.insert(Time(0.0));
        world.insert(EventBus::<Outcome>::default());
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
                flight_speed: 0.0,
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
                flight_speed: 0.0,
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

    /// A `Sensor` anchor (unlike `Existing`) owns the entity it points at:
    /// ending the link -- here via a dead caster, the same trigger as the
    /// test above -- must also reap the sensor itself via `DeleteEvent`, not
    /// just clear `SpectatingEntity`/`RemoteSense`. This is the belt half of
    /// the sensor's destroy-on-health-loss story: whichever direction ends
    /// the link, the sensor never lingers as an orphan entity.
    #[test]
    fn a_spawned_sensor_anchor_is_reaped_when_its_link_ends() {
        let mut world = setup_world();
        world.insert(EventBus::<DeleteEvent>::default());

        let caster_uid = uid(1);
        let sensor_uid = uid(2);
        let sensor = world.create_entity().with(Pos(Vec3::zero())).build();
        let body = Body::Humanoid(humanoid::Body::random());
        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Health::new(body))
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(sensor_uid, sensor);
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
                anchor: SenseAnchor::Sensor(sensor_uid),
                free_look: true,
                piloted: false,
                caster: caster_uid,
                flight_speed: 0.0,
            })
            .unwrap();
        world
            .write_storage::<Health>()
            .get_mut(caster)
            .unwrap()
            .is_dead = true;

        common_ecs::run_now::<Sys>(&world);

        assert!(world.read_storage::<RemoteSense>().get(caster).is_none());
        let deleted: Vec<_> = world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            deleted.len(),
            1,
            "exactly one DeleteEvent, for the sensor entity"
        );
        assert_eq!(deleted[0].0, sensor);
    }

    /// A `Piloted` anchor (the `arcane_eye` eye) also carries an `Is<Pilot>`
    /// on the caster, wired by hand alongside the link
    /// (`server/src/events/remote_sense.rs`'s `resolve_piloted`) rather than
    /// through the generic `Link` bookkeeping. Ending the link -- here via
    /// the eye's own destruction, which is exactly what a True-Sight holder
    /// destroying the eye's 30 HP looks like from this system's point of
    /// view (the anchor `Uid` no longer resolves) -- must reap `Is<Pilot>`
    /// too, or the caster is left permanently forwarding inputs into a
    /// `pilot::Sys` join that no longer has a target.
    #[test]
    fn destroying_the_eye_ends_the_link_and_clears_is_pilot() {
        let mut world = setup_world();
        world.insert(EventBus::<DeleteEvent>::default());

        let caster_uid = uid(1);
        let eye_uid = uid(2);
        let body = Body::Humanoid(humanoid::Body::random());
        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Health::new(body))
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            // The eye itself is already gone (its own Health hit 0 and the
            // destroy pipeline reaped it) -- its `Uid` no longer resolves to
            // a live entity, exactly like a dead sensor.
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
                anchor: SenseAnchor::Piloted(eye_uid),
                free_look: false,
                piloted: true,
                caster: caster_uid,
                flight_speed: 0.0,
            })
            .unwrap();
        world
            .write_storage::<Is<Pilot>>()
            .insert(
                caster,
                common::link::LinkHandle::from_link(common::piloting::Piloting {
                    pilot: caster_uid,
                    piloted: eye_uid,
                })
                .make_role(),
            )
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        assert!(
            world.read_storage::<RemoteSense>().get(caster).is_none(),
            "an anchor whose Uid no longer resolves must end the link"
        );
        assert!(
            world.read_storage::<Is<Pilot>>().get(caster).is_none(),
            "ending a Piloted link must also clear the caster's Is<Pilot>"
        );
    }

    /// `scrying_follow_offset`'s own shape: behind the target's horizontal
    /// facing and a fixed height above, using only the pure function -- no
    /// ECS involved.
    #[test]
    fn follow_offset_sits_behind_and_above_the_targets_facing() {
        const BEHIND_DIST: f32 = 3.0;
        const ABOVE_DIST: f32 = 2.0;
        // Facing +x: "behind" is -x.
        let facing_east = Ori::from_unnormalized_vec(Vec3::new(1.0, 0.0, 0.0)).unwrap();
        let offset = scrying_follow_offset(Some(&facing_east), BEHIND_DIST, ABOVE_DIST);
        assert!(
            offset.x < 0.0,
            "behind +x facing must be -x, got {offset:?}"
        );
        assert!(
            offset.y.abs() < 1e-3,
            "no lateral drift for a pure +x facing, got {offset:?}"
        );
        assert!(
            (offset.z - ABOVE_DIST).abs() < 1e-3,
            "must sit exactly above_dist above regardless of facing, got {offset:?}"
        );
        let horizontal_dist = (offset.x * offset.x + offset.y * offset.y).sqrt();
        assert!(
            (horizontal_dist - BEHIND_DIST).abs() < 1e-3,
            "horizontal distance must be exactly behind_dist, got {horizontal_dist}"
        );
    }

    /// A missing `Ori` (or one looking straight up/down, the degenerate
    /// horizontal case) must not collapse to a zero offset -- the sensor
    /// still floats a fixed distance away and above, just along a fallback
    /// direction.
    #[test]
    fn follow_offset_falls_back_when_the_targets_facing_is_unavailable() {
        const BEHIND_DIST: f32 = 3.0;
        const ABOVE_DIST: f32 = 2.0;
        let offset = scrying_follow_offset(None, BEHIND_DIST, ABOVE_DIST);
        assert!(
            (offset.z - ABOVE_DIST).abs() < 1e-3,
            "still floats above with no Ori, got {offset:?}"
        );
        let horizontal_dist = (offset.x * offset.x + offset.y * offset.y).sqrt();
        assert!(
            (horizontal_dist - BEHIND_DIST).abs() < 1e-3,
            "still offsets a fixed horizontal distance with no Ori, got {horizontal_dist}"
        );

        let facing_straight_up = Ori::from_unnormalized_vec(Vec3::new(0.0, 0.0, 1.0)).unwrap();
        let degenerate_offset =
            scrying_follow_offset(Some(&facing_straight_up), BEHIND_DIST, ABOVE_DIST);
        let degenerate_horizontal_dist = (degenerate_offset.x * degenerate_offset.x
            + degenerate_offset.y * degenerate_offset.y)
            .sqrt();
        assert!(
            (degenerate_horizontal_dist - BEHIND_DIST).abs() < 1e-3,
            "looking straight up must not collapse the horizontal offset to zero, got \
             {degenerate_offset:?}"
        );
    }

    /// Everything a `Tracking` link's own tests below need beyond
    /// `setup_world()`: a caster, a target, and a sensor, wired through
    /// `IdMaps`, with `RemoteSense` anchored `Tracking` between them.
    struct TrackingFixture {
        caster: Entity,
        caster_uid: Uid,
        target: Entity,
        target_uid: Uid,
        sensor: Entity,
        sensor_uid: Uid,
    }

    fn setup_tracking_fixture(
        world: &mut specs::World,
        target_pos: Vec3<f32>,
        next_resist_roll: f64,
    ) -> TrackingFixture {
        let caster_uid = uid(1);
        let target_uid = uid(2);
        let sensor_uid = uid(3);

        let body = Body::Humanoid(humanoid::Body::random());
        let caster = world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(Health::new(body))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .with({
                let mut stats = Stats::new(common_i18n::Content::Plain(String::new()), body);
                stats.magic_accuracy = 0.0;
                stats
            })
            .build();
        let target = world
            .create_entity()
            .with(Pos(target_pos))
            .with(Health::new(body))
            .build();
        let sensor = world.create_entity().with(Pos(Vec3::zero())).build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(caster_uid, caster);
            id_maps.add_entity(target_uid, target);
            id_maps.add_entity(sensor_uid, sensor);
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
                anchor: SenseAnchor::Tracking {
                    sensor: sensor_uid,
                    target: target_uid,
                    next_resist_roll,
                    behind_dist: 3.0,
                    above_dist: 2.0,
                },
                free_look: true,
                piloted: false,
                caster: caster_uid,
                flight_speed: 0.0,
            })
            .unwrap();

        TrackingFixture {
            caster,
            caster_uid,
            target,
            target_uid,
            sensor,
            sensor_uid,
        }
    }

    /// The sensor's `Pos` is rewritten directly from the target's own `Pos`
    /// and `Ori` every tick -- never `Tethered` (a physics leash), per this
    /// system's own doc comment on why. Also proves the link survives a
    /// tick where the reroll isn't due yet, and `next_resist_roll` is left
    /// untouched.
    #[test]
    fn tracking_sensor_follows_the_target_each_tick() {
        let mut world = setup_world();
        let target_pos = Vec3::new(20.0, 0.0, 5.0);
        // Far in the future: this tick's reroll must not fire.
        let fixture = setup_tracking_fixture(&mut world, target_pos, 1_000_000.0);
        world
            .write_storage::<Ori>()
            .insert(
                fixture.target,
                Ori::from_unnormalized_vec(Vec3::new(1.0, 0.0, 0.0)).unwrap(),
            )
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        let expected_pos = target_pos
            + scrying_follow_offset(
                Some(&Ori::from_unnormalized_vec(Vec3::new(1.0, 0.0, 0.0)).unwrap()),
                3.0,
                2.0,
            );
        assert_eq!(
            world.read_storage::<Pos>().get(fixture.sensor).map(|p| p.0),
            Some(expected_pos),
            "the sensor's Pos must be written directly from the target's Pos + facing offset"
        );
        let remote_sense = world
            .read_storage::<RemoteSense>()
            .get(fixture.caster)
            .copied()
            .expect("the link must survive a tick where the reroll isn't due");
        assert_eq!(remote_sense.caster, fixture.caster_uid);
        assert_eq!(
            remote_sense.anchor,
            SenseAnchor::Tracking {
                sensor: fixture.sensor_uid,
                target: fixture.target_uid,
                next_resist_roll: 1_000_000.0,
                behind_dist: 3.0,
                above_dist: 2.0,
            },
            "next_resist_roll must be untouched when the reroll doesn't fire this tick"
        );
    }

    /// A caster hopelessly outclassed by the scried target's combat rating
    /// hard-fails `scrying_success_chance`'s `outclassed_wall` to exactly
    /// `0.0` (see `combat::scrying_success_chance`), so the very first
    /// due reroll must resist and end the link -- reaping the sensor and
    /// clearing the sustaining buff the same way every other ended link
    /// does.
    #[test]
    fn an_outclassed_scrying_link_is_resisted_and_ends() {
        let mut world = setup_world();
        world.insert(EventBus::<DeleteEvent>::default());
        let target_pos = Vec3::new(20.0, 0.0, 5.0);
        // Due now (Time defaults to 0.0).
        let fixture = setup_tracking_fixture(&mut world, target_pos, 0.0);
        world
            .write_storage::<DerivedStats>()
            .insert(fixture.target, DerivedStats {
                combat_rating: 100.0,
                ..Default::default()
            })
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        assert!(
            world
                .read_storage::<RemoteSense>()
                .get(fixture.caster)
                .is_none(),
            "a hard-failed resist roll must end the link"
        );
        let deleted: Vec<_> = world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            deleted.len(),
            1,
            "the sensor must be reaped when the link ends"
        );
        assert_eq!(deleted[0].0, fixture.sensor);
        let resisted_outcomes: Vec<_> = world
            .read_resource::<EventBus<Outcome>>()
            .recv_all()
            .filter(
                |o| matches!(o, Outcome::Resisted { target, .. } if *target == fixture.target_uid),
            )
            .collect();
        assert_eq!(
            resisted_outcomes.len(),
            1,
            "a resisted roll must emit exactly one Outcome::Resisted for the scried target"
        );
    }

    /// The scried target dying (or otherwise no longer resolving) must end
    /// the link even though the sensor itself is untouched and would
    /// otherwise still be in range -- a frozen sensor with nothing left to
    /// follow is not a valid link.
    #[test]
    fn losing_the_scried_target_ends_the_link() {
        let mut world = setup_world();
        world.insert(EventBus::<DeleteEvent>::default());
        let target_pos = Vec3::new(0.0, 0.0, 0.0);
        let fixture = setup_tracking_fixture(&mut world, target_pos, 1_000_000.0);
        world
            .write_storage::<Health>()
            .get_mut(fixture.target)
            .unwrap()
            .is_dead = true;

        common_ecs::run_now::<Sys>(&world);

        assert!(
            world
                .read_storage::<RemoteSense>()
                .get(fixture.caster)
                .is_none(),
            "a dead scried target must end the link even with the reroll not yet due"
        );
    }
}
