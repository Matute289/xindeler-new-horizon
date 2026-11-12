use common::{
    comp::{
        AttunedItems, Attuning, Inventory, Presence, SkillSet, item::ItemTag, reconcile_attunement,
    },
    resources::Time,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Join, Read, ReadStorage, WriteStorage};

/// Auto-attune-on-equip wiring (ENG-D2b). Each tick this observes every
/// player's equipped loadout and drives the attunement state machine in
/// [`reconcile_attunement`]: equipping a `RequiresAttunement` item starts a timed
/// channel, the channel attunes the slot once the wearer is under the level
/// cap, and unequipping clears it instantly.
///
/// Gated to entities with a `Presence` (connected players) — attunement is a
/// player build mechanic, and there are few players, so the per-tick scan is
/// cheap. NPCs are skipped entirely. The per-item check uses the non-allocating
/// `Item::has_tag`, and idle players (no attunement item and no live state) are
/// skipped before any allocation.
#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Read<'a, Time>,
        ReadStorage<'a, Presence>,
        ReadStorage<'a, Inventory>,
        ReadStorage<'a, SkillSet>,
        WriteStorage<'a, AttunedItems>,
        WriteStorage<'a, Attuning>,
    );

    const NAME: &'static str = "attunement";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (entities, time, presences, inventories, skill_sets, mut attuned_items, mut attuning): Self::SystemData,
    ) {
        let now = *time;
        for (entity, _presence, inventory, skill_set) in
            (&entities, &presences, &inventories, &skill_sets).join()
        {
            // Cheap early-out: nothing to do for a player with no attunement
            // item equipped and no attunement state in flight (the
            // common case).
            let has_item = inventory
                .equipped_items_with_slot()
                .any(|(_, item)| item.has_tag(&ItemTag::RequiresAttunement));
            let has_state = attuned_items.get(entity).is_some_and(|a| a.count() > 0)
                || attuning.get(entity).is_some_and(|a| !a.0.is_empty());
            if !has_item && !has_state {
                continue;
            }

            let equipped: Vec<_> = inventory
                .equipped_items_with_slot()
                .map(|(slot, item)| {
                    (
                        slot,
                        item.has_tag(&ItemTag::RequiresAttunement),
                        item.quality(),
                    )
                })
                .collect();
            let level = skill_set.character_level();

            // Work on snapshots (tiny vecs) and only write back on an actual
            // change — touching the flagged storage would re-sync every tick.
            let mut attuned = attuned_items.get(entity).cloned().unwrap_or_default();
            let mut channels = attuning.get(entity).cloned().unwrap_or_default();
            reconcile_attunement(&equipped, level, now, &mut channels, &mut attuned);

            if attuned_items.get(entity) != Some(&attuned) {
                let _ = attuned_items.insert(entity, attuned);
            }
            if attuning.get(entity) != Some(&channels) {
                let _ = attuning.insert(entity, channels);
            }
        }
    }
}
