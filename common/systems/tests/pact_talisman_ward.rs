//! End-to-end behaviour of the talisman bond's ward, driven through a real
//! `buff::Sys` tick.
//!
//! The thing worth a harness for is the wiring between the two halves of the
//! rebuke mechanism: `BuffKind::PactTalisman`'s `BuffEffect::ReflectDamage`
//! has to reach `Stats::damage_reflect`, because that is the only field
//! `Attack::apply_attack` ever reads. A buff whose effect never lands there
//! is a ward that silently does nothing.

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            Body, Energy, Health, Inventory, Poise, Stats,
            buff::{Buff, BuffData, BuffKind, BuffSource, Buffs, DestInfo},
            inventory::item::MaterialStatManifest,
            tool::AbilityMap,
        },
        resources::{GameMode, Time},
        shared_server_config::ServerConstants,
        skillset_builder::SkillSetBuilder,
        terrain::{MapSizeLg, TerrainChunk},
        uid::Uid,
    };
    use common_net::sync::WorldSyncExt;
    use specs::{Builder, Entity, WorldExt};
    use std::{num::NonZeroU64, sync::Arc, time::Duration};
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

    /// A bearer already carrying the bond's ward, sourced from a Warlock.
    fn create_bearer(state: &mut common_state::State, warded: bool, strength: f32) -> Entity {
        let body = humanoid_body();
        let mut buffs = Buffs::default();
        if warded {
            let time = Time(0.0);
            buffs.insert(
                Buff::new(
                    BuffKind::PactTalisman,
                    BuffData::new(strength, None),
                    Vec::new(),
                    BuffSource::Character {
                        by: Uid(NonZeroU64::new(2).unwrap()),
                        tool_kind: None,
                    },
                    time,
                    DestInfo {
                        stats: None,
                        mass: None,
                    },
                    None,
                    None,
                    None,
                ),
                time,
            );
        }
        state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(buffs)
            .with(SkillSetBuilder::default().build())
            .with(Inventory::with_empty())
            .build()
    }

    fn stats_of(state: &common_state::State, entity: Entity) -> Stats {
        state
            .ecs()
            .read_storage::<Stats>()
            .get(entity)
            .expect("entity has stats")
            .clone()
    }

    /// The rebuke reaches the only field the attack path reads.
    #[test]
    fn the_bond_ward_populates_damage_reflect() {
        let tuning = common::comp::pact::talisman_tuning_manifest();
        let mut state = setup();
        let bearer = create_bearer(&mut state, true, 0.10);

        tick(&mut state);

        let stats = stats_of(&state, bearer);
        assert_eq!(
            stats.damage_reflect.len(),
            1,
            "the ward must contribute exactly one reflect"
        );
        let reflect = stats.damage_reflect[0];
        assert!((reflect.fraction - tuning.0.rebuke_fraction).abs() < f32::EPSILON);
        assert!((reflect.cap - tuning.0.rebuke_cap).abs() < f32::EPSILON);
    }

    /// The protection half reaches the shipped damage-reduction channel.
    #[test]
    fn the_bond_ward_populates_damage_reduction() {
        let mut state = setup();
        let bearer = create_bearer(&mut state, true, 0.10);

        tick(&mut state);

        let stats = stats_of(&state, bearer);
        assert!(
            stats.damage_reduction.modifier() > 0.0,
            "the ward must reduce incoming damage"
        );
    }

    /// Nobody else pays for it: an unwarded entity reflects nothing, and the
    /// field is rebuilt (not accumulated) every tick.
    #[test]
    fn an_unwarded_entity_reflects_nothing() {
        let mut state = setup();
        let bearer = create_bearer(&mut state, false, 0.0);

        tick(&mut state);
        tick(&mut state);

        assert!(stats_of(&state, bearer).damage_reflect.is_empty());
    }

    /// Dropping the ward drops the reflect on the very next tick -- the field
    /// tracks live state, it is not a one-way latch.
    #[test]
    fn removing_the_ward_clears_the_reflect() {
        let mut state = setup();
        let bearer = create_bearer(&mut state, true, 0.10);
        tick(&mut state);
        assert_eq!(stats_of(&state, bearer).damage_reflect.len(), 1);

        {
            let ecs = state.ecs();
            let mut buffs = ecs.write_storage::<Buffs>();
            let mut bearer_buffs = buffs.get_mut(bearer).expect("bearer has buffs");
            bearer_buffs.buffs.clear();
            bearer_buffs.kinds.clear();
        }
        tick(&mut state);

        assert!(
            stats_of(&state, bearer).damage_reflect.is_empty(),
            "a ward that is gone must leave no reflect behind"
        );
    }
}
