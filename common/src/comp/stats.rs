use common_i18n::Content;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};
use std::{error::Error, fmt};

use crate::{
    combat::{AttackEffect, AttackedModification, CombatRequirement, DamageKind, StatEffect},
    comp::{
        buff::SenseMode, creature_type::CreatureKind, detection::SenseKind,
        projectile::ProjectileConstructorEffect,
    },
    uid::Uid,
};

use super::Body;

/// Combat resolution (BL-52 P3): the typed elemental resistance channels that
/// mitigate **area** damage (the AoE counterpart to single-target evasion;
/// physical damage is mitigated by the existing armor `damage_reduction`, so it
/// is deliberately not a channel here — no double-count). Used by
/// `BuffEffect::Resistance` and content/gear sources.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResistKind {
    Fire,
    Frost,
    Poison,
    /// Catch-all for arcane / non-physical magic damage.
    Magic,
}

#[derive(Debug)]
#[expect(dead_code)] // TODO: remove once trade sim hits master
pub enum StatChangeError {
    Underflow,
    Overflow,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct StatsModifier {
    pub add_mod: f32,
    pub mult_mod: f32,
}

impl Default for StatsModifier {
    fn default() -> Self {
        Self {
            add_mod: 0.0,
            mult_mod: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct StatsSplit {
    pub pos_mod: f32,
    pub neg_mod: f32,
}

impl Default for StatsSplit {
    fn default() -> Self {
        Self {
            pos_mod: 0.0,
            neg_mod: 0.0,
        }
    }
}

impl StatsSplit {
    pub fn modifier(&self) -> f32 { self.pos_mod + self.neg_mod }
}

impl StatsModifier {
    pub fn compute_maximum(&self, base_value: f32) -> f32 {
        base_value * self.mult_mod + self.add_mod
    }

    // Note: unused for now
    pub fn update_maximum(&self) -> bool {
        self.add_mod.abs() > f32::EPSILON || (self.mult_mod - 1.0).abs() > f32::EPSILON
    }
}

impl fmt::Display for StatChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match self {
            Self::Underflow => "insufficient stat quantity",
            Self::Overflow => "stat quantity would overflow",
        })
    }
}
impl Error for StatChangeError {}

/// Declares that this entity is currently maintaining a magical sense (e.g.
/// via an active `Detect Magic`-style buff). This is the harmless-to-broadcast
/// *declaration* that a sense is active — it says nothing about what was
/// found, only that detection is happening (which the caster's own glow VFX
/// would reveal anyway). The *result* of the sense — the actual set of
/// revealed entities/points — lives in the owner-private `Detected`
/// component, never here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveSense {
    pub kind: SenseKind,
    pub radius: f32,
    pub mode: SenseMode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stats {
    pub name: Content,
    pub original_body: Body,
    pub damage_reduction: StatsSplit,
    pub poise_reduction: StatsSplit,
    pub max_health_modifiers: StatsModifier,
    pub move_speed_modifier: f32,
    pub charge_move_speed_modifier: f32,
    pub buildup_move_speed_modifier: f32,
    pub jump_modifier: f32,
    pub attack_speed_modifier: f32,
    pub charge_speed_modifier: f32,
    pub buildup_speed_modifier: f32,
    pub recovery_speed_modifier: f32,
    pub friction_modifier: f32,
    pub max_energy_modifiers: StatsModifier,
    pub poise_damage_modifier: f32,
    pub attack_damage_modifier: f32,
    pub conditional_precision_modifiers: Vec<(Option<CombatRequirement>, f32, bool)>,
    pub precision_vulnerability_multiplier_override: Option<f32>,
    pub swim_speed_modifier: f32,
    /// This adds effects to any attacks that the entity makes
    pub effects_on_attack: Vec<AttackEffect>,
    /// This is the fraction of damage reduction (from armor and other buffs)
    /// that gets ignored by attacks from this entity
    pub mitigations_penetration: f32,
    pub energy_reward_modifier: f32,
    pub energy_efficiency_modifier: f32,
    /// This creates effects when the entity is damaged
    pub effects_on_damaged: Vec<StatEffect>,
    /// This creates effects when the entity is killed
    pub effects_on_death: Vec<StatEffect>,
    pub disable_auxiliary_abilities: bool,
    /// Antimagic (BL-36): when set, magic abilities (those with an
    /// `AbilityMeta` `source`) can't be activated and attuned magic-item
    /// effects are suppressed. Physical/innate abilities are unaffected.
    /// Set each tick by `BuffEffect::DisableMagic`.
    pub disable_magic: bool,
    /// Dimensional anchor (BL-05): when set, teleport/blink abilities can't
    /// resolve. Set each tick by `BuffEffect::DisableTeleport`.
    pub disable_teleport: bool,
    /// Combat resolution (BL-52) — per-tick to-hit/crit modifiers (not
    /// persisted), sourced from class+level (`ClassAttributes`), gear and
    /// buffs. Consumed in `Attack::apply_attack`. `accuracy`/`evasion` drive
    /// the physical to-hit roll; `magic_*` the single-target spell roll
    /// (P3); `crit_chance` the crit roll (P2; magnitude stays in
    /// `precision_power`).
    pub accuracy: f32,
    pub evasion: f32,
    pub magic_accuracy: f32,
    pub magic_evasion: f32,
    pub crit_chance: f32,
    /// Cached `SkillSet::character_level()`, re-derived each tick in the same
    /// slot as the class/level attribute scaling above. 0 for bodies with no
    /// class/skill set. Exists so combat code with a `Stats` in scope (e.g.
    /// `CombatRequirement::CasterLevelRoll`) can read the caster's level
    /// without threading `SkillSet` through the whole attack pipeline.
    pub character_level: u16,
    /// Combat resolution (BL-52 P3) — typed elemental resistance (fraction in
    /// `0.0..`), reset per tick like the other modifiers. Mitigates **AoE**
    /// damage of the matching kind in `apply_attack` (soft-capped); physical
    /// AoE uses the existing `damage_reduction` instead. Set by
    /// `BuffEffect::Resistance` (+ gear/content later).
    pub resist_fire: f32,
    pub resist_frost: f32,
    pub resist_poison: f32,
    pub resist_magic: f32,
    pub crowd_control_resistance: f32,
    /// Additive resistance term (fraction in `0.0..`) against resisted
    /// mind-altering effects such as charm/domination-style buffs, distinct
    /// from `resist_magic` (which mitigates AoE spell damage magnitude, not
    /// a resisted effect's chance to land). Sourced from racial passives
    /// (`RacialTraits::apply`); consumed by the resist-chance roll for those
    /// effects.
    pub magic_resistance: f32,
    pub item_effect_reduction: f32,
    /// This modifies attacks that target this entity
    pub attacked_modifications: Vec<AttackedModification>,
    pub precision_power_mult: f32,
    pub knockback_mult: f32,
    /// Dedicated caster channels, per-tick (not persisted), multiplicative
    /// (1.0 = no change). `spell_power` scales **magic-source** outgoing
    /// damage only (gated by the `is_magic` ability signal in
    /// `apply_attack`), so caster damage passives don't leak onto weapon
    /// swings. `heal_power` scales healing the entity does
    /// (`CombatEffect::Heal`). Kept separate from `attack_damage_modifier` so
    /// a smiter Cleric and a pure healer scale independently.
    pub spell_power: f32,
    pub heal_power: f32,
    /// Extra outgoing damage vs targets of a given `CreatureKind`
    /// (`Stats.creature_kind`), indexed by the kind's discriminant. Per-tick
    /// (not persisted), additive fraction per slot (0.0 = none). Applied in
    /// `apply_attack` by indexing with the target's `creature_kind` — the
    /// Cleric's smite is the `Undead` slot; other slayer-style conditionals
    /// (e.g. vs `Fiend`) are the same mechanism, a different index.
    ///
    /// A fixed-size array, not a `Vec`/`HashMap`: this is a net-synced
    /// component the buff system rebuilds every tick, so an allocation here
    /// would be a per-tick heap cost for every entity in combat.
    pub bonus_damage_vs: [f32; CreatureKind::NUM_KINDS],
    pub projectile_speed_mult: f32,
    pub projectile_constructor_effects: Vec<ProjectileConstructorEffect>,
    /// This technically doesn't do anything. It should be used in the frontend
    /// to 'mark' an entity for a player, or used in agent to make an NPC focus
    /// on an entity.
    pub marked_entities: Vec<Uid>,
    /// Magical senses this entity currently has active, populated each tick
    /// from active buffs. See `ActiveSense`'s doc comment for why this is
    /// separate from the detection *result*.
    #[serde(default)]
    pub senses: Vec<ActiveSense>,
    /// This entity's creature kind, defaulted from
    /// `original_body.creature_kind()` at construction and overridable
    /// per-entity by `EntityConfig`'s `creature_type` field (an authored
    /// reskin can carry a different kind than the body it spawns on, e.g. a
    /// fiend wearing a humanoid body).
    ///
    /// Read this field, not `original_body.creature_kind()` directly, from
    /// any consumer that has a `Stats` in scope (e.g. anti-undead damage
    /// bonuses) — that is what makes the override effective.
    ///
    /// One asymmetry is accepted on purpose and must not be "fixed" locally:
    /// `Body::immune_to` is a `&self` method on `Body` alone, so it always
    /// reads the body's own default kind and never this override. A reskinned
    /// entity (say, a fiend spawned on a humanoid body via the config
    /// override) is therefore still charmable, because charm immunity is
    /// resolved from the body it actually wears, not from the authored kind
    /// stored here.
    #[serde(default)]
    pub creature_kind: Option<CreatureKind>,
}

impl Stats {
    pub fn new(name: Content, body: Body) -> Self {
        Self {
            name,
            creature_kind: body.creature_kind(),
            original_body: body,
            damage_reduction: StatsSplit::default(),
            poise_reduction: StatsSplit::default(),
            max_health_modifiers: StatsModifier::default(),
            move_speed_modifier: 1.0,
            charge_move_speed_modifier: 1.0,
            buildup_move_speed_modifier: 1.0,
            jump_modifier: 1.0,
            attack_speed_modifier: 1.0,
            recovery_speed_modifier: 1.0,
            charge_speed_modifier: 1.0,
            buildup_speed_modifier: 1.0,
            friction_modifier: 1.0,
            max_energy_modifiers: StatsModifier::default(),
            poise_damage_modifier: 1.0,
            attack_damage_modifier: 1.0,
            conditional_precision_modifiers: Vec::new(),
            precision_vulnerability_multiplier_override: None,
            swim_speed_modifier: 1.0,
            effects_on_attack: Vec::new(),
            mitigations_penetration: 0.0,
            energy_reward_modifier: 1.0,
            energy_efficiency_modifier: 1.0,
            effects_on_damaged: Vec::new(),
            effects_on_death: Vec::new(),
            disable_auxiliary_abilities: false,
            disable_magic: false,
            disable_teleport: false,
            accuracy: 0.0,
            evasion: 0.0,
            magic_accuracy: 0.0,
            magic_evasion: 0.0,
            crit_chance: 0.0,
            character_level: 0,
            resist_fire: 0.0,
            resist_frost: 0.0,
            resist_poison: 0.0,
            resist_magic: 0.0,
            crowd_control_resistance: 0.0,
            magic_resistance: 0.0,
            item_effect_reduction: 1.0,
            attacked_modifications: Vec::new(),
            precision_power_mult: 1.0,
            knockback_mult: 1.0,
            spell_power: 1.0,
            heal_power: 1.0,
            bonus_damage_vs: [0.0; CreatureKind::NUM_KINDS],
            projectile_speed_mult: 1.0,
            projectile_constructor_effects: Vec::new(),
            marked_entities: Vec::new(),
            senses: Vec::new(),
        }
    }

