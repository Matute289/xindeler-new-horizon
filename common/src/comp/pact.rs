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

pub mod summon_cost;

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage};

use crate::{
    assets::{AssetExt, AssetReadGuard, Ron},
    comp::{
        SkillSet,
        ethos::Moral,
        skills::{Skill, WarlockSkill},
    },
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

/// Which of the four canon boons a Warlock's pact grants -- orthogonal to
/// [`PatronId`]: the patron roster and the boon list never constrain each
/// other, and any patron can grant any boon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PactBoon {
    Tome,
    Chain,
    Blade,
    Talisman,
}

impl PactBoon {
    /// Every variant, in declaration order.
    pub const ALL: [PactBoon; 4] = [
        PactBoon::Tome,
        PactBoon::Chain,
        PactBoon::Blade,
        PactBoon::Talisman,
    ];

    /// Lowercase snake_case keyword used for DB persistence and `/pact boon`
    /// arguments.
    pub fn keyword(self) -> &'static str {
        match self {
            PactBoon::Tome => "tome",
            PactBoon::Chain => "chain",
            PactBoon::Blade => "blade",
            PactBoon::Talisman => "talisman",
        }
    }

    /// Inverse of [`Self::keyword`].
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.keyword() == keyword)
    }
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
    /// `None` = no boon chosen yet (the pre-Tier-2.5 default, and the state
    /// of any pact that never opts into one). Set via `/pact boon <id>`.
    /// Nothing reads this outside that command yet -- each boon's own
    /// mechanic (Tome/Chain/Blade/Talisman) is unbuilt.
    pub boon: Option<PactBoon>,
    /// Whether a `Blade`-boon Warlock currently has their conjured weapon
    /// out. Meaningless (and always `false`) for every other boon. Toggled
    /// via `/pact blade <summon|dismiss>`. The blade occupies no
    /// `EquipSlot` and is never a real `Item` -- this flag is its entire
    /// state. Not yet consumed by any ability or combat code; granting the
    /// blade's actual attack ability set is separate, unbuilt follow-up
    /// work.
    pub blade_summoned: bool,
    /// Reserved for a future demand/favour mechanic; not read or written by
    /// anything yet. Always `0` for now -- do not add a tick/decay system
    /// against this field without a full design pass, since nothing may set
    /// `Severed` as a side effect of an unrelated, unbounded accumulator.
    pub favour: i32,
}

