//! `ItemCondition` — a conditional buff/debuff pair attached to an equipped
//! item, evaluated per tick against the item's current bearer.
//!
//! **Distinct from [`super::inventory::item::ItemRequirements`]**, which is
//! an all-or-nothing *equip* gate: fail it and the item cannot be worn at
//! all. `ItemCondition` is the opposite shape — equippable by anyone, but
//! the [`ItemCondition::when_met`] buff set applies only while `predicate`
//! holds for the CURRENT bearer, and the (optional)
//! [`ItemCondition::when_unmet`] set applies while it does not. This lets a
//! single item read differently depending on who wears it: a grimoire that
//! dreads one kind of bearer and draws another, or a relic that quietly
//! buffs only the alignment/species/undeath it favours.
//!
//! This module ships the mechanism only — no item declares a condition yet,
//! so `Item::condition()` returns `None` for everything currently in the
//! game and the per-tick evaluation in `common/systems/src/buff.rs` never
//! does any work for it.

use super::{
    Body,
    body::humanoid,
    buff::{BuffData, BuffKind},
    ethos::{Ethos, Moral},
};
use serde::{Deserialize, Serialize};

/// The fixed strength/duration every `ItemCondition`-granted buff applies
/// at. Duration `None` = infinite: the buff is removed the instant the
/// predicate flips (via `BuffChange::RemoveByKind` in the per-tick
/// evaluation), never by timing out — the same "no expiry timer, removed
/// explicitly instead" idiom already used for the aura-granted `Shielded`
/// grant (`common/systems/src/aura.rs`).
///
/// v1 has no per-item magnitude — every `ItemCondition` buff/debuff applies
/// at the same fixed strength. Content authors pick `BuffKind`s whose effect
/// reads sensibly at this magnitude; a per-condition strength field is a
/// possible follow-up if a specific item ever needs one.
pub fn item_condition_buff_data() -> BuffData { BuffData::new(1.0, None) }

/// A conditional buff/debuff pair attached to an equipped item.
/// `when_unmet` may be empty (a buff-only item, applying nothing to a
/// bearer who fails the predicate) or populated (a buff+debuff item).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemCondition {
    pub predicate: ConditionPredicate,
    /// Buffs granted to the bearer while `predicate` holds.
    #[serde(default)]
    pub when_met: Vec<BuffKind>,
    /// Buffs granted to the bearer while `predicate` does NOT hold. Empty
    /// for a buff-only item.
    #[serde(default)]
    pub when_unmet: Vec<BuffKind>,
}

impl ItemCondition {
    /// Whether `predicate` holds for this bearer. `ethos` is `None` for
    /// entities with no `Ethos` component (NPCs without moral agency),
    /// which reads as True Neutral — matching `Ethos::default()`.
    pub fn met_by(&self, ethos: Option<&Ethos>, body: &Body) -> bool {
        self.predicate.met_by(ethos, body)
    }

    /// The `BuffKind`s that should be active for this bearer right now:
    /// `when_met` if the predicate holds, `when_unmet` otherwise.
    pub fn active_buffs(&self, ethos: Option<&Ethos>, body: &Body) -> &[BuffKind] {
        if self.met_by(ethos, body) {
            &self.when_met
        } else {
            &self.when_unmet
        }
    }

    /// The complement of [`Self::active_buffs`] — whichever list is NOT
    /// currently applicable, and therefore must not remain granted.
    pub fn inactive_buffs(&self, ethos: Option<&Ethos>, body: &Body) -> &[BuffKind] {
        if self.met_by(ethos, body) {
            &self.when_unmet
        } else {
            &self.when_met
        }
    }
}

/// The bearer conditions an item can be gated on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ConditionPredicate {
    /// Bearer's `Ethos::moral()` is at least this rank on the
    /// `Evil < Neutral < Good` scale (e.g. `Good` demands Good; `Neutral`
    /// admits Good and Neutral; `Evil` admits everyone).
    MoralAtLeast(Moral),
    /// Bearer's `Ethos::moral()` is at most this rank (e.g. `Evil` demands
    /// Evil; `Neutral` admits Evil and Neutral; `Good` admits everyone).
    MoralAtMost(Moral),
    /// Bearer is a humanoid of one of these species. A non-humanoid body
    /// never matches.
    Species(Vec<humanoid::Species>),
    /// Bearer's `Body::creature_kind()` is `CreatureKind::Undead`.
    IsUndead,
}

/// `Evil < Neutral < Good`, for `MoralAtLeast`/`MoralAtMost` ranking. Local
/// to this module — `Moral` itself stays a plain 3-box enum (no `Ord`) since
/// nothing else in the codebase needs to rank it.
fn moral_rank(moral: Moral) -> i8 {
    match moral {
        Moral::Evil => -1,
        Moral::Neutral => 0,
        Moral::Good => 1,
    }
}

