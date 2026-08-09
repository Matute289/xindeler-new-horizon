//! Integration test for `buff::Sys`'s `ItemCondition` short-circuit reading
//! `DerivedStats::has_item_condition` instead of scanning `equipped_items()`
//! itself every tick.
//!
//! Drives a real `State` tick (`add_local_systems`) end to end: an equipped
//! item declaring a met `ItemCondition` must still result in a real
//! `BuffEvent` being emitted, proving the cached flag correctly gates the
//! same code path the old inline scan used to gate directly.

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            Body, DerivedStats, Energy, Health, Inventory, Poise, Stats,
            buff::{BuffChange, BuffKind, Buffs},
            humanoid,
            inventory::{
                item::{AbilityMap, Item, ItemBase, ItemDef, MaterialStatManifest},
                loadout_builder::LoadoutBuilder,
            },
            item_condition::{ConditionPredicate, ItemCondition},
        },
        event::{BuffEvent, EventBus},
        resources::GameMode,
        shared_server_config::ServerConstants,
        skillset_builder::SkillSetBuilder,
        terrain::{MapSizeLg, TerrainChunk},
    };
    use common_net::sync::WorldSyncExt;
    use specs::{Builder, WorldExt};
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
        // A missing `EventBus<T>` resource makes `event_emitters!` silently
        // no-op every `emit` call for that type, so tests that want to
        // observe emitted events have to install the bus themselves.
        state.ecs_mut().insert(EventBus::<BuffEvent>::default());
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

    fn human_body() -> Body {
        use rand::{SeedableRng, rngs::SmallRng};
        Body::Humanoid(humanoid::Body::random_with(
            &mut SmallRng::seed_from_u64(0),
            &humanoid::Species::Human,
        ))
    }

    /// A mainhand item whose `ItemCondition` grants `Regeneration` to any
    /// Human bearer (met unconditionally by `human_body()`) and nothing
    /// otherwise.
    fn conditioned_item() -> Item {
        let mut item_def =
            ItemDef::create_test_itemdef_from_kind(common::comp::inventory::item::ItemKind::Tool(
                common::comp::inventory::item::Tool::new(
                    common::comp::inventory::item::tool::ToolKind::Sword,
                    common::comp::inventory::item::Hands::One,
                    None,
                    common::comp::inventory::item::tool::Stats::one(),
                ),
            ));
        item_def.condition = Some(ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Human]),
            when_met: vec![BuffKind::Regeneration],
            when_unmet: vec![],
        });
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(item_def)),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        )
    }

    #[test]
    fn a_met_item_condition_still_emits_a_buff_event_through_the_cached_gate() {
        let mut state = setup();
        let body = human_body();
        let inventory = Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .active_mainhand(Some(conditioned_item()))
                .build(),
        );

        let entity = state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(SkillSetBuilder::default().build())
            .with(Buffs::default())
            .with(inventory)
            .build();

        tick(&mut state);

        // The cache must actually be populated and flagged before the
        // gate can be exercised at all.
        let has_condition = state
            .ecs()
            .read_storage::<DerivedStats>()
            .get(entity)
            .expect("a geared entity has a cache after one tick")
            .has_item_condition;
        assert!(
            has_condition,
            "DerivedStats must flag the equipped conditioned item"
        );

        let emitted_regeneration = state
            .ecs()
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .any(|ev| {
                ev.entity == entity
                    && matches!(
                        ev.buff_change,
                        BuffChange::Add(buff) if buff.kind == BuffKind::Regeneration
                    )
            });
        assert!(
            emitted_regeneration,
            "a met ItemCondition must still reach `item_condition_buff_diff` and emit its buff \
             through the has_item_condition-gated path"
        );
    }

    #[test]
    fn an_unconditioned_geared_entity_never_sets_the_flag_and_emits_nothing_item_related() {
        let mut state = setup();
        let body = human_body();
        // Plain sword, no `ItemCondition` at all -- the common case for every
        // item in the game today.
        let plain_item = Item::new_from_item_base(
            ItemBase::Simple(Arc::new(ItemDef::create_test_itemdef_from_kind(
                common::comp::inventory::item::ItemKind::Tool(
                    common::comp::inventory::item::Tool::new(
                        common::comp::inventory::item::tool::ToolKind::Sword,
                        common::comp::inventory::item::Hands::One,
                        None,
                        common::comp::inventory::item::tool::Stats::one(),
                    ),
                ),
            ))),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        );
        let inventory = Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .active_mainhand(Some(plain_item))
                .build(),
        );

        let entity = state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(SkillSetBuilder::default().build())
            .with(Buffs::default())
            .with(inventory)
            .build();

        tick(&mut state);

        let has_condition = state
            .ecs()
            .read_storage::<DerivedStats>()
            .get(entity)
            .expect("a geared entity has a cache after one tick")
            .has_item_condition;
        assert!(!has_condition);

        let saw_regeneration = state
            .ecs()
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .any(|ev| {
                ev.entity == entity
                    && matches!(
                        ev.buff_change,
                        BuffChange::Add(buff) if buff.kind == BuffKind::Regeneration
                    )
            });
        assert!(!saw_regeneration);
    }
}
