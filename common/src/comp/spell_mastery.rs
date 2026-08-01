//! Per-`MagicSource` mastery progress.
//!
//! A character accumulates source-XP whenever a spell of that source lands a
//! real effect (damage, buff, or heal) on another entity. Mastery is
//! entirely orthogonal to [`crate::comp::ability::SpellGate`]: mastery gates
//! which spell LEVELS of a source may be copied into a character's
//! [`crate::comp::spell::SpellCompendium`]-backed spellbook, while
//! `SpellGate` gates which spell levels a character's class allows them to
//! CAST. Neither widens the other — see the `full_mastery_does_not_bypass_*`
//! regression test below.
use crate::comp::ability::MagicSource;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};

/// Source-XP required for 100% mastery in one source. 23.0% of a full
/// level-1->60 career (870,250 lifetime XP, see
/// `crate::comp::skillset::total_exp_for_level`) at perfect attribution --
/// every kill 100% by that source, every target at or above the caster's
/// level. Realistically `damage_share x W` lands ~0.3-0.6, putting one source
/// at 38-77% of a full career. Also imposes a free implicit level floor: no
/// character below ~level 29 can reach 100% in any source, by arithmetic
/// (`250 * (29-1)^2 ~= 196_000 < 200_000 <= 250 * (30-1)^2`), with no
/// explicit rule -- see the floor test below for the guard that keeps this
/// true if either constant is retuned.
pub const MASTERY_XP_FULL: u32 = 200_000;

/// Credit multiplier for a spell that lands a buff/heal/utility effect on
/// another entity rather than damage.
pub const NON_DAMAGE_WEIGHT: f32 = 0.25;

/// `Mage(Polyglot)` per-rank mastery-gain bonus. Max rank 3
/// (`skill_max_levels.ron`), so the cap is +24%.
pub const POLYGLOT_BONUS_PER_RANK: f32 = 0.08;

/// Accumulated source-XP per [`MagicSource`]. `Arcane`'s slot is never
/// written (the Mage knows it by default, `pct(Arcane)` always answers
/// `1.0`) but exists so the array is indexable by the enum without a special
/// case anywhere that reads it.
///
/// XP, not a stored float percentage: the same currency
/// `entity_manipulation::handle_exp_gain` already uses, monotonic and
/// additive (so it cannot desync the way a derived float can), and
/// re-tuning [`MASTERY_XP_FULL`] later re-scales every existing character
/// consistently. Percent is always derived for display, never stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpellMastery {
    source_xp: [u32; MagicSource::COUNT],
}

impl SpellMastery {
    /// This source's mastery, in `0.0..=1.0`. `Arcane` is always known, so
    /// this unconditionally answers `1.0` for it regardless of
    /// `source_xp[Arcane]` (which [`grant_source_mastery`] and
    /// [`Self::set_source_xp`] both refuse to write anyway).
    pub fn pct(&self, source: MagicSource) -> f32 {
        if matches!(source, MagicSource::Arcane) {
            return 1.0;
        }
        (self.source_xp(source) as f32 / MASTERY_XP_FULL as f32).min(1.0)
    }

    /// Raw accumulated source-XP for `source`.
    pub fn source_xp(&self, source: MagicSource) -> u32 { self.source_xp[source as usize] }

    /// Set `source`'s raw source-XP directly, bypassing
    /// [`grant_source_mastery`]'s weighting. For persistence load and the
    /// `/set_mastery` admin test tool only -- gameplay XP gain always goes
    /// through [`grant_source_mastery`]. A no-op for `Arcane`, same guard as
    /// that function.
    pub fn set_source_xp(&mut self, source: MagicSource, xp: u32) {
        if matches!(source, MagicSource::Arcane) {
            return;
        }
        self.source_xp[source as usize] = xp;
    }
}

impl Component for SpellMastery {
    type Storage = DerefFlaggedStorage<Self, specs::DenseVecStorage<Self>>;
}

