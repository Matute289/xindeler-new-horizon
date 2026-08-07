//! Server-only authority for magical senses: turns each observer's *sense
//! declarations* into the owner-private *reveal set*.
//!
//! The buff system rebuilds `Stats.senses` from scratch every tick out of the
//! observer's currently-active detection buffs. This system reads that
//! declaration, runs a spatial query per declared sense, applies the
//! per-`SenseKind` predicate, and writes the result into `Detected`.
//!
//! # Dispatch ordering
//!
//! This system **must** run after the buff system within the same tick,
//! because `Stats` (and therefore `Stats.senses`) is wiped and rebuilt there:
//! running earlier would read the previous tick's declarations, so a sense
//! would lag one tick behind the buff that grants it and — worse — a sense
//! would still be honoured for one tick after its buff was removed. That
//! system is not reachable from this crate (its module is private to
//! `common-systems`), so the dependency is named by its computed `sys_name()`,
//! `"Common_buff_sys"`, in `server/src/sys/mod.rs`.
//!
//! # Why this lives in the server crate only
//!
//! It is registered exclusively in `server/src/sys/mod.rs` and never in
//! `common/systems/src/lib.rs::add_local_systems`. That single fact is the
//! whole "the client is never trusted to decide what it can see" guarantee for
//! detection: a client cannot compute a reveal set for itself because the code
//! that computes one does not live in a crate the client dispatches. (Contrast
//! the buff system, which *is* an `add_local_systems` system and therefore
//! runs on both sides.)
//!
//! # Snapshot vs continuous
//!
//! A `SenseMode::Snapshot` sense is evaluated exactly once — on the first tick
//! its declaration is seen — and its membership is then frozen for as long as
//! the declaration lasts, including when the frozen result is *empty*. Freezing
//! cannot be inferred from `Detected` alone for exactly that reason (an empty
//! result is indistinguishable from "not computed yet"), so the set of
//! currently-frozen sense kinds per observer is tracked in the
//! [`DetectionSnapshots`] resource. `SenseMode::Continuous` senses are
//! re-evaluated on a throttle (every [`CONTINUOUS_THROTTLE_RUNS`] runs)
//! rather than every single run — an implementation detail only, never a
//! player-visible pulse: a throttled-skip run carries the previous run's
//! membership forward verbatim, the same way a frozen snapshot does, so
//! membership never flickers or drops between real re-queries.
//!
//! Whatever the mode, the component is rewritten **wholesale**: frozen entries
//! are carried over from the previous value and recomputed entries are appended
//! into a freshly built `Detected`, which then replaces the old one. The reveal
//! set is never patched in place.

use common::{
    CachedSpatialGrid,
    comp::{
        ActiveSense, Body, Buffs, Disguise, Object, PhantomIllusion, PickupItem, Pos, Stats,
        WaypointArea,
        buff::{BuffCategory, BuffKind, SenseMode},
        creature_type::CreatureKind,
        detection::{Detected, DetectedEntity, DetectedPoint, SenseKind},
    },
    uid::Uid,
};
use common_ecs::{Job, Origin, Phase, System};
use hashbrown::HashMap;
use specs::{Entities, Entity, Join, Read, ReadStorage, SystemData, Write, WriteStorage, shred};
use vek::Vec3;

/// Buff kinds that count as a poison/disease-family affliction.
const AFFLICTION_BUFFS: &[BuffKind] = &[BuffKind::Poisoned, BuffKind::Cursed, BuffKind::Burning];

/// How many detection-system runs pass between two real spatial re-queries of
/// one `SenseMode::Continuous` sense kind. The server tick runs at roughly
/// 30 Hz, so this is a cadence of about 0.13 s. On a run that isn't due, the
/// previous run's membership for that kind is carried over into `Detected`
/// verbatim (the same carry-forward the `Snapshot` freeze logic already does)
/// rather than left empty, so a throttled-skip run never flickers or drops
/// membership — only the underlying query cadence is coarser than every
/// tick, never the player-visible reveal set.
const CONTINUOUS_THROTTLE_RUNS: u64 = 4;

/// Per-observer bookkeeping for `SenseMode::Snapshot` freezing and
/// `SenseMode::Continuous` throttling.
///
/// Holds only the *kinds* that are currently frozen, or the run at which a
/// continuous kind was last actually re-queried — never the membership
/// itself, which is carried over from the observer's own `Detected`. Entries
/// are pruned every run, so an observer that stops sensing (or is deleted)
/// leaves nothing behind, and a re-cast of the same sense a tick later is
/// correctly treated as fresh (a new snapshot, or an immediate first
/// continuous evaluation).
#[derive(Default)]
pub struct DetectionSnapshots {
    /// Monotonic run counter used to prune entries that were not refreshed by
    /// the current run, and to decide whether a continuous kind is due for
    /// re-evaluation.
    run: u64,
    per_observer: HashMap<Entity, ObserverSnapshots>,
}

