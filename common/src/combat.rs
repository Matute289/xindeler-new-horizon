use crate::{
    assets::{AssetExt, Ron},
    comp::{
        Alignment, AttunedItems, Buffs, CasterGearFold, CharacterClass, CharacterState, Combo,
        DerivedStats, Energy, Group, Health, HealthChange, InputKind, Inventory, MagicSource, Mass,
        Ori, Player, Poise, PoiseChange, Stats,
        ability::Capability,
        attunement::item_effects_active,
        aura::{AuraKindVariant, EnteredAuras},
        body::MagicResistTier,
        buff::{Buff, BuffChange, BuffData, BuffDescriptor, BuffKind, BuffSource, DestInfo},
        class::ClassKind,
        inventory::{
            item::{
                ItemKind,
                tool::{self, Hands, Tool, ToolKind, WeaponRole},
            },
            slot::EquipSlot,
        },
        skillset::{MAX_CHARACTER_LEVEL, SkillGroupKind},
    },
    effect::BuffEffect,
    event::{
        BuffEvent, ComboChangeEvent, EmitExt, EnergyChangeEvent, EntityAttackedHookEvent,
        HealthChangeEvent, KnockbackEvent, ParryHookEvent, PoiseChangeEvent, TransformEvent,
    },
    generation::{EntityConfig, EntityInfo},
    outcome::Outcome,
    resources::{Secs, Time},
    states::utils::{AbilityInfo, HandInfo, StageSection},
    uid::{IdMaps, Uid},
    util::Dir,
};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use specs::{Entity as EcsEntity, ReadStorage};
use std::ops::{Mul, MulAssign};
use tracing::error;
use vek::*;

pub enum AttackTarget {
    AllInRange(f32),
    Pos(Vec3<f32>),
    Entity(EcsEntity),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GroupTarget {
    InGroup,
    OutOfGroup,
    All,
}

/// Combat-resolution tuning (BL-52). Single source of balance truth for the
/// probabilistic to-hit roll; loaded from `assets/common/combat_tuning.ron`
/// (cached, mirrors how `MaterialStatManifest` is read in `apply_attack`).
/// Later phases extend this asset with crit / resistance / armor-weight
/// sections — `#[serde(default)]` keeps older/newer assets forward-compatible.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CombatTuning {
    /// Hit chance when attacker accuracy == target evasion.
    pub base_hit: f32,
    /// Hit chance gained per net point of (accuracy − evasion).
    pub hit_k: f32,
    /// Minimum hit chance — an outmatched/frightened attacker still lands this
    /// often (BL-52 floor = 0.05).
    pub hit_floor: f32,
    /// Maximum hit chance — optimal investment can reach a guaranteed hit
    /// (BL-52 ceil = 1.00).
    pub hit_ceil: f32,
    /// Minimum rolled crit chance — every attacker can crit at least this often
    /// (no dead stat). BL-52 floor = 0.03.
    pub crit_chance_floor: f32,
    /// Maximum rolled crit chance (guaranteed positional crits bypass it).
    /// BL-52 cap = 0.75.
    pub crit_chance_cap: f32,
    /// Precision magnitude applied when a random `crit_chance` roll succeeds —
    /// the positional-precision *fraction* used for a rolled crit (1.0 = full).
    /// BL-52 = 1.0.
    pub crit_precision_mult: f32,
    /// Combat resolution (BL-52 P6): a crit's base damage multiplier floor (WoW
    /// model). A full crit deals at least this× and scales further with gear
    /// `precision_power`: total = base·(crit_damage_mult + (precision_power −
    /// 1)) for a full crit; positional precision scales the floor by its
    /// fraction. 1.5 = a level-1 crit hits for ×1.5 (vs ~×1.0 before, when
    /// it was tied solely to ungeared precision_power).
    pub crit_damage_mult: f32,
    /// Combat resolution (BL-52 P3): hard cap on typed elemental resistance
    /// when mitigating AoE damage, so stacking can't reach immunity. BL-52
    /// = 0.75.
    pub resist_soft_cap: f32,
    /// Combat resolution (BL-52 P5): armor → physical evasion. Weight is
    /// derived from total armor **protection** (Matías 2026-06-25): heavier
    /// (more protective) armor lowers evasion; unarmored = max gear
    /// evasion. Final armor evasion = `clamp(gear_evasion_cap −
    /// total_protection · armor_evasion_per_protection − shield?,
    /// gear_evasion_floor, gear_evasion_cap)`. Applies to the physical
    /// to-hit only (magic uses `magic_evasion`).
    pub gear_evasion_cap: f32,
    pub gear_evasion_floor: f32,
    pub armor_evasion_per_protection: f32,
    /// Flat evasion penalty while a shield is equipped — a shield pays off via
    /// block/mitigation, not dodge.
    pub shield_evasion_penalty: f32,
    /// Chance for a resisted magical effect (charm / domination / compulsion /
    /// banishment) to land when the caster's magic accuracy exactly equals the
    /// target's *effective* magic evasion. Deliberately below `base_hit` so
    /// these effects can be tuned without touching the damage curve.
    pub save_base_hit: f32,
    /// Minimum landing chance for a resisted magical effect, for a caster that
    /// is merely at a disadvantage rather than hard-walled by
    /// `save_outclassed_wall` — such an attempt is unlikely, never impossible.
    pub save_hit_floor: f32,
    /// Maximum landing chance for a resisted magical effect. Deliberately
    /// below `hit_ceil`: damage may become guaranteed, a save never may, so
    /// every target keeps an escape roll no matter the level gap.
    pub save_hit_ceil: f32,
    /// Hard cap on the combined magic-resistance + crowd-control-resistance
    /// subtraction, mirroring `resist_soft_cap`. Stacked resistances can never
    /// reach immunity — only `Body::immune_to` grants that.
    pub save_mr_soft_cap: f32,
    /// Points of effective magic evasion granted per point of the target's
    /// `combat_rating`. Creatures carry no class attributes, so their
    /// `Stats::magic_evasion` is 0.0; without this term a world boss would be
    /// as charmable as a rabbit.
    pub save_cr_to_evasion: f32,
    /// Flat penalty subtracted when the target is already fighting the caster
    /// or the caster's group, which makes a resisted effect an opener rather
    /// than a mid-duel button. Applies identically in PvE and PvP.
    pub save_in_combat_penalty: f32,
    /// Resistance fraction each `MagicResistTier` above `None` is worth. The
    /// tier *taxonomy* lives in code (`Body::magic_resist_tier`); only these
    /// numbers are data.
    pub magic_resist_minor: f32,
    pub magic_resist_major: f32,
    pub magic_resist_legendary: f32,
    /// Hard wall on the level term, expressed in the same post-`hit_k` units
    /// the level term itself uses (not raw accuracy/evasion points). A caster
    /// whose accuracy deficit against the target's effective magic evasion
    /// reaches this magnitude always fails outright, bypassing
    /// `save_hit_floor` entirely: being sufficiently outclassed removes the
    /// rescue roll, while a merely unfavourable matchup keeps it.
    pub save_outclassed_wall: f32,
    /// Multiplier applied to physical damage, poise/knockback and crit when
    /// the swung weapon is not in the wielder's proficiency set (a soft
    /// weapon-proficiency gate). 1.0 = no penalty.
    pub non_proficient_damage_mult: f32,
    /// Trigger-slot cooldown in **real-world seconds**, indexed by the spell
    /// circle (0 = cantrip … 9) of the ability sitting in the slot.
    ///
    /// Deliberately a **table, not a formula**. Circles 1–9 are the exponential
    /// curve `1800 · 72^((C−1)/8)` — every circle multiplies the wait by ≈1.707
    /// — rounded to values a player can read off a tooltip; the rounded values
    /// *are* the design intent, and computing the curve at runtime would
    /// silently un-round them. Circle 0 is an explicit floor, not a curve
    /// output: cantrips are basic tricks and get a short, generous wait.
    ///
    /// Anything that is not a catalogued spell (a racial innate, a weapon
    /// ability) has no circle and falls back to index 0. A circle past the end
    /// of the list clamps to the last entry, so the list may be shortened or
    /// extended without a code change.
    pub trigger_slot_cooldown_secs: Vec<f32>,
}

/// The shipped trigger-slot cooldown ladder, in real-world seconds by spell
/// circle. Also the fallback if the asset ever ships an empty list.
const DEFAULT_TRIGGER_SLOT_COOLDOWNS: [f32; 10] = [
    600.0, 1800.0, 3000.0, 5400.0, 9000.0, 15300.0, 26100.0, 44400.0, 75600.0, 129600.0,
];

impl Default for CombatTuning {
    fn default() -> Self {
        Self {
            base_hit: 0.85,
            hit_k: 0.015,
            hit_floor: 0.05,
            hit_ceil: 1.00,
            crit_chance_floor: 0.03,
            crit_chance_cap: 0.75,
            crit_precision_mult: 1.0,
            crit_damage_mult: 1.5,
            resist_soft_cap: 0.75,
            gear_evasion_cap: 12.0,
            gear_evasion_floor: -10.0,
            armor_evasion_per_protection: 0.3,
            shield_evasion_penalty: 2.0,
            save_base_hit: 0.70,
            save_hit_floor: 0.05,
            save_hit_ceil: 0.95,
            save_mr_soft_cap: 0.75,
            save_cr_to_evasion: 2.0,
            save_in_combat_penalty: 0.20,
            magic_resist_minor: 0.15,
            magic_resist_major: 0.30,
            magic_resist_legendary: 0.50,
            save_outclassed_wall: 0.30,
            non_proficient_damage_mult: 0.40,
            trigger_slot_cooldown_secs: DEFAULT_TRIGGER_SLOT_COOLDOWNS.to_vec(),
        }
    }
}

impl CombatTuning {
    /// The trigger-slot cooldown, in real-world seconds, for an ability of
    /// spell circle `circle`. Circles beyond the table clamp to its last entry.
    pub fn trigger_slot_cooldown(&self, circle: u8) -> f32 {
        let table = if self.trigger_slot_cooldown_secs.is_empty() {
            &DEFAULT_TRIGGER_SLOT_COOLDOWNS[..]
        } else {
            &self.trigger_slot_cooldown_secs[..]
        };
        table[usize::from(circle).min(table.len() - 1)]
    }

    /// The resistance fraction a creature's innate [`MagicResistTier`] is
    /// worth. `None` is always exactly 0.0 and therefore needs no constant.
    pub fn magic_resist_tier_value(&self, tier: MagicResistTier) -> f32 {
        match tier {
            MagicResistTier::None => 0.0,
            MagicResistTier::Minor => self.magic_resist_minor,
            MagicResistTier::Major => self.magic_resist_major,
            MagicResistTier::Legendary => self.magic_resist_legendary,
        }
    }
}

/// The caster side of a resisted magical roll.
#[derive(Copy, Clone, Debug)]
pub struct SaveCasterInfo {
    /// `Stats::magic_accuracy` of the caster.
    pub magic_accuracy: f32,
}

/// The target side of a resisted magical roll, gathered once from shipped
/// components at application time.
#[derive(Copy, Clone, Debug)]
pub struct SaveTargetInfo {
    /// `Stats::magic_evasion` — class/level derived, so 0.0 for a plain
    /// creature.
    pub stats_magic_evasion: f32,
    /// `Stats::crowd_control_resistance`.
    pub crowd_control_resistance: f32,
    /// `Stats::magic_resistance` — the additive contribution from racial
    /// passives and buffs, on top of the innate creature tier below.
    pub stats_magic_resistance: f32,
    /// `Body::magic_resist_tier()` — the innate per-creature tier.
    pub magic_resist_tier: MagicResistTier,
    /// `combat_rating` for this target: the difficulty axis that stands in for
    /// a character level on creatures that have no class attributes.
    pub combat_rating: f32,
}

impl SaveTargetInfo {
    /// Total magic resistance: the innate creature tier plus whatever has
    /// already accumulated on `Stats`.
    pub fn magic_resistance(&self, tuning: &CombatTuning) -> f32 {
        tuning.magic_resist_tier_value(self.magic_resist_tier) + self.stats_magic_resistance
    }
}

/// The evasion a resisted magical roll is made against: the target's
/// class/level magic evasion plus a contribution derived from its
/// `combat_rating`, which is the only difficulty signal a creature without
/// class attributes has.
pub fn effective_magic_evasion(target: &SaveTargetInfo, tuning: &CombatTuning) -> f32 {
    target.stats_magic_evasion + target.combat_rating * tuning.save_cr_to_evasion
}

/// Probability in `0.0..=1.0` that a resisted magical effect lands on
/// `target` — the engine's single saving-throw roll, shared by charm /
/// domination and by `power_word_divine_word`'s banishment. Any future
/// resisted effect uses this rather than inventing a second curve.
///
/// `fighting_caster` is the [`is_fighting_caster`] predicate: a target already
/// in a fight with the caster (or the caster's group) is harder to affect.
///
/// A caster far enough below the target's effective magic evasion returns
/// exactly `0.0` — the outclassed wall is checked *before* the clamp, which is
/// what distinguishes it from one more subtracted term and is why it can
/// produce a result below `save_hit_floor`.
pub fn saving_throw_chance(
    caster: &SaveCasterInfo,
    target: &SaveTargetInfo,
    fighting_caster: bool,
    tuning: &CombatTuning,
) -> f32 {
    let level_term =
        (caster.magic_accuracy - effective_magic_evasion(target, tuning)) * tuning.hit_k;

    if level_term <= -tuning.save_outclassed_wall {
        return 0.0;
    }

    let resist = (target.magic_resistance(tuning) + target.crowd_control_resistance)
        .clamp(0.0, tuning.save_mr_soft_cap);
    let in_combat = if fighting_caster {
        tuning.save_in_combat_penalty
    } else {
        0.0
    };
    (tuning.save_base_hit + level_term - resist - in_combat)
        .clamp(tuning.save_hit_floor, tuning.save_hit_ceil)
}

/// How recent a hostile health change must be to still count as "currently
/// fighting", matching the window the pet behaviour tree already uses to decide
/// that its owner was attacked.
pub const SAVE_RECENT_COMBAT_SECS: f64 = 5.0;

/// Everything [`is_fighting_caster`] needs, read once from already-shipped
/// components at application time — no new component, no timer, no per-tick
/// scan.
pub struct SaveCombatContext<'a> {
    pub caster_uid: Uid,
    pub caster_group: Option<Group>,
    pub target_uid: Uid,
    pub target_group: Option<Group>,
    /// `(uid, group)` of the entity the target's `Agent` is currently *hostile*
    /// towards, when the target is a creature with such a target at all. `None`
    /// for a player, which has no `Agent`.
    pub target_hostile_focus: Option<(Uid, Option<Group>)>,
    /// The target's most recent `Health::last_change` — the fallback signal for
    /// player targets.
    pub target_last_change: Option<&'a HealthChange>,
    /// The caster's most recent `Health::last_change`, so the check fires
    /// whether the caster hit the target or the target hit the caster.
    pub caster_last_change: Option<&'a HealthChange>,
    /// Current `Time`.
    pub now: f64,
}

/// Whether the target of a resisted magical effect already counts as fighting
/// the caster or the caster's group, which costs the caster
/// `save_in_combat_penalty`.
///
/// Creature targets answer this from their existing hostile agent target;
/// players, having no agent, fall back to recent attributable damage in *both*
/// directions.
pub fn is_fighting_caster(ctx: &SaveCombatContext<'_>) -> bool {
    if let Some((focus_uid, focus_group)) = ctx.target_hostile_focus
        && (focus_uid == ctx.caster_uid
            || (focus_group.is_some() && focus_group == ctx.caster_group))
    {
        return true;
    }
    recent_hostile_change_by(
        ctx.target_last_change,
        ctx.now,
        ctx.caster_uid,
        ctx.caster_group,
    ) || recent_hostile_change_by(
        ctx.caster_last_change,
        ctx.now,
        ctx.target_uid,
        ctx.target_group,
    )
}

/// Whether `change` is a damaging health change taken within
/// [`SAVE_RECENT_COMBAT_SECS`] and attributable to `uid` or to `group`.
fn recent_hostile_change_by(
    change: Option<&HealthChange>,
    now: f64,
    uid: Uid,
    group: Option<Group>,
) -> bool {
    change.is_some_and(|change| {
        change.amount < 0.0
            && now - change.time.0 < SAVE_RECENT_COMBAT_SECS
            && match change.damage_by() {
                Some(DamageContributor::Solo(by)) => by == uid,
                Some(DamageContributor::Group {
                    entity_uid,
                    group: by_group,
                }) => entity_uid == uid || group == Some(by_group),
                None => false,
            }
    })
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StatEffectTarget {
    Attacker,
    Target,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Hash)]
pub enum AttackSource {
    Melee,
    Projectile,
    Beam,
    GroundShockwave,
    AirShockwave,
    UndodgeableShockwave,
    Explosion,
    Arc,
    Pool,
}

pub const FULL_FLANK_ANGLE: f32 = std::f32::consts::PI / 4.0;
pub const PARTIAL_FLANK_ANGLE: f32 = std::f32::consts::PI * 3.0 / 4.0;
pub const BEAM_DURATION_PRECISION: f32 = 2.5;
pub const MAX_BACK_FLANK_PRECISION: f32 = 0.75;
pub const MAX_SIDE_FLANK_PRECISION: f32 = 0.25;
pub const MAX_HEADSHOT_PRECISION: f32 = 1.0;
pub const MAX_TOP_HEADSHOT_PRECISION: f32 = 0.5;
pub const MAX_BEAM_DUR_PRECISION: f32 = 0.25;
pub const MAX_MELEE_POISE_PRECISION: f32 = 0.5;
pub const MAX_BLOCK_POISE_COST: f32 = 25.0;
pub const FALLBACK_BLOCK_STRENGTH: f32 = 3.3;
pub const BEHIND_TARGET_ANGLE: f32 = 45.0;
pub const BASE_PARRIED_POISE_PUNISHMENT: f32 = 100.0 / 3.5;

#[derive(Copy, Clone)]
pub struct AttackerInfo<'a> {
    pub entity: EcsEntity,
    pub uid: Uid,
    pub group: Option<&'a Group>,
    pub energy: Option<&'a Energy>,
    pub combo: Option<&'a Combo>,
    /// The attacker's cached gear/skill/body aggregates. `None` means the
    /// entity has no `Inventory`, and every read site falls back to
    /// [`DerivedStats::default()`] — which is exactly the no-inventory result
    /// of the arithmetic this cache replaced.
    pub derived: Option<&'a DerivedStats>,
    pub stats: Option<&'a Stats>,
    pub mass: Option<&'a Mass>,
    pub pos: Option<Vec3<f32>>,
    pub buffs: Option<&'a Buffs>,
    /// The attacker's held class(es), so `CasterLevelFailChance` can resolve
    /// the caster's own class level instead of the raw character level for
    /// a multiclass character. `None` for non-caster entities.
    pub character_class: Option<&'a CharacterClass>,
}

#[derive(Copy, Clone)]
pub struct TargetInfo<'a> {
    pub entity: EcsEntity,
    pub uid: Uid,
    /// Still needed for the block/parry strength read, which is a shield
    /// lookup rather than one of the cached aggregates.
    pub inventory: Option<&'a Inventory>,
    /// The target's cached gear/skill/body aggregates — see
    /// [`AttackerInfo::derived`]. Attunement gating (ENG-D2c) is already
    /// folded into them, so no separate attuned-item set is threaded here.
    pub derived: Option<&'a DerivedStats>,
    pub stats: Option<&'a Stats>,
    pub health: Option<&'a Health>,
    pub pos: Vec3<f32>,
    pub ori: Option<&'a Ori>,
    pub char_state: Option<&'a CharacterState>,
    pub energy: Option<&'a Energy>,
    pub buffs: Option<&'a Buffs>,
    pub mass: Option<&'a Mass>,
    pub player: Option<&'a Player>,
}

#[derive(Clone, Copy)]
pub struct AttackOptions {
    pub target_dodging: bool,
    /// Result of [`permit_pvp`]
    pub permit_pvp: bool,
    pub target_group: GroupTarget,
    /// When set to `true`, entities in the same group or pets & pet owners may
    /// hit eachother albeit the target_group being OutOfGroup
    pub allow_friendly_fire: bool,
    pub precision_mult: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)] // TODO: Yeet clone derive
pub struct Attack {
    damages: Vec<AttackDamage>,
    effects: Vec<AttackEffect>,
    precision_multiplier: f32,
    pub(crate) blockable: bool,
    ability_info: Option<AbilityInfo>,
}

impl Attack {
    pub fn new(ability_info: Option<AbilityInfo>) -> Self {
        Self {
            damages: Vec::new(),
            effects: Vec::new(),
            precision_multiplier: 1.0,
            blockable: true,
            ability_info,
        }
    }

    /// The magic source to attribute to any `HealthChange` this attack
    /// causes, read once from `ability_info.ability_meta.source`. Threaded
    /// into every damage-emitting `HealthChange` this attack constructs.
    /// `None` for weapon swings, falls, environment, and sourceless
    /// abilities.
    fn magic_source(&self) -> Option<MagicSource> {
        self.ability_info.and_then(|ai| ai.ability_meta.source)
    }

    #[must_use]
    pub fn with_damage(mut self, damage: AttackDamage) -> Self {
        self.damages.push(damage);
        self
    }

    #[must_use]
    pub fn with_effect(mut self, effect: AttackEffect) -> Self {
        self.effects.push(effect);
        self
    }

    #[must_use]
    pub fn with_precision(mut self, precision_multiplier: f32) -> Self {
        self.precision_multiplier = precision_multiplier;
        self
    }

    #[must_use]
    pub fn with_blockable(mut self, blockable: bool) -> Self {
        self.blockable = blockable;
        self
    }

    #[must_use]
    pub fn with_combo_requirement(self, combo: i32, requirement: CombatRequirement) -> Self {
        self.with_effect(
            AttackEffect::new(None, CombatEffect::Combo(combo)).with_requirement(requirement),
        )
    }

    #[must_use]
    pub fn with_combo(self, combo: i32) -> Self {
        self.with_combo_requirement(combo, CombatRequirement::AnyDamage)
    }

    #[must_use]
    pub fn with_combo_increment(self) -> Self { self.with_combo(1) }

    pub fn effects(&self) -> impl Iterator<Item = &AttackEffect> { self.effects.iter() }