/// The ONLY way mastery is ever credited. Combat (damage and non-damage
/// landed-effect paths) is the first caller; a future quest/story system is
/// the second, without touching combat again. A no-op for `Arcane` -- it is
/// known by default and never accrues XP.
///
/// `base_xp` is the source-specific share of whatever reward is being spent
/// (e.g. `exp_reward * damage_share(source)`); `weight` folds together
/// [`level_delta_weight`] and any multiplicative bonus (e.g. the
/// `Mage(Polyglot)` rate). The product is rounded and saturating-added to the
/// source's running total, so a negative, `NaN`, or overflowing product
/// degrades to "no gain" / "cap at `u32::MAX`" rather than panicking.
pub fn grant_source_mastery(
    mastery: &mut SpellMastery,
    source: MagicSource,
    base_xp: f32,
    weight: f32,
) {
    if matches!(source, MagicSource::Arcane) {
        return;
    }
    let gain = (base_xp * weight).max(0.0).round() as u32;
    let idx = source as usize;
    mastery.source_xp[idx] = mastery.source_xp[idx].saturating_add(gain);
}

/// The level-delta weight. `delta = target_level - caster_level`.
///
/// Confirmed bands below zero (anti-farm): a target 10+ levels below pays
/// 15%, 1-9 below pays 50%. At or above zero the bonus curve is exactly
/// linear, `W = min(2.00, 1 + 0.02 * delta)` -- three anchors (Delta=10 ->
/// 120%, Delta=25 -> 150%, Delta=50 -> 200%, hard cap) are exactly collinear
/// on this line, so there is no piecewise fit to hide.
pub fn level_delta_weight(delta: i32) -> f32 {
    if delta <= -10 {
        0.15
    } else if delta < 0 {
        0.50
    } else {
        (1.0 + 0.02 * delta as f32).min(2.00)
    }
}

