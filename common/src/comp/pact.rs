//! The Warlock pact system: a persisted bond between a Warlock and one of a
//! fixed roster of patrons.
//!
//! Modelled on [`crate::comp::background`] (`Background`/`BackgroundKind`)
//! rather than [`crate::comp::Ethos`]: this is "one variant per lore
//! concept" (a fixed patron roster), not a pair of drifting scores. Like
//! `BackgroundKind`, [`PatronId`] is a plain `Copy` enum with a
//! `keyword()`/`from_keyword()` db-string mapping.
//!
//! **Fail-open by construction**: a character with no `Pact` component at
//! all, or a `Pact` with `patron: None`, is always `Bound` and never
//! suppresses casting. Only an explicit `Severed` standing does. This
//! mirrors `Background(None)`'s "Uncommitted" default and is required so
//! that a character predating this component, or a Warlock who simply
//! hasn't picked a patron yet, is never silently muted.

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage};

use crate::{
    assets::{AssetExt, AssetReadGuard, Ron},
    comp::ethos::Moral,
};

/// One variant per canon Warlock patron.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PatronId {
    VeiledCourt,
    HellsLord,
    DawnLord,
    VoidMind,
    CursedBlade,
    DrownedDeep,
    BoundElemental,
    Deathless,
    Undying,
    GreatOldOne,
    HorrorOfTheVoid,
    Archfey,
}

impl PatronId {
    /// Every variant, in declaration order. Persistence round-trip tests
    /// iterate this so a new variant added here cannot silently fall out of
    /// keyword conversion; a future patron-manifest asset-walk test should
    /// iterate it too, once that manifest exists.
    pub const ALL: [PatronId; 12] = [
        PatronId::VeiledCourt,
        PatronId::HellsLord,
        PatronId::DawnLord,
        PatronId::VoidMind,
        PatronId::CursedBlade,
        PatronId::DrownedDeep,
        PatronId::BoundElemental,
        PatronId::Deathless,
        PatronId::Undying,
        PatronId::GreatOldOne,
        PatronId::HorrorOfTheVoid,
        PatronId::Archfey,
    ];

    /// Lowercase snake_case keyword used for DB persistence, `/pact`
    /// arguments, and the patron manifest key.
    pub fn keyword(self) -> &'static str {
        match self {
            PatronId::VeiledCourt => "veiled_court",
            PatronId::HellsLord => "hells_lord",
            PatronId::DawnLord => "dawn_lord",
            PatronId::VoidMind => "void_mind",
            PatronId::CursedBlade => "cursed_blade",
            PatronId::DrownedDeep => "drowned_deep",
            PatronId::BoundElemental => "bound_elemental",
            PatronId::Deathless => "deathless",
            PatronId::Undying => "undying",
            PatronId::GreatOldOne => "great_old_one",
            PatronId::HorrorOfTheVoid => "horror_of_the_void",
            PatronId::Archfey => "archfey",
        }
    }

    /// Inverse of [`Self::keyword`].
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.keyword() == keyword)
    }

    /// i18n key for this patron's display name. Real prose lives in the
    /// localization assets, not here, so the actual patron names stay
    /// data/content rather than compiled into the binary.
    pub fn name_i18n_key(self) -> String { format!("warlock-patron-{}", self.keyword()) }
}

/// Whether a Warlock's pact currently grants them their patron's power.
/// `Severed` is the only standing that suppresses casting (see
/// `common/systems/src/buff.rs`'s `CharacterClass` block) -- everything else
/// (no `Pact` component, `patron: None`) reads as `Bound` by construction,
/// never as `Severed`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PactStanding {
    #[default]
    Bound,
    Severed,
}

impl PactStanding {
    pub fn keyword(self) -> &'static str {
        match self {
            PactStanding::Bound => "bound",
            PactStanding::Severed => "severed",
        }
    }

    pub fn from_keyword(keyword: &str) -> Option<Self> {
        match keyword {
            "bound" => Some(PactStanding::Bound),
            "severed" => Some(PactStanding::Severed),
            _ => None,
        }
    }
}

/// A Warlock's persisted pact state. Synced to all clients; persisted in the
/// `character` table. Present on any character, but only meaningful (and
/// only ever set to anything but the default) for `ClassKind::Warlock`.
///
/// `patron: None` means no patron has been chosen yet -- the
/// creation-time default, mirroring `Background(None)`'s "Uncommitted".
/// Assign one via `/pact bind <patron_id>`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pact {
    pub standing: PactStanding,
    pub patron: Option<PatronId>,
    /// Reserved for a future demand/favour mechanic; not read or written by
    /// anything yet. Always `0` for now -- do not add a tick/decay system
    /// against this field without a full design pass, since nothing may set
    /// `Severed` as a side effect of an unrelated, unbounded accumulator.
    pub favour: i32,
}