    pub fn compute_block_damage_decrement(
        blockable: bool,
        attacker: Option<&AttackerInfo>,
        target: &TargetInfo,
        source: AttackSource,
        dir: Dir,
        damage: Damage,
        time: Time,
        emitters: &mut (impl EmitExt<ParryHookEvent> + EmitExt<PoiseChangeEvent>),
        mut emit_outcome: impl FnMut(Outcome),
    ) -> f32 {
        if blockable && damage.value > 0.0 {
            if let (Some(char_state), Some(ori), Some(inventory)) =
                (target.char_state, target.ori, target.inventory)
            {
                let is_parry = char_state.is_parry(source);
                let is_block = char_state.is_block(source);
                let mut block_strength = block_strength(inventory, char_state);

                if ori.look_vec().angle_between(-dir.with_z(0.0)) < char_state.block_angle()
                    && (is_parry || is_block)
                    && block_strength > 0.0
                {
                    if is_parry {
                        block_strength = damage.value;

                        emitters.emit(ParryHookEvent {
                            defender: target.entity,
                            attacker: attacker.map(|a| a.entity),
                            source,
                            poise_multiplier: 2.0 - (damage.value / block_strength).min(1.0),
                        });
                    }

                    let poise_cost =
                        (damage.value / block_strength).min(1.0) * MAX_BLOCK_POISE_COST;

                    let poise_change = Poise::apply_poise_reduction(
                        poise_cost,
                        target.derived,
                        target.char_state,
                        target.stats,
                    );

                    emit_outcome(Outcome::Block {
                        parry: is_parry,
                        pos: target.pos,
                        uid: target.uid,
                    });
                    emitters.emit(PoiseChangeEvent {
                        entity: target.entity,
                        change: PoiseChange {
                            amount: -poise_change,
                            impulse: *dir,
                            by: attacker.map(|x| (*x).into()),
                            cause: Some(DamageSource::from(source)),
                            time,
                        },
                    });

                    block_strength
                } else {
                    0.0
                }
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    pub fn compute_damage_reduction(
        attacker: Option<&AttackerInfo>,
        target: &TargetInfo,
        damage: Damage,
    ) -> f32 {
        if damage.value > 0.0 {
            let attacker_penetration = attacker
                .and_then(|a| a.stats)
                .map_or(0.0, |s| s.mitigations_penetration)
                .clamp(0.0, 1.0);
            let raw_damage_reduction =
                Damage::compute_damage_reduction(Some(damage), target.derived, target.stats);

            if raw_damage_reduction >= 1.0 {
                raw_damage_reduction
            } else {
                (1.0 - attacker_penetration) * raw_damage_reduction
            }
        } else {
            0.0
        }
    }

    /// A weapon outside the wielder's trained set lands clumsily: physical
    /// output (damage, poise, knockback, crit chance) is scaled down.
    /// Spells are never affected — a caster's magic does not care what is in
    /// their hands. `tool: None` (unarmed strikes, natural weapons, an NPC
    /// `Empty`-tool attack) is always treated as proficient, and an attacker
    /// with no `Stats` (or one whose class leaves `proficient_tools`
    /// permissive) is unaffected. Narrowed by [`WeaponRole`] exactly like
    /// grip: a class proficient with a `Staff`'s caster role but not its
    /// martial role (or vice versa) is non-proficient when wielding the
    /// other role's kit, even though both share the same `ToolKind`.
    pub fn proficiency_multiplier(
        stats: Option<&Stats>,
        ability_info: Option<AbilityInfo>,
        is_magic: bool,
    ) -> f32 {
        if is_magic {
            return 1.0;
        }
        let Some(stats) = stats else {
            return 1.0;
        };
        let Some(tool) = ability_info.and_then(|ai| ai.tool) else {
            return 1.0;
        };
        let hands = ability_info.and_then(|ai| ai.hand).map(|hand| match hand {
            HandInfo::TwoHanded => Hands::Two,
            HandInfo::MainHand | HandInfo::OffHand => Hands::One,
        });
        let role = ability_info.and_then(|ai| ai.role);
        if stats.proficient_tools.allows(tool, hands, role) {
            1.0
        } else {
            stats.non_proficient_damage_mult
        }
    }

    pub fn apply_attack(
        &self,
        attacker: Option<AttackerInfo>,
        target: &TargetInfo,
        dir: Dir,
        options: AttackOptions,
        // Currently strength_modifier just modifies damage,
        // maybe look into modifying strength of other effects?
        strength_modifier: f32,
        attack_source: AttackSource,
        time: Time,
        emitters: &mut (
                 impl EmitExt<HealthChangeEvent>
                 + EmitExt<EnergyChangeEvent>
                 + EmitExt<ParryHookEvent>
                 + EmitExt<KnockbackEvent>
                 + EmitExt<BuffEvent>
                 + EmitExt<PoiseChangeEvent>
                 + EmitExt<ComboChangeEvent>
                 + EmitExt<EntityAttackedHookEvent>
                 + EmitExt<TransformEvent>
             ),
        mut emit_outcome: impl FnMut(Outcome),
        rng: &mut rand::rngs::ThreadRng,
        damage_instance_offset: u64,
    ) -> bool {
        // Combat-resolution tuning — one cached asset read per attack, reused
        // by the to-hit and crit rolls below. The `MaterialStatManifest` this
        // function used to load alongside it is gone: every gear-derived
        // number it fed is now read off the attacker's/target's cache, which
        // is where the manifest is consulted instead.
        let combat_tuning = &Ron::<CombatTuning>::load_expect("common.combat_tuning")
            .read()
            .0;

        let AttackOptions {
            target_dodging,
            permit_pvp,
            allow_friendly_fire,
            target_group,
            precision_mult,
        } = options;

        // Combat resolution to-hit roll (BL-52). After active avoidance (the
        // dodge/jump `target_dodging` below) but before damage, a hostile
        // **single-target** attack may whiff entirely based on attacker
        // `accuracy` vs target `evasion`:
        //   hit% = clamp(base + (acc − eva)·k, floor, ceil)   (floor 0.05/ceil 1.0)
        // A miss skips all damage and harmful effects (same gate as a dodge), so
        // damage stays full on the blows that land. Rolled once per `apply_attack`
        // (per blow/impact for melee/projectiles). **AoE never rolls** (Beam /
        // Shockwave / Explosion / Arc / Pool) — by design it auto-hits the radius
        // and is mitigated passively by resistances (P3), keeping the hot
        // multi-target path RNG-free for raid/AvA scale. A beneficial in-group
        // effect (e.g. an ally heal routed through an attack) is exempt below —
        // accuracy only gates hostile outcomes.
        let is_single_target = matches!(
            attack_source,
            AttackSource::Melee | AttackSource::Projectile
        );
        // BL-52 P3: a magic ability (one carrying an `AbilityMeta` `source`, the
        // same signal the BL-36 antimagic gate uses) rolls the caster's *magic*
        // accuracy against the target's *magic* evasion; physical attacks use the
        // physical pair. A missed single-target spell fizzles — the same no-op as
        // a physical miss (no damage, no harmful effects; in-group beneficial
        // effects like ally heals stay exempt in `avoid_effect` below).
        let is_magic = self
            .ability_info
            .is_some_and(|ai| ai.ability_meta.source.is_some());
        // A weapon outside the wielder's proficiency set (see `ClassProficiencies`)
        // deals reduced physical output. Resolved once here and folded into
        // damage, poise, knockback and crit chance below; never applied to
        // `is_magic` attacks.
        let proficiency_mult = Self::proficiency_multiplier(
            attacker.and_then(|a| a.stats),
            self.ability_info,
            is_magic,
        );
        let attack_missed = is_single_target && {
            let (accuracy, evasion) = if is_magic {
                (
                    attacker
                        .and_then(|a| a.stats)
                        .map_or(0.0, |s| s.magic_accuracy),
                    target.stats.map_or(0.0, |s| s.magic_evasion),
                )
            } else {
                // BL-52 P5: physical evasion = class/level + buffs (on `Stats`)
                // plus the gear contribution derived from armor weight/shield.
                // No cache means no `Inventory`, i.e. no gear to evade with —
                // the same `0.0` the no-inventory early return used to give.
                let armor_evasion = target.derived.map_or(0.0, |derived| {
                    compute_armor_evasion(derived, target.stats, combat_tuning)
                });
                (
                    attacker.and_then(|a| a.stats).map_or(0.0, |s| s.accuracy),
                    target.stats.map_or(0.0, |s| s.evasion) + armor_evasion,
                )
            };
            let hit_chance = (combat_tuning.base_hit + (accuracy - evasion) * combat_tuning.hit_k)
                .clamp(combat_tuning.hit_floor, combat_tuning.hit_ceil);
            rng.random::<f32>() >= hit_chance
        };
        // A charmed/dominated attacker's hostile attack on its charmer is a
        // no-op — the same no-op a whiff already is, so it folds into
        // `attack_missed` and every downstream gate (damage, harmful
        // effects, crit, the Miss floater) falls out for free. Gated behind
        // `is_single_target` so the AoE path stays scan-free; the
        // `charmed_by` scan itself is a no-alloc `iter_kind` walk.
        let charmed_by_target = is_single_target
            && attacker
                .and_then(|a| a.buffs)
                .is_some_and(|buffs| buffs.charmed_by(target.uid));
        let attack_missed = attack_missed || charmed_by_target;
        // Surface the whiff with a floating "Miss" over the target — but
        // never over an in-group ally (mirrors the `avoid_effect` beneficial
        // exemption, so a future single-target ally ability can't show a
        // contradictory "Miss" while its beneficial effect still lands).
        if attack_missed && !matches!(target_group, GroupTarget::InGroup) {
            emit_outcome(Outcome::Miss {
                pos: target.pos,
                target: target.uid,
            });
        }

        // target == OutOfGroup is basic heuristic that this
        // "attack" has negative effects.
        //
        // so if target dodges this "attack" or we don't want to harm target,
        // it should avoid such "damage" or effect
        let avoid_damage = |attack_damage: &AttackDamage| {
            attack_missed
                || target_dodging
                || (!permit_pvp && matches!(attack_damage.target, Some(GroupTarget::OutOfGroup)))
        };
        let avoid_effect = |attack_effect: &AttackEffect| {
            // A miss whiffs harmful effects but never an in-group beneficial one
            // (ally heal/buff). `All`/`None`-targeted beneficial effects are not
            // yet exempted — the full allied-100% rule lands in P3 (CR3.2).
            (attack_missed && !matches!(attack_effect.target, Some(GroupTarget::InGroup)))
                || target_dodging
                || (!permit_pvp && matches!(attack_effect.target, Some(GroupTarget::OutOfGroup)))
        };

        let from_precision_mult = attacker
            .and_then(|a| a.stats)
            .and_then(|s| {
                s.conditional_precision_modifiers
                    .iter()
                    .filter_map(|(req, mult, ovrd)| {
                        req.as_ref()
                            .is_none_or(|r| {
                                r.requirement_met(
                                    (
                                        target.health,
                                        target.buffs,
                                        target.char_state,
                                        target.ori,
                                        Some(target.uid),
                                    ),
                                    (
                                        attacker.map(|a| a.entity),
                                        attacker.and_then(|a| a.energy),
                                        attacker.and_then(|a| a.combo),
                                    ),
                                    attacker.map(|a| a.uid),
                                    0.0,
                                    emitters,
                                    dir,
                                    Some(attack_source),
                                    self.ability_info,
                                    rng,
                                    attacker.and_then(|a| a.stats).map(|s| s.character_level),
                                    attacker.and_then(|a| a.character_class),
                                )
                            })
                            .then_some((*mult, *ovrd))
                    })
                    .chain(precision_mult.iter().map(|val| (*val, false)))
                    .reduce(|(val_a, ovrd_a), (val_b, ovrd_b)| {
                        if ovrd_a || ovrd_b {
                            (val_a.min(val_b), true)
                        } else {
                            (val_a.max(val_b), false)
                        }
                    })
            })
            .map(|(val, _)| val);

        let from_precision_vulnerability_mult = target
            .stats
            .and_then(|s| s.precision_vulnerability_multiplier_override);

        let precision_mult = match (from_precision_mult, from_precision_vulnerability_mult) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };

        // Unified critical roll: positional/conditional precision (flank,
        // backstab, target poised, `ImminentCritical`, precision-vulnerability)
        // already fired above and folds in as a guaranteed crit. If none did,
        // roll the attacker's `crit_chance` for a random crit. Magnitude reuses
        // the existing precision system (`precision_power`). Single-target only
        // and only on a landed blow — keeping the AoE hot path RNG-free (AoE
        // crit would re-add per-target rolls). The `crit_chance_floor` gives
        // every attacker a small baseline crit chance (no dead stat); the cap
        // bounds rolled crits (guaranteed positional crits bypass it). The
        // finer "side-flank = +chance / backstab = guaranteed" split is
        // deferred — positional precision stays deterministic here to avoid
        // refactoring the shared upstream precision path.
        let precision_mult = precision_mult.or_else(|| {
            if is_single_target && !attack_missed {
                let crit_chance = (attacker
                    .and_then(|a| a.stats)
                    .map_or(0.0, |s| s.crit_chance)
                    * proficiency_mult)
                    .clamp(
                        combat_tuning.crit_chance_floor,
                        combat_tuning.crit_chance_cap,
                    );
                (rng.random::<f32>() < crit_chance).then_some(combat_tuning.crit_precision_mult)
            } else {
                None
            }
        });

        let precision_power = 1.0
            + ((self.precision_multiplier - 1.0)
                * attacker
                    .and_then(|a| a.stats)
                    .map_or(1.0, |s| s.precision_power_mult));

        let attacked_modifiers = AttackedModification::attacked_modifiers(
            target,
            attacker,
            emitters,
            dir,
            Some(attack_source),
            self.ability_info,
            rng,
        );

        let mut is_applied = false;
        let mut accumulated_damage = 0.0;
        // BL-06 (Q2/Q3): `spell_power` is a dedicated magic-damage channel that
        // multiplies outgoing damage ONLY for magic-source attacks (the same
        // `is_magic` signal used for the to-hit roll), so caster damage passives
        // never leak onto physical weapon swings. Physical attacks use the global
        // `attack_damage_modifier` alone.
        let damage_modifier = attacker.and_then(|a| a.stats).map_or(1.0, |s| {
            if is_magic {
                s.attack_damage_modifier * s.spell_power_for(self.magic_source())
            } else {
                s.attack_damage_modifier * proficiency_mult
            }
        });
        // Conditional "vs creature kind" bonus — the Cleric smite (and any
        // future slayer-style conditional) is one slot in the attacker's
        // `bonus_damage_vs` array, indexed by the target's `creature_kind`.
        // The target's kind is fixed for the whole attack, so resolve it once.
        // Reads `Stats.creature_kind`, not `original_body.creature_kind()`
        // directly, so an `EntityConfig`-authored override on the target is
        // honored. This is an array index, strictly cheaper than the previous
        // per-hit nested match.
        let damage_modifier = damage_modifier
            * target
                .stats
                .and_then(|s| s.creature_kind)
                .map_or(1.0, |kind| {
                    1.0 + attacker
                        .and_then(|a| a.stats)
                        .map_or(0.0, |s| s.bonus_damage_vs[kind as usize])
                });
        // BL-06 (Q2): the heal *source's* `heal_power` scales `CombatEffect::Heal`
        // output (the target is usually an ally). Buff/aura regen (a separate path
        // in common-systems) is deliberately NOT scaled yet — a follow-up if a
        // HoT-healer passive ever wants it.
        let heal_power = attacker.and_then(|a| a.stats).map_or(1.0, |s| s.heal_power);
        for damage in self
            .damages
            .iter()
            .filter(|d| {
                allow_friendly_fire
                    || d.target
                        .is_none_or(|t| t == GroupTarget::All || t == target_group)
            })
            .filter(|d| !avoid_damage(d))
        {
            let damage_instance = damage.instance + damage_instance_offset;
            is_applied = true;

            let damage_reduction =
                Attack::compute_damage_reduction(attacker.as_ref(), target, damage.damage);

            // BL-52 P3: AoE damage is mitigated passively by the target's typed
            // elemental resistance — the AoE counterpart to single-target evasion
            // (AoE never rolls to-hit, so this is its only stat mitigation, on top
            // of armor DR). Physical kinds return 0 here and rely on the existing
            // `damage_reduction` only (no double-count). Single-target damage is
            // gated by the to-hit roll instead, so it is NOT resistance-mitigated.
            // Resistance composes with armor DR as independent layers, soft-capped
            // so stacking can't reach immunity.
            let damage_reduction = if is_single_target {
                damage_reduction
            } else {
                // Floor at 0 so a (currently nonexistent) negative resistance
                // can't silently *amplify* AoE damage — element vulnerability, if
                // ever wanted, should be its own deliberate mechanic, not a
                // side effect of subtraction. Cap so stacking can't reach immunity.
                let resist = target
                    .stats
                    .map_or(0.0, |s| s.aoe_resistance(damage.damage.kind))
                    .clamp(0.0, combat_tuning.resist_soft_cap);
                1.0 - (1.0 - damage_reduction) * (1.0 - resist)
            };

            let block_damage_decrement = Attack::compute_block_damage_decrement(
                self.blockable,
                attacker.as_ref(),
                target,
                attack_source,
                dir,
                damage.damage,
                time,
                emitters,
                &mut emit_outcome,
            );

            let mut change = damage.damage.calculate_health_change(
                damage_reduction,
                block_damage_decrement,
                attacker.map(|x| x.into()),
                precision_mult,
                precision_power,
                combat_tuning.crit_damage_mult,
                strength_modifier * damage_modifier,
                time,
                damage_instance,
                DamageSource::from(attack_source),
            );
            // `calculate_health_change` is a method on `Damage`, which does not
            // carry the ability that caused it, so the source is attributed here
            // instead of threading a new parameter through that function.
            change.magic_source = self.magic_source();
            let applied_damage = -change.amount;
            accumulated_damage += applied_damage;

            if change.amount.abs() > Health::HEALTH_EPSILON {
                emitters.emit(HealthChangeEvent {
                    entity: target.entity,
                    change,
                });
                match damage.damage.kind {
                    DamageKind::Slashing => {
                        // For slashing damage, reduce target energy by some fraction of applied
                        // damage. When target would lose more energy than they have, deal an
                        // equivalent amount of damage
                        if let Some(target_energy) = target.energy {
                            let energy_change = applied_damage * SLASHING_ENERGY_FRACTION;
                            if energy_change > target_energy.current() {
                                let health_damage = energy_change - target_energy.current();
                                accumulated_damage += health_damage;
                                let health_change = HealthChange {
                                    amount: -health_damage,
                                    by: attacker.map(|x| x.into()),
                                    cause: Some(DamageSource::from(attack_source)),
                                    magic_source: self.magic_source(),
                                    time,
                                    precise: precision_mult.is_some(),
                                    instance: damage_instance,
                                };
                                emitters.emit(HealthChangeEvent {
                                    entity: target.entity,
                                    change: health_change,
                                });
                            }
                            emitters.emit(EnergyChangeEvent {
                                entity: target.entity,
                                change: -energy_change,
                                reset_rate: false,
                            });
                        }
                    },
                    DamageKind::Crushing => {
                        // For crushing damage, reduce target poise by some fraction of the amount
                        // of damage that was reduced by target's protection
                        // Damage reduction should never equal 1 here as otherwise the check above
                        // that health change amount is greater than 0 would fail.
                        let reduced_damage =
                            applied_damage * damage_reduction / (1.0 - damage_reduction);
                        let poise = reduced_damage
                            * CRUSHING_POISE_FRACTION
                            * attacker
                                .and_then(|a| a.stats)
                                .map_or(1.0, |s| s.poise_damage_modifier)
                            * proficiency_mult;
                        let change = -Poise::apply_poise_reduction(
                            poise,
                            target.derived,
                            target.char_state,
                            target.stats,
                        );
                        let poise_change = PoiseChange {
                            amount: change,
                            impulse: *dir,
                            by: attacker.map(|x| x.into()),
                            cause: Some(DamageSource::from(attack_source)),
                            time,
                        };
                        if change.abs() > Poise::POISE_EPSILON {
                            // If target is in a stunned state, apply extra poise damage as health
                            // damage instead
                            if let Some(CharacterState::Stunned(data)) = target.char_state {
                                let health_change =
                                    change * data.static_data.poise_state.damage_multiplier();
                                let health_change = HealthChange {
                                    amount: health_change,
                                    by: attacker.map(|x| x.into()),
                                    cause: Some(DamageSource::from(attack_source)),
                                    magic_source: self.magic_source(),
                                    instance: damage_instance,
                                    precise: precision_mult.is_some(),
                                    time,
                                };
                                accumulated_damage -= health_change.amount;
                                emitters.emit(HealthChangeEvent {
                                    entity: target.entity,
                                    change: health_change,
                                });
                            } else {
                                emitters.emit(PoiseChangeEvent {
                                    entity: target.entity,
                                    change: poise_change,
                                });
                            }
                        }
                    },
                    // Piercing damage ignores some penetration, and is handled when damage
                    // reduction is computed. Energy and the magical/elemental kinds carry no
                    // special physical mitigation here (per-kind resistances + the
                    // Radiant/Necrotic affinity are a future balance task — ENG-A2 deferral).
                    DamageKind::Piercing
                    | DamageKind::Energy
                    | DamageKind::Acid
                    | DamageKind::Cold
                    | DamageKind::Fire
                    | DamageKind::Force
                    | DamageKind::Lightning
                    | DamageKind::Necrotic
                    | DamageKind::Poison
                    | DamageKind::Psychic
                    | DamageKind::Radiant
                    | DamageKind::Thunder => {},
                }
                for effect in damage.effects.iter() {
                    match effect {
                        CombatEffect::Knockback(kb) => {
                            let impulse = kb.modify_strength(proficiency_mult).calculate_impulse(
                                dir,
                                target.char_state,
                                attacker.and_then(|a| a.stats),
                            ) * strength_modifier;
                            if !impulse.is_approx_zero() {
                                emitters.emit(KnockbackEvent {
                                    entity: target.entity,
                                    impulse,
                                });
                            }
                        },
                        CombatEffect::EnergyReward(ec) => {
                            if let Some(attacker) = attacker {
                                emitters.emit(EnergyChangeEvent {
                                    entity: attacker.entity,
                                    change: *ec
                                        * attacker.derived.map_or(1.0, |d| d.energy_reward_mod)
                                        * strength_modifier
                                        * attacker.stats.map_or(1.0, |s| s.energy_reward_modifier)
                                        * attacked_modifiers.energy_reward,
                                    reset_rate: false,
                                });
                            }
                        },
                        CombatEffect::Buff(b) => {
                            if rng.random::<f32>() < b.chance {
                                emitters.emit(BuffEvent {
                                    entity: target.entity,
                                    buff_change: BuffChange::Add(b.to_buff(
                                        time,
                                        (attacker.map(|a| a.uid), attacker.and_then(|a| a.mass)),
                                        (target.stats, target.mass),
                                        applied_damage,
                                        strength_modifier,
                                        self.ability_info,
                                    )),
                                });
                            }
                        },
                        CombatEffect::Lifesteal(l) => {
                            if let Some(attacker_entity) = attacker.map(|a| a.entity) {
                                let change = HealthChange {
                                    amount: applied_damage * l * strength_modifier,
                                    by: attacker.map(|a| a.into()),
                                    cause: None,
                                    magic_source: self.magic_source(),
                                    time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                if change.amount.abs() > Health::HEALTH_EPSILON {
                                    emitters.emit(HealthChangeEvent {
                                        entity: attacker_entity,
                                        change,
                                    });
                                }
                            }
                        },
                        CombatEffect::Poise(p) => {
                            let change = -Poise::apply_poise_reduction(
                                *p,
                                target.derived,
                                target.char_state,
                                target.stats,
                            ) * strength_modifier
                                * attacker
                                    .and_then(|a| a.stats)
                                    .map_or(1.0, |s| s.poise_damage_modifier)
                                * proficiency_mult;
                            if change.abs() > Poise::POISE_EPSILON {
                                let poise_change = PoiseChange {
                                    amount: change,
                                    impulse: *dir,
                                    by: attacker.map(|x| x.into()),
                                    cause: Some(DamageSource::from(attack_source)),
                                    time,
                                };
                                emitters.emit(PoiseChangeEvent {
                                    entity: target.entity,
                                    change: poise_change,
                                });
                            }
                        },
                        CombatEffect::Heal(h) => {
                            let change = HealthChange {
                                amount: *h * strength_modifier * heal_power,
                                by: attacker.map(|a| a.into()),
                                cause: None,
                                magic_source: self.magic_source(),
                                time,
                                precise: false,
                                instance: rand::random(),
                            };
                            if change.amount.abs() > Health::HEALTH_EPSILON {
                                emitters.emit(HealthChangeEvent {
                                    entity: target.entity,
                                    change,
                                });
                            }
                        },
                        CombatEffect::Combo(c) => {
                            if let Some(attacker_entity) = attacker.map(|a| a.entity) {
                                emitters.emit(ComboChangeEvent {
                                    entity: attacker_entity,
                                    change: (*c as f32 * strength_modifier).ceil() as i32,
                                });
                            }
                        },
                        CombatEffect::AdditionalDamage(damage) => {
                            let change = {
                                let mut change = change;
                                change.amount *= damage * strength_modifier;
                                change.instance = rand::random();
                                change
                            };
                            accumulated_damage -= change.amount;
                            emitters.emit(HealthChangeEvent {
                                entity: target.entity,
                                change,
                            });
                        },
                        CombatEffect::RefreshBuff(chance, b) => {
                            if rng.random::<f32>() < *chance {
                                emitters.emit(BuffEvent {
                                    entity: target.entity,
                                    buff_change: BuffChange::Refresh(*b),
                                });
                            }
                        },
                        CombatEffect::SelfBuff(b) => {
                            if let Some(attacker) = attacker
                                && rng.random::<f32>() < b.chance
                            {
                                emitters.emit(BuffEvent {
                                    entity: attacker.entity,
                                    buff_change: BuffChange::Add(b.to_self_buff(
                                        time,
                                        (Some(attacker.uid), attacker.stats, attacker.mass),
                                        applied_damage,
                                        strength_modifier,
                                        self.ability_info,
                                    )),
                                });
                            }
                        },
                        CombatEffect::Energy(e) => {
                            emitters.emit(EnergyChangeEvent {
                                entity: target.entity,
                                change: e * strength_modifier,
                                reset_rate: true,
                            });
                        },
                        CombatEffect::Transform {
                            entity_spec,
                            allow_players,
                        } => {
                            if target.player.is_none() || *allow_players {
                                emitters.emit(TransformEvent {
                                    target_entity: target.uid,
                                    entity_info: {
                                        let Ok(entity_config) = Ron::<EntityConfig>::load(
                                            entity_spec,
                                        )
                                        .inspect_err(|error| {
                                            error!(
                                                ?entity_spec,
                                                ?error,
                                                "Could not load entity configuration for death \
                                                 effect"
                                            )
                                        }) else {
                                            continue;
                                        };

                                        EntityInfo::at(target.pos).with_entity_config(
                                            entity_config.read().clone().into_inner(),
                                            Some(entity_spec),
                                            rng,
                                            None,
                                        )
                                    },
                                    allow_players: *allow_players,
                                    delete_on_failure: false,
                                });
                            }
                        },
                        CombatEffect::DebuffsVulnerable {
                            mult,
                            scaling,
                            filter_attacker,
                            filter_weapon,
                        } => {
                            if let Some(buffs) = target.buffs {
                                let num_debuffs = buffs.iter_active().flatten().filter(|b| {
                                    let debuff_filter = matches!(b.kind.differentiate(), BuffDescriptor::SimpleNegative);
                                    let attacker_filter = !filter_attacker || matches!(b.source, BuffSource::Character { by, .. } if Some(by) == attacker.map(|a| a.uid));
                                    let weapon_filter = filter_weapon.is_none_or(|w| matches!(b.source, BuffSource::Character { tool_kind, .. } if Some(w) == tool_kind));
                                    debuff_filter && attacker_filter && weapon_filter
                                }).count();
                                if num_debuffs > 0 {
                                    let change = {
                                        let mut change = change;
                                        change.amount *= scaling.factor(num_debuffs as f32, 1.0)
                                            * mult
                                            * strength_modifier;
                                        change.instance = rand::random();
                                        change
                                    };
                                    accumulated_damage -= change.amount;
                                    emitters.emit(HealthChangeEvent {
                                        entity: target.entity,
                                        change,
                                    });
                                }
                            }
                        },
                    }
                }
            }
        }
        for effect in self
            .effects
            .iter()
            .chain(
                attacker
                    .and_then(|a| a.stats)
                    .map(|s| s.effects_on_attack.iter())
                    .into_iter()
                    .flatten(),
            )
            .filter(|e| {
                allow_friendly_fire
                    || e.target
                        .is_none_or(|t| t == GroupTarget::All || t == target_group)
            })
            .filter(|e| !avoid_effect(e))
        {
            let requirements_met = effect.requirements.iter().all(|req| {
                req.requirement_met(
                    (
                        target.health,
                        target.buffs,
                        target.char_state,
                        target.ori,
                        Some(target.uid),
                    ),
                    (
                        attacker.map(|a| a.entity),
                        attacker.and_then(|a| a.energy),
                        attacker.and_then(|a| a.combo),
                    ),
                    attacker.map(|a| a.uid),
                    accumulated_damage,
                    emitters,
                    dir,
                    Some(attack_source),
                    self.ability_info,
                    rng,
                    attacker.and_then(|a| a.stats).map(|s| s.character_level),
                    attacker.and_then(|a| a.character_class),
                )
            });
            if requirements_met {
                let mut strength_modifier = strength_modifier;
                for modification in effect.modifications.iter() {
                    modification.apply_mod(
                        attacker.and_then(|a| a.pos),
                        Some(target.pos),
                        &mut strength_modifier,
                    );
                }
                let strength_modifier = strength_modifier;
                is_applied = true;
                match &effect.effect {
                    CombatEffect::Knockback(kb) => {
                        let impulse = kb.modify_strength(proficiency_mult).calculate_impulse(
                            dir,
                            target.char_state,
                            attacker.and_then(|a| a.stats),
                        ) * strength_modifier;
                        if !impulse.is_approx_zero() {
                            emitters.emit(KnockbackEvent {
                                entity: target.entity,
                                impulse,
                            });
                        }
                    },
                    CombatEffect::EnergyReward(ec) => {
                        if let Some(attacker) = attacker {
                            emitters.emit(EnergyChangeEvent {
                                entity: attacker.entity,
                                change: ec
                                    * attacker.derived.map_or(1.0, |d| d.energy_reward_mod)
                                    * strength_modifier
                                    * attacker.stats.map_or(1.0, |s| s.energy_reward_modifier)
                                    * attacked_modifiers.energy_reward,
                                reset_rate: false,
                            });
                        }
                    },
                    CombatEffect::Buff(b) => {
                        if rng.random::<f32>() < b.chance {
                            emitters.emit(BuffEvent {
                                entity: target.entity,
                                buff_change: BuffChange::Add(b.to_buff(
                                    time,
                                    (attacker.map(|a| a.uid), attacker.and_then(|a| a.mass)),
                                    (target.stats, target.mass),
                                    accumulated_damage,
                                    strength_modifier,
                                    self.ability_info,
                                )),
                            });
                        }
                    },
                    CombatEffect::Lifesteal(l) => {
                        if let Some(attacker_entity) = attacker.map(|a| a.entity) {
                            let change = HealthChange {
                                amount: accumulated_damage * l * strength_modifier,
                                by: attacker.map(|a| a.into()),
                                cause: None,
                                magic_source: self.magic_source(),
                                time,
                                precise: false,
                                instance: rand::random(),
                            };
                            if change.amount.abs() > Health::HEALTH_EPSILON {
                                emitters.emit(HealthChangeEvent {
                                    entity: attacker_entity,
                                    change,
                                });
                            }
                        }
                    },
                    CombatEffect::Poise(p) => {
                        let change = -Poise::apply_poise_reduction(
                            *p,
                            target.derived,
                            target.char_state,
                            target.stats,
                        ) * strength_modifier
                            * attacker
                                .and_then(|a| a.stats)
                                .map_or(1.0, |s| s.poise_damage_modifier)
                            * proficiency_mult;
                        if change.abs() > Poise::POISE_EPSILON {
                            let poise_change = PoiseChange {
                                amount: change,
                                impulse: *dir,
                                by: attacker.map(|x| x.into()),
                                cause: Some(attack_source.into()),
                                time,
                            };
                            emitters.emit(PoiseChangeEvent {
                                entity: target.entity,
                                change: poise_change,
                            });
                        }
                    },
                    CombatEffect::Heal(h) => {
                        let change = HealthChange {
                            amount: h * strength_modifier,
                            by: attacker.map(|a| a.into()),
                            cause: None,
                            magic_source: self.magic_source(),
                            time,
                            precise: false,
                            instance: rand::random(),
                        };
                        if change.amount.abs() > Health::HEALTH_EPSILON {
                            emitters.emit(HealthChangeEvent {
                                entity: target.entity,
                                change,
                            });
                        }
                    },
                    CombatEffect::Combo(c) => {
                        if let Some(attacker_entity) = attacker.map(|a| a.entity) {
                            emitters.emit(ComboChangeEvent {
                                entity: attacker_entity,
                                change: (*c as f32 * strength_modifier).ceil() as i32,
                            });
                        }
                    },
                    CombatEffect::AdditionalDamage(damage) => {
                        let change = HealthChange {
                            amount: -accumulated_damage * damage * strength_modifier,
                            by: attacker.map(|a| a.into()),
                            cause: Some(DamageSource::from(attack_source)),
                            magic_source: self.magic_source(),
                            time,
                            precise: precision_mult.is_some(),
                            instance: rand::random(),
                        };
                        accumulated_damage -= change.amount;
                        emitters.emit(HealthChangeEvent {
                            entity: target.entity,
                            change,
                        });
                    },
                    CombatEffect::RefreshBuff(chance, b) => {
                        if rng.random::<f32>() < *chance {
                            emitters.emit(BuffEvent {
                                entity: target.entity,
                                buff_change: BuffChange::Refresh(*b),
                            });
                        }
                    },
                    CombatEffect::SelfBuff(b) => {
                        if let Some(attacker) = attacker
                            && rng.random::<f32>() < b.chance
                        {
                            emitters.emit(BuffEvent {
                                entity: attacker.entity,
                                buff_change: BuffChange::Add(b.to_self_buff(
                                    time,
                                    (Some(attacker.uid), attacker.stats, attacker.mass),
                                    accumulated_damage,
                                    strength_modifier,
                                    self.ability_info,
                                )),
                            });
                        }
                    },
                    CombatEffect::Energy(e) => {
                        emitters.emit(EnergyChangeEvent {
                            entity: target.entity,
                            change: e * strength_modifier,
                            reset_rate: true,
                        });
                    },
                    CombatEffect::Transform {
                        entity_spec,
                        allow_players,
                    } => {
                        if target.player.is_none() || *allow_players {
                            emitters.emit(TransformEvent {
                                target_entity: target.uid,
                                entity_info: {
                                    let Ok(entity_config) = Ron::<EntityConfig>::load(entity_spec)
                                        .inspect_err(|error| {
                                            error!(
                                                ?entity_spec,
                                                ?error,
                                                "Could not load entity configuration for death \
                                                 effect"
                                            )
                                        })
                                    else {
                                        continue;
                                    };

                                    EntityInfo::at(target.pos).with_entity_config(
                                        entity_config.read().clone().into_inner(),
                                        Some(entity_spec),
                                        rng,
                                        None,
                                    )
                                },
                                allow_players: *allow_players,
                                delete_on_failure: false,
                            });
                        }
                    },
                    CombatEffect::DebuffsVulnerable {
                        mult,
                        scaling,
                        filter_attacker,
                        filter_weapon,
                    } => {
                        if let Some(buffs) = target.buffs {
                            let num_debuffs = buffs.iter_active().flatten().filter(|b| {
                                let debuff_filter = matches!(b.kind.differentiate(), BuffDescriptor::SimpleNegative);
                                let attacker_filter = !filter_attacker || matches!(b.source, BuffSource::Character { by, .. } if Some(by) == attacker.map(|a| a.uid));
                                let weapon_filter = filter_weapon.is_none_or(|w| matches!(b.source, BuffSource::Character { tool_kind, .. } if Some(w) == tool_kind));
                                debuff_filter && attacker_filter && weapon_filter
                            }).count();
                            if num_debuffs > 0 {
                                let change = HealthChange {
                                    amount: -accumulated_damage
                                        * scaling.factor(num_debuffs as f32, 1.0)
                                        * mult
                                        * strength_modifier,
                                    by: attacker.map(|a| a.into()),
                                    cause: Some(DamageSource::from(attack_source)),
                                    magic_source: self.magic_source(),
                                    time,
                                    precise: precision_mult.is_some(),
                                    instance: rand::random(),
                                };
                                accumulated_damage -= change.amount;
                                emitters.emit(HealthChangeEvent {
                                    entity: target.entity,
                                    change,
                                });
                            }
                        }
                    },
                }
            }
        }
        // Emits event to handle things that should happen for any successful attack,
        // regardless of if the attack had any damages or effects in it
        if is_applied {
            emitters.emit(EntityAttackedHookEvent {
                entity: target.entity,
                attacker: attacker.map(|a| a.entity),
                attack_dir: dir,
                damage_dealt: accumulated_damage,
                attack_source,
            });
        }
        is_applied
    }
}

pub fn allow_friendly_fire(
    entered_auras: &ReadStorage<EnteredAuras>,
    attacker: EcsEntity,
    target: EcsEntity,
) -> bool {
    entered_auras
        .get(attacker)
        .zip(entered_auras.get(target))
        .and_then(|(attacker, target)| {
            Some((
                attacker.auras.get(&AuraKindVariant::FriendlyFire)?,
                target.auras.get(&AuraKindVariant::FriendlyFire)?,
            ))
        })
        // Only allow friendly fire if both entities are affectd by the same FriendlyFire aura
        .is_some_and(|(attacker, target)| attacker.intersection(target).next().is_some())
}

/// Function that checks for unintentional PvP between players.
///
/// Returns `false` if attack will create unintentional conflict,
/// e.g. if player with PvE mode will harm pets of other players
/// or other players will do the same to such player.
///
/// If both players have PvP mode enabled, interact with NPC and
/// in any other case, this function will return `true`
// TODO: add parameter for doing self-harm?
pub fn permit_pvp(
    alignments: &ReadStorage<Alignment>,
    players: &ReadStorage<Player>,
    entered_auras: &ReadStorage<EnteredAuras>,
    id_maps: &IdMaps,
    attacker: Option<EcsEntity>,
    target: EcsEntity,
) -> bool {
    // Return owner entity if pet,
    // or just return entity back otherwise
    let owner_if_pet = |entity| {
        let alignment = alignments.get(entity).copied();
        if let Some(Alignment::Owned(uid)) = alignment {
            // return original entity
            // if can't get owner
            id_maps.uid_entity(uid).unwrap_or(entity)
        } else {
            entity
        }
    };

    // Just return ok if attacker is unknown, it's probably
    // environment or command.
    let attacker = match attacker {
        Some(attacker) => attacker,
        None => return true,
    };

    // "Dereference" to owner if this is a pet.
    let attacker_owner = owner_if_pet(attacker);
    let target_owner = owner_if_pet(target);

    // If both players are in the same ForcePvP aura, allow them to harm eachother
    if let (Some(attacker_auras), Some(target_auras)) = (
        entered_auras.get(attacker_owner),
        entered_auras.get(target_owner),
    ) && attacker_auras
        .auras
        .get(&AuraKindVariant::ForcePvP)
        .zip(target_auras.auras.get(&AuraKindVariant::ForcePvP))
        // Only allow forced pvp if both entities are affectd by the same FriendlyFire aura
        .is_some_and(|(attacker, target)| attacker.intersection(target).next().is_some())
    {
        return true;
    }

    // Prevent PvP between pets, unless friendly fire is enabled
    //
    // This code is NOT intended to prevent pet <-> owner combat,
    // pets and their owners being in the same group should take care of that
    if attacker_owner == target_owner {
        return allow_friendly_fire(entered_auras, attacker, target);
    }

    // Get player components
    let attacker_info = players.get(attacker_owner);
    let target_info = players.get(target_owner);

    // Return `true` if not players.
    attacker_info
        .zip(target_info)
        .is_none_or(|(a, t)| a.may_harm(t))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttackDamage {
    damage: Damage,
    target: Option<GroupTarget>,
    effects: Vec<CombatEffect>,
    /// A random ID, used to group up attacks
    instance: u64,
}

impl AttackDamage {
    pub fn new(damage: Damage, target: Option<GroupTarget>, instance: u64) -> Self {
        Self {
            damage,
            target,
            effects: Vec::new(),
            instance,
        }
    }

    #[must_use]
    pub fn with_effect(mut self, effect: CombatEffect) -> Self {
        self.effects.push(effect);
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AttackEffect {
    target: Option<GroupTarget>,
    effect: CombatEffect,
    requirements: Vec<CombatRequirement>,
    modifications: Vec<CombatModification>,
}

impl AttackEffect {
    pub fn new(target: Option<GroupTarget>, effect: CombatEffect) -> Self {
        Self {
            target,
            effect,
            requirements: Vec::new(),
            modifications: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_requirement(mut self, requirement: CombatRequirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    #[must_use]
    pub fn with_modification(mut self, modification: CombatModification) -> Self {
        self.modifications.push(modification);
        self
    }

    pub fn effect(&self) -> &CombatEffect { &self.effect }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StatEffect {
    pub target: StatEffectTarget,
    pub effect: CombatEffect,
    requirements: Vec<CombatRequirement>,
    modifications: Vec<CombatModification>,
}

impl StatEffect {
    pub fn new(target: StatEffectTarget, effect: CombatEffect) -> Self {
        Self {
            target,
            effect,
            requirements: Vec::new(),
            modifications: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_requirement(mut self, requirement: CombatRequirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    #[must_use]
    pub fn with_modification(mut self, modification: CombatModification) -> Self {
        self.modifications.push(modification);
        self
    }

    pub fn requirements(&self) -> impl Iterator<Item = &CombatRequirement> {
        self.requirements.iter()
    }

    pub fn modifications(&self) -> impl Iterator<Item = &CombatModification> {
        self.modifications.iter()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum CombatEffect {
    Heal(f32),
    Buff(CombatBuff),
    Knockback(Knockback),
    EnergyReward(f32),
    Lifesteal(f32),
    Poise(f32),
    Combo(i32),
    /// Intended to be used when gating additional damage behind some
    /// requirement
    AdditionalDamage(f32),
    /// Resets duration of all buffs of this buffkind, with some probability
    RefreshBuff(f32, BuffKind),
    /// Applies buff to yourself after attack is applied
    SelfBuff(CombatBuff),
    /// Changes energy of target
    Energy(f32),
    /// String is the entity_spec
    Transform {
        entity_spec: String,
        /// Whether this effect applies to players or not
        #[serde(default)]
        allow_players: bool,
    },
    /// If the target hit by an attack has debuffs, they will take increased
    /// damage scaling with the number of active debuffs they have
    DebuffsVulnerable {
        mult: f32,
        scaling: ScalingKind,
        /// Should debuffs only be counted if they were inflicted by the
        /// attacker
        filter_attacker: bool,
        /// Should debuffs only be counted if they were inflicted by a specific
        /// weapon
        filter_weapon: Option<ToolKind>,
    },
}

/// One rung of a capped-nearest-N, per-target-resolved tier ladder (see
/// `aura::AuraKind::TieredHealthEffect`): checks the target's CURRENT health
/// (never scaled by max health, unlike
/// `CombatRequirement::TargetHealthAtOrBelow`) and applies only the single
/// WORST tier the target qualifies for — a target under every threshold gets
/// the worst one, not all of them. Tiers must be supplied most-severe (lowest
/// `HealthTier::max_current_health`) first; resolution does not sort them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HealthTier {
    /// This tier applies when the target's current health is at or below
    /// this value.
    pub max_current_health: f32,
    pub effect: TierEffect,
    /// An optional additional gate, checked *after* the health threshold
    /// matches and *before* `effect` is applied. `None` (the default, and
    /// what every tier authored before this field existed still resolves to)
    /// always passes.
    ///
    /// Currently only meaningful with [`CombatRequirement::CasterLevelRoll`]
    /// (optionally composed via [`CombatRequirement::All`] of more rolls) --
    /// see [`HealthTier::requirement_met`] for why every other
    /// `CombatRequirement` variant is out of scope on a health-tier ladder.
    /// `power_word_divine_word`'s instant-death tier is the first (and so
    /// far only) tier to set this: real 5e's instant death at 0 HP has no
    /// caster-side margin for failure anywhere else in the spell, unlike
    /// `power_word_kill`/`pain`/`stun`, which all gate their permanent
    /// result behind the same roll.
    #[serde(default)]
    pub requirement: Option<CombatRequirement>,
}

impl HealthTier {
    /// Whether this tier's optional [`HealthTier::requirement`] is satisfied
    /// for a cast by `caster_level`/`character_class`. `None` always passes.
    ///
    /// Deliberately narrower than [`CombatRequirement::requirement_met`]: a
    /// health tier already gates on the target's current health via
    /// `max_current_health`, and the tick that resolves it
    /// (`common-systems/src/aura.rs`'s `apply_tiered_health_effect`) has no
    /// attack, target buffs/combo, stage-section, or direction to resolve
    /// the attack-pipeline-only variants against. Only `CasterLevelRoll`
    /// (and `All` composed purely of more `CasterLevelRoll`s, for forward
    /// compatibility, though nothing authors that yet) is meaningful here;
    /// anything else is an authoring error caught by the `debug_assert`
    /// below rather than silently bypassed or silently always-failing in a
    /// release build.
    pub fn requirement_met(
        &self,
        caster_level: Option<u16>,
        character_class: Option<&CharacterClass>,
    ) -> bool {
        fn met(
            req: &CombatRequirement,
            caster_level: Option<u16>,
            character_class: Option<&CharacterClass>,
        ) -> bool {
            match req {
                CombatRequirement::CasterLevelRoll(fail_chance) => fail_chance
                    .effective_caster_level(caster_level, character_class)
                    .is_some_and(|level| rand::random::<f32>() >= fail_chance.fail_chance(level)),
                CombatRequirement::All(reqs) => reqs
                    .iter()
                    .all(|req| met(req, caster_level, character_class)),
                _ => {
                    debug_assert!(
                        false,
                        "HealthTier::requirement only supports CasterLevelRoll (optionally \
                         wrapped in All) -- got {req:?}"
                    );
                    false
                },
            }
        }
        self.requirement
            .as_ref()
            .is_none_or(|req| met(req, caster_level, character_class))
    }
}

/// What a [`HealthTier`] grants once its threshold is the worst one met.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TierEffect {
    Buff(CombatBuff),
    /// Same semantics as [`CombatEffect::AdditionalDamage`].
    AdditionalDamage(f32),
}

impl CombatEffect {
    pub fn apply_multiplier(self, mult: f32) -> Self {
        match self {
            CombatEffect::Heal(h) => CombatEffect::Heal(h * mult),
            CombatEffect::Buff(CombatBuff {
                kind,
                dur_secs,
                strength,
                chance,
            }) => CombatEffect::Buff(CombatBuff {
                kind,
                dur_secs,
                strength: strength * mult,
                chance,
            }),
            CombatEffect::Knockback(Knockback {
                direction,
                strength,
            }) => CombatEffect::Knockback(Knockback {
                direction,
                strength: strength * mult,
            }),
            CombatEffect::EnergyReward(e) => CombatEffect::EnergyReward(e * mult),
            CombatEffect::Lifesteal(l) => CombatEffect::Lifesteal(l * mult),
            CombatEffect::Poise(p) => CombatEffect::Poise(p * mult),
            CombatEffect::Combo(c) => CombatEffect::Combo((c as f32 * mult).ceil() as i32),
            CombatEffect::AdditionalDamage(v) => CombatEffect::AdditionalDamage(v * mult),
            CombatEffect::RefreshBuff(c, b) => CombatEffect::RefreshBuff(c, b),
            CombatEffect::SelfBuff(CombatBuff {
                kind,
                dur_secs,
                strength,
                chance,
            }) => CombatEffect::SelfBuff(CombatBuff {
                kind,
                dur_secs,
                strength: strength * mult,
                chance,
            }),
            CombatEffect::Energy(e) => CombatEffect::Energy(e * mult),
            effect @ CombatEffect::Transform { .. } => effect,
            CombatEffect::DebuffsVulnerable {
                mult: a,
                scaling,
                filter_attacker,
                filter_weapon,
            } => CombatEffect::DebuffsVulnerable {
                mult: a * mult,
                scaling,
                filter_attacker,
                filter_weapon,
            },
        }
    }

    pub fn adjusted_by_stats(self, stats: tool::Stats) -> Self {
        match self {
            CombatEffect::Heal(h) => CombatEffect::Heal(h * stats.effect_power),
            CombatEffect::Buff(CombatBuff {
                kind,
                dur_secs,
                strength,
                chance,
            }) => CombatEffect::Buff(CombatBuff {
                kind,
                dur_secs,
                strength: strength * stats.buff_strength,
                chance,
            }),
            CombatEffect::Knockback(Knockback {
                direction,
                strength,
            }) => CombatEffect::Knockback(Knockback {
                direction,
                strength: strength * stats.effect_power,
            }),
            CombatEffect::EnergyReward(e) => CombatEffect::EnergyReward(e),
            CombatEffect::Lifesteal(l) => CombatEffect::Lifesteal(l * stats.effect_power),
            CombatEffect::Poise(p) => CombatEffect::Poise(p * stats.effect_power),
            CombatEffect::Combo(c) => CombatEffect::Combo(c),
            CombatEffect::AdditionalDamage(v) => {
                CombatEffect::AdditionalDamage(v * stats.effect_power)
            },
            CombatEffect::RefreshBuff(c, b) => CombatEffect::RefreshBuff(c, b),
            CombatEffect::SelfBuff(CombatBuff {
                kind,
                dur_secs,
                strength,
                chance,
            }) => CombatEffect::SelfBuff(CombatBuff {
                kind,
                dur_secs,
                strength: strength * stats.buff_strength,
                chance,
            }),
            CombatEffect::Energy(e) => CombatEffect::Energy(e * stats.effect_power),
            effect @ CombatEffect::Transform { .. } => effect,
            CombatEffect::DebuffsVulnerable {
                mult,
                scaling,
                filter_attacker,
                filter_weapon,
            } => CombatEffect::DebuffsVulnerable {
                mult: mult * stats.effect_power,
                scaling,
                filter_attacker,
                filter_weapon,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
struct AttackedModifiers {
    energy_reward: f32,
    damage_mult: f32,
}

impl Default for AttackedModifiers {
    fn default() -> Self {
        Self {
            energy_reward: 1.0,
            damage_mult: 1.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AttackedModification {
    modifier: AttackedModifier,
    requirements: Vec<CombatRequirement>,
    modifications: Vec<CombatModification>,
}

impl AttackedModification {
    pub fn new(modifier: AttackedModifier) -> Self {
        Self {
            modifier,
            requirements: Vec::new(),
            modifications: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_requirement(mut self, requirement: CombatRequirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    #[must_use]
    pub fn with_modification(mut self, modification: CombatModification) -> Self {
        self.modifications.push(modification);
        self
    }

    fn attacked_modifiers(
        target: &TargetInfo,
        attacker: Option<AttackerInfo>,
        emitters: &mut (impl EmitExt<EnergyChangeEvent> + EmitExt<ComboChangeEvent>),
        dir: Dir,
        attack_source: Option<AttackSource>,
        ability_info: Option<AbilityInfo>,
        rng: &mut rand::rngs::ThreadRng,
    ) -> AttackedModifiers {
        if let Some(stats) = target.stats {
            stats.attacked_modifications.iter().fold(
                AttackedModifiers::default(),
                |mut a_mods, a_mod| {
                    let requirements_met = a_mod.requirements.iter().all(|req| {
                        req.requirement_met(
                            (
                                target.health,
                                target.buffs,
                                target.char_state,
                                target.ori,
                                Some(target.uid),
                            ),
                            (
                                attacker.map(|a| a.entity),
                                attacker.and_then(|a| a.energy),
                                attacker.and_then(|a| a.combo),
                            ),
                            attacker.map(|a| a.uid),
                            0.0, /* When we call this function, no damage has been
                                  * calculated yet, so the AnyDamage requirement is
                                  * effectively broken, not sure if this will be issue in
                                  * future? */
                            emitters,
                            dir,
                            attack_source,
                            ability_info,
                            rng,
                            attacker.and_then(|a| a.stats).map(|s| s.character_level),
                            attacker.and_then(|a| a.character_class),
                        )
                    });

                    let mut strength_modifier = 1.0;
                    for modification in a_mod.modifications.iter() {
                        modification.apply_mod(
                            attacker.and_then(|a| a.pos),
                            Some(target.pos),
                            &mut strength_modifier,
                        );
                    }
                    let strength_modifier = strength_modifier;

                    if requirements_met {
                        match a_mod.modifier {
                            AttackedModifier::EnergyReward(er) => {
                                a_mods.energy_reward *= 1.0 + (er * strength_modifier);
                            },
                            AttackedModifier::DamageMultiplier(dm) => {
                                a_mods.damage_mult *= 1.0 + (dm * strength_modifier);
                            },
                        }
                    }

                    a_mods
                },
            )
        } else {
            AttackedModifiers::default()
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AttackedModifier {
    EnergyReward(f32),
    DamageMultiplier(f32),
}

/// A max-health-scaled absolute HP threshold: flat `base` up to `breakpoint`
/// max health, then `base + scale * (max_health - breakpoint)` above it —
/// continuous at the breakpoint by construction. Absolute HP, not a fraction
/// (see [`CombatRequirement::TargetHealthBelow`] for that), for effects where
/// the same flat pool of HP is always fatal regardless of the target's
/// max-health, only scaling up gently for very tanky targets.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HealthThreshold {
    pub base: f32,
    pub breakpoint: f32,
    pub scale: f32,
}

impl HealthThreshold {
    pub fn threshold(&self, max_health: f32) -> f32 {
        self.base + self.scale * (max_health - self.breakpoint).max(0.0)
    }
}

/// A caster-level-scaled chance to fail a cast outright: below
/// `unlock_level` this always fails (pair with an ability-side `min_level`
/// gate so the ability can't even be attempted that low); from
/// `unlock_level` up to `MAX_CHARACTER_LEVEL` the fail chance falls linearly
/// from `fail_chance_at_unlock` to `fail_chance_at_max_level`. This is a roll
/// against the caster's own level, independent of the target — there is no
/// target-side resistance to weigh against it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CasterLevelFailChance {
    pub unlock_level: u16,
    pub fail_chance_at_unlock: f32,
    pub fail_chance_at_max_level: f32,
    /// Classes this roll is keyed to. When the caster holds one or more of
    /// these, the roll uses the max of those classes' own levels rather than
    /// the raw character level -- a multiclass caster's fail chance tracks
    /// how far they've progressed in the class that actually grants the
    /// spell, not their unrelated total. Empty (the default) falls back to
    /// the raw character level, for any non-spell use of this same curve.
    #[serde(default)]
    pub source_classes: Vec<ClassKind>,
}

impl CasterLevelFailChance {
    pub fn fail_chance(&self, caster_level: u16) -> f32 {
        if caster_level <= self.unlock_level {
            return self.fail_chance_at_unlock;
        }
        let span = f32::from(MAX_CHARACTER_LEVEL.saturating_sub(self.unlock_level)).max(1.0);
        let progress = (f32::from(caster_level.saturating_sub(self.unlock_level)) / span).min(1.0);
        self.fail_chance_at_unlock
            + (self.fail_chance_at_max_level - self.fail_chance_at_unlock) * progress
    }

    /// Resolves the level to roll `fail_chance` against: the highest level
    /// among `source_classes` the caster actually holds, or the caster's raw
    /// character level when `source_classes` is empty or the caster holds
    /// none of them (the latter should not happen if the ability's own class
    /// gate already checked -- this is a defensive fallback, not a design
    /// path).
    pub fn effective_caster_level(
        &self,
        character_level: Option<u16>,
        character_class: Option<&CharacterClass>,
    ) -> Option<u16> {
        if self.source_classes.is_empty() {
            return character_level;
        }
        character_level
            .and_then(|level| {
                character_class.and_then(|cc| {
                    cc.class_levels(level)
                        .filter(|(class, _, _)| self.source_classes.contains(class))
                        .map(|(_, class_level, _)| class_level)
                        .max()
                })
            })
            .or(character_level)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum CombatRequirement {
    AnyDamage,
    Energy(f32),
    Combo(u32),
    TargetHasBuff(BuffKind),
    TargetPoised,
    BehindTarget,
    TargetBlocking,
    TargetUnwielded,
    AttackSource(AttackSource),
    AttackInput(InputKind),
    Attacker(Uid),
    Target(Uid),
    StageSection(StageSection),
    /// Met when the target's remaining health fraction (`Health::fraction`,
    /// `0.0..=1.0`) is strictly below the given threshold. A fraction rather
    /// than an absolute HP value, so a single tuning number scales correctly
    /// across creatures with very different max health pools.
    TargetHealthBelow(f32),
    /// Met when the target's absolute remaining HP is at or below a
    /// max-health-scaled threshold (see [`HealthThreshold`]) — a
    /// deterministic check, no roll on the target's side.
    TargetHealthAtOrBelow(HealthThreshold),
    /// Met by a random roll against the caster's own level (see
    /// [`CasterLevelFailChance`]) — no target-side term at all.
    ///
    /// Boxed for the same reason `AuraKind::Buff::pool_split` and
    /// `AuraKind::TieredHealthEffect::banishment` box `PoolSplit` /
    /// `BanishmentEffect`: `CasterLevelFailChance` carries a
    /// `Vec<ClassKind>`, which would otherwise make this the largest variant
    /// and bloat every `CombatRequirement`, most of which never use it.
    ///
    /// This alone does not make `CombatRequirement` `Copy` — `All` below
    /// already rules that out unconditionally (a `Vec` can never be `Copy`,
    /// regardless of what this variant does), so call sites that need an
    /// owned `CombatRequirement`/`CustomCombo` still `.clone()` one.
    CasterLevelRoll(Box<CasterLevelFailChance>),
    /// Met when every inner requirement is met — an AND combinator, for
    /// composing more than one `CombatRequirement` where an ability's RON
    /// shape only has room for a single requirement slot (e.g.
    /// `attack_effect: Option<(CombatEffect, CombatRequirement)>`). Shipped
    /// content (`power_word_kill`/`pain`/`stun`) already composes
    /// `TargetHealthAtOrBelow` + `CasterLevelRoll` through this variant, so it
    /// cannot be dropped to make room for `Copy`.
    All(Vec<CombatRequirement>),
}

impl CombatRequirement {
    pub fn requirement_met(
        &self,
        target: (
            Option<&Health>,
            Option<&Buffs>,
            Option<&CharacterState>,
            Option<&Ori>,
            Option<Uid>,
        ),
        // originator refers to the cause of the effect that requirements are being checked for.
        // For combat effects on an attack this will be the attacker, for damaged and death effects
        // this will be the target.
        originator: (Option<EcsEntity>, Option<&Energy>, Option<&Combo>),
        attacker: Option<Uid>,
        damage: f32,
        emitters: &mut (impl EmitExt<EnergyChangeEvent> + EmitExt<ComboChangeEvent>),
        dir: Dir,
        attack_source: Option<AttackSource>,
        ability_info: Option<AbilityInfo>,
        rng: &mut rand::rngs::ThreadRng,
        // The caster's own derived character level, when known — distinct
        // from `attacker` (a `Uid` used for identity checks). Only
        // `CasterLevelRoll` and `All` (recursively) consume it.
        caster_level: Option<u16>,
        // The caster's held class(es), so `CasterLevelRoll` can resolve a
        // class-specific level for entries with `source_classes` set. Only
        // `CasterLevelRoll` and `All` (recursively) consume it.
        character_class: Option<&CharacterClass>,
    ) -> bool {
        let (target_health, target_buffs, target_char_state, target_ori, target_uid) = target;
        let (originator_entity, originator_energy, originator_combo) = originator;
        match self {
            CombatRequirement::AnyDamage => damage > 0.0 && target_health.is_some(),
            CombatRequirement::Energy(r) => {
                if let (Some(entity), Some(energy)) = (originator_entity, originator_energy) {
                    let sufficient_energy = energy.current() >= *r;
                    if sufficient_energy {
                        emitters.emit(EnergyChangeEvent {
                            entity,
                            change: -*r,
                            reset_rate: false,
                        });
                    }

                    sufficient_energy
                } else {
                    false
                }
            },
            CombatRequirement::Combo(r) => {
                if let (Some(entity), Some(combo)) = (originator_entity, originator_combo) {
                    let sufficient_combo = combo.counter() >= *r;
                    if sufficient_combo {
                        emitters.emit(ComboChangeEvent {
                            entity,
                            change: -(*r as i32),
                        });
                    }

                    sufficient_combo
                } else {
                    false
                }
            },
            CombatRequirement::TargetHasBuff(buff) => {
                target_buffs.is_some_and(|buffs| buffs.contains(*buff))
            },
            CombatRequirement::TargetPoised => target_char_state.is_some_and(|cs| cs.is_stunned()),
            CombatRequirement::BehindTarget => {
                if let Some(ori) = target_ori {
                    ori.look_vec().angle_between(dir.with_z(0.0)) < BEHIND_TARGET_ANGLE
                } else {
                    false
                }
            },
            CombatRequirement::TargetBlocking => target_char_state
                .zip(attack_source)
                .is_some_and(|(cs, attack)| cs.is_block(attack) || cs.is_parry(attack)),
            CombatRequirement::TargetUnwielded => {
                target_char_state.is_some_and(|cs| !cs.is_wield())
            },
            CombatRequirement::AttackSource(source) => attack_source == Some(*source),
            CombatRequirement::AttackInput(input) => {
                ability_info.is_some_and(|ai| ai.input == *input)
            },
            CombatRequirement::Attacker(uid) => Some(*uid) == attacker,
            CombatRequirement::Target(uid) => Some(*uid) == target_uid,
            CombatRequirement::StageSection(s) => {
                Some(*s) == target_char_state.and_then(|cs| cs.stage_section())
            },
            CombatRequirement::TargetHealthBelow(threshold) => {
                target_health.is_some_and(|h| h.fraction() < *threshold)
            },
            CombatRequirement::TargetHealthAtOrBelow(health_threshold) => target_health
                .is_some_and(|h| h.current() <= health_threshold.threshold(h.maximum())),
            CombatRequirement::CasterLevelRoll(fail_chance) => fail_chance
                .effective_caster_level(caster_level, character_class)
                .is_some_and(|level| rng.random::<f32>() >= fail_chance.fail_chance(level)),
            CombatRequirement::All(reqs) => reqs.iter().all(|req| {
                req.requirement_met(
                    target,
                    originator,
                    attacker,
                    damage,
                    emitters,
                    dir,
                    attack_source,
                    ability_info,
                    rng,
                    caster_level,
                    character_class,
                )
            }),
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum CombatModification {
    /// Linearly decreases effect strength starting with 1 strength at some
    /// distance, ending at a minimum strength by some end distance
    RangeWeakening {
        start_dist: f32,
        end_dist: f32,
        min_str: f32,
    },
}

impl CombatModification {
    pub fn apply_mod(
        &self,
        attacker_pos: Option<Vec3<f32>>,
        target_pos: Option<Vec3<f32>>,
        strength_mod: &mut f32,
    ) {
        match self {
            Self::RangeWeakening {
                start_dist,
                end_dist,
                min_str,
            } => {
                if let Some((attacker_pos, target_pos)) = attacker_pos.zip(target_pos) {
                    let dist = attacker_pos.distance(target_pos);
                    // a = (y2 - y1) / (x2 - x1)
                    let gradient = (*min_str - 1.0) / (end_dist - start_dist).max(0.1);
                    // c = y2 - a*x1
                    let intercept = 1.0 - gradient * start_dist;
                    // y = clamp(a*x + c)
                    let strength = (gradient * dist + intercept).clamp(*min_str, 1.0);
                    *strength_mod *= strength;
                }
            },
        }
    }
}

/// Effects applied to the rider of this entity while riding.
#[derive(Clone, Debug, PartialEq)]
pub struct RiderEffects(pub Vec<BuffEffect>);

impl specs::Component for RiderEffects {
    type Storage = specs::DenseVecStorage<RiderEffects>;
}

#[derive(Clone, Debug, PartialEq)]
/// Permanent entity death effects (unlike `Stats::effects_on_death` which is
/// only active as long as ie. it has a certain buff)
pub struct DeathEffects(pub Vec<StatEffect>);

impl specs::Component for DeathEffects {
    type Storage = specs::DenseVecStorage<DeathEffects>;
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum DamageContributor {
    Solo(Uid),
    Group { entity_uid: Uid, group: Group },
}

impl DamageContributor {
    pub fn new(uid: Uid, group: Option<Group>) -> Self {
        if let Some(group) = group {
            DamageContributor::Group {
                entity_uid: uid,
                group,
            }
        } else {
            DamageContributor::Solo(uid)
        }
    }

    pub fn uid(&self) -> Uid {
        match self {
            DamageContributor::Solo(uid) => *uid,
            DamageContributor::Group {
                entity_uid,
                group: _,
            } => *entity_uid,
        }
    }
}

impl From<AttackerInfo<'_>> for DamageContributor {
    fn from(attacker_info: AttackerInfo) -> Self {
        DamageContributor::new(attacker_info.uid, attacker_info.group.copied())
    }
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageSource {
    Buff(BuffKind),
    Attack(AttackSource),
    Falling,
    Other,
}

impl From<AttackSource> for DamageSource {
    fn from(attack: AttackSource) -> Self { DamageSource::Attack(attack) }
}

/// Why an entity left the world, threaded through
/// [`crate::event::DestroyEvent`] so a removal that is *not* a kill can be
/// told apart from one that is.
///
/// Nothing reads this for quest/achievement purposes yet —
/// `QuestKind::Slay` (`rtsim/src/rule/npc_ai/quest.rs:487`) is still a no-op.
/// It exists now, deliberately, so the eventual kill-tracking system has a
/// signal to key off from day one instead of retrofitting death-cause
/// tracking later (spec §7). Extend the enum rather than adding a parallel
/// flag.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemovalCause {
    /// Killed outright. The only cause that counts as a kill.
    Killed,
    /// Banished: temporarily removed, due to return later, and therefore
    /// never a kill — see `aura::BanishmentEffect`.
    Banished,
}

impl RemovalCause {
    /// Whether this removal should be credited as a kill by any future
    /// quest / achievement / statistics consumer.
    pub fn counts_as_kill(self) -> bool { matches!(self, RemovalCause::Killed) }
}

/// A [`RemovalCause`] plus how much of the normal XP and loot the removal
/// awards. The fraction is carried alongside the cause rather than derived
/// from it so the number stays authored in RON
/// (`aura::BanishmentEffect::reward_fraction`) instead of hardcoded here.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RemovalInfo {
    pub cause: RemovalCause,
    /// Multiplier in `0.0..=1.0` applied to the XP award and to each loot
    /// entry's chance to drop. `1.0` for an ordinary kill.
    pub reward_fraction: f32,
}

impl Default for RemovalInfo {
    fn default() -> Self {
        Self {
            cause: RemovalCause::Killed,
            reward_fraction: 1.0,
        }
    }
}

impl RemovalInfo {
    /// An ordinary kill: full rewards.
    pub fn killed() -> Self { Self::default() }

    /// A banishment awarding `reward_fraction` of the normal rewards.
    pub fn banished(reward_fraction: f32) -> Self {
        Self {
            cause: RemovalCause::Banished,
            reward_fraction: reward_fraction.clamp(0.0, 1.0),
        }
    }
}

/// DamageKind for the purpose of differentiating damage reduction.
///
/// The three physical kinds (Piercing/Slashing/Crushing) carry distinct
/// mitigation behaviour (see the apply-damage match in this file). The
/// magical/elemental kinds form the broader content damage taxonomy; for now
/// they share the generic (no special physical interaction) mitigation of the
/// legacy `Energy` placeholder. **Radiant is the opposite of Necrotic** — the
/// resist/affinity interplay (e.g. undead take bonus Radiant; Necrotic harms
/// the living / spares undead) is a future balance feature and is NOT wired
/// here yet.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageKind {
    // --- physical ---
    /// Bypasses some protection from armor
    Piercing,
    /// Reduces energy of target, dealing additional damage when target energy
    /// is 0
    Slashing,
    /// Blunt/physical. Deals additional poise damage the more armored the
    /// target is. Content name: **Bludgeoning** (serde alias) — kept as
    /// `Crushing` in the engine for its poise behaviour.
    #[serde(alias = "Bludgeoning")]
    Crushing,
    // --- legacy generic ---
    /// Legacy catch-all magical damage. Retained for back-compat with existing
    /// RON; new content should use a specific kind below. Mitigated
    /// generically.
    Energy,
    // --- magical / elemental (content taxonomy, ENG-A2) ---
    Acid,
    Cold,
    Fire,
    Force,
    Lightning,
    /// Death/decay. Opposite of `Radiant` (affinity interplay deferred).
    Necrotic,
    Poison,
    Psychic,
    /// Holy/radiant light. Opposite of `Necrotic` (affinity interplay
    /// deferred).
    Radiant,
    Thunder,
}

const PIERCING_PENETRATION_FRACTION: f32 = 0.75;
const SLASHING_ENERGY_FRACTION: f32 = 0.5;
const CRUSHING_POISE_FRACTION: f32 = 1.0;

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Damage {
    pub kind: DamageKind,
    pub value: f32,
}

impl Damage {
    /// Returns the total damage reduction provided by all equipped items.
    ///
    /// Reads the target's cached armour protection rather than walking its
    /// loadout: `derived: None` means the entity has no `Inventory`, which is
    /// `Some(0.0)` — never invincible — exactly as
    /// [`DerivedStats::default()`] records.
    pub fn compute_damage_reduction(
        damage: Option<Self>,
        derived: Option<&DerivedStats>,
        stats: Option<&Stats>,
    ) -> f32 {
        // An antimagic field makes attuned magic-item effects mundane, so the
        // attunement-blind protection is selected while the target has
        // `disable_magic` — a pick between the two cached variants rather than
        // a second loadout walk. This covers every damage path (all callers
        // route here). The 3rd attunement-gated path (item-granted abilities)
        // is already covered — those are magic, so the cast gate blocks them.
        let protection = derived.map_or(Some(0.0), |derived| {
            if stats.is_some_and(|s| s.disable_magic) {
                derived.protection_unattuned
            } else {
                derived.protection
            }
        });

        let penetration = if let Some(damage) = damage {
            if let DamageKind::Piercing = damage.kind {
                (damage.value * PIERCING_PENETRATION_FRACTION)
                    .clamp(0.0, protection.unwrap_or(0.0).max(0.0))
            } else {
                0.0
            }
        } else {
            0.0
        };

        let protection = protection.map(|p| p - penetration);

        const FIFTY_PERCENT_DR_THRESHOLD: f32 = 60.0;

        let inventory_dr = match protection {
            Some(dr) => dr / (FIFTY_PERCENT_DR_THRESHOLD + dr.abs()),
            None => 1.0,
        };

        let stats_dr = if let Some(stats) = stats {
            stats.damage_reduction.modifier()
        } else {
            0.0
        };
        // Return 100% if either DR is at 100% (admin tabard or safezone buff)
        if protection.is_none() || stats_dr >= 1.0 {
            1.0
        } else {
            1.0 - (1.0 - inventory_dr) * (1.0 - stats_dr)
        }
    }

    pub fn calculate_health_change(
        self,
        damage_reduction: f32,
        block_damage_decrement: f32,
        damage_contributor: Option<DamageContributor>,
        precision_mult: Option<f32>,
        precision_power: f32,
        // BL-52 P6: a crit's base damage multiplier floor (WoW model, Matías
        // 2026-06-26). A full crit (precision_mult 1.0) deals at least
        // `crit_damage_mult`× and scales further with gear precision_power, so
        // crit matters from level 1 instead of being ~+0% at base gear. Positional
        // precision keeps its 0.25/0.75/1.0 gradation (the floor is scaled by it).
        // Irrelevant when `precision_mult` is None (no crit) — callers pass 1.0.
        crit_damage_mult: f32,
        damage_modifier: f32,
        time: Time,
        instance: u64,
        damage_source: DamageSource,
    ) -> HealthChange {
        let mut damage = self.value * damage_modifier;
        // `.max(0.0)`: a crit is always bonus damage — never let an unusually low
        // `precision_power` (e.g. a future <0.5 precision debuff) make it subtract.
        let precise_damage = (damage
            * precision_mult.unwrap_or(0.0)
            * ((crit_damage_mult - 1.0) + (precision_power - 1.0)))
            .max(0.0);
        // `Self` (`Damage`) has no access to the `Attack`'s `ability_info`, so
        // `magic_source` is left `None` here; the sole caller (`apply_attack`)
        // overwrites it with the attributed source right after this returns.
        match damage_source {
            DamageSource::Attack(_) => {
                // Precise hit
                damage += precise_damage;
                // Block
                damage = f32::max(damage - block_damage_decrement, 0.0);
                // Armor
                damage *= 1.0 - damage_reduction;

                HealthChange {
                    amount: -damage,
                    by: damage_contributor,
                    cause: Some(damage_source),
                    magic_source: None,
                    time,
                    precise: precision_mult.is_some(),
                    instance,
                }
            },
            DamageSource::Falling => {
                // Armor
                if (damage_reduction - 1.0).abs() < f32::EPSILON {
                    damage = 0.0;
                }
                HealthChange {
                    amount: -damage,
                    by: None,
                    cause: Some(damage_source),
                    magic_source: None,
                    time,
                    precise: false,
                    instance,
                }
            },
            DamageSource::Buff(_) | DamageSource::Other => HealthChange {
                amount: -damage,
                by: None,
                cause: Some(damage_source),
                magic_source: None,
                time,
                precise: false,
                instance,
            },
        }
    }

    pub fn interpolate_damage(&mut self, frac: f32, min: f32) {
        let new_damage = min + frac * (self.value - min);
        self.value = new_damage;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Knockback {
    pub direction: KnockbackDir,
    pub strength: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnockbackDir {
    Away,
    Towards,
    Up,
    TowardsUp,
}

impl Knockback {
    pub fn calculate_impulse(
        self,
        dir: Dir,
        tgt_char_state: Option<&CharacterState>,
        attacker_stats: Option<&Stats>,
    ) -> Vec3<f32> {
        let from_char = {
            let resistant = tgt_char_state
                .and_then(|cs| cs.ability_info())
                .map(|a| a.ability_meta)
                .is_some_and(|a| a.capabilities.contains(Capability::KNOCKBACK_RESISTANT));
            if resistant { 0.5 } else { 1.0 }
        };
        // TEMP: 50.0 multiplication kept until source knockback values have been
        // updated
        50.0 * self.strength
            * from_char
            * attacker_stats.map_or(1.0, |s| s.knockback_mult)
            * match self.direction {
                KnockbackDir::Away => *Dir::slerp(dir, Dir::new(Vec3::unit_z()), 0.5),
                KnockbackDir::Towards => *Dir::slerp(-dir, Dir::new(Vec3::unit_z()), 0.5),
                KnockbackDir::Up => Vec3::unit_z(),
                KnockbackDir::TowardsUp => *Dir::slerp(-dir, Dir::new(Vec3::unit_z()), 0.85),
            }
    }

    #[must_use]
    pub fn modify_strength(mut self, power: f32) -> Self {
        self.strength *= power;
        self
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct CombatBuff {
    pub kind: BuffKind,
    pub dur_secs: Secs,
    pub strength: CombatBuffStrength,
    pub chance: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum CombatBuffStrength {
    DamageFraction(f32),
    Value(f32),
}

impl CombatBuffStrength {
    fn to_strength(self, damage: f32, strength_modifier: f32) -> f32 {
        match self {
            // Not affected by strength modifier as damage already is
            CombatBuffStrength::DamageFraction(f) => damage * f,
            CombatBuffStrength::Value(v) => v * strength_modifier,
        }
    }
}

impl MulAssign<f32> for CombatBuffStrength {
    fn mul_assign(&mut self, mul: f32) { *self = *self * mul; }
}

impl Mul<f32> for CombatBuffStrength {
    type Output = Self;

    fn mul(self, mult: f32) -> Self {
        match self {
            Self::DamageFraction(val) => Self::DamageFraction(val * mult),
            Self::Value(val) => Self::Value(val * mult),
        }
    }
}

impl CombatBuff {
    pub fn to_buff(
        self,
        time: Time,
        attacker_info: (Option<Uid>, Option<&Mass>),
        target_info: (Option<&Stats>, Option<&Mass>),
        damage: f32,
        strength_modifier: f32,
        ability_info: Option<AbilityInfo>,
    ) -> Buff {
        let (attacker_uid, attacker_mass) = attacker_info;
        let (target_stats, target_mass) = target_info;
        // TODO: Generate BufCategoryId vec (probably requires damage overhaul?)
        let source = if let Some(uid) = attacker_uid {
            BuffSource::Character {
                by: uid,
                tool_kind: ability_info.and_then(|ai| ai.tool),
            }
        } else {
            BuffSource::Unknown
        };
        let dest_info = DestInfo {
            stats: target_stats,
            mass: target_mass,
        };
        let target_uid = ability_info
            .and_then(|ai| ai.input_attr)
            .and_then(|ia| ia.target_entity);
        Buff::new(
            self.kind,
            BuffData::new(
                self.strength.to_strength(damage, strength_modifier),
                Some(self.dur_secs),
            ),
            Vec::new(),
            source,
            time,
            dest_info,
            attacker_mass,
            target_uid,
            ability_info.and_then(|ai| ai.ability_meta.source),
        )
    }

    pub fn to_self_buff(
        self,
        time: Time,
        entity_info: (Option<Uid>, Option<&Stats>, Option<&Mass>),
        damage: f32,
        strength_modifier: f32,
        ability_info: Option<AbilityInfo>,
    ) -> Buff {
        let (entity_uid, entity_stats, entity_mass) = entity_info;
        // TODO: Generate BufCategoryId vec (probably requires damage overhaul?)
        let source = if let Some(uid) = entity_uid {
            BuffSource::Character {
                by: uid,
                tool_kind: ability_info.and_then(|ai| ai.tool),
            }
        } else {
            BuffSource::Unknown
        };
        let dest_info = DestInfo {
            stats: entity_stats,
            mass: entity_mass,
        };
        let target_uid = ability_info
            .and_then(|ai| ai.input_attr)
            .and_then(|ia| ia.target_entity);
        Buff::new(
            self.kind,
            BuffData::new(
                self.strength.to_strength(damage, strength_modifier),
                Some(self.dur_secs),
            ),
            Vec::new(),
            source,
            time,
            dest_info,
            entity_mass,
            target_uid,
            ability_info.and_then(|ai| ai.ability_meta.source),
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingKind {
    Linear,
    Sqrt,
}

impl ScalingKind {
    pub fn factor(&self, val: f32, norm: f32) -> f32 {
        match self {
            Self::Linear => val / norm,
            Self::Sqrt => (val / norm).sqrt(),
        }
    }
}

pub fn get_weapon_kinds(inv: &Inventory) -> (Option<ToolKind>, Option<ToolKind>) {
    (
        inv.equipped(EquipSlot::ActiveMainhand).and_then(|i| {
            if let ItemKind::Tool(tool) = &*i.kind() {
                Some(tool.kind)
            } else {
                None
            }
        }),
        inv.equipped(EquipSlot::ActiveOffhand).and_then(|i| {
            if let ItemKind::Tool(tool) = &*i.kind() {
                Some(tool.kind)
            } else {
                None
            }
        }),
    )
}

/// The `SkillGroupKind` whose earned points should count toward a given
/// equipped tool. Usually just `Weapon(tool.kind)`, but a martial-role Staff
/// has its own tree (`WeaponRoled`) kept deliberately separate from the
/// caster `Weapon(Staff)` tree it shares a `ToolKind` with.
///
/// Every consumer of "which skill group does this equipped tool belong to"
/// must go through this function, not a bare
/// `SkillGroupKind::Weapon(tool.kind)` -- combat-rating display and combat-XP
/// grant both need the same answer, and having only one of them special-case
/// the martial/caster Staff split silently strands XP or points in the wrong
/// pool.
pub fn skill_group_for_weapon(tool: &Tool) -> SkillGroupKind {
    if tool.kind == ToolKind::Staff && tool.role() == WeaponRole::Martial {
        SkillGroupKind::WeaponRoled(ToolKind::Staff, WeaponRole::Martial)
    } else {
        SkillGroupKind::Weapon(tool.kind)
    }
}

#[cfg(test)]
mod skill_group_for_weapon_tests {
    use super::*;

    fn test_stats() -> tool::Stats {
        tool::Stats {
            equip_time_secs: 0.0,
            power: 1.0,
            effect_power: 1.0,
            speed: 1.0,
            range: 1.0,
            energy_efficiency: 1.0,
            buff_strength: 1.0,
            cooldown_reduction: 1.0,
        }
    }

    /// A martial-role Staff must earn combat-rating credit from its own
    /// tree, not the caster `Weapon(Staff)` tree it deliberately does not
    /// share — otherwise a Monk who has spent points in the martial tree
    /// would silently show 0 weapon-skill contribution to their combat
    /// rating.
    #[test]
    fn martial_staff_resolves_its_own_group() {
        let tool = Tool::new(
            ToolKind::Staff,
            Hands::Two,
            Some(WeaponRole::Martial),
            test_stats(),
        );
        assert_eq!(
            skill_group_for_weapon(&tool),
            SkillGroupKind::WeaponRoled(ToolKind::Staff, WeaponRole::Martial)
        );
    }

    /// A caster-role Staff (the default for a bare `role: None`) still
    /// resolves the original `Weapon(Staff)` tree, unaffected by the new
    /// martial tree's existence.
    #[test]
    fn caster_staff_resolves_the_original_weapon_group() {
        let tool = Tool::new(ToolKind::Staff, Hands::Two, None, test_stats());
        assert_eq!(
            skill_group_for_weapon(&tool),
            SkillGroupKind::Weapon(ToolKind::Staff)
        );
    }

    /// Roles are only meaningful for `Staff` today; every other `ToolKind`
    /// resolves `Weapon(kind)` regardless of role, same as before this
    /// function existed.
    #[test]
    fn non_staff_tools_are_unaffected_by_role() {
        let tool = Tool::new(ToolKind::Sword, Hands::One, None, test_stats());
        assert_eq!(
            skill_group_for_weapon(&tool),
            SkillGroupKind::Weapon(ToolKind::Sword)
        );
    }
}

/// The gear → `comp::Stats` fold for the caster role. Every equipped `Tool`
/// item whose [`tool::WeaponRole`] resolves to `Caster` — this covers not
/// just `Staff`/`Sceptre` but every dedicated caster implement (`Tome`,
/// `HolySymbol`, `Focus`), since `ToolKind::default_role` maps all five to
/// `Caster` — folds its `effect_power`/`buff_strength`/`energy_efficiency`
/// tool stats into the matching character-level channel, gated by the same
/// attunement rule that already governs armor's item-effect contributions
/// ([`item_effects_active`]).
///
/// Called from `buff::Sys`'s per-tick `Stats` rebuild (it already holds the
/// `Inventory`/`AttunedItems` storages, so no `SystemData` widening is
/// needed) rather than from any per-ability path — this is the ONLY
/// mechanism that reaches **weaponless pool spells**, since
/// `states::utils::get_tool_stats` falls back to `tool::Stats::one()` (i.e.
/// contributes nothing) whenever no tool occupies the ability's hand.
///
/// `energy_efficiency_modifier` is deliberately left untouched here: it
/// already composes with the SAME equipped weapon's own
/// `tool::Stats.energy_efficiency` at ability-construction time
/// (`contextual_stats.energy_efficiency *= stats.energy_efficiency_modifier`
/// in `states/utils.rs`), so routing gear's `energy_efficiency` there too
/// would double-apply it whenever that weapon casts its own abilities.
/// `energy_regen_modifier` has no existing tool-stat consumer, so gear's mana
/// axis lands there instead — new, and non-overlapping with the per-ability
/// path.
///
/// Two further channels fold in the same way: `cooldown_reduction_modifier`
/// always takes the item's `cooldown_reduction` tool stat (identity 1.0 for
/// every item that doesn't declare one). `spell_power` is keyed by
/// [`tool::Tool::spell_power_source`]: an item that names no source (every
/// shipped item today) still contributes to the flat, unkeyed `spell_power`
/// channel exactly as before; an item that DOES name one routes its
/// `effect_power` into that source's `spell_power_by_source` slot ONLY,
/// instead of boosting every source.
pub fn apply_gear_caster_stats(
    stats: &mut Stats,
    inventory: Option<&Inventory>,
    attuned: Option<&AttunedItems>,
) {
    let Some(inventory) = inventory else {
        return;
    };
    for (slot, item) in inventory.equipped_items_with_slot() {
        if !item_effects_active(slot, item.requires_attunement(), attuned) {
            continue;
        }
        let ItemKind::Tool(tool) = &*item.kind() else {
            continue;
        };
        if tool.role() != tool::WeaponRole::Caster {
            continue;
        }
        let tool_stats = tool.stats(item.stats_durability_multiplier());
        match tool.spell_power_source() {
            Some(source) => stats.spell_power_by_source[source.index()] *= tool_stats.effect_power,
            None => stats.spell_power *= tool_stats.effect_power,
        }
        stats.heal_power *= tool_stats.buff_strength;
        stats.energy_regen_modifier *= tool_stats.energy_efficiency;
        stats.cooldown_reduction_modifier *= tool_stats.cooldown_reduction;
    }
}

/// Applies an already-folded [`CasterGearFold`] onto `stats`.
///
/// Same channels, same multiplicative composition and same identities as
/// [`apply_gear_caster_stats`], except that the per-item products have already
/// been accumulated once (at cache-rebuild time) instead of being recomputed by
/// walking the loadout on every tick. An entity with no cached fold is
/// indistinguishable from one with the default fold: every channel's identity
/// is `1.0`, so applying it is a no-op.
pub fn apply_caster_gear_fold(stats: &mut Stats, fold: &CasterGearFold) {
    stats.spell_power *= fold.spell_power;
    for (channel, factor) in stats
        .spell_power_by_source
        .iter_mut()
        .zip(fold.spell_power_by_source.iter())
    {
        *channel *= factor;
    }
    stats.heal_power *= fold.heal_power;
    stats.energy_regen_modifier *= fold.energy_regen_modifier;
    stats.cooldown_reduction_modifier *= fold.cooldown_reduction_modifier;
}

/// Returns a value to be included as a multiplicative factor in perception
/// distance checks.
///
/// Folds the **target's** buff-sourced `Stats.stealth` (set by
/// `BuffEffect::Stealth`, e.g. a concealment spell) into the exact same
/// `1/(1+sum)` curve [`stealth_multiplier`] already applies to item-based
/// stealth (`derived.stealth`) — one curve, not two multipliers stacked on
/// top of each other, so gear and spells read as one coherent concealment
/// value.
///
/// `pierce_concealment` is read off the **observer** — the entity doing the
/// looking, never the target — because it is granted to whoever should be
/// able to see through any amount of concealment regardless of how well
/// hidden the target is. `target_stats` and `observer_stats` are both
/// `Option<&Stats>`, the same type for two different entities; getting them
/// backwards silently breaks concealment both ways, so callers must resolve
/// each from the correct entity (the target's `Stats` vs. the observing
/// entity's own `Stats`), never the same lookup twice.
pub fn perception_dist_multiplier_from_stealth(
    derived: Option<&DerivedStats>,
    character_state: Option<&CharacterState>,
    target_stats: Option<&Stats>,
    observer_stats: Option<&Stats>,
) -> f32 {
    if observer_stats.is_some_and(|stats| stats.pierce_concealment) {
        return 1.0;
    }

    const SNEAK_MULTIPLIER: f32 = 0.7;

    let item_stealth = derived.map_or(0.0, |d| d.stealth);
    let buff_stealth = target_stats.map_or(0.0, |s| s.stealth);
    let combined_stealth_multiplier = stealth_multiplier(item_stealth + buff_stealth);
    let is_sneaking = character_state.is_some_and(|state| state.is_stealthy());

    let multiplier = combined_stealth_multiplier * if is_sneaking { SNEAK_MULTIPLIER } else { 1.0 };

    multiplier.clamp(0.0, 1.0)
}

/// Turns an entity's summed armour stealth stat into the multiplicative factor
/// applied to perception distances. `0.0` (the no-gear sum) is the identity.
pub fn stealth_multiplier(stealth_sum: f32) -> f32 { (1.0 / (1.0 + stealth_sum)).clamp(0.0, 1.0) }

#[cfg(test)]
mod concealment_wire_tests {
    use super::{DerivedStats, Stats, perception_dist_multiplier_from_stealth, stealth_multiplier};
    use crate::comp::{Body, humanoid};

    fn test_body() -> Body { Body::Humanoid(humanoid::Body::random()) }

    fn stats_with(stealth: f32, pierce_concealment: bool) -> Stats {
        let mut stats = Stats::empty(test_body());
        stats.stealth = stealth;
        stats.pierce_concealment = pierce_concealment;
        stats
    }

    fn derived_with_item_stealth(stealth: f32) -> DerivedStats {
        DerivedStats {
            stealth,
            ..Default::default()
        }
    }

    #[test]
    fn buff_only_stealth_reduces_the_multiplier() {
        let target_stats = stats_with(0.5, false);

        let mult = perception_dist_multiplier_from_stealth(None, None, Some(&target_stats), None);

        assert_eq!(mult, stealth_multiplier(0.5));
        assert!(mult < 1.0);
    }

    #[test]
    fn item_and_buff_stealth_stack_on_the_same_curve() {
        let derived = derived_with_item_stealth(0.5);
        let target_stats = stats_with(0.3, false);

        let mult = perception_dist_multiplier_from_stealth(
            Some(&derived),
            None,
            Some(&target_stats),
            None,
        );

        // One curve over the summed total, not two multipliers stacked: if
        // this regresses to two separately-applied multipliers, the result
        // changes (and would no longer equal `stealth_multiplier(0.8)`).
        assert_eq!(mult, stealth_multiplier(0.8));
        assert_ne!(mult, stealth_multiplier(0.5) * stealth_multiplier(0.3));
    }

    #[test]
    fn observer_pierce_concealment_restores_full_multiplier_regardless_of_target_stealth() {
        let derived = derived_with_item_stealth(5.0);
        let target_stats = stats_with(5.0, false);
        let observer_stats = stats_with(0.0, true);

        let mult = perception_dist_multiplier_from_stealth(
            Some(&derived),
            None,
            Some(&target_stats),
            Some(&observer_stats),
        );

        assert_eq!(mult, 1.0);
    }

    #[test]
    fn zero_buff_stealth_is_bit_identical_to_item_only_stealth() {
        // Regression guard: every shipped armor piece only ever sets item
        // stealth (`DerivedStats::stealth`), never `Stats.stealth`. This
        // pins the wire's output, at zero buff stealth, to exactly the
        // pre-wire item-only formula, `1.0 / (1.0 + item_stealth)`, with
        // bit-for-bit `f32` equality rather than an epsilon.
        for item_stealth in [0.0_f32, 0.04, 0.15, 0.5, 1.2] {
            let derived = derived_with_item_stealth(item_stealth);
            let legacy = (1.0_f32 / (1.0 + item_stealth)).clamp(0.0, 1.0);

            let with_no_stats =
                perception_dist_multiplier_from_stealth(Some(&derived), None, None, None);
            let with_zero_buff_stats = perception_dist_multiplier_from_stealth(
                Some(&derived),
                None,
                Some(&stats_with(0.0, false)),
                None,
            );

            assert_eq!(with_no_stats, legacy);
            assert_eq!(with_zero_buff_stats, legacy);
        }
    }
}

/// Combat resolution (BL-52 P5): the physical evasion contributed by worn gear.
/// Weight is **derived from total armor protection** (Matías 2026-06-25):
/// heavier (more protective) armor lowers evasion, an unarmored entity gets the
/// `gear_evasion_cap`. A shield adds a flat penalty (it pays off via block, not
/// dodge). Result is clamped to `[gear_evasion_floor, gear_evasion_cap]`.
/// Read from the target's cached gear aggregates (protection and weapon
/// kinds), and applied only to the **physical** to-hit roll — magic uses
/// `magic_evasion`. Callers with no cache (an entity with no `Inventory`) skip
/// this entirely and contribute `0.0`.
pub fn compute_armor_evasion(
    derived: &DerivedStats,
    stats: Option<&Stats>,
    tuning: &CombatTuning,
) -> f32 {
    // Under an antimagic field attuned protection is mundane, so it counts
    // toward neither DR (`compute_damage_reduction`) nor weight/evasion — keeps
    // an attuned item's two defense layers coherent. Both layers now read the
    // same cached pair, so the target's gear is walked once per gear change
    // instead of once per damage instance.
    let protection = if stats.is_some_and(|s| s.disable_magic) {
        derived.protection_unattuned
    } else {
        derived.protection
    }
    // Invincible armor (admin) reads as infinitely heavy → floored evasion.
    .unwrap_or(f32::INFINITY);
    let from_protection =
        tuning.gear_evasion_cap - protection * tuning.armor_evasion_per_protection;
    let (mainhand, offhand) = derived.weapon_kinds;
    let shield = if mainhand == Some(ToolKind::Shield) || offhand == Some(ToolKind::Shield) {
        tuning.shield_evasion_penalty
    } else {
        0.0
    };
    (from_protection - shield).clamp(tuning.gear_evasion_floor, tuning.gear_evasion_cap)
}

/// Used to compute the precision multiplier achieved by flanking a target
pub fn precision_mult_from_flank(
    attack_dir: Vec3<f32>,
    target_ori: Option<&Ori>,
    precision_flank_multipliers: FlankMults,
    precision_flank_invert: bool,
) -> Option<f32> {
    let angle = target_ori.map(|t_ori| {
        t_ori.look_dir().angle_between(if precision_flank_invert {
            -attack_dir
        } else {
            attack_dir
        })
    });
    match angle {
        Some(angle) if angle < FULL_FLANK_ANGLE => Some(
            MAX_BACK_FLANK_PRECISION
                * if precision_flank_invert {
                    precision_flank_multipliers.front
                } else {
                    precision_flank_multipliers.back
                },
        ),
        Some(angle) if angle < PARTIAL_FLANK_ANGLE => {
            Some(MAX_SIDE_FLANK_PRECISION * precision_flank_multipliers.side)
        },
        Some(_) | None => None,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlankMults {
    pub back: f32,
    pub front: f32,
    pub side: f32,
}

impl Default for FlankMults {
    fn default() -> Self {
        FlankMults {
            back: 1.0,
            front: 1.0,
            side: 1.0,
        }
    }
}

pub fn block_strength(inventory: &Inventory, char_state: &CharacterState) -> f32 {
    let (ability_block_strength, hand) = match char_state {
        CharacterState::BasicBlock(data) => (
            data.static_data.block_strength,
            data.static_data.ability_info.hand,
        ),
        CharacterState::RiposteMelee(data) => (
            data.static_data.block_strength,
            data.static_data.ability_info.hand,
        ),
        _ => char_state
            .ability_info()
            .map(|ability| (ability.ability_meta.capabilities, ability.hand))
            .map_or((0.0, None), |(capabilities, hand)| {
                (
                    if capabilities.contains(Capability::PARRIES)
                        || capabilities.contains(Capability::PARRIES_MELEE)
                        || capabilities.contains(Capability::BLOCKS)
                    {
                        FALLBACK_BLOCK_STRENGTH
                    } else {
                        0.0
                    },
                    hand,
                )
            }),
    };

    let tool_block_strength = hand
        .and_then(|hand| inventory.equipped(hand.to_equip_slot()))
        .map_or(1.0, |item| match &*item.kind() {
            ItemKind::Tool(tool) => tool.stats(item.stats_durability_multiplier()).power,
            _ => 1.0,
        });

    ability_block_strength * tool_block_strength
}

pub fn get_equip_slot_by_block_priority(inventory: Option<&Inventory>) -> EquipSlot {
    inventory
        .map(get_weapon_kinds)
        .map_or(
            EquipSlot::ActiveMainhand,
            |weapon_kinds| match weapon_kinds {
                (Some(mainhand), Some(offhand)) => {
                    if mainhand.block_priority() >= offhand.block_priority() {
                        EquipSlot::ActiveMainhand
                    } else {
                        EquipSlot::ActiveOffhand
                    }
                },
                (Some(_), None) => EquipSlot::ActiveMainhand,
                (None, Some(_)) => EquipSlot::ActiveOffhand,
                (None, None) => EquipSlot::ActiveMainhand,
            },
        )
}

#[cfg(test)]
mod power_word_kill_threshold_tests {
    use super::{CasterLevelFailChance, ClassKind, HealthThreshold};
    use crate::comp::CharacterClass;

    // Base 100 HP up to 215 max health, then 100 + (1/3)(max_health - 215)
    // above it — continuous at the breakpoint (no discontinuous jump that
    // would favor tankier targets over less tanky ones).
    const POWER_WORD_KILL: HealthThreshold = HealthThreshold {
        base: 100.0,
        breakpoint: 215.0,
        scale: 1.0 / 3.0,
    };

    #[test]
    fn flat_below_and_at_breakpoint() {
        assert_eq!(POWER_WORD_KILL.threshold(50.0), 100.0);
        assert_eq!(POWER_WORD_KILL.threshold(215.0), 100.0);
    }

    #[test]
    fn continuous_and_scaled_above_breakpoint() {
        assert!((POWER_WORD_KILL.threshold(1000.0) - 361.666_67).abs() < 0.01);
        assert_eq!(POWER_WORD_KILL.threshold(2000.0), 695.0);
    }

    #[test]
    fn caster_level_roll_clamps_below_unlock_and_interpolates_to_max_level() {
        let curve = CasterLevelFailChance {
            unlock_level: 54,
            fail_chance_at_unlock: 0.25,
            fail_chance_at_max_level: 0.05,
            source_classes: vec![],
        };
        assert_eq!(curve.fail_chance(1), 0.25);
        assert_eq!(curve.fail_chance(54), 0.25);
        assert!((curve.fail_chance(60) - 0.05).abs() < 0.001);
        // Halfway from L54 to L60 (L57) should be halfway from 25% to 5%.
        assert!((curve.fail_chance(57) - 0.15).abs() < 0.001);
    }

    fn power_word_kill_curve() -> CasterLevelFailChance {
        CasterLevelFailChance {
            unlock_level: 54,
            fail_chance_at_unlock: 0.25,
            fail_chance_at_max_level: 0.05,
            source_classes: vec![
                ClassKind::Mage,
                ClassKind::Sorcerer,
                ClassKind::Warlock,
                ClassKind::Bard,
            ],
        }
    }

    #[test]
    fn effective_caster_level_falls_back_to_character_level_with_no_source_classes() {
        let curve = CasterLevelFailChance {
            unlock_level: 50,
            fail_chance_at_unlock: 0.25,
            fail_chance_at_max_level: 0.05,
            source_classes: vec![],
        };
        assert_eq!(
            curve.effective_caster_level(Some(60), None),
            Some(60),
            "no source_classes configured -> raw character level, unaffected by multiclass"
        );
    }

    #[test]
    fn effective_caster_level_uses_the_eligible_class_own_level_for_a_single_class_caster() {
        let curve = power_word_kill_curve();
        let character_class = CharacterClass::single(ClassKind::Warlock);
        // Single-class Warlock at character level 60 -> Warlock level 60 too.
        assert_eq!(
            curve.effective_caster_level(Some(60), Some(&character_class)),
            Some(60)
        );
    }

    #[test]
    fn effective_caster_level_uses_the_eligible_secondary_not_an_ineligible_primary() {
        let curve = power_word_kill_curve();
        // Warrior (not eligible for Power Word Kill) primary, Warlock (eligible)
        // secondary at 20 of the 60 total -- the roll must use 20, not 60 or 40.
        let character_class = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 20,
            future_levels_to_secondary: false,
        };
        assert_eq!(
            curve.effective_caster_level(Some(60), Some(&character_class)),
            Some(20)
        );
    }

    #[test]
    fn effective_caster_level_takes_the_max_when_both_held_classes_are_eligible() {
        let curve = power_word_kill_curve();
        // Mage/Sorcerer split 40/20 -- both eligible, so the max (40) applies,
        // same composition rule as `energy_reward_mult`.
        let character_class = CharacterClass {
            primary: ClassKind::Mage,
            secondary: Some(ClassKind::Sorcerer),
            secondary_level: 20,
            future_levels_to_secondary: false,
        };
        assert_eq!(
            curve.effective_caster_level(Some(60), Some(&character_class)),
            Some(40)
        );
    }

    #[test]
    fn effective_caster_level_falls_back_to_character_level_if_no_held_class_is_eligible() {
        let curve = power_word_kill_curve();
        // Defensive fallback only -- should not occur if the ability's own
        // class gate already checked, but must not panic or return None.
        let character_class = CharacterClass::single(ClassKind::Warrior);
        assert_eq!(
            curve.effective_caster_level(Some(60), Some(&character_class)),
            Some(60)
        );
    }
}

/// Pins the authored RON content for the three `BasicRanged` Power Words
/// (Kill/Pain/Stun) -- unlike `power_word_kill_threshold_tests` above (which
/// exercises `CasterLevelFailChance`'s math in isolation with hand-built
/// mock values), these tests load the actual shipped assets so a future edit
/// to the RON that regresses the cooldown, the ability-side `min_level` gate,
/// or the `CasterLevelRoll.unlock_level` fails here instead of shipping
/// silently.
#[cfg(test)]
mod power_word_ron_content_tests {
    use crate::{
        assets::{AssetExt, Ron},
        combat::{CombatEffect, CombatRequirement},
        comp::{ability::CharacterAbility, buff::BuffKind},
        resources::Secs,
    };

    fn load(asset: &str) -> CharacterAbility { Ron::load_expect_cloned(asset).into_inner() }

    /// Digs the Paralyzed `CombatBuff`'s `dur_secs` out of a `BasicRanged`
    /// Power Word's `attack_effect` -- same nested shape `unlock_level_of`
    /// digs the `CasterLevelRoll` out of.
    fn paralyzed_dur_secs_of(ability: &CharacterAbility) -> Secs {
        let CharacterAbility::BasicRanged { projectile, .. } = ability else {
            panic!("expected a BasicRanged Power Word");
        };
        let attack = projectile
            .attack
            .as_ref()
            .expect("Power Word projectile must carry an attack");
        let (effect, _requirement) = attack
            .attack_effect
            .as_ref()
            .expect("Power Word attack must carry an attack_effect");
        let CombatEffect::Buff(buff) = effect else {
            panic!("expected a Buff combat effect");
        };
        assert_eq!(buff.kind, BuffKind::Paralyzed, "expected a Paralyzed buff");
        buff.dur_secs
    }

    /// Digs the `CasterLevelRoll` out of a `BasicRanged` Power Word's nested
    /// `TargetHealthAtOrBelow` + `CasterLevelRoll` `All(..)` combinator --
    /// same shape all three of Kill/Pain/Stun share.
    fn unlock_level_of(ability: &CharacterAbility) -> u16 {
        let CharacterAbility::BasicRanged { projectile, .. } = ability else {
            panic!("expected a BasicRanged Power Word");
        };
        let attack = projectile
            .attack
            .as_ref()
            .expect("Power Word projectile must carry an attack");
        let (_effect, requirement) = attack
            .attack_effect
            .as_ref()
            .expect("Power Word attack must carry an attack_effect");
        let CombatRequirement::All(reqs) = requirement else {
            panic!("expected an All(..) combinator");
        };
        reqs.iter()
            .find_map(|r| match r {
                CombatRequirement::CasterLevelRoll(curve) => Some(curve.unlock_level),
                _ => None,
            })
            .expect("no CasterLevelRoll among the All(..) requirements")
    }

    #[test]
    fn power_word_kill_unlocks_at_54_with_a_90s_cooldown() {
        let ability = load("common.abilities.spells.arcane.power_word_kill");
        let CharacterAbility::BasicRanged { meta, .. } = &ability else {
            panic!("power_word_kill is not a BasicRanged");
        };
        assert_eq!(meta.requirements.min_level, Some(54));
        assert_eq!(meta.cooldown, Some(90.0));
        assert_eq!(unlock_level_of(&ability), 54);
    }

    #[test]
    fn power_word_pain_has_a_60s_cooldown_and_unlocks_at_42() {
        let ability = load("common.abilities.spells.arcane.power_word_pain");
        let CharacterAbility::BasicRanged { meta, .. } = &ability else {
            panic!("power_word_pain is not a BasicRanged");
        };
        assert_eq!(meta.requirements.min_level, Some(42));
        assert_eq!(meta.cooldown, Some(60.0));
        assert_eq!(unlock_level_of(&ability), 42);
    }

    #[test]
    fn power_word_stun_has_a_75s_cooldown_and_unlocks_at_48() {
        let ability = load("common.abilities.spells.arcane.power_word_stun");
        let CharacterAbility::BasicRanged { meta, .. } = &ability else {
            panic!("power_word_stun is not a BasicRanged");
        };
        assert_eq!(meta.requirements.min_level, Some(48));
        assert_eq!(meta.cooldown, Some(75.0));
        assert_eq!(unlock_level_of(&ability), 48);
    }

    /// Part of a wider reorder of the "Paralyzed" spell family (handled
    /// elsewhere for `hold_person`/`hold_monster`/`irresistible_dance`) --
    /// Stun's own Paralyzed duration goes from 15s to 60s.
    #[test]
    fn power_word_stun_paralyzes_for_60_seconds() {
        let ability = load("common.abilities.spells.arcane.power_word_stun");
        assert_eq!(paralyzed_dur_secs_of(&ability), Secs(60.0));
    }
}

/// The content migration off the legacy `Energy` damage kind: `Energy` is a
/// back-compat catch-all (still valid for third-party/NPC content), but every
/// shipped ability RON that used it as a placeholder has been reassigned to
/// its real physical/elemental kind. `Energy` itself is not removed from
/// `DamageKind` -- only its usage is drained.
#[cfg(test)]
mod damage_kind_energy_migration_tests {
    use crate::{
        assets::{AssetExt, Ron},
        combat::DamageKind,
        comp::ability::CharacterAbility,
    };

    fn load(asset: &str) -> CharacterAbility { Ron::load_expect_cloned(asset).into_inner() }

    /// Digs the `DamageKind` out of whichever shape the ability's attack
    /// takes -- a `ProjectileConstructor`'s nested `attack.damage_kind` for
    /// the ranged/thrown variants, or a top-level `damage_kind` /
    /// `shockwave_damage_kind` field for the shockwave variants. Every one of
    /// the 65 migrated RONs loads through one of these shapes.
    fn damage_kind_of(ability: &CharacterAbility) -> DamageKind {
        match ability {
            CharacterAbility::BasicRanged { projectile, .. }
            | CharacterAbility::RapidRanged { projectile, .. }
            | CharacterAbility::ChargedRanged { projectile, .. }
            | CharacterAbility::Throw { projectile, .. } => {
                projectile
                    .attack
                    .as_ref()
                    .expect("expected the projectile to carry an attack")
                    .damage_kind
            },
            CharacterAbility::Shockwave { damage_kind, .. }
            | CharacterAbility::LeapShockwave { damage_kind, .. } => *damage_kind,
            CharacterAbility::LeapExplosionShockwave {
                shockwave_damage_kind,
                ..
            } => *shockwave_damage_kind,
            other => panic!("unexpected ability shape for a damage-kind check: {other:?}"),
        }
    }

    /// Pins the legacy caster-staff fire kit (plan
    /// `2026-08-01-nh69-item-categories-per-class-plan.md` §6: "starting with
    /// the legacy staff fire spells -- firebomb, fire_breath, fireshockwave,
    /// napalm_strike, pyroclasm all deal `Energy` today, so `resist_fire`
    /// does nothing against a fireball") to `Fire`. This is the flagship
    /// regression this migration exists to fix.
    #[test]
    fn legacy_staff_fire_kit_deals_fire_damage() {
        for asset in [
            "common.abilities.staff.firebomb",
            "common.abilities.staff.fire_breath",
            "common.abilities.staff.fireshockwave",
            "common.abilities.staff.napalm_strike",
            "common.abilities.staff.pyroclasm",
        ] {
            let ability = load(asset);
            assert_eq!(
                damage_kind_of(&ability),
                DamageKind::Fire,
                "{asset} should deal Fire damage, not the legacy Energy catch-all"
            );
        }
    }

    /// Every ability RON touched by the migration still parses as a valid
    /// `CharacterAbility` -- if any of the 65 edits had introduced a RON
    /// syntax error or a bad enum variant, `load` would panic here instead of
    /// shipping silently.
    #[test]
    fn every_migrated_ability_still_loads() {
        let migrated_assets = [
            "common.abilities.bow.burning_broadhead",
            "common.abilities.bow.burning_hawkstrike_shot",
            "common.abilities.bow.burning_heartseeker_shot",
            "common.abilities.bow.burning_thorn_stake",
            "common.abilities.bow.freezing_broadhead",
            "common.abilities.bow.freezing_hawkstrike_shot",
            "common.abilities.bow.freezing_heartseeker_shot",
            "common.abilities.bow.freezing_thorn_stake",
            "common.abilities.bow.lightning_thorn_stake",
            "common.abilities.bow.poison_broadhead",
            "common.abilities.bow.poison_hawkstrike_shot",
            "common.abilities.bow.poison_heartseeker_shot",
            "common.abilities.bow.poison_thorn_stake",
            "common.abilities.custom.ancienteffigy.blast",
            "common.abilities.custom.arthropods.blackwidow.poisonball",
            "common.abilities.custom.ashen_warrior.axe.flame_wave",
            "common.abilities.custom.ashen_warrior.staff.fireball",
            "common.abilities.custom.asp.firebomb",
            "common.abilities.custom.biped_large_cultist.staff.firebomb",
            "common.abilities.custom.birdlargebreathe.firebomb",
            "common.abilities.custom.birdlargefire.firerain",
            "common.abilities.custom.birdlargefire.fireshockwave",
            "common.abilities.custom.cloudwyvern.lightningbomb",
            "common.abilities.custom.cursekeeper.poisonbomb",
            "common.abilities.custom.cyclops.optic_blast",
            "common.abilities.custom.dagon.dagonbombs",
            "common.abilities.custom.dwarves.flamekeeper.mines",
            "common.abilities.custom.dwarves.forgemaster.lava_mortar",
            "common.abilities.custom.dwarves.snaretongue.bombs",
            "common.abilities.custom.flamewyvern.firebomb",
            "common.abilities.custom.frostwyvern.frostbomb",
            "common.abilities.custom.gigas_fire.lava_leap",
            "common.abilities.custom.gigas_frost.ice_volley",
            "common.abilities.custom.gravewarden.rocket",
            "common.abilities.custom.harvester.explodingpumpkin",
            "common.abilities.custom.hydra.poison_ball",
            "common.abilities.custom.icedrake.icebombs",
            "common.abilities.custom.irongolemfist.iron_pike_bomb",
            "common.abilities.custom.irrwurz.magicball",
            "common.abilities.custom.maneater.poisonball",
            "common.abilities.custom.mindflayer.necroticsphere_blast",
            "common.abilities.custom.mindflayer.necroticsphere_multiblast",
            "common.abilities.custom.mindflayer.necroticsphere",
            "common.abilities.custom.minotaur.axethrow",
            "common.abilities.custom.ogre_staff.firebomb",
            "common.abilities.custom.quadlowranged.firebomb",
            "common.abilities.custom.seawyvern.inkbomb",
            "common.abilities.custom.terracotta_demolisher.drop",
            "common.abilities.custom.terracotta_demolisher.throw",
            "common.abilities.custom.terracotta_statue.blast",
            "common.abilities.custom.wealdwyvern.poisonbomb",
            "common.abilities.custom.wendigomagic.frostbomb",
            "common.abilities.custom.yeti.snowball",
            "common.abilities.gnarling.chieftain.firebarrage",
            "common.abilities.gnarling.chieftain.fireshockwave",
            "common.abilities.haniwa.archer.explosive",
            "common.abilities.innate.draugr",
            "common.abilities.staff.fire_breath",
            "common.abilities.staff.firebomb",
            "common.abilities.staff.fireshockwave",
            "common.abilities.staff.napalm_strike",
            "common.abilities.staff.pyroclasm",
            "common.abilities.staffsimple.firebomb",
            "common.abilities.throw.bomb",
            "common.abilities.vampire.vampire_bat.drop",
        ];
        assert_eq!(
            migrated_assets.len(),
            65,
            "this list should track all 65 files the migration touched"
        );
        for asset in migrated_assets {
            let ability = load(asset);
            // None of the migrated RONs should still carry the legacy
            // catch-all -- that's the entire point of the migration.
            assert_ne!(
                damage_kind_of(&ability),
                DamageKind::Energy,
                "{asset} should no longer deal the legacy Energy damage kind"
            );
        }
    }

    /// Count-based guard: this migration drained `Energy` usage in shipped
    /// ability RONs from 65 to 0. `Energy` stays in `DamageKind` for
    /// back-compat, so new content is still free to declare it deliberately
    /// -- but this test fails loudly if usage grows past the ceiling below,
    /// so a silent regression to the catch-all doesn't ship unnoticed. Raise
    /// the ceiling (with justification in the PR) if new content genuinely
    /// needs `Energy`.
    #[test]
    fn energy_damage_kind_usage_does_not_grow_past_the_post_migration_ceiling() {
        const MAX_ENERGY_USERS: usize = 0;

        let assets_root = std::path::Path::new(
            &std::env::var("VELOREN_ASSETS").expect("VELOREN_ASSETS must be set for tests"),
        )
        .join("common/abilities");

        fn count_energy_users(dir: &std::path::Path) -> usize {
            let mut count = 0;
            for entry in std::fs::read_dir(dir).expect("abilities dir should be readable") {
                let entry = entry.expect("dir entry should be readable");
                let path = entry.path();
                if path.is_dir() {
                    count += count_energy_users(&path);
                } else if path.extension().is_some_and(|ext| ext == "ron") {
                    let contents =
                        std::fs::read_to_string(&path).expect("ability RON should be readable");
                    if contents.contains("damage_kind: Energy")
                        || contents.contains("shockwave_damage_kind: Energy")
                    {
                        count += 1;
                    }
                }
            }
            count
        }

        let energy_users = count_energy_users(&assets_root);
        // The ceiling is 0 today (this migration drained every user), but the
        // constant is written as a ceiling rather than an exact match so a
        // future PR that deliberately raises it only has to edit the
        // constant, not this comparison.
        #[allow(clippy::absurd_extreme_comparisons)]
        let within_ceiling = energy_users <= MAX_ENERGY_USERS;
        assert!(
            within_ceiling,
            "expected at most {MAX_ENERGY_USERS} ability RON(s) still using the legacy Energy \
             damage kind, found {energy_users} -- new content should declare a specific \
             DamageKind instead of regressing to the catch-all"
        );
    }
}

/// Pins the authored RON content for the martial staff's `staff_martial` kit
/// (the item categories per class content pass) -- loads the actual shipped
/// assets so a future edit that drops the baked-in elemental proc, or
/// reintroduces a class filter on a kit that must stay whitelist-free, fails
/// here instead of shipping silently.
#[cfg(test)]
mod martial_staff_ron_content_tests {
    use crate::{
        assets::{AssetExt, Ron},
        combat::CombatEffect,
        comp::{ability::CharacterAbility, buff::BuffKind, melee::MeleeConstructorKind},
    };

    fn load(asset: &str) -> CharacterAbility { Ron::load_expect_cloned(asset).into_inner() }

    /// The on-hit elemental proc (Q5a) is baked directly into the strike via
    /// `MeleeConstructor::damage_effect`, not a `BuffKind::Frigid` self-buff
    /// (that kind is reserved for the deferred passive-buff system).
    #[test]
    fn frost_strike_procs_the_frozen_debuff_on_hit() {
        let ability = load("common.abilities.staff_martial.frost_strike");
        let CharacterAbility::BasicMelee {
            melee_constructor, ..
        } = &ability
        else {
            panic!("frost_strike is not a BasicMelee");
        };
        let effect = melee_constructor
            .damage_effect
            .as_ref()
            .expect("frost_strike must carry a damage_effect to proc anything");
        let CombatEffect::Buff(buff) = effect else {
            panic!("expected a Buff combat effect, got {effect:?}");
        };
        assert_eq!(
            buff.kind,
            BuffKind::Frozen,
            "the martial staff's proc must be the Frozen debuff, not a self-buff kind"
        );
        assert!(buff.chance > 0.0, "a proc with zero chance never fires");
        // A plain Bash strike so the physical damage is Crushing, independent
        // of the proc riding along on top of it.
        assert!(matches!(
            melee_constructor.kind,
            MeleeConstructorKind::Bash { .. }
        ));
    }

    /// The primary combo is a plain physical strike with no elemental proc
    /// riding along -- only the secondary carries the Frozen proc.
    #[test]
    fn quarterstaff_strikes_primary_is_purely_physical() {
        let ability = load("common.abilities.staff_martial.quarterstaff_strikes");
        let CharacterAbility::ComboMelee2 { strikes, .. } = &ability else {
            panic!("quarterstaff_strikes is not a ComboMelee2");
        };
        assert!(!strikes.is_empty());
        for strike in strikes {
            assert!(
                strike.melee_constructor.damage_effect.is_none(),
                "the primary combo should not itself carry the elemental proc"
            );
            assert!(matches!(
                strike.melee_constructor.kind,
                MeleeConstructorKind::Bash { .. }
            ));
        }
    }
}

#[cfg(test)]
mod health_tier_requirement_tests {
    use super::{CasterLevelFailChance, ClassKind, CombatRequirement, HealthTier, TierEffect};
    use crate::comp::CharacterClass;

    fn tier_with_requirement(requirement: CombatRequirement) -> HealthTier {
        HealthTier {
            max_current_health: 35.0,
            effect: TierEffect::AdditionalDamage(2000.0),
            requirement: Some(requirement),
        }
    }

    fn cleric_roll(fail_chance_at_unlock: f32, fail_chance_at_max_level: f32) -> CombatRequirement {
        CombatRequirement::CasterLevelRoll(Box::new(CasterLevelFailChance {
            unlock_level: 42,
            fail_chance_at_unlock,
            fail_chance_at_max_level,
            source_classes: vec![ClassKind::Cleric],
        }))
    }

    /// Tiers 1-3 (and any other future tier) never authored a `requirement`
    /// at all -- `#[serde(default)]` must mean "always applies", identical to
    /// the shipped behaviour before this field existed.
    #[test]
    fn no_requirement_always_passes() {
        let tier = HealthTier {
            max_current_health: 45.0,
            effect: TierEffect::AdditionalDamage(1.0),
            requirement: None,
        };
        assert!(tier.requirement_met(Some(60), None));
        assert!(tier.requirement_met(None, None));
    }

    /// A fail chance of exactly 1.0 is a deterministic "never" -- `rng.random`
    /// draws in `0.0..1.0`, so `roll >= 1.0` can never be true regardless of
    /// the actual random draw. Picking this edge keeps the test free of any
    /// dependency on RNG seeding.
    #[test]
    fn a_guaranteed_fail_curve_never_passes() {
        let tier = tier_with_requirement(cleric_roll(1.0, 1.0));
        let character_class = CharacterClass::single(ClassKind::Cleric);
        for _ in 0..100 {
            assert!(!tier.requirement_met(Some(60), Some(&character_class)));
        }
    }

    /// A fail chance of exactly 0.0 is a deterministic "always" -- `roll >=
    /// 0.0` is true for every possible draw in `0.0..1.0`.
    #[test]
    fn a_guaranteed_pass_curve_always_passes() {
        let tier = tier_with_requirement(cleric_roll(0.0, 0.0));
        let character_class = CharacterClass::single(ClassKind::Cleric);
        for _ in 0..100 {
            assert!(tier.requirement_met(Some(60), Some(&character_class)));
        }
    }

    /// The roll must resolve against the caster's own *Cleric* level, not
    /// their raw multiclass character level -- a Warrior/Cleric multiclass
    /// whose Cleric secondary hasn't reached `unlock_level` yet must still
    /// fail the roll even though their combined character level is far above
    /// it.
    #[test]
    fn resolves_the_casters_class_level_not_the_raw_character_level() {
        let tier = tier_with_requirement(CombatRequirement::CasterLevelRoll(Box::new(
            CasterLevelFailChance {
                unlock_level: 42,
                fail_chance_at_unlock: 1.0,
                fail_chance_at_max_level: 0.0,
                source_classes: vec![ClassKind::Cleric],
            },
        )));
        let character_class = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Cleric),
            secondary_level: 10,
            future_levels_to_secondary: false,
        };
        assert!(!tier.requirement_met(Some(60), Some(&character_class)));
    }

    /// No resolvable caster level at all (e.g. a non-player source) must not
    /// panic, and is treated as failing the roll -- the same conservative
    /// default `CombatRequirement::CasterLevelRoll` already uses via
    /// `is_some_and`.
    #[test]
    fn an_unresolvable_caster_level_fails_closed() {
        let tier = tier_with_requirement(cleric_roll(0.0, 0.0));
        assert!(!tier.requirement_met(None, None));
    }
}

#[cfg(test)]
mod damage_kind_taxonomy_tests {
    use super::DamageKind;

    // ENG-A2: the full 13-kind content damage taxonomy (Matias 2026-06-20) parses
    // from RON.
    #[test]
    fn all_content_kinds_parse() {
        for s in [
            "Acid",
            "Bludgeoning",
            "Cold",
            "Fire",
            "Force",
            "Lightning",
            "Necrotic",
            "Piercing",
            "Poison",
            "Psychic",
            "Radiant",
            "Slashing",
            "Thunder",
        ] {
            ron::from_str::<DamageKind>(s)
                .unwrap_or_else(|e| panic!("DamageKind `{s}` must parse: {e}"));
        }
    }

    // `Bludgeoning` is the content-facing name for the engine's `Crushing` (kept
    // for its poise behaviour); a serde alias avoids renaming churn across code
    // + existing RON.
    #[test]
    fn bludgeoning_aliases_crushing() {
        assert_eq!(
            ron::from_str::<DamageKind>("Bludgeoning").unwrap(),
            DamageKind::Crushing
        );
    }

    // Back-compat: existing RON using the legacy generic `Energy` still loads (no
    // data migration).
    #[test]
    fn legacy_energy_still_parses() {
        assert_eq!(
            ron::from_str::<DamageKind>("Energy").unwrap(),
            DamageKind::Energy
        );
    }
}

#[cfg(test)]
mod combat_resolution_tests {
    use super::{AttackSource, CombatTuning, DamageKind};
    use crate::assets::{AssetExt, Ron};

    // BL-52: the to-hit roll is single-target only; AoE auto-hits and is
    // mitigated by resistances (P3), so it must never enter the miss roll. This
    // pins the `is_single_target` gate in `apply_attack`.
    #[test]
    fn only_single_target_sources_roll_to_hit() {
        let single = |s| matches!(s, AttackSource::Melee | AttackSource::Projectile);
        assert!(single(AttackSource::Melee));
        assert!(single(AttackSource::Projectile));
        for aoe in [
            AttackSource::Beam,
            AttackSource::GroundShockwave,
            AttackSource::AirShockwave,
            AttackSource::UndodgeableShockwave,
            AttackSource::Explosion,
            AttackSource::Arc,
            AttackSource::Pool,
        ] {
            assert!(!single(aoe), "{aoe:?} is AoE and must not roll to-hit");
        }
    }

    // Mirrors the to-hit math in `Attack::apply_attack` so the formula is unit-
    // tested without a full attack setup (BL-52 §1).
    fn hit_chance(t: &CombatTuning, accuracy: f32, evasion: f32) -> f32 {
        (t.base_hit + (accuracy - evasion) * t.hit_k).clamp(t.hit_floor, t.hit_ceil)
    }

    #[test]
    fn hit_formula_matches_balance_examples() {
        let t = CombatTuning::default();
        // Equal stats -> base 0.85.
        assert!((hit_chance(&t, 30.0, 30.0) - 0.85).abs() < 1e-4);
        // +8 advantage -> 0.97.
        assert!((hit_chance(&t, 38.0, 30.0) - 0.97).abs() < 1e-4);
        // Heavy dodge target -> 0.67.
        assert!((hit_chance(&t, 30.0, 42.0) - 0.67).abs() < 1e-4);
        // Big over-investment clamps to the 1.0 ceiling.
        assert!((hit_chance(&t, 45.0, 25.0) - 1.0).abs() < 1e-4);
        // Fear (-12 acc) vs equal evasion -> 0.67.
        assert!((hit_chance(&t, 18.0, 30.0) - 0.67).abs() < 1e-4);
    }

    #[test]
    fn hit_chance_respects_floor_and_ceiling() {
        let t = CombatTuning::default();
        // Hopelessly outmatched still lands at least the 5% floor.
        assert!((hit_chance(&t, 0.0, 1000.0) - t.hit_floor).abs() < 1e-6);
        // Wildly over-invested never exceeds the 1.0 ceiling.
        assert!((hit_chance(&t, 1000.0, 0.0) - t.hit_ceil).abs() < 1e-6);
    }

    #[test]
    fn combat_tuning_asset_loads() {
        // The shipped RON parses and carries the locked BL-52 constants.
        let tuning = Ron::<CombatTuning>::load_expect("common.combat_tuning");
        let t = &tuning.read().0;
        assert!((t.base_hit - 0.85).abs() < 1e-6);
        assert!((t.hit_floor - 0.05).abs() < 1e-6);
        assert!((t.hit_ceil - 1.0).abs() < 1e-6);
        // P2 crit fields present + sane.
        assert!((t.crit_chance_floor - 0.03).abs() < 1e-6);
        assert!((t.crit_chance_cap - 0.75).abs() < 1e-6);
        assert!(t.crit_chance_floor < t.crit_chance_cap);
        assert!(t.crit_precision_mult > 0.0);
        // P3 resistance cap present + sane.
        assert!((t.resist_soft_cap - 0.75).abs() < 1e-6);
        // P5 armor-evasion fields present + sane.
        assert!(t.gear_evasion_floor < t.gear_evasion_cap);
        assert!(t.armor_evasion_per_protection > 0.0);
        assert!(t.shield_evasion_penalty >= 0.0);
        // P6 crit-damage floor present + meaningful (>1.0).
        assert!(t.crit_damage_mult > 1.0);
        // Weapon-proficiency penalty: pinned so a typo'd RON key can't
        // silently fall back to `CombatTuning::default()`'s identical 0.40
        // and read as a no-op retune.
        assert!((t.non_proficient_damage_mult - 0.40).abs() < 1e-6);
    }

    // BL-52 P6: a full crit deals at least crit_damage_mult× at base gear and
    // scales up with precision_power; positional precision scales the floor by
    // its fraction. Mirrors the `precise_damage` formula in
    // calculate_health_change.
    #[test]
    fn crit_damage_floor_and_scaling() {
        let t = CombatTuning::default();
        // bonus fraction = precision_mult * ((crit_damage_mult - 1) + (precision_power
        // - 1))
        let bonus = |precision_mult: f32, precision_power: f32| {
            precision_mult * ((t.crit_damage_mult - 1.0) + (precision_power - 1.0))
        };
        // Full crit at base gear (precision_power 1.0) → +50% (×1.5), not ~+0%.
        assert!((bonus(1.0, 1.0) - 0.5).abs() < 1e-6);
        // Endgame precision gear (precision_power 1.5) → +100% (×2.0).
        assert!((bonus(1.0, 1.5) - 1.0).abs() < 1e-6);
        // Positional gradation preserved: a side flank (0.25) at base < full crit.
        assert!(bonus(0.25, 1.0) < bonus(1.0, 1.0));
        assert!((bonus(0.25, 1.0) - 0.125).abs() < 1e-6);
    }

    // BL-52 P5: armor evasion is derived from total protection — unarmored hits
    // the cap, heavy armor floors out, a shield subtracts a flat penalty.
    #[test]
    fn armor_evasion_from_protection() {
        let t = CombatTuning::default();
        // Mirrors `compute_armor_evasion` (protection → evasion + shield).
        let armor_eva = |protection: f32, shield: bool| {
            let from_protection = t.gear_evasion_cap - protection * t.armor_evasion_per_protection;
            let s = if shield {
                t.shield_evasion_penalty
            } else {
                0.0
            };
            (from_protection - s).clamp(t.gear_evasion_floor, t.gear_evasion_cap)
        };
        // Unarmored → capped at the ceiling (most evasive).
        assert!((armor_eva(0.0, false) - t.gear_evasion_cap).abs() < 1e-6);
        // Heavy armor (lots of protection) → floored (easiest to hit).
        assert!((armor_eva(1000.0, false) - t.gear_evasion_floor).abs() < 1e-6);
        // A shield always lowers evasion vs the same loadout without one.
        assert!(armor_eva(20.0, true) < armor_eva(20.0, false));
        // More protection is never more evasive.
        assert!(armor_eva(40.0, false) <= armor_eva(10.0, false));
    }

    // BL-52 P2: the rolled crit chance is clamped to [floor, cap] — a zero-crit
    // attacker still crits at the floor, an over-stacked one never exceeds the
    // cap (guaranteed positional crits bypass this path).
    #[test]
    fn crit_chance_clamps_to_floor_and_cap() {
        let t = CombatTuning::default();
        let clamp = |c: f32| c.clamp(t.crit_chance_floor, t.crit_chance_cap);
        assert!((clamp(0.0) - t.crit_chance_floor).abs() < 1e-6);
        assert!((clamp(1.0) - t.crit_chance_cap).abs() < 1e-6);
        assert!((clamp(0.30) - 0.30).abs() < 1e-6);
    }

    // BL-52 P3: AoE elemental resistance maps each damage kind to its channel;
    // physical kinds use armor DR only (return 0 here, no double-count).
    #[test]
    fn aoe_resistance_maps_damage_kinds() {
        use crate::comp::Stats;
        let body = crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random());
        let mut s = Stats::empty(body);
        s.resist_fire = 0.4;
        s.resist_frost = 0.3;
        s.resist_poison = 0.2;
        s.resist_magic = 0.1;
        assert!((s.aoe_resistance(DamageKind::Fire) - 0.4).abs() < 1e-6);
        assert!((s.aoe_resistance(DamageKind::Cold) - 0.3).abs() < 1e-6);
        assert!((s.aoe_resistance(DamageKind::Poison) - 0.2).abs() < 1e-6);
        assert!((s.aoe_resistance(DamageKind::Acid) - 0.2).abs() < 1e-6); // acid → poison
        assert!((s.aoe_resistance(DamageKind::Lightning) - 0.1).abs() < 1e-6); // → magic
        assert!((s.aoe_resistance(DamageKind::Energy) - 0.1).abs() < 1e-6); // legacy → magic
        // Physical kinds are handled by armor DR, not this layer.
        assert_eq!(s.aoe_resistance(DamageKind::Piercing), 0.0);
        assert_eq!(s.aoe_resistance(DamageKind::Slashing), 0.0);
        assert_eq!(s.aoe_resistance(DamageKind::Crushing), 0.0);
    }

    // BL-52 P3: AoE mitigation composes armor DR and resistance as independent
    // layers, with resistance soft-capped so stacking can't reach immunity.
    #[test]
    fn aoe_resistance_composes_and_caps() {
        let cap = CombatTuning::default().resist_soft_cap;
        let combine = |dr: f32, resist: f32| 1.0 - (1.0 - dr) * (1.0 - resist.clamp(0.0, cap));
        // No resist → unchanged DR.
        assert!((combine(0.25, 0.0) - 0.25).abs() < 1e-6);
        // 50% DR + 50% resist → 75% total (independent layers).
        assert!((combine(0.5, 0.5) - 0.75).abs() < 1e-6);
        // Resist above the cap is clamped: 0% DR + 0.95 resist → 0.75 total.
        assert!((combine(0.0, 0.95) - cap).abs() < 1e-6);
        // Negative resist is floored at 0 — never amplifies AoE damage.
        assert!((combine(0.25, -0.5) - 0.25).abs() < 1e-6);
    }
}

#[cfg(test)]
mod weapon_proficiency_tests {
    use super::{AbilityInfo, Attack, CombatTuning, HandInfo, InputKind, Stats, tool::WeaponRole};
    use crate::comp::{
        Body,
        ability::AbilityMeta,
        class::{ClassKind, class_proficiencies},
        humanoid,
        inventory::item::tool::{ToolKind, ToolKindMask},
    };

    fn test_body() -> Body { Body::Humanoid(humanoid::Body::random()) }

    fn stats_with_mask(mask: ToolKindMask) -> Stats {
        let mut stats = Stats::empty(test_body());
        stats.proficient_tools = mask;
        stats.non_proficient_damage_mult = 0.40;
        stats
    }

    fn ability_info(tool: ToolKind, hand: HandInfo) -> AbilityInfo {
        // `role: None` -- permissive on the role axis; only the tests that
        // specifically exercise WeaponRole narrowing use
        // `ability_info_with_role` below.
        AbilityInfo {
            tool: Some(tool),
            hand: Some(hand),
            role: None,
            input: InputKind::Primary,
            input_attr: None,
            ability_meta: AbilityMeta::default(),
            ability: None,
        }
    }

    fn ability_info_with_role(tool: ToolKind, hand: HandInfo, role: WeaponRole) -> AbilityInfo {
        AbilityInfo {
            role: Some(role),
            ..ability_info(tool, hand)
        }
    }

    #[test]
    fn non_proficient_weapon_deals_forty_percent_physical_output() {
        let proficient = stats_with_mask(ToolKindMask::DAGGER);
        let non_proficient = stats_with_mask(ToolKindMask::SWORD_1H | ToolKindMask::SWORD_2H);
        let ai = ability_info(ToolKind::Dagger, HandInfo::MainHand);

        let proficient_mult = Attack::proficiency_multiplier(Some(&proficient), Some(ai), false);
        let non_proficient_mult =
            Attack::proficiency_multiplier(Some(&non_proficient), Some(ai), false);

        assert!((proficient_mult - 1.0).abs() < 1e-6);
        assert!((non_proficient_mult - 0.40).abs() < 1e-6);
        // The proficient attacker deals 2.5x the non-proficient one's damage,
        // all else equal (1.0 / 0.40).
        assert!((proficient_mult / non_proficient_mult - 2.5).abs() < 1e-6);
    }

    #[test]
    fn magic_attacks_are_never_penalised() {
        let non_proficient = stats_with_mask(ToolKindMask::DAGGER);
        let ai = ability_info(ToolKind::Sword, HandInfo::TwoHanded);
        let mult = Attack::proficiency_multiplier(Some(&non_proficient), Some(ai), true);
        assert!((mult - 1.0).abs() < 1e-6);

        let proficient = stats_with_mask(ToolKindMask::all());
        let mult = Attack::proficiency_multiplier(Some(&proficient), Some(ai), true);
        assert!((mult - 1.0).abs() < 1e-6);
    }

    #[test]
    fn permissive_classes_and_classless_entities_take_no_penalty() {
        let adventurer = class_proficiencies(ClassKind::Adventurer).mask();
        assert_eq!(adventurer, ToolKindMask::all());
        let stats = stats_with_mask(adventurer);
        let ai = ability_info(ToolKind::Staff, HandInfo::TwoHanded);
        assert!((Attack::proficiency_multiplier(Some(&stats), Some(ai), false) - 1.0).abs() < 1e-6);

        // An entity with no `CharacterClass` (every NPC/summon/boss) keeps
        // `Stats::empty`'s permissive default untouched.
        let empty = Stats::empty(test_body());
        assert!((Attack::proficiency_multiplier(Some(&empty), Some(ai), false) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn no_tool_is_always_proficient() {
        let non_proficient = stats_with_mask(ToolKindMask::empty());

        // `ability_info: None` (e.g. a bare `AttackSource` with no ability
        // behind it).
        let mult = Attack::proficiency_multiplier(Some(&non_proficient), None, false);
        assert!((mult - 1.0).abs() < 1e-6);

        // `ability_info: Some(_)` but `tool: None` — unarmed strikes, natural
        // weapons, NPC `Empty`-tool attacks.
        let ai = AbilityInfo {
            tool: None,
            hand: None,
            role: None,
            input: InputKind::Primary,
            input_attr: None,
            ability_meta: AbilityMeta::default(),
            ability: None,
        };
        let mult = Attack::proficiency_multiplier(Some(&non_proficient), Some(ai), false);
        assert!((mult - 1.0).abs() < 1e-6);
    }

    #[test]
    fn main_hand_and_off_hand_are_judged_independently() {
        // Proficient with Dagger, not with Sword.
        let stats = stats_with_mask(ToolKindMask::DAGGER);
        let dagger_main_hand = ability_info(ToolKind::Dagger, HandInfo::MainHand);
        let sword_off_hand = ability_info(ToolKind::Sword, HandInfo::OffHand);

        assert!(
            (Attack::proficiency_multiplier(Some(&stats), Some(dagger_main_hand), false) - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (Attack::proficiency_multiplier(Some(&stats), Some(sword_off_hand), false) - 0.40)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn sword_grip_split_follows_the_class_manifest() {
        let one_handed = ability_info(ToolKind::Sword, HandInfo::MainHand);
        let two_handed = ability_info(ToolKind::Sword, HandInfo::TwoHanded);

        // Rogue: 1h swords (gladii) only, per class_proficiencies.ron.
        let rogue_stats = stats_with_mask(class_proficiencies(ClassKind::Rogue).mask());
        assert!(
            (Attack::proficiency_multiplier(Some(&rogue_stats), Some(one_handed), false) - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (Attack::proficiency_multiplier(Some(&rogue_stats), Some(two_handed), false) - 0.40)
                .abs()
                < 1e-6
        );

        // Warrior: both grips.
        let warrior_stats = stats_with_mask(class_proficiencies(ClassKind::Warrior).mask());
        assert!(
            (Attack::proficiency_multiplier(Some(&warrior_stats), Some(one_handed), false) - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (Attack::proficiency_multiplier(Some(&warrior_stats), Some(two_handed), false) - 1.0)
                .abs()
                < 1e-6
        );
    }

    /// Mirrors `sword_grip_split_follows_the_class_manifest` for the
    /// `WeaponRole` axis: the role travels end to end from `AbilityInfo`
    /// through `proficiency_multiplier`, not just through the `ToolKindMask`
    /// tested directly in `class.rs`.
    #[test]
    fn staff_role_split_follows_the_class_manifest() {
        let caster_kit =
            ability_info_with_role(ToolKind::Staff, HandInfo::TwoHanded, WeaponRole::Caster);
        let martial_kit =
            ability_info_with_role(ToolKind::Staff, HandInfo::TwoHanded, WeaponRole::Martial);

        // Mage: caster kit only, per class_proficiencies.ron.
        let mage_stats = stats_with_mask(class_proficiencies(ClassKind::Mage).mask());
        assert!(
            (Attack::proficiency_multiplier(Some(&mage_stats), Some(caster_kit), false) - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (Attack::proficiency_multiplier(Some(&mage_stats), Some(martial_kit), false) - 0.40)
                .abs()
                < 1e-6
        );

        // Monk: martial kit only.
        let monk_stats = stats_with_mask(class_proficiencies(ClassKind::Monk).mask());
        assert!(
            (Attack::proficiency_multiplier(Some(&monk_stats), Some(martial_kit), false) - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (Attack::proficiency_multiplier(Some(&monk_stats), Some(caster_kit), false) - 0.40)
                .abs()
                < 1e-6
        );
    }

    // Mirrors the pre-clamp scaling in `Attack::apply_attack`'s rolled-crit
    // branch: the proficiency multiplier applies to `crit_chance` before the
    // existing floor/cap clamp, so a scaled-down value can still be rescued
    // by the floor.
    #[test]
    fn crit_chance_scales_by_proficiency_before_the_floor_clamp() {
        let t = CombatTuning::default();
        let scaled_crit_chance =
            |base: f32, mult: f32| (base * mult).clamp(t.crit_chance_floor, t.crit_chance_cap);

        // A typical build (0.30 base): scaling to 0.12 still clears the floor.
        assert!((scaled_crit_chance(0.30, 0.40) - 0.12).abs() < 1e-6);

        // A low-crit build (0.05 base) whose scaled value (0.02) would fall
        // under the floor: the floor still guarantees a baseline chance.
        assert!((0.05_f32 * 0.40 - t.crit_chance_floor).abs() > 1e-6);
        assert!((scaled_crit_chance(0.05, 0.40) - t.crit_chance_floor).abs() < 1e-6);
    }
}

#[cfg(test)]
mod magic_source_attribution_tests {
    use super::{AbilityInfo, Attack, HandInfo, InputKind, MagicSource};
    use crate::comp::ability::AbilityMeta;

    fn ability_info_with_source(source: Option<MagicSource>) -> AbilityInfo {
        AbilityInfo {
            tool: None,
            hand: Some(HandInfo::MainHand),
            role: None,
            input: InputKind::Primary,
            input_attr: None,
            ability_meta: AbilityMeta {
                source,
                ..Default::default()
            },
            ability: None,
        }
    }

    /// An attack carrying an ability whose `ability_meta.source` is set
    /// reports that source; a bare attack with no ability behind it (a
    /// plain weapon swing, fall damage, etc.) reports `None`.
    #[test]
    fn magic_source_reads_from_ability_meta() {
        let sourced = Attack::new(Some(ability_info_with_source(Some(
            MagicSource::Primordial,
        ))));
        assert_eq!(sourced.magic_source(), Some(MagicSource::Primordial));

        let sourceless_ability = Attack::new(Some(ability_info_with_source(None)));
        assert_eq!(sourceless_ability.magic_source(), None);

        let no_ability = Attack::new(None);
        assert_eq!(no_ability.magic_source(), None);
    }
}

#[cfg(test)]
mod saving_throw_tests {
    use super::{
        AttackSource, CombatTuning, DamageContributor, DamageSource, SaveCasterInfo,
        SaveCombatContext, SaveTargetInfo, effective_magic_evasion, is_fighting_caster,
        saving_throw_chance,
    };
    use crate::{
        assets::{AssetExt, Ron},
        comp::{Health, HealthChange, body::MagicResistTier, group},
        resources::Time,
        uid::Uid,
    };
    use std::num::NonZeroU64;

    const EPS: f32 = 1e-4;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    /// A creature target: no class attributes, so its whole magic evasion comes
    /// from the `combat_rating` term.
    fn creature(combat_rating: f32, tier: MagicResistTier) -> SaveTargetInfo {
        SaveTargetInfo {
            stats_magic_evasion: 0.0,
            crowd_control_resistance: 0.0,
            stats_magic_resistance: 0.0,
            magic_resist_tier: tier,
            combat_rating,
        }
    }

    /// A player-character target, whose magic evasion is class/level derived.
    /// The calibration table quotes a PC's `magic_evasion` directly and gives
    /// it no `combat_rating`, so that term is held at 0 here to reproduce the
    /// table's stated accuracy differences exactly.
    fn player(magic_evasion: f32, magic_resistance: f32) -> SaveTargetInfo {
        SaveTargetInfo {
            stats_magic_evasion: magic_evasion,
            crowd_control_resistance: 0.0,
            stats_magic_resistance: magic_resistance,
            magic_resist_tier: MagicResistTier::None,
            combat_rating: 0.0,
        }
    }

    fn chance(t: &CombatTuning, accuracy: f32, target: &SaveTargetInfo, fighting: bool) -> f32 {
        saving_throw_chance(
            &SaveCasterInfo {
                magic_accuracy: accuracy,
            },
            target,
            fighting,
            t,
        )
    }

    /// The creature's `combat_rating` term is the only difficulty signal a
    /// non-class body has, and it is what puts a world boss out of reach.
    #[test]
    fn effective_evasion_folds_combat_rating() {
        let t = CombatTuning::default();
        // A rabbit-tier creature contributes almost nothing.
        assert!((effective_magic_evasion(&creature(1.0, MagicResistTier::None), &t) - 2.0) < EPS);
        // A boss-tier one contributes more evasion than any caster has accuracy.
        assert!(effective_magic_evasion(&creature(36.0, MagicResistTier::None), &t) > 70.0);
        // A player's class evasion is used as-is, added to (here zero) rating.
        assert!((effective_magic_evasion(&player(30.0, 0.0), &t) - 30.0).abs() < EPS);
    }

    /// Every row of the calibration table, recomputed by the shipped formula.
    /// Rows whose level term reaches the outclassed wall return exactly 0.0
    /// instead of resting on the 5 % floor.
    #[test]
    fn calibration_table() {
        let t = CombatTuning::default();
        let row =
            |accuracy, target: &SaveTargetInfo, fighting| chance(&t, accuracy, target, fighting);

        // 1. L10 caster (acc 12) vs a CR 1 beast with no resistance.
        assert!((row(12.0, &creature(1.0, MagicResistTier::None), false) - 0.85).abs() < EPS);
        // 2. L10 caster vs a CR 3.5 body with the major tier.
        assert!((row(12.0, &creature(3.5, MagicResistTier::Major), false) - 0.475).abs() < EPS);
        // 3. L10 caster vs a CR 5.2 body with the major tier.
        assert!((row(12.0, &creature(5.2, MagicResistTier::Major), false) - 0.424).abs() < EPS);
        // 4. L40 caster (acc 42) vs a CR 36 legendary body — outclassed wall.
        assert_eq!(
            row(42.0, &creature(36.0, MagicResistTier::Legendary), false),
            0.0
        );
        // 5. L40 caster vs an equal-level PC.
        assert!((row(42.0, &player(30.0, 0.0), false) - 0.88).abs() < EPS);
        // 6. L40 caster vs a L5 PC — clamped by the 0.95 ceiling.
        assert!((row(42.0, &player(4.0, 0.0), false) - t.save_hit_ceil).abs() < EPS);
        // 6b. Same, mid-fight: the level term is far past the ceiling, so the
        //     in-combat penalty does not rescue the victim.
        assert!((row(42.0, &player(4.0, 0.0), true) - t.save_hit_ceil).abs() < EPS);
        // 7. L5 caster (acc 7) vs a L40 PC — outclassed wall.
        assert_eq!(row(7.0, &player(30.0, 0.0), false), 0.0);
        // 7b. Same, mid-duel: still the wall, which is checked first.
        assert_eq!(row(7.0, &player(30.0, 0.0), true), 0.0);
        // 8. L5 caster vs a CR 36 legendary body — outclassed wall.
        assert_eq!(
            row(7.0, &creature(36.0, MagicResistTier::Legendary), false),
            0.0
        );
        // 9. L10 caster vs a L10 PC with a minor racial magic resistance.
        assert!((row(12.0, &player(6.0, 0.15), false) - 0.64).abs() < EPS);
        // 9b. Same, mid-fight.
        assert!((row(12.0, &player(6.0, 0.15), true) - 0.44).abs() < EPS);
    }

    /// The wall is a hard bypass of the floor, not another subtracted term: a
    /// deficit just short of it still leaves a real chance, and one just past
    /// it leaves none at all.
    #[test]
    fn outclassed_wall_bypasses_the_floor() {
        let t = CombatTuning::default();
        // -18 net points => level term -0.27, short of the -0.30 wall.
        let winnable = chance(&t, 12.0, &player(30.0, 0.0), false);
        assert!((winnable - 0.43).abs() < EPS);
        // -22 net points => level term -0.33, past the wall.
        assert_eq!(chance(&t, 8.0, &player(30.0, 0.0), false), 0.0);
        // A target beyond any reach is 0.0, never the floor.
        assert_eq!(chance(&t, 0.0, &player(1000.0, 0.0), false), 0.0);
    }

    /// A caster that is *not* outclassed still gets the floor, however
    /// resistant the target is — the two mechanisms are independent.
    #[test]
    fn floor_still_applies_below_the_wall() {
        let t = CombatTuning::default();
        let mut target = player(30.0, 0.0);
        target.crowd_control_resistance = 0.75;
        // Level term -0.27 (inside the wall) minus the capped resist and the
        // in-combat penalty lands far below zero, so the floor catches it.
        assert!((chance(&t, 12.0, &target, true) - t.save_hit_floor).abs() < EPS);
    }

    /// Magic resistance and crowd-control resistance are summed and then
    /// capped, so stacking them can never reach immunity.
    #[test]
    fn resistance_saturates_at_the_soft_cap() {
        let t = CombatTuning::default();
        // Legendary tier (0.50) + 0.60 CCR = 1.10 raw, capped to 0.75.
        let mut over_capped = player(12.0, 0.0);
        over_capped.magic_resist_tier = MagicResistTier::Legendary;
        over_capped.crowd_control_resistance = 0.60;
        // Exactly at the cap: 0.50 + 0.25.
        let mut at_cap = over_capped;
        at_cap.crowd_control_resistance = 0.25;
        let over = chance(&t, 12.0, &over_capped, false);
        let at = chance(&t, 12.0, &at_cap, false);
        assert!((over - at).abs() < EPS);
        // 0.70 - 0.75 is negative, so both rest on the floor rather than 0.0.
        assert!((over - t.save_hit_floor).abs() < EPS);
    }

    /// The in-combat penalty is exactly the tuned value in the unclamped band,
    /// and is symmetric in the sense that dropping the flag restores the value.
    #[test]
    fn in_combat_penalty_is_exact_when_unclamped() {
        let t = CombatTuning::default();
        let target = player(6.0, 0.0);
        let calm = chance(&t, 12.0, &target, false);
        let fighting = chance(&t, 12.0, &target, true);
        assert!((calm - fighting - t.save_in_combat_penalty).abs() < EPS);
        assert!((calm - 0.79).abs() < EPS);
    }

    /// A boss-tier body is out of reach for every reachable caster, with no
    /// hand-authored "uncharmable" list: its combat rating alone puts the level
    /// term past the wall for any accuracy a character can actually attain.
    ///
    /// Note the wall does *not* make it unreachable in principle: a caster
    /// whose magic accuracy came within 20 net points of the boss's
    /// effective magic evasion would clear the wall and fall back on the 5
    /// % floor. At an effective evasion of 72 that needs ~52 accuracy, well
    /// past the ~42 a max-level caster reaches, so the boss property holds
    /// in practice — but it holds because of the accuracy ceiling, not
    /// unconditionally.
    #[test]
    fn boss_tier_body_is_unreachable_at_attainable_caster_levels() {
        let t = CombatTuning::default();
        let boss = creature(36.0, MagicResistTier::Legendary);
        for accuracy in [0.0, 7.0, 12.0, 42.0, 51.0] {
            assert_eq!(chance(&t, accuracy, &boss, false), 0.0);
        }
        // Past the wall threshold the floor takes over again.
        assert!((chance(&t, 55.0, &boss, false) - t.save_hit_floor).abs() < EPS);
    }

    /// The shipped asset carries the confirmed constants, and stays loadable.
    #[test]
    fn combat_tuning_asset_carries_saving_throw_constants() {
        let tuning = Ron::<CombatTuning>::load_expect("common.combat_tuning");
        let t = &tuning.read().0;
        assert!((t.save_base_hit - 0.70).abs() < 1e-6);
        assert!((t.save_hit_floor - 0.05).abs() < 1e-6);
        assert!((t.save_hit_ceil - 0.95).abs() < 1e-6);
        assert!((t.save_mr_soft_cap - 0.75).abs() < 1e-6);
        assert!((t.save_cr_to_evasion - 2.0).abs() < 1e-6);
        assert!((t.save_in_combat_penalty - 0.20).abs() < 1e-6);
        assert!((t.magic_resist_minor - 0.15).abs() < 1e-6);
        assert!((t.magic_resist_major - 0.30).abs() < 1e-6);
        assert!((t.magic_resist_legendary - 0.50).abs() < 1e-6);
        assert!((t.save_outclassed_wall - 0.30).abs() < 1e-6);
        // A mind-altering effect is never guaranteed, unlike damage.
        assert!(t.save_hit_ceil < t.hit_ceil);
        // Charm has its own, stingier base than the damage curve.
        assert!(t.save_base_hit < t.base_hit);
        // The tier ladder is monotonic.
        assert!(t.magic_resist_minor < t.magic_resist_major);
        assert!(t.magic_resist_major < t.magic_resist_legendary);
    }

    /// An asset predating the mind-altering section still parses, and picks up
    /// the tuned values rather than zeroes: the struct-level
    /// `#[serde(default)]` fills missing fields from
    /// `CombatTuning::default()`, so no field may carry its own
    /// `#[serde(default)]` (that would resolve to `0.0` and silently make
    /// every charm land at the base rate).
    #[test]
    fn partial_asset_falls_back_to_tuned_defaults() {
        let t: CombatTuning = ron::from_str("(base_hit: 0.85, hit_k: 0.015)").unwrap();
        let d = CombatTuning::default();
        assert!((t.save_base_hit - d.save_base_hit).abs() < 1e-6);
        assert!((t.save_hit_floor - d.save_hit_floor).abs() < 1e-6);
        assert!((t.save_hit_ceil - d.save_hit_ceil).abs() < 1e-6);
        assert!((t.save_mr_soft_cap - d.save_mr_soft_cap).abs() < 1e-6);
        assert!((t.save_cr_to_evasion - d.save_cr_to_evasion).abs() < 1e-6);
        assert!((t.save_in_combat_penalty - d.save_in_combat_penalty).abs() < 1e-6);
        assert!((t.magic_resist_minor - d.magic_resist_minor).abs() < 1e-6);
        assert!((t.magic_resist_major - d.magic_resist_major).abs() < 1e-6);
        assert!((t.magic_resist_legendary - d.magic_resist_legendary).abs() < 1e-6);
        assert!((t.save_outclassed_wall - d.save_outclassed_wall).abs() < 1e-6);
        assert!(t.save_cr_to_evasion > 0.0);
        assert!(t.save_outclassed_wall > 0.0);
    }

    /// Every tier maps to its tuned number, and `None` is exactly zero.
    #[test]
    fn tier_values_map_to_tuning() {
        let t = CombatTuning::default();
        assert_eq!(t.magic_resist_tier_value(MagicResistTier::None), 0.0);
        assert!((t.magic_resist_tier_value(MagicResistTier::Minor) - 0.15).abs() < EPS);
        assert!((t.magic_resist_tier_value(MagicResistTier::Major) - 0.30).abs() < EPS);
        assert!((t.magic_resist_tier_value(MagicResistTier::Legendary) - 0.50).abs() < EPS);
        // The innate tier and the additive `Stats` term stack.
        let mut target = creature(0.0, MagicResistTier::Major);
        target.stats_magic_resistance = 0.15;
        assert!((target.magic_resistance(&t) - 0.45).abs() < EPS);
    }

    fn health_change(amount: f32, by: Option<DamageContributor>, at: f64) -> HealthChange {
        HealthChange {
            amount,
            by,
            cause: Some(DamageSource::Attack(AttackSource::Melee)),
            magic_source: None,
            time: Time(at),
            precise: false,
            instance: 0,
        }
    }

    const CASTER: u64 = 1;
    const TARGET: u64 = 2;
    const ALLY: u64 = 3;
    const STRANGER: u64 = 4;

    fn ctx<'a>(now: f64) -> SaveCombatContext<'a> {
        SaveCombatContext {
            caster_uid: uid(CASTER),
            caster_group: None,
            target_uid: uid(TARGET),
            target_group: None,
            target_hostile_focus: None,
            target_last_change: None,
            caster_last_change: None,
            now,
        }
    }

    #[test]
    fn creature_target_fighting_the_caster_is_detected() {
        let mut c = ctx(100.0);
        c.target_hostile_focus = Some((uid(CASTER), None));
        assert!(is_fighting_caster(&c));
    }

    #[test]
    fn creature_target_fighting_the_casters_group_is_detected() {
        let mut c = ctx(100.0);
        c.caster_group = Some(group::ENEMY);
        c.target_hostile_focus = Some((uid(ALLY), Some(group::ENEMY)));
        assert!(is_fighting_caster(&c));
    }

    #[test]
    fn creature_target_fighting_someone_else_is_not() {
        let mut c = ctx(100.0);
        c.caster_group = Some(group::ENEMY);
        c.target_hostile_focus = Some((uid(STRANGER), Some(group::NPC)));
        assert!(!is_fighting_caster(&c));
        // A groupless caster must not match on two `None` groups.
        let mut c = ctx(100.0);
        c.target_hostile_focus = Some((uid(STRANGER), None));
        assert!(!is_fighting_caster(&c));
    }

    #[test]
    fn idle_creature_target_is_not_fighting() {
        assert!(!is_fighting_caster(&ctx(100.0)));
    }

    #[test]
    fn player_target_recently_hurt_by_the_caster_is_detected() {
        let change = health_change(-10.0, Some(DamageContributor::Solo(uid(CASTER))), 98.0);
        let mut c = ctx(100.0);
        c.target_last_change = Some(&change);
        assert!(is_fighting_caster(&c));
    }

    #[test]
    fn player_target_hurt_long_ago_is_not_fighting() {
        let change = health_change(-10.0, Some(DamageContributor::Solo(uid(CASTER))), 90.0);
        let mut c = ctx(100.0);
        c.target_last_change = Some(&change);
        assert!(!is_fighting_caster(&c));
    }

    #[test]
    fn caster_recently_hurt_by_the_target_is_detected() {
        // The reverse direction: the target hit the caster, not the other way.
        let change = health_change(-10.0, Some(DamageContributor::Solo(uid(TARGET))), 99.0);
        let mut c = ctx(100.0);
        c.caster_last_change = Some(&change);
        assert!(is_fighting_caster(&c));
    }

    #[test]
    fn damage_from_the_casters_group_counts() {
        let change = health_change(
            -10.0,
            Some(DamageContributor::Group {
                entity_uid: uid(ALLY),
                group: group::ENEMY,
            }),
            99.0,
        );
        let mut c = ctx(100.0);
        c.caster_group = Some(group::ENEMY);
        c.target_last_change = Some(&change);
        assert!(is_fighting_caster(&c));
    }

    #[test]
    fn damage_from_an_unrelated_party_does_not_count() {
        let change = health_change(-10.0, Some(DamageContributor::Solo(uid(STRANGER))), 99.0);
        let mut c = ctx(100.0);
        c.target_last_change = Some(&change);
        assert!(!is_fighting_caster(&c));
        // Nor does an unattributed change.
        let change = health_change(-10.0, None, 99.0);
        let mut c = ctx(100.0);
        c.target_last_change = Some(&change);
        assert!(!is_fighting_caster(&c));
    }

    #[test]
    fn healing_does_not_count_as_fighting() {
        let change = health_change(10.0, Some(DamageContributor::Solo(uid(CASTER))), 99.0);
        let mut c = ctx(100.0);
        c.target_last_change = Some(&change);
        assert!(!is_fighting_caster(&c));
    }

    /// The predicate reads a real `Health`'s shipped `last_change` field, so it
    /// keeps working off the component the pet behaviour tree already consults.
    #[test]
    fn predicate_reads_a_real_health_component() {
        let health = Health::new(crate::comp::Body::Object(
            crate::comp::object::Body::TrainingDummy,
        ));
        let mut c = ctx(100.0);
        c.target_last_change = Some(&health.last_change);
        // A freshly-built `Health` has no damaging change, so it must not fire.
        assert!(!is_fighting_caster(&c));
    }

    /// The roll is not charm-specific: a fiend with Legendary innate magic
    /// resistance resolves through the same generic function a banishment
    /// save uses, with no charm-shaped types in the signature.
    #[test]
    fn saving_throw_chance_is_callable_for_a_non_charm_effect() {
        let t = CombatTuning::default();
        let target = SaveTargetInfo {
            stats_magic_evasion: 0.0,
            crowd_control_resistance: 0.0,
            stats_magic_resistance: 0.0,
            magic_resist_tier: MagicResistTier::Legendary,
            combat_rating: 10.0,
        };
        let chance = saving_throw_chance(
            &SaveCasterInfo {
                magic_accuracy: 40.0,
            },
            &target,
            false,
            &t,
        );
        // 0.70 base + (40 - 20)*0.015 - 0.50 legendary resist = 0.50
        assert!((chance - 0.50).abs() < 1e-5, "got {chance}");
        assert!(chance >= t.save_hit_floor && chance <= t.save_hit_ceil);
    }
}

#[cfg(test)]
mod removal_cause_tests {
    use super::{RemovalCause, RemovalInfo};

    #[test]
    fn a_kill_awards_the_full_reward_and_counts_as_a_kill() {
        let info = RemovalInfo::killed();
        assert_eq!(info.cause, RemovalCause::Killed);
        assert!((info.reward_fraction - 1.0).abs() < f32::EPSILON);
        assert!(info.cause.counts_as_kill());
    }

    #[test]
    fn a_banishment_is_not_a_kill_and_carries_its_own_fraction() {
        let info = RemovalInfo::banished(0.25);
        assert_eq!(info.cause, RemovalCause::Banished);
        assert!((info.reward_fraction - 0.25).abs() < f32::EPSILON);
        assert!(!info.cause.counts_as_kill());
    }

    /// A malformed RON fraction must not be able to award more than a kill
    /// does, nor a negative amount.
    #[test]
    fn a_banishment_reward_fraction_is_clamped() {
        assert!((RemovalInfo::banished(4.0).reward_fraction - 1.0).abs() < f32::EPSILON);
        assert!((RemovalInfo::banished(-1.0).reward_fraction - 0.0).abs() < f32::EPSILON);
    }

    /// `Default` exists so the ~2 shipped `DestroyEvent` construction sites
    /// stay readable; it must mean "a normal kill".
    #[test]
    fn default_removal_info_is_a_kill() {
        assert_eq!(RemovalInfo::default(), RemovalInfo::killed());
    }
}

#[cfg(test)]
mod trigger_slot_cooldown_tests {
    use super::CombatTuning;
    use crate::assets::{AssetExt, Ron};

    fn shipped() -> CombatTuning {
        Ron::<CombatTuning>::load_expect("common.combat_tuning")
            .read()
            .0
            .clone()
    }

    /// The two anchors the design fixed by hand: a circle-1 spell rests half an
    /// hour, a circle-9 spell rests a day and a half.
    #[test]
    fn the_two_anchors_are_exact() {
        let t = shipped();
        assert_eq!(t.trigger_slot_cooldown(1), 1800.0);
        assert_eq!(t.trigger_slot_cooldown(9), 129_600.0);
    }

    /// Circle 0 is an explicit floor for cantrips, not a curve output, and is
    /// deliberately shorter than extrapolating the curve would give.
    #[test]
    fn circle_zero_is_an_explicit_ten_minute_floor() {
        let t = shipped();
        assert_eq!(t.trigger_slot_cooldown(0), 600.0);
        assert!(t.trigger_slot_cooldown(0) < t.trigger_slot_cooldown(1));
    }

    #[test]
    fn the_table_is_strictly_increasing() {
        let t = shipped();
        for c in 1..10u8 {
            assert!(
                t.trigger_slot_cooldown(c) > t.trigger_slot_cooldown(c - 1),
                "circle {c} is not longer than circle {}",
                c - 1
            );
        }
    }

    /// Circles 1-9 are the rounded exponential `1800 * 72^((C-1)/8)`. Rounding
    /// for legibility is allowed; drifting off the curve is not. Circle 0 is
    /// exempt: it is a floor, not a curve output.
    #[test]
    fn circles_one_to_nine_stay_on_the_exponential_curve() {
        let t = shipped();
        for c in 1..10u8 {
            let exact = 1800.0_f64 * 72.0_f64.powf(f64::from(c - 1) / 8.0);
            let shipped = f64::from(t.trigger_slot_cooldown(c));
            let deviation = (shipped - exact).abs() / exact;
            assert!(
                deviation <= 0.031,
                "circle {c}: shipped {shipped} deviates {:.2}% from the curve value {exact}",
                deviation * 100.0
            );
        }
    }

    /// A circle beyond the catalogue clamps rather than panicking.
    #[test]
    fn an_out_of_range_circle_clamps_to_the_last_entry() {
        let t = shipped();
        assert_eq!(t.trigger_slot_cooldown(10), t.trigger_slot_cooldown(9));
        assert_eq!(t.trigger_slot_cooldown(u8::MAX), t.trigger_slot_cooldown(9));
    }
}

#[cfg(test)]
mod gear_caster_stats_tests {
    use std::sync::Arc;

    use super::{AttunedItems, Inventory, MagicSource, Stats, apply_gear_caster_stats, tool};
    use crate::comp::{
        Body, humanoid,
        inventory::{
            item::{AbilityMap, Item, ItemBase, ItemDef, ItemKind, ItemTag},
            loadout_builder::LoadoutBuilder,
            slot::EquipSlot,
        },
    };

    fn test_body() -> Body { Body::Humanoid(humanoid::Body::random()) }

    fn caster_staff_stats() -> tool::Stats {
        tool::Stats {
            equip_time_secs: 0.4,
            power: 1.0,
            effect_power: 1.5,
            speed: 1.0,
            range: 1.0,
            energy_efficiency: 1.25,
            buff_strength: 1.75,
            cooldown_reduction: 1.0,
        }
    }

    fn tool_item(kind: tool::ToolKind, role: tool::WeaponRole, stats: tool::Stats) -> Item {
        Item::create_test_item_from_kind(ItemKind::Tool(tool::Tool::new(
            kind,
            tool::Hands::Two,
            Some(role),
            stats,
        )))
    }

    fn attunement_required_tool_item(
        kind: tool::ToolKind,
        role: tool::WeaponRole,
        stats: tool::Stats,
    ) -> Item {
        let mut item_def = ItemDef::create_test_itemdef_from_kind(ItemKind::Tool(tool::Tool::new(
            kind,
            tool::Hands::Two,
            Some(role),
            stats,
        )));
        item_def.tags = vec![ItemTag::RequiresAttunement];
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(item_def)),
            Vec::new(),
            &AbilityMap::load().read(),
            &crate::comp::inventory::item::MaterialStatManifest::load().read(),
        )
    }

    fn inventory_with_mainhand(item: Item) -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty().active_mainhand(Some(item)).build(),
        )
    }

    fn empty_inventory() -> Inventory {
        Inventory::with_loadout_humanoid(LoadoutBuilder::empty().build())
    }

    #[test]
    fn no_inventory_leaves_stats_untouched() {
        let mut stats = Stats::empty(test_body());
        apply_gear_caster_stats(&mut stats, None, None);
        assert_eq!(stats.spell_power, 1.0);
        assert_eq!(stats.heal_power, 1.0);
        assert_eq!(stats.energy_regen_modifier, 1.0);
    }

    #[test]
    fn caster_staff_raises_the_three_caster_channels() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert!((stats.spell_power - 1.5).abs() < 1e-5);
        assert!((stats.heal_power - 1.75).abs() < 1e-5);
        assert!((stats.energy_regen_modifier - 1.25).abs() < 1e-5);
        // Deliberately not fed by this path: it already composes with the
        // same tool stat at ability-construction time, so routing it here
        // too would double-apply it for that weapon's own casts.
        assert_eq!(stats.energy_efficiency_modifier, 1.0);
    }

    #[test]
    fn martial_role_weapon_contributes_nothing() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Martial,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert_eq!(stats.spell_power, 1.0);
        assert_eq!(stats.heal_power, 1.0);
        assert_eq!(stats.energy_regen_modifier, 1.0);
    }

    #[test]
    fn every_caster_implement_kind_contributes_not_just_staff_and_sceptre() {
        for kind in [
            tool::ToolKind::Staff,
            tool::ToolKind::Sceptre,
            tool::ToolKind::Tome,
            tool::ToolKind::HolySymbol,
            tool::ToolKind::Focus,
        ] {
            let inv = inventory_with_mainhand(tool_item(
                kind,
                tool::WeaponRole::Caster,
                caster_staff_stats(),
            ));
            let mut stats = Stats::empty(test_body());

            apply_gear_caster_stats(&mut stats, Some(&inv), None);

            assert!(
                (stats.spell_power - 1.5).abs() < 1e-5,
                "{kind:?} caster implement should raise spell_power"
            );
        }
    }

    #[test]
    fn unattuned_requires_attunement_item_contributes_nothing() {
        let inv = inventory_with_mainhand(attunement_required_tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert_eq!(stats.spell_power, 1.0, "unattuned item must stay inert");
    }

    #[test]
    fn attuning_the_slot_activates_the_contribution() {
        let inv = inventory_with_mainhand(attunement_required_tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let attuned = AttunedItems(vec![EquipSlot::ActiveMainhand]);
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), Some(&attuned));

        assert!((stats.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn unequipping_removes_the_contribution_on_the_next_reset() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());
        apply_gear_caster_stats(&mut stats, Some(&inv), None);
        assert!((stats.spell_power - 1.5).abs() < 1e-5);

        // A later tick resets to defaults before re-running the fold; the
        // weapon is no longer equipped by then.
        stats.reset_temp_modifiers();
        apply_gear_caster_stats(&mut stats, Some(&empty_inventory()), None);

        assert_eq!(stats.spell_power, 1.0, "unequipped gear must not linger");
    }

    #[test]
    fn repeated_ticks_with_the_same_gear_do_not_compound() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);
        let first_tick = stats.spell_power;

        // A second tick: reset (mirroring what the per-tick rebuild does
        // before re-running every contribution) then re-fold the SAME gear —
        // the result must be identical, not doubled.
        stats.reset_temp_modifiers();
        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert!((stats.spell_power - first_tick).abs() < 1e-5);
        assert!((stats.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn a_weaponless_pool_caster_still_benefits_from_an_equipped_caster_implement() {
        // A per-ability tool-stat scaling path only ever reads the specific
        // ability hand, and falls back to a neutral multiplier when it finds
        // no tool there. This fold does not consult any "ability hand" at
        // all — it walks every equipped item — so a caster implement in the
        // OFFHAND slot still raises the character's spell_power even though
        // no ability-hand lookup would ever find it there.
        let offhand_item = Item::create_test_item_from_kind(ItemKind::Tool(tool::Tool::new(
            tool::ToolKind::Focus,
            tool::Hands::One,
            Some(tool::WeaponRole::Caster),
            caster_staff_stats(),
        )));
        let inv = Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .active_offhand(Some(offhand_item))
                .build(),
        );
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert!((stats.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn caster_gear_cooldown_reduction_folds_into_the_cooldown_channel() {
        let reduced_stats = tool::Stats {
            cooldown_reduction: 0.6,
            ..caster_staff_stats()
        };
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            reduced_stats,
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert!((stats.cooldown_reduction_modifier - 0.6).abs() < 1e-5);
    }

    #[test]
    fn a_martial_weapon_never_reduces_cooldowns() {
        let reduced_stats = tool::Stats {
            cooldown_reduction: 0.2,
            ..caster_staff_stats()
        };
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Martial,
            reduced_stats,
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert_eq!(stats.cooldown_reduction_modifier, 1.0);
    }

    #[test]
    fn an_unkeyed_caster_item_still_feeds_the_flat_spell_power_channel_only() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Tome,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert!((stats.spell_power - 1.5).abs() < 1e-5);
        assert_eq!(stats.spell_power_by_source, [1.0; MagicSource::NUM_SOURCES]);
    }

    #[test]
    fn a_source_keyed_caster_item_boosts_only_its_own_source() {
        let keyed_item = Item::create_test_item_from_kind(ItemKind::Tool(
            tool::Tool::new(
                tool::ToolKind::Tome,
                tool::Hands::Two,
                Some(tool::WeaponRole::Caster),
                caster_staff_stats(),
            )
            .with_spell_power_source(MagicSource::Divine),
        ));
        let inv = inventory_with_mainhand(keyed_item);
        let mut stats = Stats::empty(test_body());

        apply_gear_caster_stats(&mut stats, Some(&inv), None);

        // The keyed contribution lands ONLY in the Divine slot...
        assert!((stats.spell_power_by_source[MagicSource::Divine.index()] - 1.5).abs() < 1e-5);
        for source in [
            MagicSource::Arcane,
            MagicSource::Primordial,
            MagicSource::Psionic,
            MagicSource::Ki,
        ] {
            assert_eq!(stats.spell_power_by_source[source.index()], 1.0);
        }
        // ...and NOT the flat channel, which stays at identity: a
        // source-keyed item boosts that source specifically, not every
        // magic-source spell.
        assert_eq!(stats.spell_power, 1.0);
    }
}

#[cfg(test)]
mod attack_loadout_walk_tests {
    use super::{
        CombatTuning, Damage, DamageKind, DerivedStats, Inventory, Poise, compute_armor_evasion,
    };
    use crate::{
        comp::{
            Body, Energy, Health, humanoid,
            inventory::{
                item::{
                    Item, ItemKind,
                    armor::{self, Armor, ArmorKind, Protection},
                },
                loadout_builder::LoadoutBuilder,
                loadout_walks, reset_loadout_walks,
            },
        },
        skillset_builder::SkillSetBuilder,
    };

    fn armoured_inventory() -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(Item::create_test_item_from_kind(ItemKind::Armor(
                    Armor::new(
                        ArmorKind::Chest,
                        armor::StatsSource::Direct(armor::Stats {
                            protection: Some(Protection::Normal(12.5)),
                            poise_resilience: Some(Protection::Normal(7.25)),
                            energy_max: Some(13.0),
                            energy_reward: Some(0.35),
                            precision_power: Some(0.17),
                            stealth: Some(0.9),
                            ground_contact: Default::default(),
                        }),
                    ),
                ))))
                .build(),
        )
    }

    /// Resolving a single-target attack against a geared target must walk that
    /// target's loadout **zero** times: every gear-derived number the
    /// resolution path needs (armour protection for damage reduction, armour
    /// weight and shield for evasion, armour resilience for poise, the
    /// precision multiplier) is read off `DerivedStats`, which was folded once
    /// when the gear last changed.
    ///
    /// Before the cache these were three independent loadout walks per damage
    /// instance — the double walk the evasion path used to call out explicitly
    /// in a perf TODO, plus the poise one.
    #[test]
    fn single_attack_reads_target_gear_once() {
        let inventory = armoured_inventory();
        let body = Body::Humanoid(humanoid::Body::random());
        let skill_set = SkillSetBuilder::default().build();
        let msm = crate::comp::inventory::item::MaterialStatManifest::load().cloned();
        let tuning = CombatTuning::default();

        // Building the cache is where the loadout is walked -- the "once" in
        // the name. It happens on gear change, not per attack.
        reset_loadout_walks();
        let derived = DerivedStats::compute(
            Some(&inventory),
            None,
            Some(&skill_set),
            Some(body),
            Some(Health::new(body).base_max()),
            Some(Energy::new(body).base_max()),
            Some(Poise::new(body).base_max()),
            &msm,
        );
        let walks_to_build = loadout_walks();
        assert!(
            walks_to_build >= 3,
            "the rebuild is where the gear is read: expected at least the three walks the \
             resolution path used to do itself, got {walks_to_build}"
        );

        // Now resolve an attack against that cached target. Nothing here may
        // touch the loadout again.
        reset_loadout_walks();

        let damage = Damage {
            kind: DamageKind::Slashing,
            value: 40.0,
        };
        let damage_reduction = Damage::compute_damage_reduction(Some(damage), Some(&derived), None);
        let armor_evasion = compute_armor_evasion(&derived, None, &tuning);
        let poise_damage = Poise::apply_poise_reduction(25.0, Some(&derived), None, None);
        let precision_mult = derived.precision_mult;

        assert_eq!(
            loadout_walks(),
            0,
            "resolving an attack must read the cache, never the loadout"
        );

        // ...and it must still be reading real numbers, not silently
        // defaulting: an armoured target mitigates, evades and resists.
        assert!(damage_reduction > 0.0);
        assert!(armor_evasion < tuning.gear_evasion_cap);
        assert!(poise_damage < 25.0);
        assert!(precision_mult > DerivedStats::DEFAULT_PRECISION_MULT);
    }
}