#[derive(Default)]
struct ObserverSnapshots {
    last_run: u64,
    frozen: Vec<SenseKind>,
    /// The run counter value at which each currently-declared
    /// `SenseMode::Continuous` sense kind was last actually re-queried
    /// against the spatial grid.
    continuous_last_eval: HashMap<SenseKind, u64>,
}

impl DetectionSnapshots {
    fn begin_run(&mut self) { self.run = self.run.wrapping_add(1); }

    /// Take this observer's previous bookkeeping out of the map, so the
    /// caller owns it while it decides what carries forward.
    fn take(&mut self, observer: Entity) -> ObserverSnapshots {
        self.per_observer.remove(&observer).unwrap_or_default()
    }

    fn store(&mut self, observer: Entity, mut entry: ObserverSnapshots) {
        entry.last_run = self.run;
        self.per_observer.insert(observer, entry);
    }

    /// Drop every observer that this run did not touch.
    fn prune(&mut self) {
        let run = self.run;
        self.per_observer.retain(|_, entry| entry.last_run == run);
    }
}

#[derive(SystemData)]
pub struct ReadData<'a> {
    entities: Entities<'a>,
    cached_spatial_grid: Read<'a, CachedSpatialGrid>,
    positions: ReadStorage<'a, Pos>,
    stats: ReadStorage<'a, Stats>,
    uids: ReadStorage<'a, Uid>,
    bodies: ReadStorage<'a, Body>,
    buffs: ReadStorage<'a, Buffs>,
    objects: ReadStorage<'a, Object>,
    pickup_items: ReadStorage<'a, PickupItem>,
    waypoint_areas: ReadStorage<'a, WaypointArea>,
    /// True Sight's own reveal targets, read by [`reveals`]'s `SenseKind::True`
    /// arm.
    disguises: ReadStorage<'a, Disguise>,
    phantom_illusions: ReadStorage<'a, PhantomIllusion>,
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        ReadData<'a>,
        WriteStorage<'a, Detected>,
        Write<'a, DetectionSnapshots>,
    );

    const NAME: &'static str = "detection";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(_job: &mut Job<Self>, (read_data, mut detecteds, mut snapshots): Self::SystemData) {
        snapshots.begin_run();
        let run = snapshots.run;

        for (observer, observer_pos, observer_stats) in
            (&read_data.entities, &read_data.positions, &read_data.stats).join()
        {
            if observer_stats.senses.is_empty() {
                // Every sense ended: the reveal set no longer exists at all.
                // Leaving an empty component behind would keep syncing a dead
                // payload to the client forever.
                if detecteds.contains(observer) {
                    detecteds.remove(observer);
                }
                continue;
            }

            let senses = merged_senses(&observer_stats.senses);
            let mut bookkeeping = snapshots.take(observer);
            let previously_frozen = std::mem::take(&mut bookkeeping.frozen);
            let previous = detecteds.get(observer);

            let mut entities = Vec::new();
            let mut points = Vec::new();
            let mut frozen = Vec::new();

            for sense in &senses {
                match sense.mode {
                    SenseMode::Snapshot => {
                        frozen.push(sense.kind);

                        if previously_frozen.contains(&sense.kind) {
                            // Already computed on an earlier tick: membership
                            // is frozen, so carry the previous result over
                            // verbatim rather than re-querying the world.
                            if let Some(previous) = previous {
                                entities.extend(
                                    previous
                                        .entities
                                        .iter()
                                        .filter(|entry| entry.sense == sense.kind)
                                        .copied(),
                                );
                                points.extend(
                                    previous
                                        .points
                                        .iter()
                                        .filter(|point| point.sense == sense.kind)
                                        .copied(),
                                );
                            }
                            continue;
                        }

                        evaluate_sense(
                            &read_data,
                            observer,
                            observer_pos,
                            *sense,
                            &mut entities,
                            &mut points,
                        );
                    },
                    SenseMode::Continuous => {
                        let last_eval = bookkeeping.continuous_last_eval.get(&sense.kind).copied();
                        let due = last_eval
                            .is_none_or(|last| run.wrapping_sub(last) >= CONTINUOUS_THROTTLE_RUNS);

                        if due {
                            evaluate_sense(
                                &read_data,
                                observer,
                                observer_pos,
                                *sense,
                                &mut entities,
                                &mut points,
                            );
                            bookkeeping.continuous_last_eval.insert(sense.kind, run);
                        } else if let Some(previous) = previous {
                            // Not due this run: reuse the previous run's
                            // membership for this kind verbatim, exactly the
                            // same carry-forward the snapshot branch above
                            // uses, so a throttled-skip run never flickers or
                            // drops membership.
                            entities.extend(
                                previous
                                    .entities
                                    .iter()
                                    .filter(|entry| entry.sense == sense.kind)
                                    .copied(),
                            );
                            points.extend(
                                previous
                                    .points
                                    .iter()
                                    .filter(|point| point.sense == sense.kind)
                                    .copied(),
                            );
                        }
                    },
                }
            }

            // Drop bookkeeping for continuous kinds no longer declared, so a
            // kind that disappears and later reappears is treated as a fresh
            // first evaluation rather than reading a stale, possibly-recent
            // timestamp.
            bookkeeping.continuous_last_eval.retain(|kind, _| {
                senses
                    .iter()
                    .any(|sense| sense.mode == SenseMode::Continuous && sense.kind == *kind)
            });
            bookkeeping.frozen = frozen;
            snapshots.store(observer, bookkeeping);

            if entities.is_empty() && points.is_empty() {
                if detecteds.contains(observer) {
                    detecteds.remove(observer);
                }
            } else {
                let detected = Detected { entities, points };
                // The storage is flagged, so writing marks the component dirty
                // and re-syncs it to the owning client. A frozen snapshot
                // rebuilds to the exact same value every tick, so only write
                // when the reveal set actually changed.
                if detecteds.get(observer) != Some(&detected) {
                    let _ = detecteds.insert(observer, detected);
                }
            }
        }

        snapshots.prune();
    }
}

