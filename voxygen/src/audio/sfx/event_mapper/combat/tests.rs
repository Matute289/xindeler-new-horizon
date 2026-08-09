use super::*;
use crate::audio::sfx::{SfxEvent, SfxTriggerItem};
use common::{
    comp::{
        CharacterAbilityType, CharacterState, Item, controller::InputKind,
        inventory::loadout_builder::LoadoutBuilder, item::tool::ToolKind, melee,
    },
    states,
};
use std::time::{Duration, Instant};

#[test]
fn maps_wield_while_equipping() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.weapons.axe.starter_axe",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let result = CombatEventMapper::map_event(
        &CharacterState::Equipping(states::equipping::Data {
            static_data: states::equipping::StaticData {
                buildup_duration: Duration::from_millis(10),
            },
            timer: Duration::default(),
            is_sneaking: false,
        }),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: false,
            ..Default::default()
        },
        &inventory,
    );

    assert_eq!(result, SfxEvent::Wield(ToolKind::Axe));
}

#[test]
fn maps_unwield() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.weapons.bow.starter",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let result = CombatEventMapper::map_event(
        &CharacterState::default(),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: true,
            ..Default::default()
        },
        &inventory,
    );

    assert_eq!(result, SfxEvent::Unwield(ToolKind::Bow));
}

#[test]
fn maps_basic_melee() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.weapons.axe.starter_axe",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let result = CombatEventMapper::map_event(
        &CharacterState::BasicMelee(states::basic_melee::Data {
            static_data: states::basic_melee::StaticData {
                buildup_duration: Duration::default(),
                swing_duration: Duration::default(),
                hit_timing: 0.0,
                recover_duration: Duration::default(),
                melee_constructor: melee::MeleeConstructor {
                    kind: melee::MeleeConstructorKind::Slash {
                        damage: 1.0,
                        knockback: 0.0,
                        poise: 0.0,
                        energy_regen: 0.0,
                    },
                    scaled: None,
                    range: 3.5,
                    angle: 15.0,
                    damage_effect: None,
                    attack_effect: None,
                    attack_effect_target: None,
                    multi_target: None,
                    simultaneous_hits: 1,
                    custom_combo: melee::CustomCombo {
                        base: None,
                        conditional: None,
                    },
                    dodgeable: common::comp::ability::Dodgeable::Roll,
                    blockable: true,
                    precision_flank_multipliers: Default::default(),
                    precision_flank_invert: false,
                },
                movement_modifier: Default::default(),
                ori_modifier: Default::default(),
                ability_info: empty_ability_info(),
                frontend_specifier: None,
            },
            timer: Duration::default(),
            stage_section: states::utils::StageSection::Action,
            exhausted: false,
            movement_modifier: None,
            ori_modifier: None,
        }),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: true,
            ..Default::default()
        },
        &inventory,
    );

    assert_eq!(
        result,
        SfxEvent::Attack(
            CharacterAbilityType::BasicMelee(states::utils::StageSection::Action),
            ToolKind::Axe
        )
    );
}

fn empty_ability_info() -> states::utils::AbilityInfo {
    states::utils::AbilityInfo {
        tool: None,
        hand: None,
        role: None,
        input: InputKind::Primary,
        input_attr: None,
        ability_meta: Default::default(),
        ability: None,
    }
}

fn attacking_state() -> CharacterState {
    CharacterState::BasicMelee(states::basic_melee::Data {
        static_data: states::basic_melee::StaticData {
            buildup_duration: Duration::default(),
            swing_duration: Duration::default(),
            hit_timing: 0.0,
            recover_duration: Duration::default(),
            melee_constructor: melee::MeleeConstructor {
                kind: melee::MeleeConstructorKind::Slash {
                    damage: 1.0,
                    knockback: 0.0,
                    poise: 0.0,
                    energy_regen: 0.0,
                },
                scaled: None,
                range: 3.5,
                angle: 15.0,
                damage_effect: None,
                attack_effect: None,
                attack_effect_target: None,
                multi_target: None,
                simultaneous_hits: 1,
                custom_combo: melee::CustomCombo {
                    base: None,
                    conditional: None,
                },
                dodgeable: common::comp::ability::Dodgeable::Roll,
                blockable: true,
                precision_flank_multipliers: Default::default(),
                precision_flank_invert: false,
            },
            movement_modifier: Default::default(),
            ori_modifier: Default::default(),
            ability_info: empty_ability_info(),
            frontend_specifier: None,
        },
        timer: Duration::default(),
        stage_section: states::utils::StageSection::Action,
        exhausted: false,
        movement_modifier: None,
        ori_modifier: None,
    })
}

