//! Character background identity (BL-31). See
//! docs/design/specs/2026-06-27-backgrounds-design.md and
//! docs/design/plans/2026-07-01-backgrounds-p0-triage.md.
//!
//! Modelled on [`crate::comp::class::ClassKind`] /
//! [`crate::comp::CharacterClass`] rather than [`crate::comp::Ethos`]: `Ethos`
//! is a pair of drifting scores, which is the wrong shape for "one variant per
//! lore background". `ClassKind` is a plain `Copy` enum with a
//! `keyword()`/`from_keyword()` db-string mapping — this enum follows the same
//! shape: a plain `Copy` enum with `keyword()`/`from_keyword()` taking/
//! returning `self` by value.
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

/// One variant per lore background
/// (`docs/design/lore/chargen/20-backgrounds.md`, 24 backgrounds across 7
/// categories). Like [`crate::comp::class::ClassKind`] this derives `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackgroundKind {
    // Spiritual (3)
    Acolyte,
    Hermit,
    Inquisitor,
    // Academic (4)
    Sage,
    Archaeologist,
    Scribe,
    Investigator,
    // Martial (2)
    Soldier,
    Guard,
    // Criminal (3)
    Criminal,
    Charlatan,
    BountyHunter,
    // Social (4)
    Noble,
    Entertainer,
    FolkHero,
    Merchant,
    // Trade & Craft (4)
    Artisan,
    Farmer,
    Fisher,
    Miner,
    // Wilderness (4)
    Outlander,
    Guide,
    Sailor,
    Urchin,
}

impl BackgroundKind {
    /// Every variant, in declaration order (mirrors
    /// [`crate::comp::class::ClassKind::ALL`]). Persistence round-trip tests
    /// and the creation-UI listing iterate this so a new variant added here
    /// cannot silently fall out of either.
    pub const ALL: [BackgroundKind; 24] = [
        BackgroundKind::Acolyte,
        BackgroundKind::Hermit,
        BackgroundKind::Inquisitor,
        BackgroundKind::Sage,
        BackgroundKind::Archaeologist,
        BackgroundKind::Scribe,
        BackgroundKind::Investigator,
        BackgroundKind::Soldier,
        BackgroundKind::Guard,
        BackgroundKind::Criminal,
        BackgroundKind::Charlatan,
        BackgroundKind::BountyHunter,
        BackgroundKind::Noble,
        BackgroundKind::Entertainer,
        BackgroundKind::FolkHero,
        BackgroundKind::Merchant,
        BackgroundKind::Artisan,
        BackgroundKind::Farmer,
        BackgroundKind::Fisher,
        BackgroundKind::Miner,
        BackgroundKind::Outlander,
        BackgroundKind::Guide,
        BackgroundKind::Sailor,
        BackgroundKind::Urchin,
    ];

    /// Lowercase snake_case keyword used for DB persistence and (future)
    /// chat commands / asset specifiers.
    pub fn keyword(self) -> &'static str {
        match self {
            BackgroundKind::Acolyte => "acolyte",
            BackgroundKind::Hermit => "hermit",
            BackgroundKind::Inquisitor => "inquisitor",
            BackgroundKind::Sage => "sage",
            BackgroundKind::Archaeologist => "archaeologist",
            BackgroundKind::Scribe => "scribe",
            BackgroundKind::Investigator => "investigator",
            BackgroundKind::Soldier => "soldier",
            BackgroundKind::Guard => "guard",
            BackgroundKind::Criminal => "criminal",
            BackgroundKind::Charlatan => "charlatan",
            BackgroundKind::BountyHunter => "bounty_hunter",
            BackgroundKind::Noble => "noble",
            BackgroundKind::Entertainer => "entertainer",
            BackgroundKind::FolkHero => "folk_hero",
            BackgroundKind::Merchant => "merchant",
            BackgroundKind::Artisan => "artisan",
            BackgroundKind::Farmer => "farmer",
            BackgroundKind::Fisher => "fisher",
            BackgroundKind::Miner => "miner",
            BackgroundKind::Outlander => "outlander",
            BackgroundKind::Guide => "guide",
            BackgroundKind::Sailor => "sailor",
            BackgroundKind::Urchin => "urchin",
        }
    }

    /// Inverse of [`Self::keyword`].
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.keyword() == keyword)
    }

    /// Human-readable title-case name, derived from [`Self::keyword`] (e.g.
    /// `"city_watch"` -> `"City Watch"`). **P1 stand-in only**: BL-31 P3
    /// (Haiku, content phase) authors real `background-<name>-title` i18n
    /// keys sourced from lore text (spec §2.4) — this exists so the P1
    /// creation-UI step has real, non-placeholder labels to show before that
    /// content lands, without inventing 44+ i18n keys ahead of the content
    /// pass.
    pub fn display_name(self) -> String {
        self.keyword()
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The background a player character chose at creation (P0 §Q1: `None` for
/// legacy characters and any character that has not committed to one —
/// displayed as "Uncommitted"/hidden, never forced). Synced to all clients;
/// persisted in the `character` table (migration V73).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background(pub Option<BackgroundKind>);

impl Component for Background {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_len_matches_const() {
        assert_eq!(BackgroundKind::ALL.len(), 24);
    }

    #[test]
    fn keyword_round_trips_for_all_fixed_variants() {
        for background in BackgroundKind::ALL {
            assert_eq!(
                BackgroundKind::from_keyword(background.keyword()),
                Some(background),
                "{background:?} did not round-trip through its keyword"
            );
        }
    }

    #[test]
    fn acolyte_serializes_to_expected_keyword() {
        assert_eq!(BackgroundKind::Acolyte.keyword(), "acolyte");
        assert_eq!(
            BackgroundKind::from_keyword("acolyte"),
            Some(BackgroundKind::Acolyte)
        );
    }

    #[test]
    fn display_name_title_cases_the_keyword() {
        assert_eq!(BackgroundKind::Acolyte.display_name(), "Acolyte");
        assert_eq!(BackgroundKind::BountyHunter.display_name(), "Bounty Hunter");
        assert_eq!(BackgroundKind::FolkHero.display_name(), "Folk Hero");
    }

    #[test]
    fn unknown_keyword_returns_none() {
        assert_eq!(BackgroundKind::from_keyword("necromancer"), None);
    }

    #[test]
    fn default_background_is_none() {
        assert_eq!(Background::default(), Background(None));
    }

    #[test]
    fn background_none_is_clone_eq_stable() {
        let a = Background(None);
        let b = a.clone();
        assert_eq!(a, b);
        let c = Background(Some(BackgroundKind::Soldier));
        let d = c.clone();
        assert_eq!(c, d);
        assert_ne!(a, c);
    }
}