/// The highest [`crate::comp::spell::SpellDef::level`] of a source this
/// mastery percentage may COPY into a spellbook (escalonado tiers: 25% -> L2,
/// 50% -> L4, 75% -> L6, 100% -> every level). `None` below 25% means no
/// cross-source copying yet -- only levels the character's class already
/// grants directly. `Some(u8::MAX)` at 100% stands for "every level",
/// deliberately not clamped to 9 so a future capstone tier is already
/// covered.
pub fn mastery_tier_max_level(pct: f32) -> Option<u8> {
    if pct >= 1.0 {
        Some(u8::MAX)
    } else if pct >= 0.75 {
        Some(6)
    } else if pct >= 0.50 {
        Some(4)
    } else if pct >= 0.25 {
        Some(2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::{
        ClassKind, ability::SpellGate, class::CharacterClass, skillset::total_exp_for_level,
    };

    #[test]
    fn level_delta_weight_hits_the_confirmed_anchors_exactly() {
        // The bonus side (Delta >= 0): three anchors, exactly linear.
        assert_eq!(level_delta_weight(0), 1.00);
        assert_eq!(level_delta_weight(10), 1.20);
        assert_eq!(level_delta_weight(25), 1.50);
        assert_eq!(level_delta_weight(50), 2.00);
        // Hard cap: never exceeds 2.00 past Delta=50.
        assert_eq!(level_delta_weight(51), 2.00);
        assert_eq!(level_delta_weight(200), 2.00);

        // The penalty side: flat confirmed bands, with the Delta=-1 cliff
        // (spec's flagged-not-changed balance note) landing exactly as
        // specified.
        assert_eq!(level_delta_weight(-1), 0.50);
        assert_eq!(level_delta_weight(-9), 0.50);
        assert_eq!(level_delta_weight(-10), 0.15);
        assert_eq!(level_delta_weight(-60), 0.15);
    }

    #[test]
    fn mastery_tier_max_level_hits_every_boundary() {
        assert_eq!(mastery_tier_max_level(0.24), None);
        assert_eq!(mastery_tier_max_level(0.25), Some(2));
        assert_eq!(mastery_tier_max_level(0.49), Some(2));
        assert_eq!(mastery_tier_max_level(0.50), Some(4));
        assert_eq!(mastery_tier_max_level(0.74), Some(4));
        assert_eq!(mastery_tier_max_level(0.75), Some(6));
        assert_eq!(mastery_tier_max_level(0.99), Some(6));
        assert_eq!(mastery_tier_max_level(1.00), Some(u8::MAX));
    }

    #[test]
    fn pct_saturates_at_one_and_never_exceeds_it() {
        let mut mastery = SpellMastery::default();
        mastery.set_source_xp(MagicSource::Divine, u32::MAX);
        assert_eq!(mastery.pct(MagicSource::Divine), 1.0);

        mastery.set_source_xp(MagicSource::Divine, MASTERY_XP_FULL);
        assert_eq!(mastery.pct(MagicSource::Divine), 1.0);

        mastery.set_source_xp(MagicSource::Divine, MASTERY_XP_FULL / 2);
        assert!((mastery.pct(MagicSource::Divine) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn arcane_is_never_written() {
        let mut mastery = SpellMastery::default();
        for _ in 0..100 {
            grant_source_mastery(&mut mastery, MagicSource::Arcane, 10_000.0, 1.0);
        }
        assert_eq!(mastery.source_xp(MagicSource::Arcane), 0);
        assert_eq!(mastery.pct(MagicSource::Arcane), 1.0);

        // The direct setter refuses it too.
        mastery.set_source_xp(MagicSource::Arcane, 12345);
        assert_eq!(mastery.source_xp(MagicSource::Arcane), 0);
    }

    /// The arithmetic that makes the ~level-29 floor free (module doc
    /// comment): the constant and the level curve must stay consistent if
    /// either is retuned, or the floor silently moves without anyone
    /// noticing. This should fail the build, not the design review.
    #[test]
    fn mastery_xp_full_stays_above_the_level_29_floor() {
        assert!(MASTERY_XP_FULL >= total_exp_for_level(29));
        assert!(MASTERY_XP_FULL < total_exp_for_level(30));
    }

    #[test]
    fn magic_source_all_matches_count_and_covers_every_variant() {
        assert_eq!(MagicSource::ALL.len(), MagicSource::COUNT);
        for source in [
            MagicSource::Arcane,
            MagicSource::Divine,
            MagicSource::Primordial,
            MagicSource::Psionic,
            MagicSource::Ki,
        ] {
            assert!(
                MagicSource::ALL.contains(&source),
                "{source:?} missing from ALL"
            );
        }
    }

    /// Regression: mastery and `SpellGate` are AND, never OR (spec's
    /// "mastery never widens SpellGate" rule). A character at 100% Divine
    /// mastery whose `SpellGate` for a level-9 Divine spell is not unlocked
    /// (class level < 54) still cannot cast it -- `SpellGate::is_unlocked`
    /// takes no `SpellMastery` parameter at all, so there is structurally no
    /// way for this percentage to widen it. This test exists to stop a
    /// future reader "simplifying" the two gates into an OR.
    #[test]
    fn full_mastery_does_not_bypass_the_class_level_spell_gate() {
        let mut mastery = SpellMastery::default();
        for _ in 0..30 {
            grant_source_mastery(&mut mastery, MagicSource::Divine, 200_000.0, 1.0);
        }
        assert_eq!(mastery.pct(MagicSource::Divine), 1.0);

        // A level-9 spell needs class level 54 (spell_level_unlocked =
        // class_level / 6). A level-6 Cleric is nowhere near that, no matter
        // what `mastery` says -- `is_unlocked` never reads it.
        let gate = SpellGate::new(ClassKind::Cleric, 9);
        let character_class = CharacterClass::single(ClassKind::Cleric);
        assert!(!gate.is_unlocked(Some(&character_class), 6));

        // Sanity: the same gate at class level 54 unlocks, independent of
        // mastery either way.
        assert!(gate.is_unlocked(Some(&character_class), 54));
    }
}
