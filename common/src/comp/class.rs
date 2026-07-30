//! Character class identity. See
//! docs/design/specs/2026-06-10-classes-races-design.md.
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

use crate::{
    assets::{AssetExt, Ron},
    comp::{Stats, body::humanoid::Species, skillset::MAX_CHARACTER_LEVEL},
};

#[derive(
    Clone, Copy, Debug, Default, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd,
)]
pub enum ClassKind {
    /// Legacy/default class for pre-class characters. Not selectable at
    /// creation, has no class skill tree, and is never listed in item class
    /// whitelists (equipment-restrictions Spec B).
    #[default]
    Adventurer,
    Warrior,
    Mage,
    Cleric,
    Rogue,
    // Classes-wave (BL-04). Skill trees empty until BL-06.
    Barbarian,
    Sorcerer,
    Warlock,
    Bard,
    Paladin,
    Druid,
    Ranger,
    Monk,
    Artificer,
    /// Blood-Hunter-pattern (ours): blood-rite hunter, practises Hemomancy
    /// (≤ circle 5). "Bloodborne" was rejected (FromSoftware trademark).
    BloodSlayer,
}

impl ClassKind {
    /// Every variant, including Adventurer. Single source of truth for
    /// enumeration — persistence round-trips and tests iterate this, so a
    /// new variant added here cannot silently fall out of any converter.
    pub const ALL: [ClassKind; 15] = [
        ClassKind::Adventurer,
        ClassKind::Warrior,
        ClassKind::Mage,
        ClassKind::Cleric,
        ClassKind::Rogue,
        ClassKind::Barbarian,
        ClassKind::Sorcerer,
        ClassKind::Warlock,
        ClassKind::Bard,
        ClassKind::Paladin,
        ClassKind::Druid,
        ClassKind::Ranger,
        ClassKind::Monk,
        ClassKind::Artificer,
        ClassKind::BloodSlayer,
    ];
    /// Classes selectable at character creation (excludes Adventurer).
    pub const PLAYABLE: [ClassKind; 14] = [
        ClassKind::Warrior,
        ClassKind::Mage,
        ClassKind::Cleric,
        ClassKind::Rogue,
        ClassKind::Barbarian,
        ClassKind::Sorcerer,
        ClassKind::Warlock,
        ClassKind::Bard,
        ClassKind::Paladin,
        ClassKind::Druid,
        ClassKind::Ranger,
        ClassKind::Monk,
        ClassKind::Artificer,
        ClassKind::BloodSlayer,
    ];

    pub fn is_playable(self) -> bool { !matches!(self, ClassKind::Adventurer) }

    /// Lowercase keyword used by chat commands and asset specifiers.
    pub fn keyword(self) -> &'static str {
        match self {
            ClassKind::Adventurer => "adventurer",
            ClassKind::Warrior => "warrior",
            ClassKind::Mage => "mage",
            ClassKind::Cleric => "cleric",
            ClassKind::Rogue => "rogue",
            ClassKind::Barbarian => "barbarian",
            ClassKind::Sorcerer => "sorcerer",
            ClassKind::Warlock => "warlock",
            ClassKind::Bard => "bard",
            ClassKind::Paladin => "paladin",
            ClassKind::Druid => "druid",
            ClassKind::Ranger => "ranger",
            ClassKind::Monk => "monk",
            ClassKind::Artificer => "artificer",
            ClassKind::BloodSlayer => "blood_slayer",
        }
    }

    /// Inverse of [`Self::keyword`] for the playable classes only.
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::PLAYABLE
            .iter()
            .copied()
            .find(|c| c.keyword() == keyword)
    }
}

/// The class(es) a player character holds. `secondary` is `None` for the
/// overwhelming majority of characters and can only be set by a multiclass
/// grant (never at creation). Hard cap of 2 — there is no third field, by
/// design. Synced to all clients; persisted in the `character` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterClass {
    pub primary: ClassKind,
    pub secondary: Option<ClassKind>,
    /// Class levels banked into the secondary class (only meaningful when
    /// `secondary.is_some()`). The primary's own level is never stored — it
    /// is always derived as `character_level - secondary_level`, so it can
    /// never desync from the XP-derived character level the way a directly
    /// stored value could. Always `0` for single-class characters.
    pub secondary_level: u16,
}