impl Component for Pact {
    // `DenseVecStorage`, not `VecStorage`: rare (only Warlocks who've bound a
    // patron), same class of component as `TriggerSlots`/`SpellMastery` --
    // a plain `VecStorage` would reserve a slot per entity index up to the
    // server's max, the wrong tradeoff for something this uncommon.
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

/// One canon patron's manifest data. Deliberately minimal -- emissary NPCs,
/// demand tables, and boon eligibility are unbuilt follow-ups, not fields on
/// this struct yet.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct PatronData {
    /// Gates which `Ethos.moral()` values may choose this patron (via
    /// `Moral::compatible_npc_morals`) -- no new alignment machinery, reuses
    /// the shipped `Ethos` axis. `Moral::Neutral` accepts any alignment,
    /// matching patrons open to Good/Neutral/Evil Warlocks alike.
    pub moral: Moral,
}

/// Per-patron manifest read for per-tick consumers (mirrors
/// [`crate::comp::class::class_attributes_manifest`]). Do NOT call
/// per-entity in tick systems -- hoist once per run.
pub fn patrons_manifest() -> AssetReadGuard<Ron<HashMap<PatronId, PatronData>>> {
    Ron::<HashMap<PatronId, PatronData>>::load_expect("common.class.patrons").read()
}

/// A patron's `moral` from the manifest, or `Moral::Neutral` (open to any
/// alignment) if the manifest has no row for it. Never panics.
pub fn patron_moral(patron: PatronId) -> Moral {
    patrons_manifest().0.get(&patron).map_or_else(
        || {
            tracing::warn!(
                ?patron,
                "Patron missing from patrons.ron, defaulting to Neutral"
            );
            Moral::Neutral
        },
        |data| data.moral,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patrons_manifest_covers_every_patron() {
        // Every PatronId::ALL variant must be an actual key in the RON
        // file, not merely resolve via the permissive fallback -- a patron
        // silently missing from the manifest is a data bug even though the
        // fallback happens to be safe.
        let manifest = patrons_manifest();
        for patron in PatronId::ALL {
            assert!(
                manifest.0.contains_key(&patron),
                "patrons.ron is missing a row for {patron:?}"
            );
        }
    }

    #[test]
    fn patron_moral_reads_the_manifest_row() {
        assert_eq!(patron_moral(PatronId::HellsLord), Moral::Evil);
        assert_eq!(patron_moral(PatronId::DawnLord), Moral::Good);
        assert_eq!(patron_moral(PatronId::VeiledCourt), Moral::Neutral);
    }

    #[test]
    fn all_len_matches_const() {
        assert_eq!(PatronId::ALL.len(), 12);
    }

    #[test]
    fn patron_keyword_round_trips_for_all_fixed_variants() {
        for patron in PatronId::ALL {
            assert_eq!(
                PatronId::from_keyword(patron.keyword()),
                Some(patron),
                "{patron:?} did not round-trip through its keyword"
            );
        }
    }

    #[test]
    fn unknown_patron_keyword_returns_none() {
        assert_eq!(PatronId::from_keyword("cthulhu"), None);
    }

    #[test]
    fn pact_standing_keyword_round_trips() {
        for standing in [PactStanding::Bound, PactStanding::Severed] {
            assert_eq!(
                PactStanding::from_keyword(standing.keyword()),
                Some(standing)
            );
        }
        assert_eq!(PactStanding::from_keyword("cursed"), None);
    }

    #[test]
    fn default_pact_is_bound_with_no_patron() {
        let pact = Pact::default();
        assert_eq!(pact.standing, PactStanding::Bound);
        assert_eq!(pact.patron, None);
        assert_eq!(pact.favour, 0);
    }

    #[test]
    fn patron_name_i18n_key_is_namespaced_and_stable() {
        assert_eq!(
            PatronId::VeiledCourt.name_i18n_key(),
            "warlock-patron-veiled_court"
        );
        assert_eq!(
            PatronId::HorrorOfTheVoid.name_i18n_key(),
            "warlock-patron-horror_of_the_void"
        );
    }
}
