use crate::{
    combat::{GroupTarget, HealthTier},
    comp::{
        CharacterClass,
        buff::{BuffCategory, BuffData, BuffKind, BuffSource, MiscBuffData},
        class::ClassKind,
        creature_type::CreatureKind,
        skillset::MAX_CHARACTER_LEVEL,
        tool::ToolKind,
    },
    resources::{Secs, Time},
    uid::Uid,
};
use serde::{Deserialize, Serialize};
use slotmap::{SlotMap, new_key_type};
use specs::{Component, DerefFlaggedStorage, VecStorage};
use std::collections::{HashMap, HashSet};

new_key_type! { pub struct AuraKey; }

/// AuraKind is what kind of effect an aura applies
/// Currently only buffs are implemented
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AuraKind {
    /// The Buff kind is (surprise!) a buff :D
    Buff {
        kind: BuffKind,
        data: BuffData,
        category: Option<BuffCategory>,
        source: BuffSource,
        /// When set, `data.strength` is ignored and each activation instead
        /// divides `PoolSplit::total` evenly among however many eligible
        /// targets are found (nearest to the aura's origin first, capped at
        /// `PoolSplit::max_targets`) — a shared pool, not a flat per-target
        /// amount. Boxed: this variant sits inside `AuraChange::Add(Aura)`,
        /// and `PoolSplit`'s `Vec<ClassKind>` would otherwise bloat every
        /// `Aura`, most of which never use it.
        pool_split: Option<Box<PoolSplit>>,
    },
    /// Enables free-for-all friendly-fire. Includes group members, and pets.
    /// BattleMode checks still apply.
    FriendlyFire,
    /// Ignores the [`crate::resources::BattleMode`] of all entities
    /// affected by this aura, only player entities will be affected by this
    /// aura.
    ForcePvP,
    /// Selects up to `max_targets` nearest eligible targets (same
    /// group/range eligibility as `Buff`), and for each one independently —
    /// not shared like `Buff::pool_split` — resolves the single worst tier of
    /// `tiers` that target's OWN current health qualifies for, applying that
    /// tier's effect to just that target. Reuses `combat::HealthTier`, the
    /// same tiered-ladder shape `CombatEffect::TieredHealthEffect` uses for
    /// single-target attacks, so spell authors write one kind of tier table
    /// regardless of whether the effect comes from an attack or an aura.
    /// Built for `power_word_divine_word`'s capped-area judgment and meant to
    /// be reused by any future capped-nearest-N, per-target-resolved spell
    /// (e.g. a `Prismatic Spray`-style effect).
    ///
    /// Semantic difference from the attack-pipeline version: there is no
    /// underlying attack damage here, so `TierEffect::AdditionalDamage(v)` is
    /// interpreted as a flat health change of `v` (not a multiplier), and
    /// `CombatBuffStrength::DamageFraction` in a `TierEffect::Buff` resolves
    /// against `0.0` damage (effectively always `0`) — tier tables meant for
    /// aura use should stick to `CombatBuffStrength::Value`.
    ///
    /// `banishment`, when set, is checked *before* the tier ladder for each
    /// selected target: a target whose creature kind it matches is resolved
    /// **exclusively** via banishment and never enters the tier ladder at
    /// all, at any HP — it is one or the other, never both. Every other
    /// creature kind is unaffected by `banishment` and resolves the tier
    /// ladder exactly as if it were absent. Boxed for the same reason
    /// `PoolSplit` is: this variant lives inside `AuraChange::Add(Aura)`,
    /// and the `Vec<CreatureKind>` would otherwise bloat every `Aura`.
    TieredHealthEffect {
        tiers: Vec<HealthTier>,
        max_targets: usize,
        source: BuffSource,
        banishment: Option<Box<BanishmentEffect>>,
    },
    /* TODO: Implement other effects here. Things to think about
     * are terrain/sprite effects, collision and physics, and
     * environmental conditions like temperature and humidity
     * Multiple auras can be given to an entity. */
}

/// Variants of [`AuraKind`] without data
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuraKindVariant {
    Buff,
    FriendlyFire,
    ForcePvP,
    TieredHealthEffect,
}

