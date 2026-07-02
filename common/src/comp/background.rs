//! Character background identity (BL-31). See
//! docs/design/specs/2026-06-27-backgrounds-design.md and
//! docs/design/plans/2026-07-01-backgrounds-p0-triage.md.
//!
//! Modelled on [`crate::comp::class::ClassKind`] /
//! [`crate::comp::CharacterClass`] rather than [`crate::comp::Ethos`]: `Ethos`
//! is a pair of drifting scores, which is the wrong shape for "one variant per
//! lore background". `ClassKind` is a plain `Copy` enum with a
//! `keyword()`/`from_keyword()` db-string mapping — the shape we want, except
//! `BackgroundKind` needs a `Custom(String)` catch-all (P0 §Q5), so **this enum
//! cannot derive `Copy`** (a data-carrying variant breaks `Copy`) and
//! `keyword()`/`from_keyword()` take `&self`/return owned data where
//! `ClassKind`'s take `self` by value.
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

/// One variant per lore background
/// (`docs/design/lore/chargen/20-backgrounds.md`, 44 backgrounds across 7
/// categories) plus a `Custom(String)` catch-all for DM-approved freeform
/// backgrounds (P0 §Q5). Unlike [`crate::comp::class::ClassKind`] this does **
/// not** derive `Copy`: `Custom` carries an owned `String`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundKind {
    // Spiritual (5)
    Acolyte,
    Hermit,
    HauntedOne,
    Anthropologist,
    Inquisitor,
    // Academic (5)
    Sage,
    CloisteredScholar,
    Archaeologist,
    Scribe,
    Investigator,
    // Martial (6)
    Soldier,
    Guard,
    CityWatch,
    Marine,
    MercenaryVeteran,
    KnightOfAnOrder,
    // Criminal (6)
    Criminal,
    Charlatan,
    Smuggler,
    Gambler,
    UrbanBountyHunter,
    FactionAgent,
    // Social (10)
    Noble,
    Courtier,
    FarTraveler,
    Entertainer,
    FolkHero,
    Merchant,
    FailedMerchant,
    Carouser,
    Rewarded,
    Ruined,
    // Trade & Craft (7)
    GuildArtisan,
    Artisan,
    Shipwright,
    Farmer,
    Fisher,
    Miner,
    CaravanSpecialist,
    // Wilderness (5)
    Outlander,
    Guide,
    Sailor,
    Wayfarer,
    Urchin,
    /// DM-approved freeform background (P0 §Q5): no kit, no stat passive:
    /// the string is a narrative note only, stored verbatim.
    Custom(String),
}

impl BackgroundKind {
    /// Every non-`Custom` variant, in declaration order (mirrors
    /// [`crate::comp::class::ClassKind::ALL`]). Persistence round-trip tests
    /// and the (future P2) creation-UI listing iterate this so a new variant
    /// added here cannot silently fall out of either.
    pub const ALL: [BackgroundKind; 44] = [
        BackgroundKind::Acolyte,
        BackgroundKind::Hermit,
        BackgroundKind::HauntedOne,
        BackgroundKind::Anthropologist,
        BackgroundKind::Inquisitor,
        BackgroundKind::Sage,
        BackgroundKind::CloisteredScholar,
        BackgroundKind::Archaeologist,
        BackgroundKind::Scribe,
        BackgroundKind::Investigator,
        BackgroundKind::Soldier,
        BackgroundKind::Guard,
        BackgroundKind::CityWatch,
        BackgroundKind::Marine,
        BackgroundKind::MercenaryVeteran,
        BackgroundKind::KnightOfAnOrder,
        BackgroundKind::Criminal,
        BackgroundKind::Charlatan,
        BackgroundKind::Smuggler,
        BackgroundKind::Gambler,
        BackgroundKind::UrbanBountyHunter,
        BackgroundKind::FactionAgent,
        BackgroundKind::Noble,
        BackgroundKind::Courtier,
        BackgroundKind::FarTraveler,
        BackgroundKind::Entertainer,
        BackgroundKind::FolkHero,
        BackgroundKind::Merchant,
        BackgroundKind::FailedMerchant,
        BackgroundKind::Carouser,
        BackgroundKind::Rewarded,
        BackgroundKind::Ruined,
        BackgroundKind::GuildArtisan,
        BackgroundKind::Artisan,
        BackgroundKind::Shipwright,
        BackgroundKind::Farmer,
        BackgroundKind::Fisher,
        BackgroundKind::Miner,
        BackgroundKind::CaravanSpecialist,
        BackgroundKind::Outlander,
        BackgroundKind::Guide,
        BackgroundKind::Sailor,
        BackgroundKind::Wayfarer,
        BackgroundKind::Urchin,
    ];