/// Collapse duplicate declarations of the same `SenseKind` into one sense.
///
/// Two buffs can declare the same kind (a shorter, stronger detection cast on
/// top of a longer one). Querying twice would put the same target in the reveal
/// set twice, so they merge: the widest radius wins, and a `Continuous`
/// declaration wins over a `Snapshot` one — a sense that is meant to track the
/// world live must not be frozen by a weaker duplicate.
fn merged_senses(senses: &[ActiveSense]) -> Vec<ActiveSense> {
    let mut merged: Vec<ActiveSense> = Vec::with_capacity(senses.len());
    for sense in senses {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.kind == sense.kind)
        {
            existing.radius = existing.radius.max(sense.radius);
            if sense.mode == SenseMode::Continuous {
                existing.mode = SenseMode::Continuous;
            }
        } else {
            merged.push(*sense);
        }
    }
    merged
}

/// Run one sense's spatial query and append whatever it reveals.
///
/// Candidates come from the cached spatial grid — never a full-world join. The
/// grid answers with the entities in the *bounding box* of the circle, so an
/// exact radius check still has to be applied per candidate.
///
/// `SenseKind::True` shares this exact spatial query (same grid, same radius
/// and distance check) with every other sense kind, and is judged by the same
/// [`reveals`] dispatcher below — including its anti-divination early-out.
fn evaluate_sense(
    read_data: &ReadData,
    observer: Entity,
    observer_pos: &Pos,
    sense: ActiveSense,
    entities: &mut Vec<DetectedEntity>,
    points: &mut Vec<DetectedPoint>,
) {
    let radius_sqr = sense.radius * sense.radius;
    // `Path` reveals a place rather than a set of things, so it resolves to the
    // single closest candidate instead of appending every match.
    let mut nearest_point: Option<(f32, Vec3<f32>)> = None;

    read_data
        .cached_spatial_grid
        .0
        .in_circle_aabr(observer_pos.0.xy(), sense.radius)
        .for_each(|target| {
            if target == observer || !read_data.entities.is_alive(target) {
                return;
            }
            let Some(target_pos) = read_data.positions.get(target) else {
                return;
            };
            let dist_sqr = target_pos.0.distance_squared(observer_pos.0);
            if dist_sqr > radius_sqr {
                return;
            }

            if sense.kind == SenseKind::Path {
                // A revealed route resolves to the nearest waypoint: the
                // engine's existing marker for "a place a traveller can head
                // for". Points are `Vec3<f32>` and render through the in-world
                // label surface, deliberately not through the 2-D map-pin one.
                if read_data.waypoint_areas.contains(target)
                    && nearest_point.is_none_or(|(nearest, _)| dist_sqr < nearest)
                {
                    nearest_point = Some((dist_sqr, target_pos.0));
                }
                return;
            }

            let revealed = reveals(read_data, target, sense.kind);

            if revealed && let Some(uid) = read_data.uids.get(target) {
                entities.push(DetectedEntity {
                    uid: *uid,
                    sense: sense.kind,
                    // Single-target inspect permissions are granted by the
                    // single-target cast path, never by an area sense.
                    detail: None,
                });
            }
        });

    if let Some((_, pos)) = nearest_point {
        points.push(DetectedPoint {
            pos,
            sense: SenseKind::Path,
        });
    }
}

