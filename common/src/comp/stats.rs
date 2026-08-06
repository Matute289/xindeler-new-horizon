use common_i18n::Content;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};
use std::{error::Error, fmt};

use crate::{
    combat::{AttackEffect, AttackedModification, CombatRequirement, DamageKind, StatEffect},
    comp::{
        ability::MagicSource, buff::SenseMode, creature_type::CreatureKind, detection::SenseKind,
        inventory::item::tool::ToolKindMask, projectile::ProjectileConstructorEffect,
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
    /// Multiplier on energy regeneration rate (both the per-tick
    /// acceleration and the rate ceiling it climbs toward — see
    /// `Energy::regen`'s `modifier` parameter). 1.0 = no change. Sourced from
    /// class-skill passives (`ClassPassiveStat::EnergyRegen`) and, for a
    /// caster-role implement's `energy_efficiency` tool stat, from equipped
    /// gear (`combat::apply_gear_caster_stats`).
    pub energy_regen_modifier: f32,
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
    /// The concealment wire: buff-sourced addition to this entity's stealth,
    /// summed with item-based stealth (`DerivedStats::stealth`) in the same
    /// `1/(1+sum)` curve by
    /// `combat::perception_dist_multiplier_from_stealth`. Per-tick, not
    /// persisted. Set by `BuffEffect::Stealth`.
    pub stealth: f32,
    /// Whether this entity currently sees through any amount of another
    /// entity's concealment, regardless of that entity's `stealth` value —
    /// read as the **observer** side of
    /// `combat::perception_dist_multiplier_from_stealth`, same boolean shape
    /// as `disable_magic` above. Set by `BuffEffect::PierceConcealment`.
    pub pierce_concealment: bool,
    /// Whether this entity currently sees through illusions and disguises
    /// unconditionally, regardless of the target's own suspicion/reveal
    /// state — read as the **observer** side, same boolean shape as
    /// `pierce_concealment` above. Set by `BuffEffect::PierceIllusion`.
    pub pierce_illusion: bool,
    /// Whether this entity currently sees through mundane and magical
    /// darkness. A flag only in this row — its lighting-override consumer is
    /// a separate future addition, not built here. Same boolean shape as
    /// `pierce_concealment` above. Set by `BuffEffect::PierceDarkness`.
    pub pierce_darkness: bool,
    /// Anti-divination (`nondetection` spell): when set, this entity is
    /// skipped entirely by `server/src/sys/detection.rs`'s per-`SenseKind`
    /// predicate — "can't be targeted by divination," unconditionally,
    /// regardless of which sense is queried or what `false_aura` below
    /// claims. Same boolean shape as `disable_magic` above. Set by
    /// `BuffEffect::Nondetection`.
    pub nondetection: bool,
    /// `magic_aura`'s lie: when set, `server/src/sys/detection.rs`'s
    /// predicate reports this entity as revealed by the given `SenseKind`
    /// regardless of whether it actually matches that sense's real
    /// predicate (e.g. registering as magical while carrying no magic
    /// effect, or vice versa — the spell's own choice of which lie to tell).
    /// Overridden by `nondetection` above when both are set: "can't be
    /// targeted by divination" is the stronger, unconditional claim. Set by
    /// `BuffEffect::FalseAura`.
    pub false_aura: Option<SenseKind>,
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
    /// a smiter Cleric and a pure healer scale independently. Sourced from
    /// class-skill passives (`ClassPassiveStat::SpellPower`/`HealPower`) and
    /// from equipped caster-role gear (`combat::apply_gear_caster_stats`) —
    /// the latter is the only path that also reaches weaponless pool spells,
    /// since tool-stat scaling never applies with no tool in hand.
    pub spell_power: f32,
    pub heal_power: f32,
    /// Per-`MagicSource` spell-power multiplier, layered ON TOP of the flat
    /// `spell_power` above rather than replacing it: `spell_power_for`
    /// multiplies the two together, so a source that never names a specific
    /// `MagicSource` keeps boosting every magic-source spell exactly as
    /// before (every slot defaults to 1.0 = a no-op), while a source that
    /// DOES name one (e.g. an implement that boosts Divine spells
    /// specifically) raises only that slot. Indexed by
    /// `MagicSource::index`, sized `MagicSource::NUM_SOURCES` — a fixed
    /// array for the same per-tick-rebuild reason as `bonus_damage_vs`
    /// above (no per-entity heap allocation).
    ///
    /// This keys DAMAGE MAGNITUDE only. It must never be used to decide who
    /// may cast a spell — that is the unrelated, spell-side `classes:`
    /// compendium check; an implement's stats and a spell's `source` tag
    /// stay independent axes. See `spell_power_for`.
    pub spell_power_by_source: [f32; MagicSource::NUM_SOURCES],
    /// Multiplicative modifier on `AbilityMeta.cooldown`, applied where the
    /// cooldown is written in `states::utils::handle_ability` — not read by
    /// the ready-check itself, since the stored `AbilityCooldowns` entry
    /// already carries the reduced value (the write is the only place the
    /// reduction needs to apply). 1.0 = no change; a value below 1.0
    /// shortens the gate. The final cooldown is always floored (see
    /// `states::utils::MIN_ABILITY_COOLDOWN_SECS`) so a large reduction can
    /// never zero out or invert the gate. Sourced from equipped caster-role
    /// gear via `combat::apply_gear_caster_stats`'s `cooldown_reduction`
    /// tool stat.
    pub cooldown_reduction_modifier: f32,
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
    /// The weapon kinds this entity swings competently. Per-tick, not
    /// persisted. Narrowed only for entities carrying a `CharacterClass`
    /// (set from `class_proficiencies.ron` every tick in the buff system);
    /// everything else (every NPC, summon and boss) stays fully permissive
    /// — see `Stats::new`'s default. Permissive (`ToolKindMask::all()`) is
    /// the SAFE default here: an empty mask reaching an entity that never
    /// goes through the class-narrowing block (i.e. anything without a
    /// `CharacterClass`) would silently mute every NPC's weapon attacks.
    /// This is why the default is pinned via `tool_mask_all`, not the
    /// type's own (deliberately empty) `Default` impl — `#[serde(default)]`
    /// alone would resolve to `ToolKindMask::empty()` and invert this
    /// invariant.
    #[serde(default = "tool_mask_all")]
    pub proficient_tools: ToolKindMask,
    /// The physical-damage (and poise/knockback/crit) multiplier applied
    /// when the swung weapon is NOT in `proficient_tools`. 1.0 = no
    /// penalty. Sourced from `combat_tuning.non_proficient_damage_mult` for
    /// class-carrying entities; a future feat/buff can raise it back toward
    /// 1.0.
    #[serde(default = "one_f32")]
    pub non_proficient_damage_mult: f32,
}

fn one_f32() -> f32 { 1.0 }

/// Serde default for [`Stats::proficient_tools`]. Deliberately NOT
/// `ToolKindMask::default()` (which is empty by design — see that type's
/// own doc comment) — a missing key in a self-describing format must fail
/// open (permissive), mirroring `Stats::new`'s constructor default.
fn tool_mask_all() -> ToolKindMask { ToolKindMask::all() }

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
            energy_regen_modifier: 1.0,
            effects_on_damaged: Vec::new(),
            effects_on_death: Vec::new(),
            disable_auxiliary_abilities: false,
            disable_magic: false,
            disable_teleport: false,
            stealth: 0.0,
            pierce_concealment: false,
            pierce_illusion: false,
            pierce_darkness: false,
            nondetection: false,
            false_aura: None,
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
            spell_power_by_source: [1.0; MagicSource::NUM_SOURCES],
            cooldown_reduction_modifier: 1.0,
            bonus_damage_vs: [0.0; CreatureKind::NUM_KINDS],
            projectile_speed_mult: 1.0,
            projectile_constructor_effects: Vec::new(),
            marked_entities: Vec::new(),
            senses: Vec::new(),
            // Permissive by construction: an entity with no `CharacterClass`
            // (every NPC, summon and boss) must stay fully able to swing
            // any weapon. Narrowed only inside the per-tick class block for
            // entities that actually carry a `CharacterClass`. Getting this
            // backwards (empty by default) would silently mute every NPC's
            // weapon attacks.
            proficient_tools: ToolKindMask::all(),
            non_proficient_damage_mult: 1.0,
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

    /// The effective spell-power multiplier for a magic attack whose
    /// `AbilityMeta.source` is `source`. Composes the flat, unkeyed
    /// `spell_power` channel (what every caster passive/gear source
    /// contributes to today) with the `source`-specific
    /// `spell_power_by_source` slot: a spell with no `source` (should not
    /// reach a magic-damage path at all, since callers gate on `is_magic`
    /// first) simply skips the keyed lookup and returns the flat multiplier
    /// alone.
    pub fn spell_power_for(&self, source: Option<MagicSource>) -> f32 {
        let keyed = source.map_or(1.0, |source| self.spell_power_by_source[source.index()]);
        self.spell_power * keyed
    }

    /// Resets temporary modifiers to default values.
    ///
    /// Runs for every entity, every tick, so this resets fields in place
    /// rather than reconstructing via `*self = Self::new(...)`: scalars are
    /// overwritten directly and `Vec` fields are `.clear()`ed instead of
    /// dropped and reallocated, which keeps their allocated capacity across
    /// ticks. `name` and `original_body` are untouched since they are not
    /// temporary.
    ///
    /// ⚠️ Must stay in lockstep, field for field, with `Self::new`. The old
    /// `*self = Self::new(...)` idiom made that automatic; now it is only
    /// guaranteed by
    /// `reset_restores_every_field_from_a_fully_dirtied_stats` below — keep
    /// that test in sync with any new field added to `Stats`.
    pub fn reset_temp_modifiers(&mut self) {
        self.creature_kind = self.original_body.creature_kind();
        self.damage_reduction = StatsSplit::default();
        self.poise_reduction = StatsSplit::default();
        self.max_health_modifiers = StatsModifier::default();
        self.move_speed_modifier = 1.0;
        self.charge_move_speed_modifier = 1.0;
        self.buildup_move_speed_modifier = 1.0;
        self.jump_modifier = 1.0;
        self.attack_speed_modifier = 1.0;
        self.charge_speed_modifier = 1.0;
        self.buildup_speed_modifier = 1.0;
        self.recovery_speed_modifier = 1.0;
        self.friction_modifier = 1.0;
        self.max_energy_modifiers = StatsModifier::default();
        self.poise_damage_modifier = 1.0;
        self.attack_damage_modifier = 1.0;
        self.conditional_precision_modifiers.clear();
        self.precision_vulnerability_multiplier_override = None;
        self.swim_speed_modifier = 1.0;
        self.effects_on_attack.clear();
        self.mitigations_penetration = 0.0;
        self.energy_reward_modifier = 1.0;
        self.energy_efficiency_modifier = 1.0;
        self.energy_regen_modifier = 1.0;
        self.effects_on_damaged.clear();
        self.effects_on_death.clear();
        self.disable_auxiliary_abilities = false;
        self.disable_magic = false;
        self.disable_teleport = false;
        self.stealth = 0.0;
        self.pierce_concealment = false;
        self.pierce_illusion = false;
        self.pierce_darkness = false;
        self.nondetection = false;
        self.false_aura = None;
        self.accuracy = 0.0;
        self.evasion = 0.0;
        self.magic_accuracy = 0.0;
        self.magic_evasion = 0.0;
        self.crit_chance = 0.0;
        self.character_level = 0;
        self.resist_fire = 0.0;
        self.resist_frost = 0.0;
        self.resist_poison = 0.0;
        self.resist_magic = 0.0;
        self.crowd_control_resistance = 0.0;
        self.magic_resistance = 0.0;
        self.item_effect_reduction = 1.0;
        self.attacked_modifications.clear();
        self.precision_power_mult = 1.0;
        self.knockback_mult = 1.0;
        self.spell_power = 1.0;
        self.heal_power = 1.0;
        self.spell_power_by_source = [1.0; MagicSource::NUM_SOURCES];
        self.cooldown_reduction_modifier = 1.0;
        self.bonus_damage_vs = [0.0; CreatureKind::NUM_KINDS];
        self.projectile_speed_mult = 1.0;
        self.projectile_constructor_effects.clear();
        self.marked_entities.clear();
        self.senses.clear();
        self.proficient_tools = ToolKindMask::all();
        self.non_proficient_damage_mult = 1.0;
    }
}

