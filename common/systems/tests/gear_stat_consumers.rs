//! Behaviour tests for the three per-tick gear-derived stat consumers in
//! `buff::Sys` and `stats::Sys`.
//!
//! These are deliberately written against the **observable behaviour** of both
//! systems — what ends up on `Stats` / `Energy` after a real tick — and not
//! against their internals, so the exact same assertions hold whether the
//! numbers are produced by walking the loadout at the read site or by reading
//! the `DerivedStats` cache.
//!
//! The three consumers under test:
//! 1. the caster-implement fold onto `Stats` (`spell_power` / `heal_power` /
//!    `energy_regen_modifier` / …)
//! 2. the `infinite_damage_reduction` probe in `buff::Sys`
//! 3. the gear energy bonus in `stats::Sys`, including the antimagic
//!    (`disable_magic`) attuned/unattuned split

#[cfg(test)]
mod tests {
    use common::{
        comp::{
            AttunedItems, Body, Energy, Health, Inventory, Poise, Stats,
            buff::{Buff, BuffData, BuffKind, BuffSource, Buffs, DestInfo},
            inventory::{
                item::{
                    AbilityMap, Item, ItemBase, ItemDef, ItemKind, ItemTag, MaterialStatManifest,
                    armor::{self, Armor, ArmorKind, Protection},
                    tool,
                },
                loadout_builder::LoadoutBuilder,
                slot::{ArmorSlot, EquipSlot},
            },
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

    const CHEST_ENERGY_MAX: f32 = 13.0;

    fn chest_armor(protection: Option<Protection>, requires_attunement: bool) -> Item {
        item_from_kind(
            ItemKind::Armor(Armor::new(
                ArmorKind::Chest,
                armor::StatsSource::Direct(armor::Stats {
                    protection,
                    poise_resilience: Some(Protection::Normal(7.25)),
                    energy_max: Some(CHEST_ENERGY_MAX),
                    energy_reward: Some(0.35),
                    precision_power: Some(0.17),
                    stealth: Some(0.9),
                    ground_contact: Default::default(),
                }),
            )),
            requires_attunement,
        )
    }

    /// The caster-implement fixture uses the same tool stats as the unit tests
    /// in `common/src/comp/derived_stats.rs`, so the expected numbers below
    /// match the ones pinned there.
    fn caster_staff() -> Item {
        item_from_kind(
            ItemKind::Tool(tool::Tool::new(
                tool::ToolKind::Staff,
                tool::Hands::Two,
                Some(tool::WeaponRole::Caster),
                tool::Stats {
                    equip_time_secs: 0.4,
                    power: 1.0,
                    effect_power: 1.5,
                    speed: 1.0,
                    range: 1.0,
                    energy_efficiency: 1.25,
                    buff_strength: 1.75,
                    cooldown_reduction: 1.0,
                },
            )),
            false,
        )
    }

    fn empty_loadout() -> Inventory {
        Inventory::with_loadout_humanoid(LoadoutBuilder::empty().build())
    }

    fn chest_loadout(protection: Option<Protection>, requires_attunement: bool) -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(chest_armor(protection, requires_attunement)))
                .build(),
        )
    }

    fn caster_loadout() -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .active_mainhand(Some(caster_staff()))
                .build(),
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
            .with(Buffs::default())
            .with(SkillSetBuilder::default().build())
            .with(inventory)
            .build()
    }

    fn now(state: &common_state::State) -> Time { *state.ecs().read_resource::<Time>() }

    /// Inserts a permanent buff straight onto the entity's `Buffs`, bypassing
    /// the `BuffEvent` round trip (whose handler lives server-side and so is
    /// not dispatched by a `common-systems` integration test).
    fn add_buff(state: &mut common_state::State, entity: Entity, kind: BuffKind, strength: f32) {
        let time = now(state);
        let buff = Buff::new(
            kind,
            BuffData::new(strength, None),
            vec![],
            BuffSource::World,
            time,
            DestInfo {
                stats: None,
                mass: None,
            },
            None,
            None,
            None,
        );
        state
            .ecs()
            .write_storage::<Buffs>()
            .get_mut(entity)
            .expect("entity has buffs")
            .insert(buff, time);
    }

    fn attune_chest(state: &mut common_state::State, entity: Entity) {
        state
            .ecs()
            .write_storage::<AttunedItems>()
            .insert(
                entity,
                AttunedItems(vec![EquipSlot::Armor(ArmorSlot::Chest)]),
            )
            .expect("inserting AttunedItems should succeed");
    }

    fn stats_of(state: &common_state::State, entity: Entity) -> Stats {
        state
            .ecs()
            .read_storage::<Stats>()
            .get(entity)
            .cloned()
            .expect("entity has stats")
    }

    fn max_energy(state: &common_state::State, entity: Entity) -> f32 {
        state
            .ecs()
            .read_storage::<Energy>()
            .get(entity)
            .expect("entity has energy")
            .maximum()
    }

    fn base_energy(state: &common_state::State, entity: Entity) -> f32 {
        state
            .ecs()
            .read_storage::<Energy>()
            .get(entity)
            .expect("entity has energy")
            .base_max()
    }

    // ------------------------------------------------------------------
    // (1) The caster-implement fold onto `Stats`.
    // ------------------------------------------------------------------

    #[test]
    fn caster_gear_raises_the_caster_channels_on_stats() {
        let mut state = setup();
        let entity = create_entity(&mut state, caster_loadout());

        tick(&mut state);

        let stats = stats_of(&state, entity);
        assert!(
            (stats.spell_power - 1.5).abs() < 1e-5,
            "spell_power should carry the staff's effect_power, got {}",
            stats.spell_power
        );
        assert!(
            (stats.heal_power - 1.75).abs() < 1e-5,
            "heal_power should carry the staff's buff_strength, got {}",
            stats.heal_power
        );
        assert!(
            (stats.energy_regen_modifier - 1.25).abs() < 1e-5,
            "energy_regen_modifier should carry the staff's energy_efficiency, got {}",
            stats.energy_regen_modifier
        );
    }

    #[test]
    fn no_caster_gear_leaves_the_caster_channels_at_identity() {
        let mut state = setup();
        let entity = create_entity(&mut state, empty_loadout());

        tick(&mut state);

        let stats = stats_of(&state, entity);
        assert_eq!(stats.spell_power, 1.0);
        assert_eq!(stats.heal_power, 1.0);
        assert_eq!(stats.energy_regen_modifier, 1.0);
        assert_eq!(stats.cooldown_reduction_modifier, 1.0);
    }

    /// A caster implement does not linger once unequipped — the contribution
    /// is recomputed from the current loadout rather than accumulated.
    #[test]
    fn unequipping_the_caster_implement_drops_the_bonus_again() {
        let mut state = setup();
        let entity = create_entity(&mut state, caster_loadout());

        tick(&mut state);
        assert!((stats_of(&state, entity).spell_power - 1.5).abs() < 1e-5);

        let time = now(&state);
        state
            .ecs()
            .write_storage::<Inventory>()
            .get_mut(entity)
            .expect("entity has an inventory")
            .replace_loadout_item(EquipSlot::ActiveMainhand, None, time);

        tick(&mut state);

        assert_eq!(stats_of(&state, entity).spell_power, 1.0);
    }

    // ------------------------------------------------------------------
    // (2) The `infinite_damage_reduction` probe.
    //
    // Observable through the other thing the flag gates: while it is set, a
    // debuff's effects are skipped entirely. `Slowed` writes
    // `Stats::move_speed_modifier`, which survives the tick and can simply be
    // read back.
    // ------------------------------------------------------------------

    #[test]
    fn a_debuff_applies_normally_without_any_damage_immunity() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Normal(12.5)), false),
        );
        add_buff(&mut state, entity, BuffKind::Slowed, 0.5);

        tick(&mut state);

        assert!(
            stats_of(&state, entity).move_speed_modifier < 1.0,
            "a Slowed debuff should slow an entity that is not damage-immune"
        );
    }

    /// Invincible armour — `protection == None` — suppresses debuff effects.
    #[test]
    fn invincible_armour_suppresses_debuff_effects() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Invincible), false),
        );
        add_buff(&mut state, entity, BuffKind::Slowed, 0.5);

        tick(&mut state);

        assert_eq!(
            stats_of(&state, entity).move_speed_modifier,
            1.0,
            "invincible armour should suppress the debuff entirely"
        );
    }

    /// Armour is not the only source of total damage immunity: a 100%
    /// damage-reduction buff (admin tabard, safezone `Invulnerability`) sets
    /// the same flag, and must keep doing so.
    ///
    /// Two ticks are needed because the probe reads `Stats` *before*
    /// `reset_temp_modifiers`, i.e. the previous tick's accumulated
    /// `damage_reduction`.
    #[test]
    fn full_damage_reduction_from_stats_also_suppresses_debuff_effects() {
        let mut state = setup();
        let entity = create_entity(&mut state, empty_loadout());
        add_buff(&mut state, entity, BuffKind::Invulnerability, 1.0);
        add_buff(&mut state, entity, BuffKind::Slowed, 0.5);

        // Tick 1: `Stats` still carries no damage reduction when the probe
        // runs, so the debuff lands...
        tick(&mut state);
        assert!(
            stats_of(&state, entity).move_speed_modifier < 1.0,
            "the debuff should still land on the tick the immunity is first applied"
        );
        assert!(
            stats_of(&state, entity).damage_reduction.modifier() >= 1.0,
            "Invulnerability should have pushed damage reduction to 100%"
        );

        // ...tick 2: the probe now sees 100% damage reduction and skips it.
        tick(&mut state);
        assert_eq!(
            stats_of(&state, entity).move_speed_modifier,
            1.0,
            "a fully damage-immune entity should have its debuff effects skipped"
        );
    }

    /// Attunement gates the probe's protection read too: an *unattuned*
    /// invincible piece is inert, so the debuff lands, and attuning it flips
    /// the immunity on.
    ///
    /// (The mirror case — an antimagic field making an attuned invincible
    /// piece mundane again — is not reachable in practice and so is not
    /// asserted here: `Antimagic` is itself a debuff, so an already-immune
    /// entity skips its effects and never gets `disable_magic` set. The
    /// selection is still made, so the probe stays equivalent to the damage
    /// path if that ordering ever changes.)
    #[test]
    fn attunement_gates_the_probes_protection_read() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Invincible), true),
        );
        add_buff(&mut state, entity, BuffKind::Slowed, 0.5);

        // Unattuned: the invincible piece contributes nothing.
        tick(&mut state);
        assert!(
            stats_of(&state, entity).move_speed_modifier < 1.0,
            "an unattuned invincible piece must not grant immunity"
        );

        attune_chest(&mut state, entity);
        tick(&mut state);
        assert_eq!(
            stats_of(&state, entity).move_speed_modifier,
            1.0,
            "attuning the piece should switch the immunity on"
        );
    }

    // ------------------------------------------------------------------
    // (3) The gear energy bonus in `stats::Sys`, and its antimagic split.
    // ------------------------------------------------------------------

    #[test]
    fn armour_energy_bonus_raises_max_energy() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Normal(12.5)), false),
        );

        tick(&mut state);

        let base = base_energy(&state, entity);
        assert!(
            (max_energy(&state, entity) - (base + CHEST_ENERGY_MAX)).abs() < 0.1,
            "max energy should include the chest's energy_max ({} vs base {})",
            max_energy(&state, entity),
            base
        );
    }

    #[test]
    fn no_gear_leaves_max_energy_at_base() {
        let mut state = setup();
        let entity = create_entity(&mut state, empty_loadout());

        tick(&mut state);

        assert_eq!(max_energy(&state, entity), base_energy(&state, entity));
    }

    /// Antimagic split, direction 1: an attunement-gated energy bonus applies
    /// while the slot is attuned and no antimagic field is in play.
    #[test]
    fn attuned_energy_bonus_applies_without_antimagic() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Normal(12.5)), true),
        );
        attune_chest(&mut state, entity);

        tick(&mut state);

        let base = base_energy(&state, entity);
        assert!(
            (max_energy(&state, entity) - (base + CHEST_ENERGY_MAX)).abs() < 0.1,
            "an attuned item's energy bonus should apply"
        );
    }

    /// Antimagic split, direction 2: `disable_magic` makes the same attuned
    /// bonus mundane, i.e. the unattuned value is used instead.
    #[test]
    fn antimagic_drops_the_attuned_energy_bonus() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Normal(12.5)), true),
        );
        attune_chest(&mut state, entity);
        add_buff(&mut state, entity, BuffKind::Antimagic, 1.0);

        // `stats::Sys` runs after `buff::Sys` in the same tick, so
        // `disable_magic` is visible immediately.
        tick(&mut state);

        assert!(stats_of(&state, entity).disable_magic);
        assert_eq!(
            max_energy(&state, entity),
            base_energy(&state, entity),
            "under antimagic the attuned energy bonus must be dropped"
        );
    }

    /// An unattuned attunement-gated item never contributed in the first
    /// place — the pair above must not be passing merely because attunement is
    /// broken.
    #[test]
    fn unattuned_gated_energy_bonus_does_not_apply() {
        let mut state = setup();
        let entity = create_entity(
            &mut state,
            chest_loadout(Some(Protection::Normal(12.5)), true),
        );

        tick(&mut state);

        assert_eq!(max_energy(&state, entity), base_energy(&state, entity));
    }
}