/// Aura
/// Applies a buff to entities in the radius if meeting
/// conditions set forth in the aura system.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Aura {
    /// The kind of aura applied
    pub aura_kind: AuraKind,
    /// The radius of the aura
    pub radius: f32,
    // None corresponds to an indefinite aura
    pub end_time: Option<Time>,
    /* TODO: Add functionality for fading or a gradient */
    /// Used to filter which entities this aura will apply to. For example,
    /// globally neutral auras which affect all entities will have the type
    /// `AuraTarget::All`. Whereas auras which only affect a player's party
    /// members will have the type `AuraTarget::GroupOf`.
    pub target: AuraTarget,
    /// Contains data about the original state of the aura that does not change
    /// over time
    pub data: AuraData,
    //Specifies if there should be a persistent visual effect during the aura
    pub frontend_specifier: Option<Specifier>,
}

/// Information about whether aura addition or removal was requested.
/// This to implement "on_add" and "on_remove" hooks for auras
#[derive(Clone, Debug)]
pub enum AuraChange {
    /// Adds this aura
    Add(Aura),
    /// Removes auras of these indices
    RemoveByKey(Vec<AuraKey>),
    EnterAura(Uid, AuraKey, AuraKindVariant),
    ExitAura(Uid, AuraKey, AuraKindVariant),
}

/// Used by the aura system to filter entities when applying an effect.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub enum AuraTarget {
    /// Targets the group of the entity specified by the `Uid`. This is useful
    /// for auras which should only affect a player's party.
    GroupOf(Uid),
    /// Targets everyone not in the group of the entity specified by the `Uid`.
    /// This is useful for auras which should only affect a player's
    /// enemies.
    NotGroupOf(Uid),
    /// Targets all entities. This is for auras which are global or neutral.
    All,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Specifier {
    WardingAura,
    HealingAura,
    Frozen,
    FieryAura,
}

impl From<(Option<GroupTarget>, Option<&Uid>)> for AuraTarget {
    fn from((target, uid): (Option<GroupTarget>, Option<&Uid>)) -> Self {
        match (target, uid) {
            (Some(GroupTarget::InGroup), Some(uid)) => Self::GroupOf(*uid),
            (Some(GroupTarget::OutOfGroup), Some(uid)) => Self::NotGroupOf(*uid),
            (Some(GroupTarget::All), _) => Self::All,
            _ => Self::All,
        }
    }
}