/// The per-`SenseKind` predicate: does this sense reveal this target?
fn reveals(read_data: &ReadData, target: Entity, kind: SenseKind) -> bool {
    // Anti-divination early-out. `Stats.nondetection` is the stronger,
    // unconditional claim ("can't be targeted by divination" regardless of
    // which sense is queried or what `false_aura` below says), so it is
    // checked first and short-circuits every other sense. `Stats.false_aura`
    // is a narrower lie: this target registers as revealed by exactly one
    // chosen `SenseKind`, regardless of whether it actually matches that
    // sense's real predicate below.
    if let Some(stats) = read_data.stats.get(target) {
        if stats.nondetection {
            return false;
        }
        if stats.false_aura == Some(kind) {
            return true;
        }
    }
    match kind {
        SenseKind::Magic => is_magical(read_data, target),
        SenseKind::Affliction => read_data
            .buffs
            .get(target)
            .is_some_and(|buffs| buffs.contains_any(AFFLICTION_BUFFS)),
        // Sapient minds, which is the creature *kind* `Humanoid` and not the
        // body *shape* `Body::Humanoid`: a small biped person has a mind
        // without sharing the playable-race skeleton.
        SenseKind::Thought => is_creature_kind(read_data, target, |kind| {
            matches!(kind, CreatureKind::Humanoid)
        }),
        // Portals are entities carrying an `Object::Portal`, not terrain
        // blocks — the block/sprite highlight surface cannot express a set.
        SenseKind::Portal => matches!(read_data.objects.get(target), Some(Object::Portal { .. })),
        // Anything the bestiary classifies as a creature at all. Narrowing this
        // to one *named* creature needs a target descriptor on the sense
        // declaration, which the declaration does not carry today.
        SenseKind::Creature => is_creature_kind(read_data, target, |_| true),
        SenseKind::Fauna => is_creature_kind(read_data, target, |kind| {
            matches!(kind, CreatureKind::Beast)
        }),
        SenseKind::Flora => is_creature_kind(read_data, target, |kind| {
            matches!(kind, CreatureKind::Plant)
        }),
        // Any dropped item stack. Narrowing this to one item definition needs
        // the same descriptor `Creature` above is missing.
        SenseKind::Object => read_data.pickup_items.contains(target),
        // The living half of a wide survey of the surroundings. The terrain
        // half (water, ground) is not an entity reveal and so is not expressed
        // here.
        SenseKind::Nature => is_creature_kind(read_data, target, |kind| {
            matches!(kind, CreatureKind::Beast | CreatureKind::Plant)
        }),
        // TODO: evil/good detection predicate, needs a fiend-classification
        // helper that doesn't exist in the engine yet. It resolves to
        // fiends ∪ undead once that helper lands; until then this sense
        // deliberately reveals nothing rather than guessing at a subset.
        SenseKind::Aberrant => false,
        // A phantasm has no separate "real form" hiding underneath it the
        // way a disguised entity does; being caught by this arm at all is
        // itself the reveal — it tells the observer the entity in front of
        // them is not what it appears to be. Shares the same anti-divination
        // early-out above as every other sense: a `nondetection`-protected
        // disguise/phantasm stays hidden even from True Sight, matching
        // `Stats.nondetection`'s own "stronger, unconditional claim" doc.
        SenseKind::True => {
            read_data.disguises.contains(target) || read_data.phantom_illusions.contains(target)
        },
        // Handled before the predicate runs — points, not entities.
        SenseKind::Path => false,
    }
}

fn is_creature_kind(
    read_data: &ReadData,
    target: Entity,
    matches_kind: impl Fn(CreatureKind) -> bool,
) -> bool {
    read_data
        .bodies
        .get(target)
        .and_then(Body::creature_kind)
        .is_some_and(matches_kind)
}