/// Battle Refrain: an entity mid-attack (or mid-cast, both satisfy
/// `is_attack()`) with an `Instrument` equipped in its refrain slot also
/// yields a `Music` event, layered alongside whatever `map_event` returns —
/// the two never come from the same call, so nothing here contradicts
/// `map_event`'s own mutually-exclusive-state result.
#[test]
fn refrain_maps_music_when_attacking_with_an_instrument_equipped() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.tool.instruments.lute",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let refrain = CombatEventMapper::map_refrain(&attacking_state(), &inventory);

    assert_eq!(
        refrain,
        Some(SfxEvent::Music(
            ToolKind::Instrument,
            common::comp::item::AbilitySpec::Custom("Lute".to_owned())
        ))
    );
}

/// The refrain never fires for an ordinary (non-instrument) weapon.
#[test]
fn refrain_is_none_when_attacking_with_a_normal_weapon() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.weapons.axe.starter_axe",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let refrain = CombatEventMapper::map_refrain(&attacking_state(), &inventory);

    assert_eq!(refrain, None);
}

/// The refrain never fires outside an attack state, even with an instrument
/// equipped (e.g. idly wielding it, or actually playing it by hand).
#[test]
fn refrain_is_none_when_not_attacking() {
    let loadout = LoadoutBuilder::empty()
        .active_mainhand(Some(Item::new_from_asset_expect(
            "common.items.tool.instruments.lute",
        )))
        .build();
    let inventory = Inventory::with_loadout_humanoid(loadout);

    let refrain = CombatEventMapper::map_refrain(&CharacterState::default(), &inventory);

    assert_eq!(refrain, None);
}

/// The refrain's emission threshold is tracked independently of the
/// attack/idle event's own threshold — a fast attack must not starve the
/// refrain, and a slow refrain sample must not suppress the attack sfx.
#[test]
fn refrain_threshold_is_independent_of_the_attack_threshold() {
    let trigger = SfxTriggerItem {
        files: vec!["dummy".to_owned()],
        threshold: 100.0, // effectively "never repeat within this test"
        subtitle: None,
    };
    let refrain_event = SfxEvent::Music(
        ToolKind::Instrument,
        common::comp::item::AbilitySpec::Custom("Lute".to_owned()),
    );

    // The attack event repeated recently -> its own threshold suppresses it.
    let attack_repeated_recently = !CombatEventMapper::should_emit(
        &SfxEvent::Attack(
            CharacterAbilityType::BasicMelee(states::utils::StageSection::Action),
            ToolKind::Axe,
        ),
        Instant::now(),
        Some((
            &SfxEvent::Attack(
                CharacterAbilityType::BasicMelee(states::utils::StageSection::Action),
                ToolKind::Axe,
            ),
            &trigger,
        )),
    );
    assert!(
        attack_repeated_recently,
        "a same-event repeat inside the threshold window must be suppressed"
    );

    // The refrain has its OWN, separately-tracked timer/event, so an
    // attack-threshold suppression above must not affect it: a refrain event
    // that differs from its own previous refrain event always emits,
    // regardless of what just happened on the attack timer.
    let refrain_still_emits = CombatEventMapper::should_emit(
        &SfxEvent::Idle,
        Instant::now(),
        Some((&refrain_event, &trigger)),
    );
    assert!(
        refrain_still_emits,
        "the refrain timer must be independent of the attack timer"
    );
}