impl AsRef<AuraKindVariant> for AuraKind {
    fn as_ref(&self) -> &AuraKindVariant {
        match self {
            AuraKind::Buff { .. } => &AuraKindVariant::Buff,
            AuraKind::FriendlyFire => &AuraKindVariant::FriendlyFire,
            AuraKind::ForcePvP => &AuraKindVariant::ForcePvP,
            AuraKind::TieredHealthEffect { .. } => &AuraKindVariant::TieredHealthEffect,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuraData {
    pub duration: Option<Secs>,
}

impl AuraData {
    #[must_use]
    fn new(duration: Option<Secs>) -> Self { Self { duration } }
}

impl Aura {
    /// Creates a new Aura to be assigned to an entity
    pub fn new(
        aura_kind: AuraKind,
        radius: f32,
        duration: Option<Secs>,
        target: AuraTarget,
        time: Time,
        frontend_specifier: Option<Specifier>,
    ) -> Self {
        Self {
            aura_kind,
            radius,
            end_time: duration.map(|dur| Time(time.0 + dur.0)),
            target,
            data: AuraData::new(duration),
            frontend_specifier,
        }
    }
}

/// Component holding all auras emitted by an entity.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Auras {
    pub auras: SlotMap<AuraKey, Aura>,
}

impl Auras {
    pub fn new(auras: Vec<Aura>) -> Self {
        let mut auras_comp: SlotMap<AuraKey, Aura> = SlotMap::with_key();
        for aura in auras {
            auras_comp.insert(aura);
        }
        Self { auras: auras_comp }
    }

    pub fn insert(&mut self, aura: Aura) { self.auras.insert(aura); }

    pub fn remove(&mut self, key: AuraKey) { self.auras.remove(key); }
}

/// A shared pool divided evenly among however many eligible targets an aura
/// activation finds (nearest to the aura's origin first, capped at
/// `max_targets`), instead of a flat per-target buff strength. Used by
/// abilities like `power_word_fortify`, whose temporary-HP pool shrinks or
/// grows per recipient depending on how many allies are actually present.
///
/// The pool's own total scales linearly with the *applying* entity's caster
/// level between `unlock_level` and `MAX_CHARACTER_LEVEL`, same shape as
/// `combat::CasterLevelFailChance` -- and the same class-aware resolution:
/// when `source_classes` is non-empty, the level used is the max among those
/// classes the caster actually holds (post-multiclass correctness), not the
/// raw character level.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PoolSplit {
    pub unlock_level: u16,
    pub value_at_unlock: f32,
    pub value_at_max_level: f32,
    #[serde(default)]
    pub source_classes: Vec<ClassKind>,
    pub max_targets: usize,
}

impl PoolSplit {
    /// Resolves the actual pool total for this activation: linear
    /// interpolation between `value_at_unlock` and `value_at_max_level`
    /// against the caster's own class level (or raw character level when
    /// `source_classes` is empty or none are held).
    pub fn resolved_total(
        &self,
        character_level: Option<u16>,
        character_class: Option<&CharacterClass>,
    ) -> f32 {
        let effective_level = if self.source_classes.is_empty() {
            character_level
        } else {
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
        };
        let Some(level) = effective_level else {
            return self.value_at_unlock;
        };
        if level <= self.unlock_level {
            return self.value_at_unlock;
        }
        let span = f32::from(MAX_CHARACTER_LEVEL.saturating_sub(self.unlock_level)).max(1.0);
        let progress = (f32::from(level.saturating_sub(self.unlock_level)) / span).min(1.0);
        self.value_at_unlock + (self.value_at_max_level - self.value_at_unlock) * progress
    }
}

/// A saving-throw-resisted banishment that pre-empts a
/// [`AuraKind::TieredHealthEffect`] ladder for any creature kind it applies
/// to.
///
/// Exclusive of the tier ladder on purpose: a target whose `CreatureKind`
/// this effect applies to is resolved solely by the banishment check,
/// regardless of its current hit points — it never enters the HP-tier
/// ladder at all, at any HP. A celestial above every HP threshold is still
/// banished; a fiend that would otherwise land in the Blinded tier is
/// banished instead of Blinded, never both. Every number here is authored
/// in the ability's RON, not hardcoded — this type carries no balance
/// defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BanishmentEffect {
    /// Only creatures whose `Stats::creature_kind` is in this list can be
    /// banished at all.
    pub creature_kinds: Vec<CreatureKind>,
    /// Lower bound of the return delay, in hours of **real** time.
    pub min_return_hours: f64,
    /// Upper bound of the return delay, in hours of **real** time. Drawn
    /// uniformly against `min_return_hours`.
    pub max_return_hours: f64,
    /// Fraction of the normal XP and loot a banishment awards, forwarded into
    /// `combat::RemovalInfo::banished`.
    pub reward_fraction: f32,
}

