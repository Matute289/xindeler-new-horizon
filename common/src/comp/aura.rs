use crate::{
    combat::GroupTarget,
    comp::{
        CharacterClass,
        buff::{BuffCategory, BuffData, BuffKind, BuffSource},
        class::ClassKind,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuraBuffConstructor {
    pub kind: BuffKind,
    pub strength: f32,
    pub duration: Option<Secs>,
    pub category: Option<BuffCategory>,
    #[serde(default)]
    pub pool_split: Option<PoolSplit>,
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
        let aura_kind = AuraKind::Buff {
            kind: self.kind,
            data: BuffData::new(self.strength, self.duration),
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
}