impl CharacterClass {
    pub fn single(class: ClassKind) -> Self {
        Self {
            primary: class,
            secondary: None,
            secondary_level: 0,
        }
    }

    /// Iterates 1 or 2 classes. THE accessor — prefer it over `primary`
    /// wherever "does this character have class X" or "apply this to every
    /// class held" is the actual question.
    pub fn classes(&self) -> impl Iterator<Item = ClassKind> + '_ {
        std::iter::once(self.primary).chain(self.secondary)
    }

    pub fn has(&self, class: ClassKind) -> bool { self.classes().any(|c| c == class) }

    pub fn is_multiclass(&self) -> bool { self.secondary.is_some() }

    /// The primary class's own level at the given character level. Ignores
    /// `secondary_level` when there is no secondary class, so a stale value
    /// left behind by a bug can never make a single-class character's
    /// primary level read low.
    pub fn primary_level(&self, character_level: u16) -> u16 {
        let secondary_level = if self.secondary.is_some() {
            self.secondary_level
        } else {
            0
        };
        character_level.saturating_sub(secondary_level)
    }

    /// `(class, level, is_primary)` for every class held (1 or 2), primary
    /// first. THE accessor for anything that must apply class-level-scaled
    /// stats once per held class.
    pub fn class_levels(
        &self,
        character_level: u16,
    ) -> impl Iterator<Item = (ClassKind, u16, bool)> + '_ {
        let primary = (self.primary, self.primary_level(character_level), true);
        let secondary = self.secondary.map(|c| (c, self.secondary_level, false));
        std::iter::once(primary).chain(secondary)
    }

    /// Sets the secondary class's banked level count directly, clamped to
    /// `0..=character_level` so `primary_level` can never underflow or
    /// exceed the character's total. Used by the multiclass grant (the
    /// one-time reallocation of already-earned levels) and by
    /// `/set_class_level` for testing — never called on a per-tick path.
    pub fn set_secondary_level(&mut self, new_secondary_level: u16, character_level: u16) {
        self.secondary_level = new_secondary_level.min(character_level);
    }
}

impl Component for CharacterClass {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

/// Per-species passive stat modifiers (spec §6). All values small (≤10%).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct RacialTraits {
    pub move_speed_mult: f32,
    pub attack_damage_mult: f32,
    pub energy_reward_mult: f32,
    pub max_energy_mult: f32,
    pub damage_reduction_add: f32,
    pub crowd_control_resistance_add: f32,
    /// Additive resistance (fraction) against resisted mind-altering effects
    /// such as charm/domination, mirroring how `crowd_control_resistance_add`
    /// works for crowd-control durations. Only meaningful for humanoid
    /// species, since `RacialTraits` itself is keyed by `Species`.
    pub magic_resistance_add: f32,
}

impl Default for RacialTraits {
    fn default() -> Self {
        Self {
            move_speed_mult: 1.0,
            attack_damage_mult: 1.0,
            energy_reward_mult: 1.0,
            max_energy_mult: 1.0,
            damage_reduction_add: 0.0,
            crowd_control_resistance_add: 0.0,
            magic_resistance_add: 0.0,
        }
    }
}

/// Single asset-cache lookup. Do NOT call per-entity in tick systems — hoist
/// one lookup per system run (hot-reload still works per-run); see buff.rs.
pub fn racial_traits(species: Species) -> RacialTraits {
    racial_traits_manifest()
        .0
        .get(&species)
        .copied()
        .unwrap_or_default()
}

/// One manifest read for per-tick consumers: one cache access covers all
/// species. Hot-reloads between system runs.
pub fn racial_traits_manifest() -> crate::assets::AssetReadGuard<Ron<HashMap<Species, RacialTraits>>>
{
    Ron::<HashMap<Species, RacialTraits>>::load_expect("common.class.racial_traits").read()
}

impl RacialTraits {
    /// Applies these passives onto freshly-reset stats. Must run right after
    /// `Stats::reset_temp_modifiers` so traits stack with buffs (spec §6).
    pub fn apply(self, stats: &mut Stats) {
        stats.move_speed_modifier *= self.move_speed_mult;
        stats.attack_damage_modifier *= self.attack_damage_mult;
        stats.energy_reward_modifier *= self.energy_reward_mult;
        stats.max_energy_modifiers.mult_mod *= self.max_energy_mult;
        stats.damage_reduction.pos_mod += self.damage_reduction_add;
        stats.crowd_control_resistance += self.crowd_control_resistance_add;
        stats.magic_resistance += self.magic_resistance_add;
    }
}