/// A target radiates magic if a magical effect is currently running on it, or
/// if it is a dropped item that carries magic of its own.
///
/// The item half keys off the engine's existing data-driven "this item's power
/// is magical" tag rather than resolving the item's ability metadata: that
/// resolution needs an ability-map lookup per item, which does not belong
/// inside a per-tick spatial query.
fn is_magical(read_data: &ReadData, target: Entity) -> bool {
    read_data.buffs.get(target).is_some_and(|buffs| {
        buffs
            .iter_active()
            .flatten()
            .any(|buff| buff.cat_ids.contains(&BuffCategory::Magical))
    }) || read_data
        .pickup_items
        .get(target)
        .is_some_and(|pickup| pickup.item().requires_attunement())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{
            Body,
            buff::{Buff, BuffData, BuffSource, DestInfo},
            humanoid, object, quadruped_small,
        },
        resources::{ProgramTime, Time},
        uid::{IdMaps, Uid},
        util::SpatialGrid,
    };
    use specs::{Builder, WorldExt};
    use std::num::NonZeroU64;

    const RADIUS: f32 = 20.0;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Stats>();
        world.register::<Uid>();
        world.register::<Body>();
        world.register::<Buffs>();
        world.register::<Object>();
        world.register::<PickupItem>();
        world.register::<WaypointArea>();
        world.register::<Disguise>();
        world.register::<PhantomIllusion>();
        world.register::<Detected>();
        world.insert(IdMaps::default());
        world.insert(CachedSpatialGrid::default());
        world.insert(DetectionSnapshots::default());
        // `common_ecs::run_now`'s `Job<T>` wrapper records CPU-time metrics
        // per system and expects this resource to already exist.
        world.insert(common_ecs::SysMetrics::default());
        world
    }

    /// The spatial grid is normally filled by the physics system, which does
    /// not run in these tests, so mirror what it would have inserted.
    fn rebuild_spatial_grid(world: &specs::World) {
        let mut grid = SpatialGrid::new(5, 6, 8);
        for (entity, pos) in (&world.entities(), &world.read_storage::<Pos>()).join() {
            grid.insert(pos.0.xy().map(|e| e as i32), 1, entity);
        }
        *world.write_resource::<CachedSpatialGrid>() = CachedSpatialGrid(grid);
    }

    /// An observer at the origin with one active sense of `kind`.
    fn spawn_observer(world: &mut specs::World, kind: SenseKind, mode: SenseMode) -> Entity {
        let mut stats = Stats::empty(Body::Humanoid(humanoid::Body::random()));
        stats.senses = vec![ActiveSense {
            kind,
            radius: RADIUS,
            mode,
        }];
        world
            .create_entity()
            .with(Pos(Vec3::zero()))
            .with(stats)
            .with(uid(1))
            .build()
    }

    fn spawn_target(world: &mut specs::World, n: u64, distance: f32) -> specs::EntityBuilder<'_> {
        world
            .create_entity()
            .with(Pos(Vec3::new(distance, 0.0, 0.0)))
            .with(uid(n))
    }

    fn detected_uids(world: &specs::World, observer: Entity) -> Vec<Uid> {
        world
            .read_storage::<Detected>()
            .get(observer)
            .map(|detected| detected.entities.iter().map(|entry| entry.uid).collect())
            .unwrap_or_default()
    }

    fn detected_points(world: &specs::World, observer: Entity) -> Vec<Vec3<f32>> {
        world
            .read_storage::<Detected>()
            .get(observer)
            .map(|detected| detected.points.iter().map(|point| point.pos).collect())
            .unwrap_or_default()
    }

    fn buff_with_categories(kind: BuffKind, cat_ids: Vec<BuffCategory>) -> Buff {
        Buff::new(
            kind,
            BuffData::new(1.0, None),
            cat_ids,
            BuffSource::World,
            Time(0.0),
            DestInfo::default(),
            None,
            None,
            None,
        )
    }

    fn buffs_with(kind: BuffKind, cat_ids: Vec<BuffCategory>) -> Buffs {
        let mut buffs = Buffs::default();
        buffs.insert(buff_with_categories(kind, cat_ids), Time(0.0));
        buffs
    }

    fn beast_body() -> Body {
        Body::QuadrupedSmall(quadruped_small::Body::random_with(
            &mut rand::rng(),
            &quadruped_small::Species::Fox,
        ))
    }

    fn plant_body() -> Body {
        Body::QuadrupedSmall(quadruped_small::Body::random_with(
            &mut rand::rng(),
            &quadruped_small::Species::Fungome,
        ))
    }

    fn pickup(item_id: &str) -> PickupItem {
        PickupItem::new(
            common::comp::Item::new_from_asset_expect(item_id),
            ProgramTime(0.0),
            true,
        )
    }

    /// Runs the system and asserts exactly which of the two spawned targets the
    /// sense revealed. `hit`/`miss` are the uids of the target that must be
    /// revealed and the one that must not.
    fn assert_reveals_only(world: &specs::World, observer: Entity, hit: Uid, miss: Uid) {
        rebuild_spatial_grid(world);
        common_ecs::run_now::<Sys>(world);
        let revealed = detected_uids(world, observer);
        assert!(
            revealed.contains(&hit),
            "the sense must reveal its own target, got {revealed:?}"
        );
        assert!(
            !revealed.contains(&miss),
            "the sense must not reveal an unrelated entity, got {revealed:?}"
        );
    }

    #[test]
    fn magic_sense_reveals_magically_buffed_entities_only() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Magic, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(buffs_with(BuffKind::Frenzied, vec![BuffCategory::Magical]))
            .build();
        spawn_target(&mut world, 3, 6.0)
            .with(buffs_with(BuffKind::Frenzied, vec![BuffCategory::Natural]))
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn magic_sense_reveals_a_dropped_item_that_carries_magic() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Magic, SenseMode::Snapshot);
        // A mundane dropped item must stay dark even though it is an item.
        spawn_target(&mut world, 3, 6.0)
            .with(pickup("common.items.crafting_ing.twigs"))
            .build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(
            detected_uids(&world, observer).is_empty(),
            "a mundane dropped item is not magical"
        );
    }

    #[test]
    fn affliction_sense_reveals_poisoned_entities_only() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Affliction, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(buffs_with(BuffKind::Poisoned, Vec::new()))
            .build();
        spawn_target(&mut world, 3, 6.0)
            .with(buffs_with(BuffKind::Frenzied, Vec::new()))
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn thought_sense_reveals_minds_and_not_animals() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Thought, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(Body::Humanoid(humanoid::Body::random()))
            .build();
        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn portal_sense_reveals_portal_entities_only() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Portal, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(Object::Portal {
                target: Vec3::zero(),
                requires_no_aggro: false,
                buildup_time: common::resources::Secs(1.0),
            })
            .build();
        spawn_target(&mut world, 3, 6.0)
            .with(Object::DeleteAfter {
                spawned_at: Time(0.0),
                timeout: std::time::Duration::from_secs(1),
            })
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn creature_sense_reveals_creatures_and_not_inert_objects() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();
        spawn_target(&mut world, 3, 6.0)
            .with(Body::Object(object::Body::Arrow))
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn fauna_sense_reveals_beasts_and_not_plants() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Fauna, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();
        spawn_target(&mut world, 3, 6.0).with(plant_body()).build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn flora_sense_reveals_plants_and_not_beasts() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Flora, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(plant_body()).build();
        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn object_sense_reveals_dropped_items_and_not_creatures() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Object, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(pickup("common.items.crafting_ing.twigs"))
            .build();
        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn nature_sense_reveals_living_things_and_not_inert_objects() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Nature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(plant_body()).build();
        spawn_target(&mut world, 3, 6.0)
            .with(Body::Object(object::Body::Arrow))
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn nature_sense_covers_the_beast_half_too() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Nature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();
        spawn_target(&mut world, 3, 6.0)
            .with(Body::Object(object::Body::Arrow))
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn a_sense_ignores_targets_outside_its_radius() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, RADIUS - 1.0)
            .with(beast_body())
            .build();
        spawn_target(&mut world, 3, RADIUS + 5.0)
            .with(beast_body())
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn path_sense_produces_a_point_at_the_nearest_waypoint() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Path, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 12.0)
            .with(WaypointArea::default())
            .build();
        spawn_target(&mut world, 3, 4.0)
            .with(WaypointArea::default())
            .build();
        // A non-waypoint entity must not become a path point.
        spawn_target(&mut world, 4, 2.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);

        assert_eq!(detected_points(&world, observer), vec![Vec3::new(
            4.0, 0.0, 0.0
        )]);
        assert!(
            detected_uids(&world, observer).is_empty(),
            "a path sense reveals a place, never entities"
        );
    }

    #[test]
    fn the_blocked_aberrant_sense_reveals_nothing_yet() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Aberrant, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(detected_uids(&world, observer).is_empty());
    }

    // ───────────────────────── True Sight reveal targets ────────────────────

    fn disguise_component() -> Disguise {
        Disguise {
            apparent_body: beast_body(),
            apparent_name: None,
            caster: uid(99),
            cast_accuracy: 0.5,
        }
    }

    #[test]
    fn true_sight_reveals_a_disguised_entity_and_a_phantasm() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::True, SenseMode::Continuous);
        spawn_target(&mut world, 2, 5.0)
            .with(disguise_component())
            .build();
        spawn_target(&mut world, 3, 6.0)
            .with(PhantomIllusion)
            .build();
        // An ordinary, undisguised entity must not show up in the True
        // Sight reveal set — being caught by neither `Disguise` nor
        // `PhantomIllusion` is the "nothing to see through" case.
        spawn_target(&mut world, 4, 7.0).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        let mut revealed = detected_uids(&world, observer);
        revealed.sort_by_key(|uid| uid.0);
        assert_eq!(revealed, vec![uid(2), uid(3)]);
        for entry in &world
            .read_storage::<Detected>()
            .get(observer)
            .unwrap()
            .entities
        {
            assert_eq!(
                entry.sense,
                SenseKind::True,
                "a True Sight reveal must always be tagged SenseKind::True"
            );
        }
    }

    #[test]
    fn an_observer_without_true_sight_does_not_see_disguises_or_phantasms() {
        let mut world = setup_world();
        // A completely unrelated sense: neither Disguise nor PhantomIllusion
        // is anywhere in its own predicate, so this doubles as a check that
        // ordinary senses never accidentally pick up an illusory target.
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0)
            .with(disguise_component())
            .build();
        spawn_target(&mut world, 3, 6.0)
            .with(PhantomIllusion)
            .build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(
            detected_uids(&world, observer).is_empty(),
            "an observer with no True Sight sense must never see through a disguise or phantasm"
        );
    }

    #[test]
    fn nondetection_hides_a_disguise_even_from_true_sight() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::True, SenseMode::Continuous);

        // An ordinary disguise: True Sight's own predicate would reveal it.
        spawn_target(&mut world, 2, 5.0)
            .with(disguise_component())
            .build();

        // Same disguise, but the wearer also carries nondetection -- the
        // stronger, unconditional anti-divination claim must win even
        // against True Sight, exactly as it does for every other sense.
        let mut hidden_stats = Stats::empty(beast_body());
        hidden_stats.nondetection = true;
        spawn_target(&mut world, 3, 6.0)
            .with(disguise_component())
            .with(hidden_stats)
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn true_sight_ignores_illusions_outside_its_radius() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::True, SenseMode::Continuous);
        spawn_target(&mut world, 2, RADIUS - 1.0)
            .with(disguise_component())
            .build();
        spawn_target(&mut world, 3, RADIUS + 5.0)
            .with(PhantomIllusion)
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    // ─────────────────────────── snapshot lifecycle ─────────────────────────

    #[test]
    fn a_snapshot_freezes_its_membership_for_the_life_of_the_sense() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(detected_uids(&world, observer), vec![uid(2)]);

        // A second creature walks into range after the snapshot was taken. A
        // frozen sense must not pick it up.
        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();
        world.maintain();
        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(
            detected_uids(&world, observer),
            vec![uid(2)],
            "a frozen snapshot must not grow"
        );
    }

    #[test]
    fn an_empty_snapshot_stays_empty_even_when_a_target_arrives() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(detected_uids(&world, observer).is_empty());

        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();
        world.maintain();
        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(
            detected_uids(&world, observer).is_empty(),
            "an empty result is still a taken snapshot, not a pending one"
        );
    }

    #[test]
    fn a_continuous_sense_tracks_the_world_instead_of_freezing() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Continuous);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        // The very first run always evaluates immediately -- there is no
        // previous timestamp to throttle against, so a freshly-cast
        // continuous sense is never delayed by up to a full throttle window.
        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(detected_uids(&world, observer), vec![uid(2)]);

        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();
        world.maintain();
        rebuild_spatial_grid(&world);

        // The throttle window (`CONTINUOUS_THROTTLE_RUNS`) hasn't elapsed
        // yet, so this run reuses the previous membership verbatim -- the
        // new target is not dropped from view, but it also isn't picked up
        // yet.
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(
            detected_uids(&world, observer),
            vec![uid(2)],
            "a throttled-skip run must not flicker or drop existing membership"
        );

        // Advance far enough into the throttle window for a real re-query to
        // become due again.
        for _ in 0..CONTINUOUS_THROTTLE_RUNS {
            common_ecs::run_now::<Sys>(&world);
        }
        let mut revealed = detected_uids(&world, observer);
        revealed.sort_by_key(|uid| uid.0);
        assert_eq!(
            revealed,
            vec![uid(2), uid(3)],
            "once the throttle is due again, the reveal set catches up to the world"
        );
    }

    #[test]
    fn a_continuous_sense_does_not_flicker_when_a_member_leaves_range_mid_window() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Continuous);
        let target = spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(detected_uids(&world, observer), vec![uid(2)]);

        // Move the target out of range before the throttle window elapses
        // again.
        world.write_storage::<Pos>().get_mut(target).unwrap().0 = Vec3::new(RADIUS + 5.0, 0.0, 0.0);
        rebuild_spatial_grid(&world);

        // Every run inside the throttle window must keep showing the stale
        // (but not yet re-queried) membership -- no flicker, no premature
        // drop, even though the real world has already changed underneath.
        for _ in 0..(CONTINUOUS_THROTTLE_RUNS - 1) {
            common_ecs::run_now::<Sys>(&world);
            assert_eq!(
                detected_uids(&world, observer),
                vec![uid(2)],
                "membership must not drop before the throttle is due"
            );
        }

        // The throttle is now due: the re-query reflects the target having
        // actually left range.
        common_ecs::run_now::<Sys>(&world);
        assert!(
            detected_uids(&world, observer).is_empty(),
            "once due, the re-query must reflect the target's real position"
        );
    }

    #[test]
    fn ending_the_sense_drops_the_reveal_set_and_removes_the_component() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(world.read_storage::<Detected>().get(observer).is_some());

        // The buff expired: the buff system rebuilds `Stats` every tick, so the
        // declaration simply disappears.
        world
            .write_storage::<Stats>()
            .get_mut(observer)
            .unwrap()
            .senses
            .clear();

        common_ecs::run_now::<Sys>(&world);
        assert!(
            world.read_storage::<Detected>().get(observer).is_none(),
            "an observer with no active sense must carry no reveal set at all"
        );
    }

    #[test]
    fn recasting_after_expiry_takes_a_fresh_snapshot() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(detected_uids(&world, observer), vec![uid(2)]);

        // Expire the sense, then let a second creature arrive while it is down.
        world
            .write_storage::<Stats>()
            .get_mut(observer)
            .unwrap()
            .senses
            .clear();
        common_ecs::run_now::<Sys>(&world);

        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();
        world.maintain();
        world
            .write_storage::<Stats>()
            .get_mut(observer)
            .unwrap()
            .senses = vec![ActiveSense {
            kind: SenseKind::Creature,
            radius: RADIUS,
            mode: SenseMode::Snapshot,
        }];

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        let mut revealed = detected_uids(&world, observer);
        revealed.sort_by_key(|uid| uid.0);
        assert_eq!(
            revealed,
            vec![uid(2), uid(3)],
            "a re-cast sense must snapshot the world as it is now"
        );
    }

    #[test]
    fn duplicate_declarations_of_one_kind_reveal_a_target_once() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);
        world
            .write_storage::<Stats>()
            .get_mut(observer)
            .unwrap()
            .senses
            .push(ActiveSense {
                kind: SenseKind::Creature,
                radius: RADIUS * 2.0,
                mode: SenseMode::Snapshot,
            });
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert_eq!(detected_uids(&world, observer), vec![uid(2)]);
    }

    #[test]
    fn nondetection_hides_a_target_from_a_sense_that_would_otherwise_reveal_it() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Creature, SenseMode::Snapshot);

        // An ordinary creature: the sense's own predicate would reveal it.
        spawn_target(&mut world, 2, 5.0).with(beast_body()).build();

        // Same predicate match, but flagged nondetection -- must be skipped
        // regardless of the sense queried.
        let mut hidden_stats = Stats::empty(beast_body());
        hidden_stats.nondetection = true;
        spawn_target(&mut world, 3, 6.0)
            .with(beast_body())
            .with(hidden_stats)
            .build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn false_aura_reveals_a_target_the_real_predicate_would_have_missed() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Magic, SenseMode::Snapshot);

        // Not magically buffed at all -- the real Magic predicate misses it.
        // A false_aura for the queried kind must reveal it anyway.
        let mut lying_stats = Stats::empty(beast_body());
        lying_stats.false_aura = Some(SenseKind::Magic);
        spawn_target(&mut world, 2, 5.0)
            .with(beast_body())
            .with(lying_stats)
            .build();

        // Same non-magical target, no false_aura -- must stay unrevealed.
        spawn_target(&mut world, 3, 6.0).with(beast_body()).build();

        assert_reveals_only(&world, observer, uid(2), uid(3));
    }

    #[test]
    fn nondetection_overrides_a_false_aura_claiming_the_same_sense() {
        let mut world = setup_world();
        let observer = spawn_observer(&mut world, SenseKind::Magic, SenseMode::Snapshot);

        // Both flags set: nondetection is the stronger, unconditional claim
        // and must win even though false_aura alone would reveal this target
        // for the exact same sense.
        let mut stats = Stats::empty(beast_body());
        stats.nondetection = true;
        stats.false_aura = Some(SenseKind::Magic);
        spawn_target(&mut world, 2, 5.0)
            .with(beast_body())
            .with(stats)
            .build();

        rebuild_spatial_grid(&world);
        common_ecs::run_now::<Sys>(&world);
        assert!(
            !detected_uids(&world, observer).contains(&uid(2)),
            "nondetection must win over a same-sense false_aura"
        );
    }
}