impl BanishmentEffect {
    /// Whether a target of this creature kind is banishable. `None` (a body
    /// that is not a creature at all) never is.
    pub fn applies_to(&self, kind: Option<CreatureKind>) -> bool {
        kind.is_some_and(|kind| self.creature_kinds.contains(&kind))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuraBuffConstructor {
    pub kind: BuffKind,
    pub strength: f32,
    pub duration: Option<Secs>,
    pub category: Option<BuffCategory>,
    #[serde(default)]
    pub pool_split: Option<PoolSplit>,
    /// Extra payload some `BuffKind`s need beyond strength/duration — e.g.
    /// `BuffKind::Disguised` requires a `MiscBuffData::Body` to know which
    /// apparent body to render for observers. `#[serde(default)]` so every
    /// existing shipped aura RON (`crusaders_mantle`, `bless`, `wardingaura`,
    /// `healingaura`, etc.) stays byte-unchanged and continues to parse with
    /// `misc_data: None`, the same additive pattern `pool_split` above
    /// already established.
    ///
    /// 🔴 **Sync scope warning for future authors.** This rides on `Auras`
    /// (`SyncFrom::AnyEntity`), so while the aura exists on the caster,
    /// `misc_data` is visible to every nearby client, not just whoever the
    /// aura ends up buffing — the same class of leak
    /// `MiscBuffData::RemoteSense`'s own doc comment already warns about,
    /// and for the same reason that type deliberately carries no
    /// identity/position fields. Harmless for today's only consumer
    /// (`seeming`'s disguise body is a fixed, public template, not a
    /// secret), but a *future* aura-delivered buff whose payload must stay
    /// hidden from bystanders must not go through this field unmodified.
    #[serde(default)]
    pub misc_data: Option<MiscBuffData>,
}

impl AuraBuffConstructor {
    pub fn to_aura(
        &self,
        entity_info: (&Uid, Option<ToolKind>),
        radius: f32,
        duration: Option<Secs>,
        target: AuraTarget,
        time: Time,
        frontend_specifier: Option<Specifier>,
    ) -> Aura {
        let mut data = BuffData::new(self.strength, self.duration);
        if let Some(misc_data) = self.misc_data {
            data = data.with_misc_data(misc_data);
        }
        let aura_kind = AuraKind::Buff {
            kind: self.kind,
            data,
            category: self.category.clone(),
            source: BuffSource::Character {
                by: *entity_info.0,
                tool_kind: entity_info.1,
            },
            pool_split: self.pool_split.clone().map(Box::new),
        };
        Aura::new(
            aura_kind,
            radius,
            duration,
            target,
            time,
            frontend_specifier,
        )
    }
}

/// RON-facing config for [`AuraKind::TieredHealthEffect`]. Kept separate from
/// `AuraBuffConstructor` (rather than adding an optional field there) because
/// it builds a fundamentally different `AuraKind` variant, not a `Buff` with
/// extra knobs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TieredHealthEffectConstructor {
    pub tiers: Vec<HealthTier>,
    pub max_targets: usize,
    #[serde(default)]
    pub banishment: Option<BanishmentEffect>,
}

impl TieredHealthEffectConstructor {
    pub fn to_aura(
        &self,
        entity_info: (&Uid, Option<ToolKind>),
        radius: f32,
        duration: Option<Secs>,
        target: AuraTarget,
        time: Time,
        frontend_specifier: Option<Specifier>,
    ) -> Aura {
        let aura_kind = AuraKind::TieredHealthEffect {
            tiers: self.tiers.clone(),
            max_targets: self.max_targets,
            source: BuffSource::Character {
                by: *entity_info.0,
                tool_kind: entity_info.1,
            },
            banishment: self.banishment.clone().map(Box::new),
        };
        Aura::new(
            aura_kind,
            radius,
            duration,
            target,
            time,
            frontend_specifier,
        )
    }
}

/// Auras affecting an entity
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnteredAuras {
    /// [`AuraKey`] is local to each [`Auras`] component, therefore we also
    /// store the [`Uid`] of the aura caster
    pub auras: HashMap<AuraKindVariant, HashSet<(Uid, AuraKey)>>,
}

impl EnteredAuras {
    pub fn flatten(&self) -> impl Iterator<Item = (Uid, AuraKey)> + '_ {
        self.auras.values().flat_map(|i| i.iter().copied())
    }
}

impl Component for Auras {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

impl Component for EnteredAuras {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::creature_type::CreatureKind;

    fn pool_split() -> PoolSplit {
        PoolSplit {
            unlock_level: 42,
            value_at_unlock: 120.0,
            value_at_max_level: 240.0,
            source_classes: vec![
                ClassKind::Mage,
                ClassKind::Sorcerer,
                ClassKind::Warlock,
                ClassKind::Bard,
            ],
            max_targets: 6,
        }
    }

    #[test]
    fn resolved_total_clamps_at_unlock_and_max_level() {
        let split = pool_split();
        assert_eq!(split.resolved_total(Some(1), None), 120.0);
        assert_eq!(split.resolved_total(Some(42), None), 120.0);
        assert_eq!(split.resolved_total(Some(60), None), 240.0);
        // Above MAX_CHARACTER_LEVEL should still clamp, not extrapolate.
        assert_eq!(split.resolved_total(Some(200), None), 240.0);
    }

    #[test]
    fn resolved_total_interpolates_linearly_between_unlock_and_max() {
        let split = pool_split();
        // Halfway from 42 to 60 (level 51) should be halfway from 120 to 240.
        assert!((split.resolved_total(Some(51), None) - 180.0).abs() < 0.5);
    }