/// Applies racial passives onto freshly-reset stats. Must run right after
/// `Stats::reset_temp_modifiers` so traits stack with buffs (spec §6).
pub fn apply_racial_traits(stats: &mut Stats, species: Species) {
    racial_traits(species).apply(stats);
}

/// Level at which growth accelerates, and the slope multiplier past it, so the
/// last `MAX_CHARACTER_LEVEL - LEVEL_ACCEL_START` levels feel epic (BL-01 spec
/// §7 Q2). Tunable; the two constants move together with `MAX_CHARACTER_LEVEL`.
pub const LEVEL_ACCEL_START: u16 = 50;
pub const LEVEL_ACCEL_FACTOR: f32 = 2.5;

/// Total growth contributed by `per_level` at character `level`: linear up to
/// `LEVEL_ACCEL_START`, then `LEVEL_ACCEL_FACTOR`× per level beyond it. `level`
/// is clamped to `1..=MAX_CHARACTER_LEVEL`.
pub fn level_scaled(per_level: f32, level: u16) -> f32 {
    let level = level.clamp(1, MAX_CHARACTER_LEVEL);
    let linear = level.min(LEVEL_ACCEL_START).saturating_sub(1) as f32;
    let accelerated = level.saturating_sub(LEVEL_ACCEL_START) as f32;
    per_level * (linear + LEVEL_ACCEL_FACTOR * accelerated)
}

/// Per-class attribute scaling (BL-01,
/// `specs/2026-06-21-class-attributes-scaling-design.md`). A **permanent**
/// modifier re-applied each tick right after `Stats::reset_temp_modifiers`
/// (mirrors [`RacialTraits`]); needs no persistence because character level is
/// derived. `base_*` = L1 offset over the body base; `per_level_*` = slope
/// (accelerated past L50 via [`level_scaled`]). Damage is a small per-level
/// baseline only — gear + skills stay the dominant power (Diablo 4 / WoW
/// model).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct ClassAttributes {
    pub base_health: f32,
    pub per_level_health: f32,
    pub base_energy: f32,
    pub per_level_energy: f32,
    /// Fraction added to `attack_damage_modifier` per level (e.g. 0.005 =
    /// +0.5%/level), applied multiplicatively.
    pub per_level_damage: f32,
    /// Per-class multiplier on `energy_reward_modifier` (caster sustain tier);
    /// flat per class, not scaled by level.
    pub energy_reward_mult: f32,
    /// Combat resolution (BL-52). `base_*` = L1 value, `per_level_*` = slope
    /// (accelerated past L50 via `level_scaled`, like HP/energy). Additive into
    /// the matching `Stats` fields. Defaults 0.0 → Adventurer stays neutral.
    pub base_accuracy: f32,
    pub per_level_accuracy: f32,
    pub base_evasion: f32,
    pub per_level_evasion: f32,
    pub base_crit: f32,
    pub per_level_crit: f32,
    pub base_magic_accuracy: f32,
    pub per_level_magic_accuracy: f32,
    pub base_magic_evasion: f32,
    pub per_level_magic_evasion: f32,
}

impl Default for ClassAttributes {
    fn default() -> Self {
        Self {
            base_health: 0.0,
            per_level_health: 0.0,
            base_energy: 0.0,
            per_level_energy: 0.0,
            per_level_damage: 0.0,
            energy_reward_mult: 1.0,
            base_accuracy: 0.0,
            per_level_accuracy: 0.0,
            base_evasion: 0.0,
            per_level_evasion: 0.0,
            base_crit: 0.0,
            per_level_crit: 0.0,
            base_magic_accuracy: 0.0,
            per_level_magic_accuracy: 0.0,
            base_magic_evasion: 0.0,
            per_level_magic_evasion: 0.0,
        }
    }
}

