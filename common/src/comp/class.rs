//! Character class identity. See
//! docs/design/specs/2026-06-10-classes-races-design.md.
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

use crate::{
    assets::{AssetExt, Ron},
    comp::{
        Stats,
        body::humanoid::Species,
        inventory::item::tool::{Hands, ToolKind, ToolKindMask, WeaponRole},
        skillset::{MAX_CHARACTER_LEVEL, SkillGroupKind, SkillSet},
    },
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

    /// The cross-class equipment taxonomy group this class belongs to: a
    /// real, queryable grouping used for equip-gating decisions, not just a
    /// documentation label. `None` only for [`ClassKind::Adventurer`], which
    /// is not playable and has no equipment-gating identity of its own —
    /// every one of the 14 playable classes maps to exactly one group.
    /// Deliberately no `_` arm: a future `ClassKind` variant fails to
    /// compile here until someone decides which group it belongs to.
    pub fn equipment_group(self) -> Option<EquipmentGroup> {
        match self {
            ClassKind::Adventurer => None,
            ClassKind::Warrior
            | ClassKind::Barbarian
            | ClassKind::Paladin
            | ClassKind::BloodSlayer => Some(EquipmentGroup::HeavyMartial),
            ClassKind::Rogue | ClassKind::Ranger | ClassKind::Bard => {
                Some(EquipmentGroup::LightFinesse)
            },
            ClassKind::Monk => Some(EquipmentGroup::Disciplined),
            ClassKind::Artificer => Some(EquipmentGroup::Artisan),
            ClassKind::Mage | ClassKind::Sorcerer | ClassKind::Warlock => {
                Some(EquipmentGroup::ArcaneCaster)
            },
            ClassKind::Cleric => Some(EquipmentGroup::DivineCaster),
            ClassKind::Druid => Some(EquipmentGroup::PrimordialCaster),
        }
    }
}

/// Cross-class equipment taxonomy: "martial gear is gated by ROLE and
/// shared broadly; caster implements are gated per IMPLEMENT and shared
/// narrowly." One group per playable class — see [`ClassKind::equipment_group`]
/// for the mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EquipmentGroup {
    /// Warrior, Barbarian, Paladin, BloodSlayer. All martial gear ungated;
    /// Paladin alone also carries a HolySymbol.
    HeavyMartial,
    /// Rogue, Ranger, Bard. All martial gear ungated; Ranger carries a
    /// Focus, Bard an Instrument.
    LightFinesse,
    /// Monk. All martial gear ungated, plus the martial-role Staff that no
    /// other class in this group has access to.
    Disciplined,
    /// Artificer. All martial gear ungated (Pick/Farming included).
    Artisan,
    /// Mage, Sorcerer, Warlock. Martial gear ungated but heavily
    /// proficiency-penalised; caster Staff (all three) and Tome (Mage
    /// only).
    ArcaneCaster,
    /// Cleric. Martial gear ungated but penalised; caster Sceptre and
    /// HolySymbol.
    DivineCaster,
    /// Druid — the widest caster (Staff, Sceptre and Focus). Martial gear
    /// ungated but penalised.
    PrimordialCaster,
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
    /// Set-and-forget toggle: when `true`, character levels earned AFTER the
    /// multiclass grant are routed to the secondary class instead of the
    /// primary. Always `false` on grant (opt-in) and forced `false` for
    /// single-class characters, since there is no secondary to route to.
    pub future_levels_to_secondary: bool,
}