    #[test]
    fn resolved_total_uses_the_eligible_class_own_level_for_multiclass() {
        let split = pool_split();
        // Warrior (ineligible) primary, Warlock (eligible) secondary at 42 of
        // the 60 total -- must use 42 (Warlock's own level), not 60 or 18.
        let character_class = CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Warlock),
            secondary_level: 42,
            future_levels_to_secondary: false,
        };
        assert_eq!(
            split.resolved_total(Some(60), Some(&character_class)),
            120.0
        );
    }

    #[test]
    fn resolved_total_falls_back_to_character_level_with_no_source_classes() {
        let split = PoolSplit {
            source_classes: vec![],
            ..pool_split()
        };
        assert_eq!(split.resolved_total(Some(60), None), 240.0);
    }

    fn divine_word_banishment() -> BanishmentEffect {
        BanishmentEffect {
            creature_kinds: vec![
                CreatureKind::Celestial,
                CreatureKind::Elemental,
                CreatureKind::Fey,
                CreatureKind::Fiend,
            ],
            min_return_hours: 24.0,
            max_return_hours: 168.0,
            reward_fraction: 0.25,
        }
    }

    #[test]
    fn banishment_applies_only_to_the_authored_creature_kinds() {
        let effect = divine_word_banishment();
        assert!(effect.applies_to(Some(CreatureKind::Celestial)));
        assert!(effect.applies_to(Some(CreatureKind::Elemental)));
        assert!(effect.applies_to(Some(CreatureKind::Fey)));
        assert!(effect.applies_to(Some(CreatureKind::Fiend)));
        assert!(!effect.applies_to(Some(CreatureKind::Beast)));
        assert!(!effect.applies_to(Some(CreatureKind::Undead)));
        assert!(!effect.applies_to(Some(CreatureKind::Dragon)));
    }

    /// A body with no creature kind at all (arrows, ships, most objects) is
    /// never banishable, and neither is a humanoid — which is the whole
    /// reason no player can be banished.
    #[test]
    fn banishment_never_applies_to_unclassified_bodies_or_humanoids() {
        let effect = divine_word_banishment();
        assert!(!effect.applies_to(None));
        assert!(!effect.applies_to(Some(CreatureKind::Humanoid)));
    }

    /// Pins the authored tier-4 + banishment content, so a future edit to the
    /// RON that drops the death tier or the banishment block fails here
    /// instead of silently shipping a 3-tier spell again.
    #[test]
    fn divine_words_ron_carries_four_tiers_and_a_banishment() {
        use crate::{
            assets::{AssetExt, Ron},
            comp::ability::CharacterAbility,
        };

        // Same load shape `EntityInfo::with_asset_expect` uses
        // (`common/src/generation.rs:312`): `load_expect_cloned` yields a
        // `Ron<T>` wrapper, `into_inner()` unwraps it.
        let ability: CharacterAbility =
            Ron::load_expect_cloned("common.abilities.spells.divine.power_word_divine_word")
                .into_inner();
        let CharacterAbility::BasicAura {
            tiered_health_effects,
            meta,
            ..
        } = &ability
        else {
            panic!("power_word_divine_word is not a BasicAura");
        };
        // Heavier than Pain's 60s despite sharing its spell level (7) --
        // Divine Word's 4-target reach plus the death/banishment branch
        // weighs more, so it gets its own 75s. min_level 42 unlock stays.
        assert_eq!(meta.requirements.min_level, Some(42));
        assert_eq!(meta.cooldown, Some(75.0));
        let effect = tiered_health_effects
            .first()
            .expect("no tiered health effect authored");

        // Most severe first — resolution takes the first match, it does not sort.
        let thresholds = effect
            .tiers
            .iter()
            .map(|t| t.max_current_health)
            .collect::<Vec<_>>();
        assert_eq!(thresholds, vec![35.0, 45.0, 55.0, 65.0]);

        // Tier 4's instant death is the only permanent result in the whole
        // ladder, so — unlike tiers 1-3, which have no `requirement` at all —
        // it must carry the same caster-side `CasterLevelRoll` margin
        // power_word_kill/pain/stun already give their own permanent results.
        use crate::combat::CombatRequirement;
        let tier4 = &effect.tiers[0];
        let requirement = tier4
            .requirement
            .as_ref()
            .expect("tier 4's instant death must be gated by a CasterLevelRoll");
        match requirement {
            CombatRequirement::CasterLevelRoll(curve) => {
                assert_eq!(curve.unlock_level, 42);
                assert!((curve.fail_chance_at_unlock - 0.25).abs() < f32::EPSILON);
                assert!((curve.fail_chance_at_max_level - 0.05).abs() < f32::EPSILON);
                assert_eq!(curve.source_classes, vec![ClassKind::Cleric]);
            },
            other => panic!("tier 4's requirement should be a CasterLevelRoll, got {other:?}"),
        }

        // Tier 3 (the hour-long, `dur_secs: 3600` Paralyzed result) is the
        // worst temporary outcome in the ladder, so it now carries the same
        // caster-side margin as tier 4's instant death instead of applying
        // unconditionally.
        let tier3 = &effect.tiers[1];
        assert_eq!(tier3.max_current_health, 45.0);
        let tier3_requirement = tier3
            .requirement
            .as_ref()
            .expect("tier 3's hour-long paralysis must be gated by a CasterLevelRoll");
        match tier3_requirement {
            CombatRequirement::CasterLevelRoll(curve) => {
                assert_eq!(curve.unlock_level, 42);
                assert!((curve.fail_chance_at_unlock - 0.25).abs() < f32::EPSILON);
                assert!((curve.fail_chance_at_max_level - 0.05).abs() < f32::EPSILON);
                assert_eq!(curve.source_classes, vec![ClassKind::Cleric]);
            },
            other => panic!("tier 3's requirement should be a CasterLevelRoll, got {other:?}"),
        }
        assert!(
            effect.tiers[2..].iter().all(|t| t.requirement.is_none()),
            "only tiers 3 and 4 (the two most severe results) are gated -- tiers 1-2 (Blinded, \
             the short Paralyzed) are unaffected"
        );

        let banishment = effect
            .banishment
            .as_ref()
            .expect("no banishment authored on divine word");
        assert_eq!(banishment.creature_kinds, vec![
            CreatureKind::Celestial,
            CreatureKind::Elemental,
            CreatureKind::Fey,
            CreatureKind::Fiend,
        ]);
        assert!((banishment.min_return_hours - 24.0).abs() < f64::EPSILON);
        assert!((banishment.max_return_hours - 168.0).abs() < f64::EPSILON);
        assert!((banishment.reward_fraction - 0.25).abs() < f32::EPSILON);
    }

    /// Regression guard for adding `AuraBuffConstructor::misc_data`: every
    /// aura RON shipped before that field existed must still parse
    /// byte-identically and come back with `misc_data: None`, proving the
    /// `#[serde(default)]` on the new field is doing its job rather than
    /// just being assumed to.
    #[test]
    fn pre_existing_aura_rons_still_parse_with_no_misc_data() {
        use crate::{
            assets::{AssetExt, Ron},
            comp::ability::CharacterAbility,
        };

        for asset in [
            "common.abilities.spells.gravesong.crusaders_mantle",
            "common.abilities.spells.gravesong.bless",
            "common.abilities.sceptre.wardingaura",
            "common.abilities.sceptre.healingaura",
        ] {
            let ability: CharacterAbility = Ron::load_expect_cloned(asset).into_inner();
            let CharacterAbility::BasicAura { auras, .. } = &ability else {
                panic!("{asset} is not a BasicAura");
            };
            for aura in auras {
                assert_eq!(
                    aura.misc_data, None,
                    "{asset}'s aura must still carry no misc_data after the field was added"
                );
            }
        }
    }

    /// Pins `seeming.ron`'s use of the new `misc_data` field: it must carry
    /// the `Disguised` buff's required `MiscBuffData::Body` payload all the
    /// way through `AuraBuffConstructor`, proving the field threads through
    /// rather than only compiling.
    #[test]
    fn seeming_ron_carries_a_disguise_body_via_misc_data() {
        use crate::{
            assets::{AssetExt, Ron},
            comp::{
                ability::CharacterAbility,
                body::{Body, humanoid},
                buff::{BuffKind, MiscBuffData},
            },
        };

        let ability: CharacterAbility =
            Ron::load_expect_cloned("common.abilities.spells.illusion.seeming").into_inner();
        let CharacterAbility::BasicAura {
            auras,
            aura_duration,
            ..
        } = &ability
        else {
            panic!("seeming is not a BasicAura");
        };
        // Instant compendium duration -> a single brief pulse, same idiom
        // power_word_fortify.ron/power_word_divine_word.ron use.
        assert_eq!(*aura_duration, Some(Secs(0.5)));

        let disguise = auras
            .iter()
            .find(|a| a.kind == BuffKind::Disguised)
            .expect("seeming must author a Disguised aura");
        // The long, non-concentration fixed duration the sheet calls for.
        assert_eq!(disguise.duration, Some(Secs(3600.0)));
        match disguise.misc_data {
            Some(MiscBuffData::Body(Body::Humanoid(humanoid::Body { species, .. }))) => {
                assert_eq!(species, humanoid::Species::Human);
            },
            other => panic!("seeming's Disguised aura must carry a humanoid Body, got {other:?}"),
        }
    }
}