impl ClassAttributes {
    /// Applies this class' scaling onto freshly-reset stats at `level` —
    /// that class's own level, not necessarily the character level (a
    /// multiclass character's primary and secondary each have their own).
    /// Must run right after `Stats::reset_temp_modifiers` so it stacks with
    /// buffs + racial passives (spec §6/§7.1). Call once per class the
    /// character holds (1 or 2): `is_primary` gates the flat `base_*` L1
    /// offsets, which come from the primary only — summing `base_*` from
    /// both classes would make holding a second class a free one-time
    /// bonus on top of a pure class of the same level, backwards from
    /// multiclass's whole point. `energy_reward_modifier` is deliberately
    /// NOT touched here: composing two classes' `energy_reward_mult` takes
    /// the max, not the product of two `apply` calls, so callers apply it
    /// once themselves after computing that max across every held class.
    pub fn apply(self, stats: &mut Stats, level: u16, is_primary: bool) {
        let base = |v: f32| if is_primary { v } else { 0.0 };
        stats.max_health_modifiers.add_mod +=
            base(self.base_health) + level_scaled(self.per_level_health, level);
        stats.max_energy_modifiers.add_mod +=
            base(self.base_energy) + level_scaled(self.per_level_energy, level);
        stats.attack_damage_modifier *= 1.0 + level_scaled(self.per_level_damage, level);
        // Same `level_scaled` curve as HP/energy above.
        stats.accuracy += base(self.base_accuracy) + level_scaled(self.per_level_accuracy, level);
        stats.evasion += base(self.base_evasion) + level_scaled(self.per_level_evasion, level);
        stats.crit_chance += base(self.base_crit) + level_scaled(self.per_level_crit, level);
        stats.magic_accuracy +=
            base(self.base_magic_accuracy) + level_scaled(self.per_level_magic_accuracy, level);
        stats.magic_evasion +=
            base(self.base_magic_evasion) + level_scaled(self.per_level_magic_evasion, level);
    }
}

/// Per-class attributes for `class` (one cache read). Do NOT call per-entity in
/// tick systems — hoist [`class_attributes_manifest`] once per run.
pub fn class_attributes(class: ClassKind) -> ClassAttributes {
    class_attributes_manifest()
        .0
        .get(&class)
        .copied()
        .unwrap_or_default()
}