    /// Creates an empty `Stats` instance - used during character loading from
    /// the database
    pub fn empty(body: Body) -> Self { Self::new(Content::dummy(), body) }

    /// Combat resolution (BL-52 P3): the elemental resistance fraction that
    /// mitigates **AoE** damage of `kind`. Physical kinds return 0.0 — they are
    /// handled by the existing armor `damage_reduction`, not this layer (avoids
    /// double-counting). The legacy generic `Energy` and the other non-physical
    /// kinds fold into the catch-all `resist_magic`.
    pub fn aoe_resistance(&self, kind: DamageKind) -> f32 {
        match kind {
            DamageKind::Fire => self.resist_fire,
            DamageKind::Cold => self.resist_frost,
            DamageKind::Poison | DamageKind::Acid => self.resist_poison,
            DamageKind::Energy
            | DamageKind::Force
            | DamageKind::Lightning
            | DamageKind::Necrotic
            | DamageKind::Psychic
            | DamageKind::Radiant
            | DamageKind::Thunder => self.resist_magic,
            // Physical: mitigated by `damage_reduction`, not the elemental layer.
            DamageKind::Piercing | DamageKind::Slashing | DamageKind::Crushing => 0.0,
        }
    }

    /// Adds to the elemental resistance channel `kind` (used by
    /// `BuffEffect::Resistance`; gear/content later).
    pub fn add_resistance(&mut self, kind: ResistKind, amount: f32) {
        match kind {
            ResistKind::Fire => self.resist_fire += amount,
            ResistKind::Frost => self.resist_frost += amount,
            ResistKind::Poison => self.resist_poison += amount,
            ResistKind::Magic => self.resist_magic += amount,
        }
    }

    /// Resets temporary modifiers to default values
    pub fn reset_temp_modifiers(&mut self) {
        // "consume" name and body and re-create from scratch
        let name = std::mem::replace(&mut self.name, Content::dummy());
        let body = self.original_body;

        *self = Self::new(name, body);
    }
}

impl Component for Stats {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}
