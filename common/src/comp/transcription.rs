//! Transcription: copying a spell out of a "page" -- an `ItemKind::SpellGroup`
//! item listed by another item's `spell_pages` (in practice, an equipped
//! Tome) -- permanently into a character's `comp::SpellBook`.
//!
//! This module holds the two pure rules that decide whether a page is
//! presently copyable and what it costs, kept independent of ECS/server
//! plumbing so both are unit-testable on their own. The event that actually
//! performs the copy lives in the server crate.
//!
//! **Scope note on the time cost:** `TRANSCRIPTION_HOURS_PER_CIRCLE` is
//! computed and exposed for display, but nothing yet enforces it as a
//! blocking, uninterruptible wait -- no equivalent "spend N in-game hours
//! uninterrupted" mechanic exists anywhere else in the codebase to build on,
//! and building one from scratch is out of this module's scope. The gold
//! cost and the mastery-tier gate below ARE enforced.

use super::{
    ability::MagicSource,
    spell::SpellCompendium,
    spell_mastery::{SpellMastery, mastery_tier_max_level},
};

/// Gold cost per circle (`SpellDef::level`; a cantrip, level 0, is free).
pub const TRANSCRIPTION_GOLD_PER_CIRCLE: u32 = 100;

/// In-game-hour cost per circle, at the same rate. See the module doc
/// comment for why this is exposed but not yet enforced as a wait.
pub const TRANSCRIPTION_HOURS_PER_CIRCLE: u32 = 1;

/// `Mage(Polyglot)`'s transcription-cost discount, per rank. Max rank 3
/// (`skill_max_levels.ron`), so the cap is -30%.
pub const POLYGLOT_TRANSCRIBE_DISCOUNT_PER_RANK: f32 = 0.10;

/// Gold cost to transcribe a spell of `level`. A spell with no compendium
/// entry (`level: None`) has no circle to charge by and is priced as free --
/// see [`spell_is_transcribable`]'s doc comment for the matching mastery-gate
/// treatment of the same case.
pub fn transcription_gold_cost(level: Option<u8>) -> u32 {
    TRANSCRIPTION_GOLD_PER_CIRCLE * u32::from(level.unwrap_or(0))
}

/// In-game-hour cost to transcribe a spell of `level`, at the same rate.
pub fn transcription_hour_cost(level: Option<u8>) -> u32 {
    TRANSCRIPTION_HOURS_PER_CIRCLE * u32::from(level.unwrap_or(0))
}

/// The multiplier `Mage(Polyglot)` applies to both costs above at `rank`
/// (0..=3, read at point of use the same way `POLYGLOT_BONUS_PER_RANK` is).
/// Clamped at zero so a hand-edited or future higher rank never produces a
/// negative cost.
pub fn polyglot_cost_multiplier(rank: u16) -> f32 {
    (1.0 - POLYGLOT_TRANSCRIBE_DISCOUNT_PER_RANK * rank as f32).max(0.0)
}

/// Whether `spell_id` may presently be copied into a spellbook, given
/// `mastery`.
///
/// - No compendium entry at all: always copyable. Bespoke content with no
///   catalogue entry is not governed by this gate -- its own gating, if any, is
///   authored elsewhere -- and it earns no mastery either way, since there is
///   no source to credit.
/// - Source `Arcane`: always copyable (known by default; `SpellMastery` never
///   accrues XP for it).
/// - Any other source: copyable only if that source's mastery tier ceiling (see
///   [`mastery_tier_max_level`]) is at or above this spell's level.
pub fn spell_is_transcribable(
    spell_id: &str,
    mastery: &SpellMastery,
    compendium: &SpellCompendium,
) -> bool {
    let Some(spell) = compendium.get(spell_id) else {
        return true;
    };
    if matches!(spell.source, MagicSource::Arcane) {
        return true;
    }
    mastery_tier_max_level(mastery.pct(spell.source)).is_some_and(|max| spell.level <= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real, currently-catalogued Divine spells spanning the escalonado
    // tiers: L1 below every tier, L2/L3 straddling the 25% boundary, L7 the
    // highest Divine level catalogued as of writing (stands in for "a high
    // level admitted once mastery is 100%").
    const DIVINE_L1: &str = "spells.enchantment.censure";
    const DIVINE_L2: &str = "spells.evocation.branding_smite";
    const DIVINE_L3: &str = "spells.evocation.blinding_smite";
    const DIVINE_L7: &str = "spells.transmutation.regenerate";

    fn compendium() -> SpellCompendium { SpellCompendium::load_expect_cloned() }

    fn mastery_at(source: MagicSource, pct_times_100: u32) -> SpellMastery {
        let mut mastery = SpellMastery::default();
        // MASTERY_XP_FULL is 200_000; set XP directly to hit an exact %.
        mastery.set_source_xp(source, pct_times_100 * 2_000);
        mastery
    }

    #[test]
    fn zero_mastery_refuses_a_level_one_spell_of_that_source() {
        let mastery = mastery_at(MagicSource::Divine, 0);
        assert!(!spell_is_transcribable(DIVINE_L1, &mastery, &compendium()));
    }

    #[test]
    fn quarter_mastery_allows_level_two_and_refuses_level_three() {
        let mastery = mastery_at(MagicSource::Divine, 25);
        let book = compendium();
        assert!(spell_is_transcribable(DIVINE_L2, &mastery, &book));
        assert!(!spell_is_transcribable(DIVINE_L3, &mastery, &book));
    }

    #[test]
    fn full_mastery_allows_the_highest_catalogued_level() {
        let mastery = mastery_at(MagicSource::Divine, 100);
        assert!(spell_is_transcribable(DIVINE_L7, &mastery, &compendium()));
    }

    #[test]
    fn arcane_is_always_transcribable_regardless_of_mastery() {
        // Arcane's SpellMastery slot is never written and pct() always
        // answers 1.0, but this asserts the source-level exemption directly
        // rather than relying on that as an implementation detail.
        let mastery = SpellMastery::default();
        let book = compendium();
        let arcane_spell = book
            .iter()
            .find(|s| matches!(s.source, MagicSource::Arcane) && s.level >= 1)
            .expect("the compendium has at least one leveled Arcane spell");
        assert!(spell_is_transcribable(&arcane_spell.id, &mastery, &book));
    }

    #[test]
    fn an_uncatalogued_spell_id_is_always_transcribable() {
        let mastery = SpellMastery::default();
        assert!(spell_is_transcribable(
            "spells.testing.not_in_any_compendium",
            &mastery,
            &compendium()
        ));
    }

    #[test]
    fn gold_and_hour_costs_scale_with_circle_and_cantrips_are_free() {
        assert_eq!(transcription_gold_cost(Some(0)), 0);
        assert_eq!(transcription_gold_cost(Some(1)), 100);
        assert_eq!(transcription_gold_cost(Some(7)), 700);
        assert_eq!(transcription_hour_cost(Some(0)), 0);
        assert_eq!(transcription_hour_cost(Some(7)), 7);
    }

    #[test]
    fn an_uncatalogued_spell_costs_nothing() {
        assert_eq!(transcription_gold_cost(None), 0);
        assert_eq!(transcription_hour_cost(None), 0);
    }

    #[test]
    fn polyglot_discount_scales_per_rank_and_never_goes_negative() {
        assert_eq!(polyglot_cost_multiplier(0), 1.0);
        assert!((polyglot_cost_multiplier(3) - 0.70).abs() < 1e-6);
        // A hand-edited or future higher rank never produces a negative
        // multiplier (which would otherwise pay the player to transcribe).
        assert_eq!(polyglot_cost_multiplier(20), 0.0);
    }
}