impl ConditionPredicate {
    /// Whether this predicate holds for `(ethos, body)`. A missing `Ethos`
    /// component reads as True Neutral (`Ethos::default()`).
    pub fn met_by(&self, ethos: Option<&Ethos>, body: &Body) -> bool {
        match self {
            ConditionPredicate::MoralAtLeast(threshold) => {
                moral_rank(ethos.copied().unwrap_or_default().moral()) >= moral_rank(*threshold)
            },
            ConditionPredicate::MoralAtMost(threshold) => {
                moral_rank(ethos.copied().unwrap_or_default().moral()) <= moral_rank(*threshold)
            },
            ConditionPredicate::Species(species) => match body {
                Body::Humanoid(humanoid_body) => species.contains(&humanoid_body.species),
                _ => false,
            },
            ConditionPredicate::IsUndead => {
                body.creature_kind() == Some(super::creature_type::CreatureKind::Undead)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::{
        body::{biped_large, humanoid},
        buff::BuffKind,
        ethos::{Ethos, Order},
    };

    fn human() -> Body {
        Body::Humanoid(humanoid::Body::random_with(
            &mut rand::rng(),
            &humanoid::Species::Human,
        ))
    }

    fn draugr() -> Body {
        Body::Humanoid(humanoid::Body::random_with(
            &mut rand::rng(),
            &humanoid::Species::Draugr,
        ))
    }

    fn skeleton_undead() -> Body {
        Body::BipedLarge(biped_large::Body {
            species: biped_large::Species::Cursekeeper,
            body_type: biped_large::BodyType::Male,
        })
    }

    // ---- ConditionPredicate::met_by ----

    #[test]
    fn moral_at_least_good_admits_only_good() {
        let good = Ethos::from_box(Order::Neutral, Moral::Good);
        let neutral = Ethos::default();
        let evil = Ethos::from_box(Order::Neutral, Moral::Evil);
        let p = ConditionPredicate::MoralAtLeast(Moral::Good);
        assert!(p.met_by(Some(&good), &human()));
        assert!(!p.met_by(Some(&neutral), &human()));
        assert!(!p.met_by(Some(&evil), &human()));
    }

    #[test]
    fn moral_at_least_evil_admits_everyone() {
        let good = Ethos::from_box(Order::Neutral, Moral::Good);
        let p = ConditionPredicate::MoralAtLeast(Moral::Evil);
        assert!(p.met_by(Some(&good), &human()));
        assert!(p.met_by(None, &human()));
    }

    #[test]
    fn moral_at_most_evil_admits_only_evil() {
        let evil = Ethos::from_box(Order::Neutral, Moral::Evil);
        let neutral = Ethos::default();
        let good = Ethos::from_box(Order::Neutral, Moral::Good);
        let p = ConditionPredicate::MoralAtMost(Moral::Evil);
        assert!(p.met_by(Some(&evil), &human()));
        assert!(!p.met_by(Some(&neutral), &human()));
        assert!(!p.met_by(Some(&good), &human()));
    }

    #[test]
    fn moral_predicate_with_no_ethos_reads_as_true_neutral() {
        // No Ethos component (e.g. a beast) reads as Ethos::default() == Neutral.
        assert!(ConditionPredicate::MoralAtLeast(Moral::Neutral).met_by(None, &human()));
        assert!(ConditionPredicate::MoralAtMost(Moral::Neutral).met_by(None, &human()));
        assert!(!ConditionPredicate::MoralAtLeast(Moral::Good).met_by(None, &human()));
        assert!(!ConditionPredicate::MoralAtMost(Moral::Evil).met_by(None, &human()));
    }

    #[test]
    fn species_predicate_matches_only_the_listed_humanoid_species() {
        let p = ConditionPredicate::Species(vec![humanoid::Species::Draugr]);
        assert!(p.met_by(None, &draugr()));
        assert!(!p.met_by(None, &human()));
    }

    #[test]
    fn species_predicate_never_matches_a_non_humanoid_body() {
        let p = ConditionPredicate::Species(vec![humanoid::Species::Human]);
        assert!(!p.met_by(None, &skeleton_undead()));
    }

    #[test]
    fn is_undead_predicate() {
        assert!(ConditionPredicate::IsUndead.met_by(None, &skeleton_undead()));
        assert!(!ConditionPredicate::IsUndead.met_by(None, &human()));
        // Draugr is a living lineage, not Undead (see `Body::creature_kind`'s
        // own doc comment) — a good regression pin for this predicate.
        assert!(!ConditionPredicate::IsUndead.met_by(None, &draugr()));
    }

    // ---- ItemCondition::active_buffs / inactive_buffs ----

    #[test]
    fn buff_only_condition_grants_nothing_when_unmet() {
        // `when_unmet` empty — the shape a buff-only item needs.
        let condition = ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Draugr]),
            when_met: vec![BuffKind::Shielded],
            when_unmet: vec![],
        };
        assert_eq!(condition.active_buffs(None, &draugr()), &[
            BuffKind::Shielded
        ]);
        assert!(condition.active_buffs(None, &human()).is_empty());
        // The "inactive" side for a met bearer is the (empty) when_unmet list.
        assert!(condition.inactive_buffs(None, &draugr()).is_empty());
        assert_eq!(condition.inactive_buffs(None, &human()), &[
            BuffKind::Shielded
        ]);
    }

    #[test]
    fn buff_and_debuff_condition_is_bidirectional() {
        // Both shapes populated — a buff-one-way, debuff-the-other item.
        let condition = ItemCondition {
            predicate: ConditionPredicate::MoralAtMost(Moral::Evil),
            when_met: vec![BuffKind::Rooted],   // "drawn to it"
            when_unmet: vec![BuffKind::Frozen], // "dread"
        };
        let evil = Ethos::from_box(Order::Neutral, Moral::Evil);
        let good = Ethos::from_box(Order::Neutral, Moral::Good);
        assert_eq!(condition.active_buffs(Some(&evil), &human()), &[
            BuffKind::Rooted
        ]);
        assert_eq!(condition.active_buffs(Some(&good), &human()), &[
            BuffKind::Frozen
        ]);
        assert_eq!(condition.inactive_buffs(Some(&evil), &human()), &[
            BuffKind::Frozen
        ]);
        assert_eq!(condition.inactive_buffs(Some(&good), &human()), &[
            BuffKind::Rooted
        ]);
    }

    #[test]
    fn item_condition_buff_data_is_infinite_duration() {
        let data = item_condition_buff_data();
        assert_eq!(data.duration, None);
    }
}