impl Pact {
    /// This Warlock's total Cadena (`PactBoon::Chain`) summon point pool, or
    /// `0` if their boon isn't `Chain` -- no pact, `patron: None`, or any
    /// other boon all read as "no pool", which in turn makes every
    /// `Summons`-gated cast unaffordable by construction. This is the ONE
    /// place in the pact system that intentionally fails *closed*: the
    /// module doc's "fail-open by construction" note is about casting
    /// suppression (`Severed`), not about a resource pool that simply
    /// doesn't exist for a non-Chain character.
    pub fn chain_summon_pool(&self, skill_set: &SkillSet) -> u16 {
        if self.boon != Some(PactBoon::Chain) {
            return 0;
        }
        let chain_rank = skill_set
            .skill_level(Skill::Warlock(WarlockSkill::ChainMastery))
            .unwrap_or(0);
        chain_pool(skill_set.character_level().into(), chain_rank)
    }
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

/// Tunable numbers for the Cadena boon's summon-cost derivation and point
/// pool. `assets/common/pact/summon_tuning.ron` -- kept as data, not Rust
/// constants, so a balance pass can retune without a recompile (same
/// convention as `combat::CombatTuning`).
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct SummonTuning {
    /// [`summon_cost::summon_cost`]'s multiplier on combat rating.
    pub cost_multiplier: f32,
    /// [`summon_cost::summon_cost`]'s floor -- nothing is ever free to
    /// summon.
    pub cost_floor: f32,
    /// The hard ceiling on a Cadena Warlock's summon point pool -- no
    /// level, no investment, reaches past it. Deliberately below every
    /// raid-boss-tier [`summon_cost::npc_summon_cost`] (e.g. a Mindflayer
    /// costs far more), so the pool excludes those by construction rather
    /// than a special-case list.
    pub pool_ceiling: u16,
}

/// Do NOT call per-entity in tick systems -- hoist once per run, same as
/// `patrons_manifest`.
pub fn summon_tuning_manifest() -> AssetReadGuard<Ron<SummonTuning>> {
    Ron::<SummonTuning>::load_expect("common.pact.summon_tuning").read()
}

/// `(level, base)` milestones for the Cadena point pool, ascending by level.
/// `assets/common/pact/chain_pool.ron`. Do NOT call per-entity in tick
/// systems -- hoist once per run, same as `patrons_manifest`.
pub fn chain_pool_manifest() -> AssetReadGuard<Ron<Vec<(u32, u16)>>> {
    Ron::<Vec<(u32, u16)>>::load_expect("common.pact.chain_pool").read()
}

/// A Cadena Warlock's total summon point pool: the highest level milestone
/// at or below `character_level`, plus one point per `chain_rank`
/// (`Skill::Warlock(ChainMastery)`'s level, `max_level: 5`), capped at
/// [`SummonTuning::pool_ceiling`]. A level below every milestone (should not
/// happen -- the table's first entry is level 1) reads as `0 + chain_rank`.
pub fn chain_pool(character_level: u32, chain_rank: u16) -> u16 {
    let base = chain_pool_manifest()
        .0
        .iter()
        .filter(|(level, _)| *level <= character_level)
        .map(|(_, base)| *base)
        .max()
        .unwrap_or(0);
    (base + chain_rank).min(summon_tuning_manifest().0.pool_ceiling)
}

/// A summoner's live Cadena summons ledger: which entities they currently
/// have out, and the pool cost each one is charged. Server-authoritative;
/// net-synced read-only so the HUD can show `spent() / pool`. Present on
/// any character, but only ever populated for a Cadena Warlock -- an empty
/// ledger is indistinguishable from "no Cadena boon" and costs nothing.
///
/// Freeing an entry must be driven by the same cleanup path that despawns
/// the summon (death, lifetime expiry, dismiss, owner logout/death), never
/// a separate timer -- two independently-driven ledgers is exactly how a
/// player permanently loses pool capacity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summons {
    pub active: Vec<(crate::uid::Uid, u16)>,
}

impl Summons {
    pub fn spent(&self) -> u16 { self.active.iter().map(|(_, cost)| *cost).sum() }

    /// Records a newly-spawned summon against the ledger. Callers (currently
    /// only `server::events::entity_creation::handle_create_npc`) MUST have
    /// already verified `spent() + cost <= pool` -- this method does not
    /// re-check the ceiling itself, since the pool figure lives on `Pact`,
    /// not here, and re-deriving it here would risk the two ever
    /// disagreeing.
    pub fn charge(&mut self, summon_uid: crate::uid::Uid, cost: u16) {
        self.active.push((summon_uid, cost));
    }

    /// Removes a summon from the ledger (it died, expired, was dismissed, or
    /// its owner logged out/died) and returns the points it frees, or `None`
    /// if this uid was never charged -- the common, harmless case for any
    /// entity deletion that isn't a Cadena summon at all. Idempotent: a
    /// second release of the same uid (which should never happen given a
    /// single funnel, but is cheap to make safe) is also `None`.
    pub fn release(&mut self, summon_uid: crate::uid::Uid) -> Option<u16> {
        let index = self.active.iter().position(|(uid, _)| *uid == summon_uid)?;
        Some(self.active.remove(index).1)
    }
}

impl Component for Summons {
    // `DenseVecStorage`: rare, same reasoning as `Pact` itself -- only
    // Cadena Warlocks with something currently summoned ever have one.
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_warlock_pool_is_two() {
        assert_eq!(chain_pool(1, 0), 2);
    }

