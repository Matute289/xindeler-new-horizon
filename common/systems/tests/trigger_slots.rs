//! End-to-end behaviour of reactive trigger slots, driven through a real ECS
//! tick.
//!
//! The two things worth the harness cost are the security surface (what a
//! trigger is and is not allowed to do to `AbilityCooldowns`) and the state
//! machine (it must fire once, and a refused cast must cost nothing).

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            AbilityCooldowns, AbilityPool, ActiveAbilities, CharacterActivity, CharacterState,
            Combo, Controller, Energy, Health, Ori, PhysicsState, Poise, Pos, SkillSet, SlotState,
            Stats, TriggerCondition, TriggerSlot, TriggerSlots, Vel,
            ability::AuxiliaryAbility,
            controller::{ControlAction, InputKind},
            item::MaterialStatManifest,
            tool::AbilityMap,
        },
        event::{EventBus, SetAbilityCooldownEvent},
        resources::{GameMode, Time},
        shared_server_config::ServerConstants,
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

    /// The Danari racial blink: no target, no energy cost, and a 45 s cooldown
    /// of its own — which is exactly what makes it the right probe for "the
    /// trigger ignores that cooldown, and does not touch it either".
    const BLINK: &str = "innate.danari";

    const DEFAULT_WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 1, y: 1 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

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
        // The ability-cooldown event bus is a server resource; the tests read
        // it directly to see whether a cast asked for a cooldown write.
        state
            .ecs_mut()
            .insert(EventBus::<SetAbilityCooldownEvent>::default());
        state
    }

    fn tick(state: &mut State) {
        state.tick(
            Duration::from_secs_f32(0.1),
            false,
            None,
            &ServerConstants {
                day_cycle_coefficient: 24.0,
                oracle_live: false,
            },
            |_, _| {},
        );
    }

    fn now(state: &State) -> Time { *state.ecs().read_resource::<Time>() }

    /// A level-60 humanoid holding exactly one pool ability (the blink) and one
    /// hotbar slot bound to it, so both the manual and the triggered route to
    /// the same ability exist and can be compared.
    fn create_caster(state: &mut State, condition: TriggerCondition) -> Entity {
        let body = common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut rng(),
            &common::comp::humanoid::Species::Danari,
        ));
        let mut skill_set = SkillSet::default();
        skill_set.set_level(60);

        let pool = AbilityPool {
            abilities: vec![BLINK.to_string()],
            spell_gates: vec![None],
        };
        let mut auxiliary_sets = hashbrown::HashMap::new();
        auxiliary_sets.insert((None, None), vec![AuxiliaryAbility::Innate(0)]);

        let mut slots = TriggerSlots::default();
        slots.slots[0] = Some(TriggerSlot::new(AuxiliaryAbility::Innate(0), condition));

        state
            .ecs_mut()
            .create_entity()
            .with(CharacterState::Idle(common::states::idle::Data::default()))
            .with(CharacterActivity::default())
            .with(Pos(Vec3::zero()))
            .with(Vel::default())
            .with(Ori::default())
            .with(body.mass())
            .with(body.density())
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Controller::default())
            .with(Combo::default())
            .with(Poise::new(body))
            .with(skill_set)
            .with(PhysicsState::default())
            .with(Stats::empty(body))
            .with(pool)
            .with(ActiveAbilities::from_auxiliary(auxiliary_sets, None))
            .with(AbilityCooldowns::default())
            .with(slots)
            .with(Uid(NonZeroU64::new(1).unwrap()))
            .build()
    }

    fn slot_state(state: &State, entity: Entity) -> SlotState {
        state
            .ecs()
            .read_storage::<TriggerSlots>()
            .get(entity)
            .and_then(|s| s.get(0).map(|slot| slot.state.clone()))
            .expect("slot 0 must exist")
    }

    fn blinked(state: &State, entity: Entity) -> bool {
        matches!(
            state.ecs().read_storage::<CharacterState>().get(entity),
            Some(CharacterState::Blink(_))
        )
    }

    /// Run ticks until the caster enters the blink, or give up.
    fn tick_until_cast(state: &mut State, entity: Entity, max_ticks: usize) -> bool {
        for _ in 0..max_ticks {
            tick(state);
            if blinked(state, entity) {
                return true;
            }
        }
        false
    }

    fn drain_cooldown_events(state: &State) -> Vec<SetAbilityCooldownEvent> {
        state
            .ecs()
            .read_resource::<EventBus<SetAbilityCooldownEvent>>()
            .recv_all()
            .collect()
    }

    /// An always-true condition, so a test never has to model damage.
    fn always() -> TriggerCondition { TriggerCondition::HealthBelow(1.5) }

    // ---------------------------------------------------------------- firing

    #[test]
    fn a_trigger_fires_once_and_then_cools_down() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());

        assert!(
            tick_until_cast(&mut state, entity, 6),
            "the trigger never cast"
        );
        assert!(matches!(
            slot_state(&state, entity),
            SlotState::CoolingDown { .. }
        ));

        // The wait is the circle-0 floor: ten real-world minutes, projected
        // into in-game time.
        let ready_at = slot_state(&state, entity)
            .ready_at_time()
            .expect("a cooling slot carries a projection");
        assert!(
            (ready_at.0 - (now(&state).0 + 600.0)).abs() < 1.0,
            "expected a ~600 s wait, got {ready_at:?} at {:?}",
            now(&state)
        );
        // The authoritative wall-clock instant is set server-side.
        assert!(slot_state(&state, entity).ready_at().is_some());
    }

    #[test]
    fn a_cooling_trigger_does_not_re_fire_while_its_condition_stays_true() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        assert!(tick_until_cast(&mut state, entity, 6));
        let _ = drain_cooldown_events(&state);

        // The condition is still true every one of these ticks.
        for _ in 0..40 {
            tick(&mut state);
            assert!(
                matches!(slot_state(&state, entity), SlotState::CoolingDown { .. }),
                "the slot left its cooldown early"
            );
        }
    }

    #[test]
    fn a_trigger_whose_condition_is_false_never_fires() {
        let mut state = setup();
        let entity = create_caster(&mut state, TriggerCondition::HealthBelow(0.05));

        for _ in 0..10 {
            tick(&mut state);
        }
        assert_eq!(slot_state(&state, entity), SlotState::Ready);
        assert!(!blinked(&state, entity));
    }

    #[test]
    fn a_hidden_slot_does_not_fire_but_keeps_its_configuration() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        // Slot 0 unlocks at level 15; this character is below it.
        state
            .ecs()
            .write_storage::<SkillSet>()
            .get_mut(entity)
            .expect("skill set")
            .set_level(14);

        for _ in 0..10 {
            tick(&mut state);
        }
        assert_eq!(slot_state(&state, entity), SlotState::Ready);
        assert!(!blinked(&state, entity));
        // Hidden, never cleared.
        assert_eq!(
            state
                .ecs()
                .read_storage::<TriggerSlots>()
                .get(entity)
                .and_then(|s| s.configured_ability(0)),
            Some(AuxiliaryAbility::Innate(0)),
        );
    }

    // -------------------------------------------------------------- security

    /// The one privilege: a triggered cast fires even though the ability's own
    /// cooldown is running.
    #[test]
    fn a_trigger_fires_through_the_abilitys_own_cooldown() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        {
            let mut cooldowns = state.ecs().write_storage::<AbilityCooldowns>();
            let mut cds = cooldowns.get_mut(entity).expect("cooldowns");
            cds.set(BLINK, Time(0.0), 45.0);
        }

        assert!(
            tick_until_cast(&mut state, entity, 6),
            "the trigger did not bypass the ability cooldown"
        );
    }

    /// 🔴 The bypass is symmetric: the triggered cast neither reads nor writes
    /// the triggered ability's `AbilityCooldowns` entry. Neither cleared (which
    /// would turn one trigger into two free casts) nor set (which would put the
    /// player's own manual cast on a cooldown he never asked for).
    #[test]
    fn a_triggered_cast_leaves_the_ability_cooldown_entry_untouched() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        let before = {
            let mut cooldowns = state.ecs().write_storage::<AbilityCooldowns>();
            let mut cds = cooldowns.get_mut(entity).expect("cooldowns");
            cds.set(BLINK, Time(0.0), 45.0);
            cds.ready_at(BLINK).expect("just set")
        };
        let _ = drain_cooldown_events(&state);

        assert!(tick_until_cast(&mut state, entity, 6));

        // Not cleared, not moved.
        let after = state
            .ecs()
            .read_storage::<AbilityCooldowns>()
            .get(entity)
            .and_then(|cds| cds.ready_at(BLINK));
        assert_eq!(after, Some(before), "the entry was rewritten or cleared");

        // And no write was even requested.
        let requested = drain_cooldown_events(&state);
        assert!(
            requested.iter().all(|ev| ev.ability_id != BLINK),
            "a triggered cast asked to set its own ability cooldown",
        );
    }

    /// A manual cast of the very same ability still sets its cooldown — the
    /// suppression above must be specific to the triggered route, not a
    /// blanket disabling of ability cooldowns.
    #[test]
    fn a_manual_cast_of_the_same_ability_still_sets_its_cooldown() {
        let mut state = setup();
        // No trigger fires: the condition never holds.
        let entity = create_caster(&mut state, TriggerCondition::HealthBelow(0.05));
        let _ = drain_cooldown_events(&state);

        state
            .ecs()
            .write_storage::<Controller>()
            .get_mut(entity)
            .expect("controller")
            .push_action(ControlAction::StartInput {
                input: InputKind::Ability(0),
                target_entity: None,
                select_pos: None,
            });

        assert!(
            tick_until_cast(&mut state, entity, 6),
            "the manual cast never happened"
        );
        let requested = drain_cooldown_events(&state);
        assert!(
            requested.iter().any(|ev| ev.ability_id == BLINK),
            "a manual cast did not set the ability's cooldown",
        );
    }

    /// 🔴 The token is bound to one input and one ability id, so an explicit
    /// player input landing on the same tick cannot spend it. The player's
    /// input wins the tick (it has the lower discriminant) and is then judged
    /// on its own merits — here, refused, because the ability is on cooldown
    /// and a key press has no bypass.
    #[test]
    fn an_explicit_input_cannot_steal_the_bypass_token() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        {
            let mut cooldowns = state.ecs().write_storage::<AbilityCooldowns>();
            cooldowns
                .get_mut(entity)
                .expect("cooldowns")
                .set(BLINK, Time(0.0), 45.0);
        }
        // Held down for the whole test, so it always wins `attempt_input`.
        {
            let mut controllers = state.ecs().write_storage::<Controller>();
            let controller = controllers.get_mut(entity).expect("controller");
            controller.push_action(ControlAction::StartInput {
                input: InputKind::Ability(0),
                target_entity: None,
                select_pos: None,
            });
        }

        for _ in 0..8 {
            tick(&mut state);
            assert!(
                !blinked(&state, entity),
                "a key press consumed the trigger's bypass and cast for free",
            );
        }
        // And nothing wrote the ability's cooldown either.
        let after = state
            .ecs()
            .read_storage::<AbilityCooldowns>()
            .get(entity)
            .and_then(|cds| cds.ready_at(BLINK));
        assert_eq!(after, Some(Time(45.0)));
    }

    /// Everything except the cooldown check still applies. An antimagic field
    /// blocks a triggered magic cast exactly as it blocks a manual one — and,
    /// because the cast never happened, the slot costs nothing.
    #[test]
    fn a_blocked_cast_costs_the_slot_nothing() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());

        // A trigger for an ability that cannot resolve at all: the slot arms,
        // pushes its input, and nothing ever consumes it.
        {
            let mut pools = state.ecs().write_storage::<AbilityPool>();
            let mut pool = pools.get_mut(entity).expect("pool");
            pool.abilities[0] = "innate.does_not_exist".to_string();
        }

        let (mut saw_firing, mut saw_ready_again) = (false, false);
        for _ in 0..120 {
            tick(&mut state);
            match slot_state(&state, entity) {
                SlotState::Firing { .. } => saw_firing = true,
                SlotState::Ready if saw_firing => saw_ready_again = true,
                SlotState::CoolingDown { .. } => {
                    panic!("a cast that never resolved burned the slot cooldown")
                },
                SlotState::Ready => {},
            }
        }
        assert!(saw_firing, "the slot never armed");
        assert!(
            saw_ready_again,
            "the slot never came back after its attempt lapsed",
        );
    }

    /// A self-buff spell: costs energy, costs HP, is magical, and carries its
    /// own cooldown. Everything a trigger must still be judged by.
    const WARD: &str = "spells.hemomancy.sanguine_ward";

    fn point_slot_at_the_ward(state: &mut State, entity: Entity) {
        let mut pools = state.ecs().write_storage::<AbilityPool>();
        let mut pool = pools.get_mut(entity).expect("pool");
        pool.abilities[0] = WARD.to_string();
    }

    fn warded(state: &State, entity: Entity) -> bool {
        matches!(
            state.ecs().read_storage::<CharacterState>().get(entity),
            Some(CharacterState::SelfBuff(_))
        )
    }

    /// The baseline for the two tests below: with nothing in the way, the
    /// triggered spell does cast.
    #[test]
    fn a_triggered_spell_casts_when_nothing_blocks_it() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        point_slot_at_the_ward(&mut state, entity);

        let mut cast = false;
        for _ in 0..8 {
            tick(&mut state);
            cast |= warded(&state, entity);
        }
        assert!(cast, "the triggered spell never cast");
    }

    /// 🔴 Only the ability's own cooldown is bypassed. The energy cost is still
    /// charged and still gates the cast — and because the cast never happened,
    /// the slot keeps its charge for when the caster can afford it. Losing a
    /// twelve-hour trigger to a few points of missing energy would be the worst
    /// bug this feature could ship.
    #[test]
    fn a_triggered_cast_without_the_energy_is_refused_and_costs_the_slot_nothing() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        point_slot_at_the_ward(&mut state, entity);
        {
            let mut energies = state.ecs().write_storage::<Energy>();
            let mut energy = energies.get_mut(entity).expect("energy");
            let drain = energy.current();
            energy.change_by(-drain);
        }

        for _ in 0..40 {
            tick(&mut state);
            assert!(!warded(&state, entity), "cast without paying for it");
            assert!(
                !matches!(slot_state(&state, entity), SlotState::CoolingDown { .. }),
                "a refused cast burned the slot cooldown",
            );
        }
    }

    /// 🔴 An antimagic field blocks a triggered magic cast exactly as it blocks
    /// a manual one, and the slot again costs nothing.
    #[test]
    fn a_triggered_cast_inside_an_antimagic_field_is_refused() {
        let mut state = setup();
        let entity = create_caster(&mut state, always());
        point_slot_at_the_ward(&mut state, entity);
        {
            let mut stats = state.ecs().write_storage::<Stats>();
            let mut stat = stats.get_mut(entity).expect("stats");
            stat.disable_magic = true;
        }

        for _ in 0..40 {
            tick(&mut state);
            // `Stats` temp modifiers are normally reset each tick by the stats
            // system, which this harness does not run — so the flag stays set.
            state
                .ecs()
                .write_storage::<Stats>()
                .get_mut(entity)
                .expect("stats")
                .disable_magic = true;
            assert!(!warded(&state, entity), "antimagic did not block a trigger");
            assert!(
                !matches!(slot_state(&state, entity), SlotState::CoolingDown { .. }),
                "a blocked cast burned the slot cooldown",
            );
        }
    }

    /// An entity without the component must never pay for the feature, and must
    /// never be affected by it.
    #[test]
    fn an_entity_without_trigger_slots_is_untouched() {
        let mut state = setup();
        let body = common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut rng(),
            &common::comp::humanoid::Species::Human,
        ));
        let entity = state
            .ecs_mut()
            .create_entity()
            .with(CharacterState::Idle(common::states::idle::Data::default()))
            .with(CharacterActivity::default())
            .with(Pos(Vec3::zero()))
            .with(Vel::default())
            .with(Ori::default())
            .with(body.mass())
            .with(body.density())
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Controller::default())
            .with(Poise::new(body))
            .with(SkillSet::default())
            .with(PhysicsState::default())
            .with(Stats::empty(body))
            .with(Uid(NonZeroU64::new(2).unwrap()))
            .build();

        for _ in 0..10 {
            tick(&mut state);
        }
        assert!(
            state
                .ecs()
                .read_storage::<TriggerSlots>()
                .get(entity)
                .is_none()
        );
        assert!(
            state
                .ecs()
                .read_storage::<Controller>()
                .get(entity)
                .expect("controller")
                .queued_inputs
                .keys()
                .all(|k| !matches!(k, InputKind::TriggerAbility(_)))
        );
    }
}