    /// Lowercase snake_case keyword used for DB persistence and (future)
    /// chat commands / asset specifiers. `Custom` is not covered here — its
    /// db string is always the literal `"custom"`, with the freeform note
    /// stored in a separate DB column (see
    /// `server::persistence::character::conversions`).
    pub fn keyword(&self) -> &'static str {
        match self {
            BackgroundKind::Acolyte => "acolyte",
            BackgroundKind::Hermit => "hermit",
            BackgroundKind::HauntedOne => "haunted_one",
            BackgroundKind::Anthropologist => "anthropologist",
            BackgroundKind::Inquisitor => "inquisitor",
            BackgroundKind::Sage => "sage",
            BackgroundKind::CloisteredScholar => "cloistered_scholar",
            BackgroundKind::Archaeologist => "archaeologist",
            BackgroundKind::Scribe => "scribe",
            BackgroundKind::Investigator => "investigator",
            BackgroundKind::Soldier => "soldier",
            BackgroundKind::Guard => "guard",
            BackgroundKind::CityWatch => "city_watch",
            BackgroundKind::Marine => "marine",
            BackgroundKind::MercenaryVeteran => "mercenary_veteran",
            BackgroundKind::KnightOfAnOrder => "knight_of_an_order",
            BackgroundKind::Criminal => "criminal",
            BackgroundKind::Charlatan => "charlatan",
            BackgroundKind::Smuggler => "smuggler",
            BackgroundKind::Gambler => "gambler",
            BackgroundKind::UrbanBountyHunter => "urban_bounty_hunter",
            BackgroundKind::FactionAgent => "faction_agent",
            BackgroundKind::Noble => "noble",
            BackgroundKind::Courtier => "courtier",
            BackgroundKind::FarTraveler => "far_traveler",
            BackgroundKind::Entertainer => "entertainer",
            BackgroundKind::FolkHero => "folk_hero",
            BackgroundKind::Merchant => "merchant",
            BackgroundKind::FailedMerchant => "failed_merchant",
            BackgroundKind::Carouser => "carouser",
            BackgroundKind::Rewarded => "rewarded",
            BackgroundKind::Ruined => "ruined",
            BackgroundKind::GuildArtisan => "guild_artisan",
            BackgroundKind::Artisan => "artisan",
            BackgroundKind::Shipwright => "shipwright",
            BackgroundKind::Farmer => "farmer",
            BackgroundKind::Fisher => "fisher",
            BackgroundKind::Miner => "miner",
            BackgroundKind::CaravanSpecialist => "caravan_specialist",
            BackgroundKind::Outlander => "outlander",
            BackgroundKind::Guide => "guide",
            BackgroundKind::Sailor => "sailor",
            BackgroundKind::Wayfarer => "wayfarer",
            BackgroundKind::Urchin => "urchin",
            BackgroundKind::Custom(_) => "custom",
        }
    }

    /// Inverse of [`Self::keyword`] for the fixed (non-`Custom`) variants
    /// only. `"custom"` deliberately returns `None` here: reconstructing a
    /// `Custom(String)` needs the freeform note from a second DB column, so
    /// callers handle that case themselves (see
    /// `server::persistence::character::conversions::convert_background_from_database`).
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.keyword() == keyword)
    }

    /// Human-readable title-case name, derived from [`Self::keyword`] (e.g.
    /// `"city_watch"` -> `"City Watch"`). **P1 stand-in only**: BL-31 P3
    /// (Haiku, content phase) authors real `background-<name>-title` i18n
    /// keys sourced from lore text (spec §2.4) — this exists so the P1
    /// creation-UI step has real, non-placeholder labels to show before that
    /// content lands, without inventing 44+ i18n keys ahead of the content
    /// pass. `Custom` returns a fixed label; the freeform note is shown
    /// separately by the UI.
    pub fn display_name(&self) -> String {
        if matches!(self, BackgroundKind::Custom(_)) {
            return "Custom Background".to_string();
        }
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background(pub Option<BackgroundKind>);

impl Component for Background {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_len_matches_const() {
        assert_eq!(BackgroundKind::ALL.len(), 44);
    }

    #[test]
    fn keyword_round_trips_for_all_fixed_variants() {
        for background in BackgroundKind::ALL {
            assert_eq!(
                BackgroundKind::from_keyword(background.keyword()),
                Some(background.clone()),
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
    fn custom_keyword_is_fixed_and_not_reversible_via_from_keyword() {
        let custom = BackgroundKind::Custom("Raised by wolves".to_string());
        assert_eq!(custom.keyword(), "custom");
        // `from_keyword("custom")` can't reconstruct the freeform note; it's
        // not in the fixed `ALL` table by design (see doc comment).
        assert_eq!(BackgroundKind::from_keyword("custom"), None);
    }

    #[test]
    fn display_name_title_cases_the_keyword() {
        assert_eq!(BackgroundKind::Acolyte.display_name(), "Acolyte");
        assert_eq!(BackgroundKind::CityWatch.display_name(), "City Watch");
        assert_eq!(
            BackgroundKind::KnightOfAnOrder.display_name(),
            "Knight Of An Order"
        );
        assert_eq!(
            BackgroundKind::Custom("anything".to_string()).display_name(),
            "Custom Background"
        );
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