    #[test]
    fn an_uninvested_level_60_pool_is_twenty() {
        // Levels 50-60 do not widen the base further (plan §14.1d).
        assert_eq!(chain_pool(50, 0), 20);
        assert_eq!(chain_pool(60, 0), 20);
    }

    #[test]
    fn pool_widens_at_every_milestone() {
        assert_eq!(chain_pool(4, 0), 2, "still under the level-5 milestone");
        assert_eq!(chain_pool(5, 0), 4);
        assert_eq!(chain_pool(19, 0), 4, "still under the level-20 milestone");
        assert_eq!(chain_pool(20, 0), 8);
        assert_eq!(chain_pool(30, 0), 12);
        assert_eq!(chain_pool(40, 0), 16);
    }

    #[test]
    fn chain_rank_adds_one_point_per_level() {
        assert_eq!(chain_pool(1, 3), 5);
    }

    #[test]
    fn pool_is_capped_at_the_ceiling_even_fully_invested() {
        assert_eq!(chain_pool(60, 5), summon_tuning_manifest().0.pool_ceiling);
    }

    #[test]
    fn summons_spent_sums_every_active_entry() {
        let uid = |n: u64| crate::uid::Uid::from(std::num::NonZeroU64::new(n).unwrap());
        let summons = Summons {
            active: vec![(uid(1), 3), (uid(2), 7)],
        };
        assert_eq!(summons.spent(), 10);
    }

    #[test]
    fn empty_summons_has_spent_zero() {
        assert_eq!(Summons::default().spent(), 0);
    }

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
        assert_eq!(pact.boon, None);
        assert_eq!(pact.favour, 0);
    }

    #[test]
    fn boon_keyword_round_trips_for_all_fixed_variants() {
        for boon in PactBoon::ALL {
            assert_eq!(
                PactBoon::from_keyword(boon.keyword()),
                Some(boon),
                "{boon:?} did not round-trip through its keyword"
            );
        }
    }

    #[test]
    fn unknown_boon_keyword_returns_none() {
        assert_eq!(PactBoon::from_keyword("staff"), None);
    }

    #[test]
    fn chain_summon_pool_is_zero_without_the_chain_boon() {
        let mut skill_set = SkillSet::default();
        skill_set.set_level(60);
        let no_boon = Pact::default();
        assert_eq!(no_boon.chain_summon_pool(&skill_set), 0);

        let other_boon = Pact {
            boon: Some(PactBoon::Tome),
            ..Pact::default()
        };
        assert_eq!(other_boon.chain_summon_pool(&skill_set), 0);
    }

    #[test]
    fn chain_summon_pool_reads_the_manifest_through_character_level() {
        let mut skill_set = SkillSet::default();
        skill_set.set_level(1);
        let chain = Pact {
            boon: Some(PactBoon::Chain),
            ..Pact::default()
        };
        assert_eq!(chain.chain_summon_pool(&skill_set), 2);

        skill_set.set_level(60);
        assert_eq!(chain.chain_summon_pool(&skill_set), 20);
    }

    #[test]
    fn summons_charge_then_release_round_trips_the_ledger() {
        let uid = |n: u64| crate::uid::Uid::from(std::num::NonZeroU64::new(n).unwrap());
        let mut summons = Summons::default();
        summons.charge(uid(1), 3);
        summons.charge(uid(2), 7);
        assert_eq!(summons.spent(), 10);

        assert_eq!(summons.release(uid(1)), Some(3));
        assert_eq!(summons.spent(), 7);
        assert_eq!(summons.active, vec![(uid(2), 7)]);

        // Releasing an unknown or already-released uid is a harmless no-op,
        // not an error -- most entity deletions aren't Cadena summons at all.
        assert_eq!(summons.release(uid(1)), None);
        assert_eq!(summons.release(uid(99)), None);
        assert_eq!(summons.spent(), 7);
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