impl Component for Stats {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        combat::{AttackedModifier, CombatEffect, StatEffectTarget},
        comp::projectile::ProjectileConstructorEffectKind,
    };

    fn test_body() -> Body { Body::Humanoid(crate::comp::humanoid::Body::random()) }

    #[test]
    fn empty_is_permissive_on_proficiency() {
        let stats = Stats::empty(test_body());
        assert_eq!(stats.proficient_tools, ToolKindMask::all());
        assert_eq!(stats.non_proficient_damage_mult, 1.0);
    }

    #[test]
    fn reset_temp_modifiers_restores_the_permissive_defaults() {
        let mut stats = Stats::empty(test_body());
        // Clobber the fields the way a narrowing consumer would.
        stats.proficient_tools = ToolKindMask::empty();
        stats.non_proficient_damage_mult = 0.40;

        stats.reset_temp_modifiers();

        assert_eq!(stats.proficient_tools, ToolKindMask::all());
        assert_eq!(stats.non_proficient_damage_mult, 1.0);
    }

    // Regression test for an inverted `#[serde(default)]` on
    // `proficient_tools`: `ToolKindMask`'s own `Default` impl is deliberately
    // EMPTY (see its doc comment), so a bare `#[serde(default)]` would
    // resolve a missing key to "proficient with nothing" -- the exact
    // inversion of the permissive-by-default invariant the rest of this
    // module is built on. The production wire/save format (bincode) is
    // non-self-describing and never omits fields, so this is unreachable
    // there; it only bites the moment `Stats` gains a self-describing
    // serialization path. JSON stands in for that here since it's already a
    // crate dependency.
    #[test]
    fn missing_proficiency_field_deserializes_permissive_through_a_self_describing_format() {
        let stats = Stats::new(Content::dummy(), test_body());
        let mut value = serde_json::to_value(&stats).expect("Stats must serialize to JSON");
        let map = value
            .as_object_mut()
            .expect("Stats must serialize as a JSON object");
        // Simulate a payload authored/saved before the field existed.
        map.remove("proficient_tools");

        let restored: Stats =
            serde_json::from_value(value).expect("Stats must deserialize with the key absent");
        assert_eq!(restored.proficient_tools, ToolKindMask::all());
    }

    #[test]
    fn fresh_stats_have_identity_caster_channels() {
        let stats = Stats::empty(test_body());
        assert_eq!(stats.cooldown_reduction_modifier, 1.0);
        assert_eq!(stats.spell_power_by_source, [1.0; MagicSource::NUM_SOURCES]);
    }

    #[test]
    fn reset_temp_modifiers_restores_the_caster_channel_defaults() {
        let mut stats = Stats::empty(test_body());
        stats.cooldown_reduction_modifier = 0.5;
        stats.spell_power_by_source[MagicSource::Divine.index()] = 2.0;

        stats.reset_temp_modifiers();

        assert_eq!(stats.cooldown_reduction_modifier, 1.0);
        assert_eq!(stats.spell_power_by_source, [1.0; MagicSource::NUM_SOURCES]);
    }

    /// Guards against the exact drift the old `*self = Self::new(...)`
    /// idiom protected against for free: sets literally every field on
    /// `Stats` to a non-default value, calls `reset_temp_modifiers`, and
    /// asserts every field matches a freshly-constructed `Stats::new(...)`.
    /// If a new field is ever added to `Stats` without updating both
    /// `Self::new` and `reset_temp_modifiers`, this test is the thing that
    /// is supposed to catch it — keep it in sync.
    #[test]
    fn reset_restores_every_field_from_a_fully_dirtied_stats() {
        let body = test_body();
        let name = Content::Plain("dirtied-stats-test".to_owned());
        let mut stats = Stats::new(name.clone(), body);

        // Dirty every temporary field to a value `Self::new` would never
        // produce.
        stats.creature_kind = Some(CreatureKind::Fiend);
        stats.damage_reduction = StatsSplit {
            pos_mod: 0.3,
            neg_mod: -0.1,
        };
        stats.poise_reduction = StatsSplit {
            pos_mod: 0.2,
            neg_mod: -0.2,
        };
        stats.max_health_modifiers = StatsModifier {
            add_mod: 10.0,
            mult_mod: 2.0,
        };
        stats.move_speed_modifier = 2.0;
        stats.charge_move_speed_modifier = 2.0;
        stats.buildup_move_speed_modifier = 2.0;
        stats.jump_modifier = 2.0;
        stats.attack_speed_modifier = 2.0;
        stats.charge_speed_modifier = 2.0;
        stats.buildup_speed_modifier = 2.0;
        stats.recovery_speed_modifier = 2.0;
        stats.friction_modifier = 2.0;
        stats.max_energy_modifiers = StatsModifier {
            add_mod: 5.0,
            mult_mod: 3.0,
        };
        stats.poise_damage_modifier = 2.0;
        stats.attack_damage_modifier = 2.0;
        stats
            .conditional_precision_modifiers
            .push((None, 1.0, true));
        stats.precision_vulnerability_multiplier_override = Some(1.5);
        stats.swim_speed_modifier = 2.0;
        stats
            .effects_on_attack
            .push(AttackEffect::new(None, CombatEffect::Heal(1.0)));
        stats.mitigations_penetration = 0.5;
        stats.energy_reward_modifier = 2.0;
        stats.energy_efficiency_modifier = 2.0;
        stats.energy_regen_modifier = 2.0;
        stats.effects_on_damaged.push(StatEffect::new(
            StatEffectTarget::Target,
            CombatEffect::Heal(1.0),
        ));
        stats.effects_on_death.push(StatEffect::new(
            StatEffectTarget::Attacker,
            CombatEffect::Heal(1.0),
        ));
        stats.disable_auxiliary_abilities = true;
        stats.disable_magic = true;
        stats.disable_teleport = true;
        stats.stealth = 5.0;
        stats.pierce_concealment = true;
        stats.pierce_illusion = true;
        stats.pierce_darkness = true;
        stats.nondetection = true;
        stats.false_aura = Some(SenseKind::Magic);
        stats.accuracy = 0.5;
        stats.evasion = 0.5;
        stats.magic_accuracy = 0.5;
        stats.magic_evasion = 0.5;
        stats.crit_chance = 0.5;
        stats.character_level = 42;
        stats.resist_fire = 0.5;
        stats.resist_frost = 0.5;
        stats.resist_poison = 0.5;
        stats.resist_magic = 0.5;
        stats.crowd_control_resistance = 0.5;
        stats.magic_resistance = 0.5;
        stats.item_effect_reduction = 0.5;
        stats.attacked_modifications.push(AttackedModification::new(
            AttackedModifier::EnergyReward(1.0),
        ));
        stats.precision_power_mult = 2.0;
        stats.knockback_mult = 2.0;
        stats.spell_power = 2.0;
        stats.heal_power = 2.0;
        stats.spell_power_by_source = [2.0; MagicSource::NUM_SOURCES];
        stats.cooldown_reduction_modifier = 0.5;
        stats.bonus_damage_vs = [1.0; CreatureKind::NUM_KINDS];
        stats.projectile_speed_mult = 2.0;
        stats
            .projectile_constructor_effects
            .push(ProjectileConstructorEffect {
                kind: ProjectileConstructorEffectKind::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::Heal(1.0),
                )),
                tool_filter: None,
            });
        stats
            .marked_entities
            .push(Uid(std::num::NonZeroU64::new(1).unwrap()));
        stats.senses.push(ActiveSense {
            kind: SenseKind::Magic,
            radius: 10.0,
            mode: SenseMode::Snapshot,
        });
        stats.proficient_tools = ToolKindMask::empty();
        stats.non_proficient_damage_mult = 0.4;

        stats.reset_temp_modifiers();

        let expected = Stats::new(name, body);
        assert_eq!(stats.name, expected.name);
        assert_eq!(stats.original_body, expected.original_body);
        assert_eq!(stats.creature_kind, expected.creature_kind);
        assert_eq!(
            stats.damage_reduction.pos_mod,
            expected.damage_reduction.pos_mod
        );
        assert_eq!(
            stats.damage_reduction.neg_mod,
            expected.damage_reduction.neg_mod
        );
        assert_eq!(
            stats.poise_reduction.pos_mod,
            expected.poise_reduction.pos_mod
        );
        assert_eq!(
            stats.poise_reduction.neg_mod,
            expected.poise_reduction.neg_mod
        );
        assert_eq!(
            stats.max_health_modifiers.add_mod,
            expected.max_health_modifiers.add_mod
        );
        assert_eq!(
            stats.max_health_modifiers.mult_mod,
            expected.max_health_modifiers.mult_mod
        );
        assert_eq!(stats.move_speed_modifier, expected.move_speed_modifier);
        assert_eq!(
            stats.charge_move_speed_modifier,
            expected.charge_move_speed_modifier
        );
        assert_eq!(
            stats.buildup_move_speed_modifier,
            expected.buildup_move_speed_modifier
        );
        assert_eq!(stats.jump_modifier, expected.jump_modifier);
        assert_eq!(stats.attack_speed_modifier, expected.attack_speed_modifier);
        assert_eq!(stats.charge_speed_modifier, expected.charge_speed_modifier);
        assert_eq!(
            stats.buildup_speed_modifier,
            expected.buildup_speed_modifier
        );
        assert_eq!(
            stats.recovery_speed_modifier,
            expected.recovery_speed_modifier
        );
        assert_eq!(stats.friction_modifier, expected.friction_modifier);
        assert_eq!(
            stats.max_energy_modifiers.add_mod,
            expected.max_energy_modifiers.add_mod
        );
        assert_eq!(
            stats.max_energy_modifiers.mult_mod,
            expected.max_energy_modifiers.mult_mod
        );
        assert_eq!(stats.poise_damage_modifier, expected.poise_damage_modifier);
        assert_eq!(
            stats.attack_damage_modifier,
            expected.attack_damage_modifier
        );
        assert_eq!(
            stats.conditional_precision_modifiers,
            expected.conditional_precision_modifiers
        );
        assert_eq!(
            stats.precision_vulnerability_multiplier_override,
            expected.precision_vulnerability_multiplier_override
        );
        assert_eq!(stats.swim_speed_modifier, expected.swim_speed_modifier);
        assert_eq!(
            stats.effects_on_attack.len(),
            expected.effects_on_attack.len()
        );
        assert!(stats.effects_on_attack.is_empty());
        assert_eq!(
            stats.mitigations_penetration,
            expected.mitigations_penetration
        );
        assert_eq!(
            stats.energy_reward_modifier,
            expected.energy_reward_modifier
        );
        assert_eq!(
            stats.energy_efficiency_modifier,
            expected.energy_efficiency_modifier
        );
        assert_eq!(stats.energy_regen_modifier, expected.energy_regen_modifier);
        assert!(stats.effects_on_damaged.is_empty());
        assert_eq!(
            stats.effects_on_damaged.len(),
            expected.effects_on_damaged.len()
        );
        assert!(stats.effects_on_death.is_empty());
        assert_eq!(
            stats.effects_on_death.len(),
            expected.effects_on_death.len()
        );
        assert_eq!(
            stats.disable_auxiliary_abilities,
            expected.disable_auxiliary_abilities
        );
        assert_eq!(stats.disable_magic, expected.disable_magic);
        assert_eq!(stats.disable_teleport, expected.disable_teleport);
        assert_eq!(stats.stealth, expected.stealth);
        assert_eq!(stats.pierce_concealment, expected.pierce_concealment);
        assert_eq!(stats.pierce_illusion, expected.pierce_illusion);
        assert_eq!(stats.pierce_darkness, expected.pierce_darkness);
        assert_eq!(stats.nondetection, expected.nondetection);
        assert_eq!(stats.false_aura, expected.false_aura);
        assert_eq!(stats.accuracy, expected.accuracy);
        assert_eq!(stats.evasion, expected.evasion);
        assert_eq!(stats.magic_accuracy, expected.magic_accuracy);
        assert_eq!(stats.magic_evasion, expected.magic_evasion);
        assert_eq!(stats.crit_chance, expected.crit_chance);
        assert_eq!(stats.character_level, expected.character_level);
        assert_eq!(stats.resist_fire, expected.resist_fire);
        assert_eq!(stats.resist_frost, expected.resist_frost);
        assert_eq!(stats.resist_poison, expected.resist_poison);
        assert_eq!(stats.resist_magic, expected.resist_magic);
        assert_eq!(
            stats.crowd_control_resistance,
            expected.crowd_control_resistance
        );
        assert_eq!(stats.magic_resistance, expected.magic_resistance);
        assert_eq!(stats.item_effect_reduction, expected.item_effect_reduction);
        assert!(stats.attacked_modifications.is_empty());
        assert_eq!(
            stats.attacked_modifications.len(),
            expected.attacked_modifications.len()
        );
        assert_eq!(stats.precision_power_mult, expected.precision_power_mult);
        assert_eq!(stats.knockback_mult, expected.knockback_mult);
        assert_eq!(stats.spell_power, expected.spell_power);
        assert_eq!(stats.heal_power, expected.heal_power);
        assert_eq!(stats.spell_power_by_source, expected.spell_power_by_source);
        assert_eq!(
            stats.cooldown_reduction_modifier,
            expected.cooldown_reduction_modifier
        );
        assert_eq!(stats.bonus_damage_vs, expected.bonus_damage_vs);
        assert_eq!(stats.projectile_speed_mult, expected.projectile_speed_mult);
        assert!(stats.projectile_constructor_effects.is_empty());
        assert_eq!(
            stats.projectile_constructor_effects.len(),
            expected.projectile_constructor_effects.len()
        );
        assert!(stats.marked_entities.is_empty());
        assert_eq!(stats.marked_entities.len(), expected.marked_entities.len());
        assert!(stats.senses.is_empty());
        assert_eq!(stats.senses.len(), expected.senses.len());
        assert_eq!(stats.proficient_tools, expected.proficient_tools);
        assert_eq!(
            stats.non_proficient_damage_mult,
            expected.non_proficient_damage_mult
        );
    }

    #[test]
    fn unkeyed_spell_power_boosts_every_source_like_before() {
        let mut stats = Stats::empty(test_body());
        stats.spell_power = 2.0;

        assert_eq!(stats.spell_power_for(Some(MagicSource::Arcane)), 2.0);
        assert_eq!(stats.spell_power_for(Some(MagicSource::Divine)), 2.0);
        assert_eq!(stats.spell_power_for(None), 2.0);
    }

    #[test]
    fn keyed_spell_power_boosts_only_its_own_source() {
        let mut stats = Stats::empty(test_body());
        stats.spell_power_by_source[MagicSource::Divine.index()] = 3.0;

        assert_eq!(stats.spell_power_for(Some(MagicSource::Divine)), 3.0);
        assert_eq!(stats.spell_power_for(Some(MagicSource::Arcane)), 1.0);
        assert_eq!(stats.spell_power_for(Some(MagicSource::Primordial)), 1.0);
    }

    #[test]
    fn flat_and_keyed_spell_power_compose() {
        let mut stats = Stats::empty(test_body());
        stats.spell_power = 1.5;
        stats.spell_power_by_source[MagicSource::Arcane.index()] = 2.0;

        assert!((stats.spell_power_for(Some(MagicSource::Arcane)) - 3.0).abs() < 1e-5);
        assert_eq!(stats.spell_power_for(Some(MagicSource::Divine)), 1.5);
    }
}