impl CharacterClass {
    pub fn single(class: ClassKind) -> Self {
        Self {
            primary: class,
            secondary: None,
            secondary_level: 0,
            future_levels_to_secondary: false,
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

    /// Sets the "future levels" routing preference. Forced to `false` when
    /// there is no secondary class, so the flag can never be left dangling
    /// in a `true` state that would silently activate the moment a
    /// secondary is later granted.
    pub fn set_future_levels_to_secondary(&mut self, value: bool) {
        self.future_levels_to_secondary = value && self.is_multiclass();
    }

    /// Call once per character-level-up (i.e. when `character_level` has
    /// just increased by `levels_gained`). Routes the newly earned levels to
    /// the secondary class's bank when the player has opted in via
    /// [`Self::set_future_levels_to_secondary`]; otherwise the new levels
    /// fall to the primary automatically, since `primary_level` is derived
    /// by subtraction and needs no action. A no-op for single-class
    /// characters.
    pub fn route_levels_gained(&mut self, levels_gained: u16, character_level: u16) {
        if self.future_levels_to_secondary && levels_gained > 0 {
            self.set_secondary_level(self.secondary_level + levels_gained, character_level);
        }
    }

    /// The union (bitwise OR) of every held class's weapon-proficiency mask,
    /// looked up in the already-hoisted `manifest` (see
    /// [`class_proficiencies_manifest`]'s own doc comment — never pass a
    /// manifest freshly loaded per entity). A class absent from `manifest`
    /// resolves to [`ClassProficiencies::All`] (permissive fail-open),
    /// mirroring [`class_proficiencies`]'s own missing-key rule. Iterates
    /// [`Self::classes`] so a multiclass character's mask covers both held
    /// classes, not just `primary`.
    pub fn proficient_tools_mask(
        &self,
        manifest: &HashMap<ClassKind, ClassProficiencies>,
    ) -> ToolKindMask {
        self.classes().fold(ToolKindMask::empty(), |mask, class| {
            mask | manifest
                .get(&class)
                .map_or(ToolKindMask::all(), ClassProficiencies::mask)
        })
    }
}

impl Component for CharacterClass {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

/// Minimum character level required to accept a second class. Multiclassing
/// is never a character-creation pick — it is granted in play, always to an
/// existing single-class character — so this alone does not gate anything by
/// itself; it composes with whatever narrative content offers the grant.
pub const MULTICLASS_MIN_LEVEL: u16 = 20;

/// The error a rejected [`grant_second_class`] call returns, so callers can
/// render a specific message rather than a generic failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticlassError {
    AlreadyMulticlass,
    SameAsPrimary,
    NoPrimaryClass,
    BelowMinimumLevel,
}

/// Grants `class` as `character_class`'s secondary, reassigning
/// `initial_secondary_level` of the character's already-earned levels to it
/// (the one-time, grant-moment reallocation of banked levels — the player's
/// choice of how to split their current total, not a forced 50/50 or a
/// from-1 start). Also unlocks the secondary's skill group so its tree, XP
/// pool, and skill points exist from the moment of the grant. Mutates
/// nothing and returns an error if any guard fails.
pub fn grant_second_class(
    character_class: &mut CharacterClass,
    skill_set: &mut SkillSet,
    class: ClassKind,
    initial_secondary_level: u16,
) -> Result<(), MulticlassError> {
    if character_class.is_multiclass() {
        return Err(MulticlassError::AlreadyMulticlass);
    }
    if class == character_class.primary {
        return Err(MulticlassError::SameAsPrimary);
    }
    if character_class.primary == ClassKind::Adventurer {
        return Err(MulticlassError::NoPrimaryClass);
    }
    let character_level = skill_set.character_level();
    if character_level < MULTICLASS_MIN_LEVEL {
        return Err(MulticlassError::BelowMinimumLevel);
    }

    character_class.secondary = Some(class);
    character_class.set_secondary_level(initial_secondary_level, character_level);
    skill_set.unlock_skill_group(SkillGroupKind::Class(class));
    Ok(())
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

/// One proficiency entry as written in the RON. `Any` covers every grip (and
/// every role) of a tool kind; `Handed` restricts to one grip (today only
/// meaningful for `Sword`, whose 1h/2h assets share a `ToolKind`); `Roled`
/// restricts to one `WeaponRole` (today only meaningful for `Staff`/
/// `Sceptre`, whose caster and martial kits share a `ToolKind`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum ProficientTool {
    Any(ToolKind),
    Handed(ToolKind, Hands),
    Roled(ToolKind, WeaponRole),
}

/// A class's weapon proficiency (soft gate: non-proficient use is never
/// blocked, only penalised elsewhere). `All` is the legacy-permissive value
/// used by `Adventurer`, and is future-proof against new `ToolKind` variants
/// in a way an exhaustive list would not be.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum ClassProficiencies {
    All,
    Only(Vec<ProficientTool>),
}

impl ClassProficiencies {
    pub fn mask(&self) -> ToolKindMask {
        match self {
            ClassProficiencies::All => ToolKindMask::all(),
            ClassProficiencies::Only(tools) => {
                tools.iter().fold(ToolKindMask::empty(), |mask, tool| {
                    mask | match *tool {
                        ProficientTool::Any(kind) => ToolKindMask::for_tool(kind, None, None),
                        ProficientTool::Handed(kind, hands) => {
                            ToolKindMask::for_tool(kind, Some(hands), None)
                        },
                        ProficientTool::Roled(kind, role) => {
                            ToolKindMask::for_tool(kind, None, Some(role))
                        },
                    }
                })
            },
        }
    }
}

/// Per-class weapon proficiency for `class` (one cache read). A class
/// missing from the manifest resolves to [`ClassProficiencies::All`]
/// (permissive fail-open) — a typo in the RON must never silently mute a
/// class's own proficiency. Do NOT call per-entity in tick systems — hoist
/// [`class_proficiencies_manifest`] once per run.
pub fn class_proficiencies(class: ClassKind) -> ClassProficiencies {
    class_proficiencies_manifest()
        .0
        .get(&class)
        .cloned()
        .unwrap_or(ClassProficiencies::All)
}

/// One manifest read for per-tick consumers (mirrors
/// [`class_attributes_manifest`]).
pub fn class_proficiencies_manifest()
-> crate::assets::AssetReadGuard<Ron<HashMap<ClassKind, ClassProficiencies>>> {
    Ron::<HashMap<ClassKind, ClassProficiencies>>::load_expect("common.class.class_proficiencies")
        .read()
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
            future_levels_to_secondary: false,
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
            future_levels_to_secondary: false,
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
            future_levels_to_secondary: false,
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
            future_levels_to_secondary: false,
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
    fn grant_second_class_succeeds_and_reallocates_levels() {
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);

        let result =
            grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warlock, 20);
        assert_eq!(result, Ok(()));
        assert_eq!(character_class.secondary, Some(ClassKind::Warlock));
        assert_eq!(character_class.secondary_level, 20);
        assert_eq!(character_class.primary_level(40), 20);
        assert!(
            skill_set
                .skill_groups()
                .any(|sg| sg.skill_group_kind == SkillGroupKind::Class(ClassKind::Warlock))
        );
    }

    #[test]
    fn grant_second_class_rejects_already_multiclass() {
        let mut character_class = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 20,
            future_levels_to_secondary: false,
        };
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);

        assert_eq!(
            grant_second_class(&mut character_class, &mut skill_set, ClassKind::Mage, 10),
            Err(MulticlassError::AlreadyMulticlass)
        );
    }

    #[test]
    fn grant_second_class_rejects_same_as_primary() {
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);

        assert_eq!(
            grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warrior, 10),
            Err(MulticlassError::SameAsPrimary)
        );
    }

    #[test]
    fn grant_second_class_rejects_adventurer_primary() {
        let mut character_class = CharacterClass::single(ClassKind::Adventurer);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);

        assert_eq!(
            grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warlock, 10),
            Err(MulticlassError::NoPrimaryClass)
        );
    }

    #[test]
    fn grant_second_class_rejects_below_minimum_level() {
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(MULTICLASS_MIN_LEVEL - 1);

        assert_eq!(
            grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warlock, 5),
            Err(MulticlassError::BelowMinimumLevel)
        );
        // Rejected: nothing should have been mutated.
        assert!(!character_class.is_multiclass());
    }

    #[test]
    fn future_levels_toggle_forced_off_without_a_secondary() {
        let mut single = CharacterClass::single(ClassKind::Warrior);
        single.set_future_levels_to_secondary(true);
        assert!(!single.future_levels_to_secondary);
    }

    #[test]
    fn future_levels_toggle_routes_new_levels_to_secondary() {
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);
        grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warlock, 20).unwrap();
        character_class.set_future_levels_to_secondary(true);

        // 3 new character levels, all routed to the secondary.
        character_class.route_levels_gained(3, 43);
        assert_eq!(character_class.secondary_level, 23);
        assert_eq!(character_class.primary_level(43), 20);
    }

    #[test]
    fn future_levels_toggle_off_leaves_new_levels_on_primary() {
        // Default (opt-out) behaviour: the primary absorbs new levels
        // automatically since `primary_level` is derived by subtraction, so
        // `route_levels_gained` must be a no-op here.
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        let mut skill_set = SkillSet::default();
        skill_set.set_level(40);
        grant_second_class(&mut character_class, &mut skill_set, ClassKind::Warlock, 20).unwrap();

        character_class.route_levels_gained(3, 43);
        assert_eq!(character_class.secondary_level, 20);
        assert_eq!(character_class.primary_level(43), 23);
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

    #[test]
    fn class_proficiencies_manifest_covers_every_class() {
        // Every ClassKind::ALL variant must be an actual key in the RON
        // file, not merely resolve via the permissive fallback — a class
        // silently missing from the manifest is a data bug even though the
        // fallback happens to be safe.
        let manifest = class_proficiencies_manifest();
        for class in ClassKind::ALL {
            assert!(
                manifest.0.contains_key(&class),
                "class_proficiencies.ron is missing a row for {class:?}"
            );
        }
    }

    #[test]
    fn adventurer_is_permissive_on_weapon_proficiency() {
        assert_eq!(
            class_proficiencies(ClassKind::Adventurer).mask(),
            ToolKindMask::all()
        );
    }

    #[test]
    fn sword_proficiency_respects_the_1h_2h_split() {
        let rogue = class_proficiencies(ClassKind::Rogue).mask();
        assert!(rogue.allows(ToolKind::Sword, Some(Hands::One), None));
        assert!(!rogue.allows(ToolKind::Sword, Some(Hands::Two), None));

        let warrior = class_proficiencies(ClassKind::Warrior).mask();
        assert!(warrior.allows(ToolKind::Sword, Some(Hands::One), None));
        assert!(warrior.allows(ToolKind::Sword, Some(Hands::Two), None));
    }

    /// Mirrors `sword_proficiency_respects_the_1h_2h_split` for the
    /// `WeaponRole` axis: Monk is proficient with the martial staff kit and
    /// not the caster kit; Mage is the exact inverse -- both narrowed from
    /// the old blanket `Any(Staff)` in `class_proficiencies.ron`.
    #[test]
    fn staff_proficiency_respects_the_caster_martial_split() {
        let monk = class_proficiencies(ClassKind::Monk).mask();
        assert!(monk.allows(ToolKind::Staff, None, Some(WeaponRole::Martial)));
        assert!(!monk.allows(ToolKind::Staff, None, Some(WeaponRole::Caster)));

        let mage = class_proficiencies(ClassKind::Mage).mask();
        assert!(mage.allows(ToolKind::Staff, None, Some(WeaponRole::Caster)));
        assert!(!mage.allows(ToolKind::Staff, None, Some(WeaponRole::Martial)));
    }

    #[test]
    fn tool_kind_mask_for_tool_is_injective_across_kinds() {
        // Every ToolKind, hand-written (not derived from an iterator over
        // the enum) so a new variant added without extending this array is
        // caught here instead of silently passing. Fixed at grip
        // `Hands::Two` / each kind's own default role throughout -- for
        // every kind other than `Sword`/`Staff`/`Sceptre` the grip and role
        // are both ignored and a single bit comes back; for `Sword` this
        // grip yields only `SWORD_2H`, never `SWORD_1H`, and for
        // `Staff`/`Sceptre` the default (caster) role yields only their
        // `_CASTER` bit, never `_MARTIAL`.
        let kinds = [
            ToolKind::Sword,
            ToolKind::Axe,
            ToolKind::Hammer,
            ToolKind::Bow,
            ToolKind::Staff,
            ToolKind::Sceptre,
            ToolKind::Tome,
            ToolKind::HolySymbol,
            ToolKind::Focus,
            ToolKind::Dagger,
            ToolKind::Shield,
            ToolKind::Spear,
            ToolKind::Blowgun,
            ToolKind::Debug,
            ToolKind::Farming,
            ToolKind::Pick,
            ToolKind::Shovel,
            ToolKind::Instrument,
            ToolKind::Throwable,
            ToolKind::Natural,
            ToolKind::Empty,
        ];
        let union = kinds.iter().fold(ToolKindMask::empty(), |mask, &k| {
            mask | ToolKindMask::for_tool(k, Some(Hands::Two), Some(k.default_role()))
        });
        // 21 ToolKind variants, one bit each at this fixed grip/role --
        // `Sword` contributes only `SWORD_2H` (its `SWORD_1H` bit is only
        // reachable via `Hands::One` or `None`, checked separately below),
        // and `Staff`/`Sceptre` each contribute only their `_CASTER` bit
        // (their `_MARTIAL` bit is only reachable via an explicit
        // `WeaponRole::Martial` or `None`, also checked below). So this
        // union can never equal `ToolKindMask::all()` (24 bits): it is
        // exactly `all()` minus those three unreached bits. If any two
        // kinds collided on the same bit, this count would be lower than 21.
        assert_eq!(
            union,
            ToolKindMask::all()
                .difference(ToolKindMask::SWORD_1H)
                .difference(ToolKindMask::STAFF_MARTIAL)
                .difference(ToolKindMask::SCEPTRE_MARTIAL)
        );
        assert_eq!(union.bits().count_ones(), 21);

        // The grip split itself: 1h and 2h swords are different bits, and
        // an unknown grip (`None`) is the union of both.
        assert_ne!(
            ToolKindMask::for_tool(ToolKind::Sword, Some(Hands::One), None),
            ToolKindMask::for_tool(ToolKind::Sword, Some(Hands::Two), None)
        );
        assert_eq!(
            ToolKindMask::for_tool(ToolKind::Sword, None, None),
            ToolKindMask::for_tool(ToolKind::Sword, Some(Hands::One), None)
                | ToolKindMask::for_tool(ToolKind::Sword, Some(Hands::Two), None)
        );

        // The role split, mirrored for Staff and Sceptre: caster and
        // martial are different bits, and an unknown role (`None`) is the
        // union of both.
        for kind in [ToolKind::Staff, ToolKind::Sceptre] {
            assert_ne!(
                ToolKindMask::for_tool(kind, None, Some(WeaponRole::Caster)),
                ToolKindMask::for_tool(kind, None, Some(WeaponRole::Martial))
            );
            assert_eq!(
                ToolKindMask::for_tool(kind, None, None),
                ToolKindMask::for_tool(kind, None, Some(WeaponRole::Caster))
                    | ToolKindMask::for_tool(kind, None, Some(WeaponRole::Martial))
            );
        }
    }

    // `Handed(kind, _)` is meaningless for any `ToolKind` whose grips
    // resolve to the same bit -- it parses and loads but silently behaves
    // exactly like `Any(kind)`, discarding a designer's intended
    // restriction with no error anywhere. Walk every manifest row and fail
    // loudly on that shape.
    #[test]
    fn class_proficiencies_manifest_never_uses_handed_on_a_kind_without_a_grip_split() {
        let manifest = class_proficiencies_manifest();
        for (class, proficiencies) in manifest.0.iter() {
            let ClassProficiencies::Only(tools) = proficiencies else {
                continue;
            };
            for tool in tools {
                let ProficientTool::Handed(kind, _) = *tool else {
                    continue;
                };
                let one = ToolKindMask::for_tool(kind, Some(Hands::One), None);
                let two = ToolKindMask::for_tool(kind, Some(Hands::Two), None);
                assert_ne!(
                    one, two,
                    "class_proficiencies.ron: {class:?} uses Handed({kind:?}, _), but {kind:?} \
                     has no grip split (both hands resolve to the same bit) -- Handed is \
                     meaningless here and silently behaves like Any({kind:?}). Use Any({kind:?}) \
                     instead, or give {kind:?} its own grip-split bits like Sword's \
                     SWORD_1H/SWORD_2H."
                );
            }
        }
    }

    // Mirrors the `Handed` guard above for the `Roled` axis: `Roled(kind,
    // _)` is meaningless for any `ToolKind` whose roles resolve to the same
    // bit.
    #[test]
    fn class_proficiencies_manifest_never_uses_roled_on_a_kind_without_a_role_split() {
        let manifest = class_proficiencies_manifest();
        for (class, proficiencies) in manifest.0.iter() {
            let ClassProficiencies::Only(tools) = proficiencies else {
                continue;
            };
            for tool in tools {
                let ProficientTool::Roled(kind, _) = *tool else {
                    continue;
                };
                let caster = ToolKindMask::for_tool(kind, None, Some(WeaponRole::Caster));
                let martial = ToolKindMask::for_tool(kind, None, Some(WeaponRole::Martial));
                assert_ne!(
                    caster, martial,
                    "class_proficiencies.ron: {class:?} uses Roled({kind:?}, _), but {kind:?} has \
                     no role split (both roles resolve to the same bit) -- Roled is meaningless \
                     here and silently behaves like Any({kind:?}). Use Any({kind:?}) instead, or \
                     give {kind:?} its own role-split bits like Staff's \
                     STAFF_CASTER/STAFF_MARTIAL."
                );
            }
        }
    }

    #[test]
    fn multiclass_proficiency_mask_unions_across_both_held_classes() {
        let multi = CharacterClass {
            primary: ClassKind::Cleric,
            secondary: Some(ClassKind::Mage),
            secondary_level: 20,
            future_levels_to_secondary: false,
        };

        let proficiencies = class_proficiencies_manifest();
        let proficiency_mask = multi.proficient_tools_mask(&proficiencies.0);
        let cleric_mask = class_proficiencies(ClassKind::Cleric).mask();
        let mage_mask = class_proficiencies(ClassKind::Mage).mask();
        assert_eq!(proficiency_mask, cleric_mask | mage_mask);
        // Sanity: the union must be a strict superset of either class alone
        // whenever they differ (Cleric carries HolySymbol, Mage carries Tome).
        assert!(proficiency_mask.contains(ToolKindMask::HOLY_SYMBOL));
        assert!(proficiency_mask.contains(ToolKindMask::TOME));
    }

    #[test]
    fn equipment_group_covers_every_playable_class() {
        for class in ClassKind::PLAYABLE {
            assert!(
                class.equipment_group().is_some(),
                "{class:?} has no equipment group"
            );
        }
        assert_eq!(ClassKind::Adventurer.equipment_group(), None);
    }

    #[test]
    fn equipment_group_matches_the_locked_grouping_table() {
        use EquipmentGroup::*;
        let expected = [
            (ClassKind::Warrior, HeavyMartial),
            (ClassKind::Barbarian, HeavyMartial),
            (ClassKind::Paladin, HeavyMartial),
            (ClassKind::BloodSlayer, HeavyMartial),
            (ClassKind::Rogue, LightFinesse),
            (ClassKind::Ranger, LightFinesse),
            (ClassKind::Bard, LightFinesse),
            (ClassKind::Monk, Disciplined),
            (ClassKind::Artificer, Artisan),
            (ClassKind::Mage, ArcaneCaster),
            (ClassKind::Sorcerer, ArcaneCaster),
            (ClassKind::Warlock, ArcaneCaster),
            (ClassKind::Cleric, DivineCaster),
            (ClassKind::Druid, PrimordialCaster),
        ];
        assert_eq!(expected.len(), ClassKind::PLAYABLE.len());
        for (class, group) in expected {
            assert_eq!(
                class.equipment_group(),
                Some(group),
                "{class:?} should be in {group:?}"
            );
        }
    }

    /// `EquipmentGroup` and `assets/common/equip_gates.ron` are two
    /// independent data sources describing the same reality — the manifest
    /// is what actually gates equipping, `equipment_group()` is a coarser
    /// classification for display/reasoning. Nothing forces them to stay in
    /// sync. This test is that consumer: every class an implement's
    /// whitelist names must be explainable either by that implement's
    /// "home" `EquipmentGroup` or by one of the small set of documented
    /// per-group secondary-access exceptions (Paladin+HolySymbol,
    /// Ranger+Focus, Druid+Staff, Druid+Sceptre — see each `EquipmentGroup`
    /// variant's own doc comment).
    /// A future manifest edit that grants some class access to a caster
    /// implement without also updating either the mapping or this exception
    /// list fails here instead of silently drifting unnoticed.
    #[test]
    fn equipment_group_and_the_equip_gates_manifest_agree_on_who_gets_which_caster_implement() {
        use crate::comp::inventory::item::{ToolKind, WeaponRole, equip_gates_manifest};

        // (implement, its "home" EquipmentGroup, extra classes documented as
        // a per-group secondary-access exception on top of the home group)
        let implements = [
            (ToolKind::Staff, EquipmentGroup::ArcaneCaster, vec![
                ClassKind::Druid,
            ]),
            (ToolKind::Sceptre, EquipmentGroup::DivineCaster, vec![
                ClassKind::Druid,
            ]),
            (ToolKind::Tome, EquipmentGroup::ArcaneCaster, vec![]),
            (ToolKind::HolySymbol, EquipmentGroup::DivineCaster, vec![
                ClassKind::Paladin,
            ]),
            (ToolKind::Focus, EquipmentGroup::PrimordialCaster, vec![
                ClassKind::Ranger,
            ]),
        ];

        let manifest = equip_gates_manifest();
        for (tool_kind, home_group, exceptions) in implements {
            let Some(whitelist) = manifest
                .0
                .get(&(tool_kind, WeaponRole::Caster))
                .cloned()
                .flatten()
            else {
                // No whitelist at all (or an absent key) means ungated --
                // nothing to cross-check for this implement.
                continue;
            };
            for class in whitelist {
                let in_home_group = class.equipment_group() == Some(home_group);
                let is_documented_exception = exceptions.contains(&class);
                assert!(
                    in_home_group || is_documented_exception,
                    "{tool_kind:?}'s equip_gates.ron whitelist grants {class:?} access, but \
                     {class:?} is neither in {home_group:?} (its expected home group) nor a \
                     documented exception for {tool_kind:?} -- update either the manifest, \
                     equipment_group(), or this test's exception list"
                );
            }
        }
    }
}