/// One manifest read for per-tick consumers (mirrors
/// [`racial_traits_manifest`]).
pub fn class_attributes_manifest()
-> crate::assets::AssetReadGuard<Ron<HashMap<ClassKind, ClassAttributes>>> {
    Ron::<HashMap<ClassKind, ClassAttributes>>::load_expect("common.class.class_attributes").read()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playable_is_all_minus_adventurer() {
        assert_eq!(ClassKind::ALL.len(), ClassKind::PLAYABLE.len() + 1);
        for class in ClassKind::PLAYABLE {
            assert!(ClassKind::ALL.contains(&class));
        }
        assert!(ClassKind::ALL.contains(&ClassKind::Adventurer));
    }

    #[test]
    fn default_class_is_adventurer() {
        assert_eq!(ClassKind::default(), ClassKind::Adventurer);
        assert_eq!(CharacterClass::default().primary, ClassKind::Adventurer);
        assert_eq!(CharacterClass::default().secondary, None);
        assert!(!CharacterClass::default().is_multiclass());
    }

    #[test]
    fn character_class_single_and_multiclass_helpers() {
        let single = CharacterClass::single(ClassKind::Warrior);
        assert_eq!(single.classes().collect::<Vec<_>>(), vec![
            ClassKind::Warrior
        ]);
        assert!(single.has(ClassKind::Warrior));
        assert!(!single.has(ClassKind::Warlock));
        assert!(!single.is_multiclass());

        let multi = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 20,
        };
        assert_eq!(multi.classes().collect::<Vec<_>>(), vec![
            ClassKind::Warrior,
            ClassKind::Warlock
        ]);
        assert!(multi.has(ClassKind::Warrior));
        assert!(multi.has(ClassKind::Warlock));
        assert!(!multi.has(ClassKind::Mage));
        assert!(multi.is_multiclass());
    }

    #[test]
    fn single_class_primary_level_always_equals_character_level() {
        let single = CharacterClass::single(ClassKind::Warrior);
        for character_level in [1, 20, 40, 60] {
            assert_eq!(single.primary_level(character_level), character_level);
            assert_eq!(single.secondary_level, 0);
            assert_eq!(
                single.class_levels(character_level).collect::<Vec<_>>(),
                vec![(ClassKind::Warrior, character_level, true)]
            );
        }
    }

    #[test]
    fn multiclass_primary_level_is_character_level_minus_secondary() {
        // 40 primary + 20 secondary levels summing to a 60 character level.
        let multi = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 20,
        };
        assert_eq!(multi.primary_level(60), 40);
        assert_eq!(multi.class_levels(60).collect::<Vec<_>>(), vec![
            (ClassKind::Warrior, 40, true),
            (ClassKind::Warlock, 20, false)
        ]);
    }

    #[test]
    fn secondary_level_ignored_without_a_secondary_class() {
        // A stale `secondary_level` left behind by a bug must never make a
        // single-class character's primary level read low.
        let stale = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: None,
            secondary_level: 20,
        };
        assert_eq!(stale.primary_level(60), 60);
        assert_eq!(stale.class_levels(60).collect::<Vec<_>>(), vec![(
            ClassKind::Warrior,
            60,
            true
        )]);
    }

    #[test]
    fn set_secondary_level_clamps_to_character_level() {
        let mut multi = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 0,
        };
        multi.set_secondary_level(20, 60);
        assert_eq!(multi.secondary_level, 20);
        assert_eq!(multi.primary_level(60), 40);

        // Can never exceed the character's total level.
        multi.set_secondary_level(100, 60);
        assert_eq!(multi.secondary_level, 60);
        assert_eq!(multi.primary_level(60), 0);
    }

    #[test]
    fn keyword_round_trips_for_playable_classes() {
        for class in ClassKind::PLAYABLE {
            assert!(class.is_playable());
            assert_eq!(ClassKind::from_keyword(class.keyword()), Some(class));
        }
        // Adventurer is deliberately not re-pickable by keyword
        assert_eq!(ClassKind::from_keyword("adventurer"), None);
        assert_eq!(ClassKind::from_keyword("necromancer"), None);
    }

    #[test]
    fn racial_traits_manifest_loads_with_expected_values() {
        use crate::comp::body::humanoid::Species;
        // Spec §6 v1 values
        assert!(racial_traits(Species::Human).energy_reward_mult > 1.0);
        assert!(racial_traits(Species::Dwarf).damage_reduction_add > 0.0);
        assert!(racial_traits(Species::Elf).move_speed_mult > 1.0);
        assert!(racial_traits(Species::Orc).attack_damage_mult > 1.0);
        assert!(racial_traits(Species::Danari).max_energy_mult > 1.0);
        assert!(racial_traits(Species::Draugr).crowd_control_resistance_add > 0.0);
        assert!(racial_traits(Species::Elf).magic_resistance_add > 0.0);
        assert!(racial_traits(Species::Danari).magic_resistance_add > 0.0);
        assert!(racial_traits(Species::Draugr).magic_resistance_add > 0.0);
        // Dwarven Resilience is poison resistance, not charm resistance — must
        // not be mapped onto this field.
        assert_eq!(racial_traits(Species::Dwarf).magic_resistance_add, 0.0);
    }

    #[test]
    fn racial_traits_apply_to_stats() {
        use crate::comp::{Stats, body::humanoid::Species};
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());
        let mut stats = Stats::empty(body);
        let before = stats.attack_damage_modifier;
        apply_racial_traits(&mut stats, Species::Orc);
        assert!(stats.attack_damage_modifier > before);
    }

    #[test]
    fn racial_traits_apply_magic_resistance_to_stats() {
        use crate::comp::{Stats, body::humanoid::Species};
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());
        let mut stats = Stats::empty(body);
        assert_eq!(stats.magic_resistance, 0.0);
        apply_racial_traits(&mut stats, Species::Elf);
        assert!(stats.magic_resistance > 0.0);
    }

    #[test]
    fn level_scaled_is_linear_then_accelerates() {
        assert_eq!(level_scaled(10.0, 1), 0.0); // L1 = no growth
        assert_eq!(level_scaled(10.0, 50), 490.0); // 10 * 49 (linear)
        assert_eq!(level_scaled(10.0, 60), 740.0); // 10 * (49 + 2.5*10) — epic last 10
        assert_eq!(level_scaled(10.0, 0), 0.0); // clamp low
        assert_eq!(level_scaled(10.0, 100), 740.0); // clamp to L60
    }

    #[test]
    fn class_attributes_apply_per_class() {
        use crate::comp::Stats;
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());

        // Mage L60: energy add = 30 + 7*(49+25) = 548; HP add = 0 + 4*74 = 296.
        let mut mage = Stats::empty(body);
        class_attributes(ClassKind::Mage).apply(&mut mage, 60, true);
        assert_eq!(mage.max_energy_modifiers.add_mod, 548.0);
        assert_eq!(mage.max_health_modifiers.add_mod, 296.0);
        assert!(mage.attack_damage_modifier > 1.0); // per-level damage baseline
        // energy_reward_modifier composition (single- vs multi-class) is now
        // the caller's job, not apply()'s -- see the shipped rate directly.
        assert!((class_attributes(ClassKind::Mage).energy_reward_mult - 1.4).abs() < f32::EPSILON);

        // Warrior L1: HP add = base 40; energy stays tiny.
        let mut warrior1 = Stats::empty(body);
        class_attributes(ClassKind::Warrior).apply(&mut warrior1, 1, true);
        assert_eq!(warrior1.max_health_modifiers.add_mod, 40.0);
        // Warrior L60 energy = 0 + 2*74 = 148 (can't spam high-circle spells).
        let mut warrior60 = Stats::empty(body);
        class_attributes(ClassKind::Warrior).apply(&mut warrior60, 60, true);
        assert_eq!(warrior60.max_energy_modifiers.add_mod, 148.0);
        // Warrior is far tankier than Mage at L60.
        assert!(warrior60.max_health_modifiers.add_mod > mage.max_health_modifiers.add_mod);
        // ...and the Mage has far more energy.
        assert!(mage.max_energy_modifiers.add_mod > warrior60.max_energy_modifiers.add_mod);

        // Adventurer (legacy default) = neutral no-op.
        let mut adv = Stats::empty(body);
        class_attributes(ClassKind::Adventurer).apply(&mut adv, 60, true);
        assert_eq!(adv.max_energy_modifiers.add_mod, 0.0);
        assert_eq!(adv.max_health_modifiers.add_mod, 0.0);
        assert_eq!(adv.attack_damage_modifier, 1.0);
        assert_eq!(adv.accuracy, 0.0);
        assert_eq!(adv.evasion, 0.0);
    }

    #[test]
    fn class_attributes_apply_base_only_from_primary() {
        // A "secondary" application (is_primary = false) must not add the
        // flat base_* offset -- only the per-level-scaled growth.
        use crate::comp::Stats;
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());

        let mut warrior_secondary = Stats::empty(body);
        class_attributes(ClassKind::Warrior).apply(&mut warrior_secondary, 20, false);
        // per_level_health = 12, base_health = 40 (from class_attributes.ron)
        // -- base must be excluded, only 12 * level_scaled(20) contributes.
        let expected_per_level_only = crate::comp::class::level_scaled(12.0, 20);
        assert_eq!(
            warrior_secondary.max_health_modifiers.add_mod,
            expected_per_level_only
        );
        assert_ne!(
            warrior_secondary.max_health_modifiers.add_mod,
            40.0 + expected_per_level_only
        );
    }

    #[test]
    fn class_attributes_apply_combat_resolution() {
        // BL-52: accuracy/evasion/crit + magic_* scale per class via level_scaled.
        use crate::comp::Stats;
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());

        // Warrior L60 accuracy = 5 + 0.50*(49+25) = 42.
        let mut warrior = Stats::empty(body);
        class_attributes(ClassKind::Warrior).apply(&mut warrior, 60, true);
        assert!((warrior.accuracy - 42.0).abs() < 1e-3);

        // Rogue out-evades and out-crits the Warrior; Mage out-magics it.
        let mut rogue = Stats::empty(body);
        class_attributes(ClassKind::Rogue).apply(&mut rogue, 60, true);
        let mut mage = Stats::empty(body);
        class_attributes(ClassKind::Mage).apply(&mut mage, 60, true);
        assert!(rogue.evasion > warrior.evasion);
        assert!(rogue.crit_chance > warrior.crit_chance);
        assert!(mage.magic_accuracy > warrior.magic_accuracy);
        // Everyone crits at least a little at L60 (no dump stat).
        assert!(mage.crit_chance > 0.0);
        // BL-52 P3 identity: the magic/physical to-hit split is meaningful — a
        // caster leans on magic accuracy, a martial on physical.
        assert!(mage.magic_accuracy > mage.accuracy);
        assert!(warrior.accuracy > warrior.magic_accuracy);
    }
}
