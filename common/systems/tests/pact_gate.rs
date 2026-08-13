//! End-to-end behaviour of the Warlock pact-severed magic gate, driven
//! through a real `buff::Sys` tick.
//!
//! Mirrors the antimagic-buff tests in `gear_stat_consumers.rs`: the thing
//! worth a harness for is that `Pact { standing: Severed, .. }` sets
//! `Stats.disable_magic` exactly like `BuffKind::Antimagic` does, without
//! going through a buff at all, and that a `Bound` (or absent) pact never
//! does.

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            Body, Energy, Health, Inventory, Pact, PactStanding, Poise, Stats, buff::Buffs,
            inventory::item::MaterialStatManifest, tool::AbilityMap,
        },
        resources::GameMode,
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

    fn create_entity(state: &mut common_state::State, pact: Option<Pact>) -> Entity {
        let body = humanoid_body();
        let mut builder = state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(Buffs::default())
            .with(SkillSetBuilder::default().build())
            .with(Inventory::with_empty());
        if let Some(pact) = pact {
            builder = builder.with(pact);
        }
        builder.build()
    }

    fn disable_magic(state: &common_state::State, entity: Entity) -> bool {
        state
            .ecs()
            .read_storage::<Stats>()
            .get(entity)
            .expect("entity has stats")
            .disable_magic
    }

    #[test]
    fn severed_pact_sets_disable_magic() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            Some(Pact {
                standing: PactStanding::Severed,
                patron: None,
                boon: None,
                blade_summoned: false,
                favour: 0,
            }),
        );

        tick(&mut state);

        assert!(disable_magic(&state, entity));
    }

    #[test]
    fn bound_pact_does_not_set_disable_magic() {
        let mut state = setup();
        let entity = create_entity(&mut state, Some(Pact::default()));

        tick(&mut state);

        assert!(!disable_magic(&state, entity));
    }

    /// No `Pact` component at all (any non-Warlock, or a save from before
    /// this component existed) must read exactly like `Bound` -- never
    /// severed by default.
    #[test]
    fn no_pact_component_does_not_set_disable_magic() {
        let mut state = setup();
        let entity = create_entity(&mut state, None);

        tick(&mut state);

        assert!(!disable_magic(&state, entity));
    }

    /// Once re-bound, magic works again -- the gate tracks live state every
    /// tick, it isn't a one-way latch.
    #[test]
    fn rebinding_clears_disable_magic() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            Some(Pact {
                standing: PactStanding::Severed,
                patron: None,
                boon: None,
                blade_summoned: false,
                favour: 0,
            }),
        );
        tick(&mut state);
        assert!(disable_magic(&state, entity));

        state
            .ecs()
            .write_storage::<Pact>()
            .get_mut(entity)
            .expect("entity has a pact")
            .standing = PactStanding::Bound;
        tick(&mut state);

        assert!(!disable_magic(&state, entity));
    }
}
