//! Rebuilds the [`DerivedStats`] cache, and *only* for the entities that
//! actually need it.
//!
//! `Inventory`, `AttunedItems`, `SkillSet` and `Body` are all
//! `DerefFlaggedStorage`s, so specs already emits a `ComponentEvent` whenever
//! one of them is inserted, mutated or removed. This system drains those four
//! channels once per tick and recomputes the cache for exactly the entities
//! named in them. In a world where nobody swaps gear, levels up or transforms,
//! the rebuild count per tick is **zero** — which is the whole point of the
//! component existing.
//!
//! It runs before `buff::Sys` and `stats::Sys`, both of which consume the
//! cache, so a gear change is visible on the same tick it happened rather than
//! the next one.

use common::comp::{
    AttunedItems, Body, DerivedStats, DerivedStatsTrackers, Energy, Health, Inventory, Poise,
    SkillSet, item::MaterialStatManifest,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{
    BitSet, Entities, Join, ReadExpect, ReadStorage, SystemData, WriteExpect, WriteStorage, shred,
};

#[derive(SystemData)]
pub struct ReadData<'a> {
    entities: Entities<'a>,
    inventories: ReadStorage<'a, Inventory>,
    attuned_items: ReadStorage<'a, AttunedItems>,
    skill_sets: ReadStorage<'a, SkillSet>,
    bodies: ReadStorage<'a, Body>,
    healths: ReadStorage<'a, Health>,
    energies: ReadStorage<'a, Energy>,
    poises: ReadStorage<'a, Poise>,
    /// The same fetch shape `stats::Sys` already uses. On the client this
    /// resource arrives over the wire and is inserted at connect; on the
    /// server it is a startup clone. It never changes at runtime on either
    /// side, so a cached value derived from it can never go stale under a
    /// running session.
    msm: ReadExpect<'a, MaterialStatManifest>,
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        ReadData<'a>,
        // `WriteExpect`, never `Write`: `DerivedStatsTrackers` has no `Default`
        // precisely so that a world which forgot to build it panics loudly at
        // startup instead of silently registering no readers and leaving the
        // cache permanently stale.
        WriteExpect<'a, DerivedStatsTrackers>,
        WriteStorage<'a, DerivedStats>,
    );

    const NAME: &'static str = "derived_stats";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(_job: &mut Job<Self>, (read_data, mut trackers, mut derived_stats): Self::SystemData) {
        trackers.record_changes(
            &read_data.inventories,
            &read_data.attuned_items,
            &read_data.skill_sets,
            &read_data.bodies,
        );

        // `BitSet::new()` and the joins below do not allocate while the set
        // stays empty, so a tick in which nothing changed costs a channel read
        // and two empty bitset intersections.
        let mut targets = BitSet::new();
        for (entity, _) in (&read_data.entities, trackers.dirty()).join() {
            targets.add(entity.id());
        }
        // The entity-creation case, and the only safety net against an entity
        // that somehow holds gear without a cache: an `Inventory` with no
        // `DerivedStats` is always rebuilt. Once every geared entity has one,
        // this join yields nothing.
        for (entity, _, _) in (&read_data.entities, &read_data.inventories, !&derived_stats).join()
        {
            targets.add(entity.id());
        }

        for (entity, _) in (&read_data.entities, &targets).join() {
            let Some(inventory) = read_data.inventories.get(entity) else {
                // No inventory (never had one, or just lost it) means no cache:
                // every read site falls back to `DerivedStats::default()`, which
                // is exactly the no-inventory result.
                derived_stats.remove(entity);
                continue;
            };

            let derived = DerivedStats::compute(
                Some(inventory),
                read_data.attuned_items.get(entity),
                read_data.skill_sets.get(entity),
                read_data.bodies.get(entity).copied(),
                read_data.healths.get(entity).map(|h| h.base_max()),
                read_data.energies.get(entity).map(|e| e.base_max()),
                read_data.poises.get(entity).map(|p| p.base_max()),
                &read_data.msm,
            );
            let _ = derived_stats.insert(entity, derived);
            trackers.note_rebuild();
        }
    }
}
