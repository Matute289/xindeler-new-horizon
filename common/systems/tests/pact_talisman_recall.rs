//! End-to-end behaviour of the talisman bearer's anchored recall, driven
//! through a real `character_behavior::Sys` tick.
//!
//! Two things are worth a harness rather than a unit test here:
//!
//! 1. The recall really does resolve its destination from the bond buff's
//!    source, not from whatever the bearer happens to be aiming at.
//! 2. It really does go through the one shipped teleport emit site, so a
//!    dimensional anchor (`Stats.disable_teleport`) fizzles it -- which is only
//!    provable by driving the actual state, since the whole point is that no
//!    second teleport path was written.

#[cfg(test)]
mod tests {
    use common::{
        SkillSetBuilder,
        comp::{
            CharacterActivity, CharacterState, Controller, Energy, Ori, PhysicsState, Poise, Pos,
            Stats, Vel,
            buff::{Buff, BuffData, BuffKind, BuffSource, Buffs, DestInfo},
            item::MaterialStatManifest,
            tool::AbilityMap,
        },
        event::{EventBus, TeleportToEvent},
        resources::{DeltaTime, GameMode, Time},
        shared_server_config::ServerConstants,
        states::{
            blink::{self, BlinkAnchor},
            utils::{AbilityInfo, StageSection},
        },
        terrain::{MapSizeLg, TerrainChunk},
        uid::Uid,
    };
    use common_ecs::dispatch;
    use common_state::State;
    use rand::rng;
    use specs::{Builder, Entity, WorldExt};
    use std::{num::NonZeroU64, sync::Arc, time::Duration};
    use vek::{Vec2, Vec3};
    use xindeler_common_systems::character_behavior;

    const DEFAULT_WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 1, y: 1 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    const WARLOCK_UID: u64 = 2;

    fn setup() -> State {
        let pools = State::pools(GameMode::Server);
        let mut state = State::new(
            GameMode::Server,
            pools,
            DEFAULT_WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                dispatch::<character_behavior::Sys>(dispatch_builder, &[]);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        state
            .ecs_mut()
            .insert(MaterialStatManifest::load().cloned());
        state.ecs_mut().insert(AbilityMap::load().cloned());
        // `TeleportToEvent` is a server-side event; nothing in this minimal
        // harness registers its bus, and nothing drains it either, so the
        // emissions are still readable after the ticks below.
        state
            .ecs_mut()
            .insert(EventBus::<TeleportToEvent>::default());
        state.ecs_mut().read_resource::<Time>();
        state.ecs_mut().read_resource::<DeltaTime>();
        state
    }

    fn recall_state() -> CharacterState {
        CharacterState::Blink(blink::Data {
            static_data: blink::StaticData {
                buildup_duration: Duration::from_millis(100),
                recover_duration: Duration::from_millis(100),
                max_range: 100.0,
                frontend_specifier: None,
                anchor: Some(BlinkAnchor::BuffSource(BuffKind::PactTalisman)),
                ability_info: AbilityInfo {
                    tool: None,
                    hand: None,
                    role: None,
                    input: common::comp::InputKind::Ability(0),
                    // Deliberately `None`: an anchored recall must not need
                    // (or consult) the bearer's aim at all.
                    input_attr: None,
                    ability_meta: Default::default(),
                    ability: None,
                },
            },
            timer: Duration::default(),
            stage_section: StageSection::Buildup,
        })
    }

    fn warded_buffs(body: common::comp::Body) -> Buffs {
        let mut buffs = Buffs::default();
        let time = Time(0.0);
        buffs.insert(
            Buff::new(
                BuffKind::PactTalisman,
                BuffData::new(0.1, None),
                Vec::new(),
                BuffSource::Character {
                    by: Uid(NonZeroU64::new(WARLOCK_UID).unwrap()),
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
        let _ = body;
        buffs
    }

    /// A bearer mid-recall. `anchored` controls whether the bond buff is
    /// present; `disable_teleport` stands in for a dimensional anchor, which
    /// the buff system would otherwise set from `BuffKind::Anchored`.
    fn create_bearer(state: &mut State, anchored: bool, disable_teleport: bool) -> Entity {
        let body = common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut rng(),
            &common::comp::humanoid::Species::Human,
        ));
        let mut stats = Stats::empty(body);
        stats.disable_teleport = disable_teleport;
        let buffs = if anchored {
            warded_buffs(body)
        } else {
            Buffs::default()
        };
        state
            .ecs_mut()
            .create_entity()
            .with(recall_state())
            .with(CharacterActivity::default())
            .with(Pos(Vec3::zero()))
            .with(Vel::default())
            .with(Ori::default())
            .with(body.mass())
            .with(body.density())
            .with(body)
            .with(Energy::new(body))
            .with(Controller::default())
            .with(Poise::new(body))
            .with(SkillSetBuilder::default().build())
            .with(PhysicsState::default())
            .with(stats)
            .with(buffs)
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build()
    }

    fn tick(state: &mut State) {
        state.tick(
            Duration::from_millis(60),
            false,
            None,
            &ServerConstants {
                day_cycle_coefficient: 24.0,
                oracle_live: false,
            },
            |_, _| {},
        );
    }

    /// Enough ticks to carry a 100 ms buildup past its end, where the
    /// teleport (if any) is emitted.
    fn run_through_buildup(state: &mut State) {
        for _ in 0..4 {
            tick(state);
        }
    }

    fn teleports(state: &State) -> Vec<(Uid, Option<f32>)> {
        state
            .ecs()
            .read_resource::<EventBus<TeleportToEvent>>()
            .recv_all()
            .map(|event| (event.target, event.max_range))
            .collect()
    }

    #[test]
    fn an_anchored_recall_teleports_to_the_bond_source() {
        let mut state = setup();
        let _bearer = create_bearer(&mut state, true, false);

        run_through_buildup(&mut state);

        let events = teleports(&state);
        assert_eq!(events.len(), 1, "the recall must emit exactly one teleport");
        assert_eq!(
            events[0].0,
            Uid(NonZeroU64::new(WARLOCK_UID).unwrap()),
            "the recall must go to the bond's source, not to any aimed target"
        );
        assert_eq!(events[0].1, Some(100.0));
    }

    /// The dimensional-anchor proof: the recall reuses the shipped teleport
    /// path, so `Stats.disable_teleport` stops it. A separately-written
    /// teleport would silently pass this bearer through.
    #[test]
    fn an_anchored_bearer_cannot_recall() {
        let mut state = setup();
        let _bearer = create_bearer(&mut state, true, true);

        run_through_buildup(&mut state);

        assert!(
            teleports(&state).is_empty(),
            "a dimensional anchor must fizzle the recall exactly as it fizzles every other \
             teleport"
        );
    }

    /// Losing the bond takes the destination with it: the recall fizzles
    /// rather than degrading into an ordinary aim-driven blink.
    #[test]
    fn a_recall_without_the_bond_buff_fizzles() {
        let mut state = setup();
        let _bearer = create_bearer(&mut state, false, false);

        run_through_buildup(&mut state);

        assert!(
            teleports(&state).is_empty(),
            "an anchor that resolves to nothing must emit no teleport at all"
        );
    }
}
