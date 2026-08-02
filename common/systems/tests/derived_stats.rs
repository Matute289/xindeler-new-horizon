//! Integration tests for the `DerivedStats` rebuild system.
//!
//! These drive the real ECS dispatch (`add_local_systems` on a real `State`)
//! rather than calling the system directly, because what is under test is not
//! the arithmetic — `common/src/comp/derived_stats.rs` pins that against the
//! free functions it replaces — but the *invalidation*: which entities get
//! recomputed, when, and how often.
//!
//! The central claim, and the reason the component exists at all, is
//! `cache_is_not_rebuilt_when_nothing_changes`.

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            AttunedItems, Body, DerivedStats, DerivedStatsTrackers, Energy, Health, Inventory,
            Poise, SkillSet, Stats, biped_large,
            inventory::{
                item::{
                    AbilityMap, Item, ItemBase, ItemDef, ItemKind, ItemTag, MaterialStatManifest,
                    armor::{self, Armor, ArmorKind, Protection},
                },
                loadout_builder::LoadoutBuilder,
                slot::{ArmorSlot, EquipSlot},
            },
            skillset::SkillGroupKind,
        },
        resources::{GameMode, Time},
        shared_server_config::ServerConstants,
        skillset_builder::SkillSetBuilder,
        terrain::{MapSizeLg, TerrainChunk},
    };
    use common_net::sync::WorldSyncExt;
    use specs::{Builder, Entity, WorldExt};
    use std::{sync::Arc, time::Duration};
    use vek::Vec2;
    use xindeler_common_systems::add_local_systems;

    const DEFAULT_WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    fn setup() -> common_state::State {
        let pools = common_state::State::pools(GameMode::Server);
        let mut state = common_state::State::new(
            GameMode::Server,
            pools,
            DEFAULT_WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        state
            .ecs_mut()
            .insert(MaterialStatManifest::load().cloned());
        state.ecs_mut().insert(AbilityMap::load().cloned());
        state
    }

    fn tick(state: &mut common_state::State) {
        state.tick(
            Duration::from_millis(16),
            false,
            None,
            &ServerConstants {
                day_cycle_coefficient: 24.0,
                oracle_live: false,
            },
            |_, _| {},
        );
    }

    fn humanoid_body() -> Body {
        use rand::{SeedableRng, rngs::SmallRng};
        Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut SmallRng::seed_from_u64(0),
            &common::comp::humanoid::Species::Human,
        ))
    }

    /// A body with a `combat_multiplier` that is deliberately NOT 1.0, so a
    /// body swap visibly moves `combat_rating`.
    fn mindflayer_body() -> Body {
        use rand::{SeedableRng, rngs::SmallRng};
        Body::BipedLarge(biped_large::Body::random_with(
            &mut SmallRng::seed_from_u64(0),
            &biped_large::Species::Mindflayer,
        ))
    }

    fn item_from_kind(kind: ItemKind, requires_attunement: bool) -> Item {
        let mut item_def = ItemDef::create_test_itemdef_from_kind(kind);
        if requires_attunement {
            item_def.tags = vec![ItemTag::RequiresAttunement];
        }
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(item_def)),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        )
    }

    const CHEST_PROTECTION: f32 = 12.5;

    fn chest_armor(requires_attunement: bool) -> Item {
        item_from_kind(
            ItemKind::Armor(Armor::new(
                ArmorKind::Chest,
                armor::StatsSource::Direct(armor::Stats {
                    protection: Some(Protection::Normal(CHEST_PROTECTION)),
                    poise_resilience: Some(Protection::Normal(7.25)),
                    energy_max: Some(13.0),
                    energy_reward: Some(0.35),
                    precision_power: Some(0.17),
                    stealth: Some(0.9),
                    ground_contact: Default::default(),
                }),
            )),
            requires_attunement,
        )
    }

    fn create_entity(state: &mut common_state::State, inventory: Inventory) -> Entity {
        let body = humanoid_body();
        state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(SkillSetBuilder::default().build())
            .with(inventory)
            .build()
    }

    fn empty_loadout() -> Inventory {
        Inventory::with_loadout_humanoid(LoadoutBuilder::empty().build())
    }

    fn chest_loadout(requires_attunement: bool) -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(chest_armor(requires_attunement)))
                .build(),
        )
    }

    fn rebuilds(state: &common_state::State) -> u64 {
        state
            .ecs()
            .read_resource::<DerivedStatsTrackers>()
            .rebuilds()
    }

    fn derived_of(state: &common_state::State, entity: Entity) -> DerivedStats {
        state
            .ecs()
            .read_storage::<DerivedStats>()
            .get(entity)
            .cloned()
            .expect("entity should have a DerivedStats cache")
    }

    fn now(state: &common_state::State) -> Time { *state.ecs().read_resource::<Time>() }

    /// (1) An entity that has just been created gets its cache built on the
    /// very first tick, without anyone having to ask for it.
    #[test]
    fn cache_is_populated_on_first_tick_for_a_new_entity() {
        let mut state = setup();
        let entity = create_entity(&mut state, chest_loadout(false));

        assert!(
            state
                .ecs()
                .read_storage::<DerivedStats>()
                .get(entity)
                .is_none(),
            "the cache should not exist before the first tick"
        );

        tick(&mut state);

        let derived = derived_of(&state, entity);
        assert_eq!(derived.protection, Some(CHEST_PROTECTION));
        assert!(derived.combat_rating > 0.0);
        assert_eq!(rebuilds(&state), 1);
    }

    /// (2) 🔴 **The load-bearing test.** A steady-state tick — nobody equips,
    /// levels up, attunes or transforms — must rebuild nothing at all. This is
    /// the entire justification for the component: the previous design
    /// recomputed all of these aggregates for every entity on every single
    /// tick (and, on the client, every single frame).
    #[test]
    fn cache_is_not_rebuilt_when_nothing_changes() {
        let mut state = setup();
        let a = create_entity(&mut state, chest_loadout(false));
        let b = create_entity(&mut state, empty_loadout());

        tick(&mut state);
        let after_first = rebuilds(&state);
        assert_eq!(after_first, 2, "both entities are built on the first tick");
        let snapshot_a = derived_of(&state, a);
        let snapshot_b = derived_of(&state, b);

        // Several more ticks, with regen/buff/stat systems all running as
        // normal. None of the four invalidating components is touched by any
        // of them, so the rebuild counter must not move at all.
        for _ in 0..5 {
            tick(&mut state);
            assert_eq!(
                rebuilds(&state),
                after_first,
                "a tick in which nothing changed must rebuild nothing"
            );
        }

        // ...and the cached values are still the right ones, not merely stale
        // leftovers of a cache that stopped being maintained.
        assert_eq!(derived_of(&state, a), snapshot_a);
        assert_eq!(derived_of(&state, b), snapshot_b);
    }

    /// (3) Equipping an item rebuilds the wearer's cache — and only the
    /// wearer's. A change filter that rebuilt the world would be no better
    /// than recomputing unconditionally.
    #[test]
    fn equipping_an_item_rebuilds_exactly_that_entity() {
        let mut state = setup();
        let wearer = create_entity(&mut state, empty_loadout());
        let bystander = create_entity(&mut state, empty_loadout());

        tick(&mut state);
        let baseline = rebuilds(&state);
        let bystander_before = derived_of(&state, bystander);
        assert_eq!(derived_of(&state, wearer).protection, Some(0.0));

        let time = now(&state);
        state
            .ecs()
            .write_storage::<Inventory>()
            .get_mut(wearer)
            .expect("wearer has an inventory")
            .replace_loadout_item(
                EquipSlot::Armor(ArmorSlot::Chest),
                Some(chest_armor(false)),
                time,
            );

        tick(&mut state);

        assert_eq!(
            rebuilds(&state),
            baseline + 1,
            "exactly one entity should have been rebuilt"
        );
        assert_eq!(
            derived_of(&state, wearer).protection,
            Some(CHEST_PROTECTION)
        );
        assert_eq!(
            derived_of(&state, bystander),
            bystander_before,
            "an untouched entity's cache must not change"
        );
    }

    /// (4) `combat_rating` is cached whole, not just its gear half, so a skill
    /// purchase has to invalidate it. This is the correctness proof for
    /// widening the invalidation set to include `SkillSet`.
    #[test]
    fn skill_purchase_rebuilds_combat_rating() {
        let mut state = setup();
        let entity = create_entity(&mut state, chest_loadout(false));

        tick(&mut state);
        let before = derived_of(&state, entity);
        let baseline = rebuilds(&state);

        state
            .ecs()
            .write_storage::<SkillSet>()
            .get_mut(entity)
            .expect("entity has a skillset")
            .grant_skill_point(SkillGroupKind::General);

        tick(&mut state);

        assert_eq!(rebuilds(&state), baseline + 1);
        let after = derived_of(&state, entity);
        assert!(
            after.combat_rating > before.combat_rating,
            "earning a skill point should raise the cached combat rating ({} -> {})",
            before.combat_rating,
            after.combat_rating
        );
        // Nothing gear-derived moved.
        assert_eq!(after.protection, before.protection);
    }

    /// (5) Same story for `Body`: `combat_rating` ends in
    /// `* body.combat_multiplier()`, so a transform must invalidate the cache.
    #[test]
    fn body_change_rebuilds_combat_rating() {
        let mut state = setup();
        let entity = create_entity(&mut state, chest_loadout(false));

        tick(&mut state);
        let before = derived_of(&state, entity);
        let baseline = rebuilds(&state);

        let new_body = mindflayer_body();
        assert_ne!(
            new_body.combat_multiplier(),
            humanoid_body().combat_multiplier(),
            "the fixture must actually change the multiplier"
        );
        *state
            .ecs()
            .write_storage::<Body>()
            .get_mut(entity)
            .expect("entity has a body") = new_body;

        tick(&mut state);

        assert_eq!(rebuilds(&state), baseline + 1);
        let after = derived_of(&state, entity);
        assert!(
            (after.combat_rating
                - before.combat_rating / humanoid_body().combat_multiplier()
                    * new_body.combat_multiplier())
            .abs()
                < 1e-3,
            "the cached combat rating should have followed the new body's multiplier ({} -> {})",
            before.combat_rating,
            after.combat_rating
        );
    }

    /// (6) Attuning gates the *combat-facing* protection but not
    /// `combat_rating`, which is deliberately attunement-blind (it reads the
    /// `_unattuned` variants). Both halves of that have to hold at once.
    #[test]
    fn attuning_an_item_rebuilds_protection_but_not_combat_rating() {
        let mut state = setup();
        let entity = create_entity(&mut state, chest_loadout(true));

        tick(&mut state);
        let before = derived_of(&state, entity);
        let baseline = rebuilds(&state);
        assert_eq!(
            before.protection,
            Some(0.0),
            "an unattuned attunement item must contribute nothing"
        );

        state
            .ecs()
            .write_storage::<AttunedItems>()
            .insert(
                entity,
                AttunedItems(vec![EquipSlot::Armor(ArmorSlot::Chest)]),
            )
            .expect("inserting AttunedItems should succeed");

        tick(&mut state);

        assert_eq!(rebuilds(&state), baseline + 1);
        let after = derived_of(&state, entity);
        assert_eq!(
            after.protection,
            Some(CHEST_PROTECTION),
            "attuning should switch the item's protection on"
        );
        assert_eq!(
            after.protection_unattuned, before.protection_unattuned,
            "the attunement-blind variant must not move"
        );
        assert_eq!(
            after.combat_rating, before.combat_rating,
            "combat_rating is attunement-blind and must not move"
        );
    }

    /// (7) Losing the `Inventory` drops the cache with it, so a read site
    /// falls back to `DerivedStats::default()` — which is exactly the
    /// no-inventory result — instead of reading a stale one.
    #[test]
    fn removing_the_inventory_removes_the_cache() {
        let mut state = setup();
        let entity = create_entity(&mut state, chest_loadout(false));

        tick(&mut state);
        assert_eq!(
            derived_of(&state, entity).protection,
            Some(CHEST_PROTECTION)
        );

        state.ecs().write_storage::<Inventory>().remove(entity);

        tick(&mut state);

        assert!(
            state
                .ecs()
                .read_storage::<DerivedStats>()
                .get(entity)
                .is_none(),
            "the cache must be dropped along with the inventory that fed it"
        );
    }
}
