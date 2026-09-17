//! Per-species balance numbers, as data.
//!
//! `mass`, `base_health`, `base_poise` and `base_energy` used to be `match`
//! arms in [`super::Body`], each ending in a wildcard:
//!
//! ```ignore
//! Body::QuadrupedMedium(body) => match body.species {
//!     quadruped_medium::Species::Bear => 500.0,
//!     // …
//!     _ => 200.0,
//! },
//! ```
//!
//! Rust cannot tell "explicitly 200 kg" from "nobody ever gave this species a
//! mass", so a new creature compiled clean, rendered, and was quietly a 200 kg
//! animal. [`super::attr_audit`] made that *countable* (a test failed by name
//! against a checked-in ledger); this module makes it **impossible**.
//!
//! The trick is that [`AllSpecies<T>`](super::quadruped_medium::AllSpecies) is
//! a struct with one required field per species, not a map. A RON file of that
//! shape therefore cannot omit a species: serde fails the load, by name, at
//! start-up, for free — no ledger, no audit macro, no test to keep in sync.
//! Adding a species to the enum adds a field to the struct, and the file stops
//! deserialising until somebody fills it in.
//!
//! # What is here and what is not
//!
//! Only numbers that are **read once, when an entity spawns** live here:
//! `Health`, `Poise`, `Energy` and `Mass` are all components built at spawn
//! from these four getters. That matters, because reading an asset costs a
//! `RwLock` read in hot-reloading (dev) builds and nothing at all otherwise —
//! see [`BODY_STATS`].
//!
//! Deliberately left in code:
//!
//! - [`Body::threat_tier`](super::Body::threat_tier) and
//!   [`Body::magic_resist_tier`](super::Body::magic_resist_tier) — both are
//!   *taxonomies* (which bucket is this creature in), not tunable numbers; the
//!   numbers each bucket maps to already live in data (`Body::TIER_*` and
//!   `combat_tuning.ron`). Both are now exhaustive matches, which is a
//!   strictly stronger check than this file's — a compile error rather than a
//!   start-up error. `threat_tier` is also read per entity per tick by the
//!   buff system, where an asset read would not be free.
//! - `scale`, `spacing_radius`, `combat_multiplier` — sparse matches whose
//!   catch-all is the *neutral* value (`1.0`, `2.0`, `1.0`), i.e. "this
//!   creature has no such trait", not a forgotten decision. They stay wrapped
//!   in `attr_fallback!` so that stays visible.
//! - Humanoid (derived from the body scaler), Object, Item, Ship (their own
//!   modules) and Plugin (`plugin_bodies.ron`) — none of them is an
//!   `AllSpecies` roster. [`Rowless`] enumerates them exhaustively.

use common_assets::{AssetExt, AssetHandle, Ron};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

use super::{
    Body, arthropod, biped_large, biped_small, bird_large, bird_medium, crustacean, dragon,
    fish_medium, fish_small, golem, humanoid, item, object, plugin, quadruped_low,
    quadruped_medium, quadruped_small, ship, theropod,
};

/// The balance numbers one species carries.
///
/// Every field is required: there is no `#[serde(default)]` and no `Default`
/// impl, on purpose. `deny_unknown_fields` closes the other half of the same
/// hole — a typo'd key would otherwise be silently dropped and leave the field
/// it meant to set at whatever the row's other value was.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeciesStats {
    /// Mass in kilograms, before the entity's `Scale` is applied.
    pub mass: f32,
    /// Hit points before any scaling.
    pub base_health: u16,
    /// Stagger pool. 100 is the standard creature value.
    pub base_poise: u16,
    /// Stamina/ability pool. 100 is the standard creature value.
    pub base_energy: u16,
}

/// Every creature body kind's species roster.
///
/// One field per body kind that *has* an `AllSpecies` roster; the five that do
/// not are [`Rowless`]. Both this struct and `AllSpecies` are plain structs, so
/// a missing body kind and a missing species are the same kind of error:
/// serde's, at load, naming the field.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BodyStats {
    pub arthropod: arthropod::AllSpecies<SpeciesStats>,
    pub biped_large: biped_large::AllSpecies<SpeciesStats>,
    pub biped_small: biped_small::AllSpecies<SpeciesStats>,
    pub bird_large: bird_large::AllSpecies<SpeciesStats>,
    pub bird_medium: bird_medium::AllSpecies<SpeciesStats>,
    pub crustacean: crustacean::AllSpecies<SpeciesStats>,
    pub dragon: dragon::AllSpecies<SpeciesStats>,
    pub fish_medium: fish_medium::AllSpecies<SpeciesStats>,
    pub fish_small: fish_small::AllSpecies<SpeciesStats>,
    pub golem: golem::AllSpecies<SpeciesStats>,
    pub quadruped_low: quadruped_low::AllSpecies<SpeciesStats>,
    pub quadruped_medium: quadruped_medium::AllSpecies<SpeciesStats>,
    pub quadruped_small: quadruped_small::AllSpecies<SpeciesStats>,
    pub theropod: theropod::AllSpecies<SpeciesStats>,
}

/// A [`Body`] that carries no row in `body_stats.ron`, together with the body
/// it actually is.
///
/// This exists so the getters in `mod.rs` can be *total* without an
/// `unreachable!()`: every `Body` is either a species with a stats row or one
/// of exactly these five, and [`Body::stats_row`] is an exhaustive match, so a
/// new `Body` variant does not compile until it has been classified.
pub enum Rowless<'a> {
    /// Derived from the body scaler, not tabulated — see `Body::mass`.
    Humanoid(&'a humanoid::Body),
    /// Props, projectiles and turrets; per-object arms in `mod.rs`.
    Object(&'a object::Body),
    /// Dropped items; flat values in `mod.rs`.
    Item(&'a item::Body),
    /// Airships and boats; `ship.rs`.
    Ship(&'a ship::Body),
    /// Plugin-defined bodies; `assets/common/plugin_bodies.ron`.
    Plugin(&'a plugin::Body),
}

impl Body {
    /// This body's row in `body_stats.ron`, or which rowless kind it is.
    ///
    /// The match is exhaustive over `Body` on purpose: that is the mechanism
    /// that stops a newly added body kind from quietly picking up somebody
    /// else's numbers.
    pub fn stats_row<'s>(&self, stats: &'s BodyStats) -> Result<&'s SpeciesStats, Rowless<'_>> {
        Ok(match self {
            Body::Arthropod(b) => &stats.arthropod[&b.species],
            Body::BipedLarge(b) => &stats.biped_large[&b.species],
            Body::BipedSmall(b) => &stats.biped_small[&b.species],
            Body::BirdLarge(b) => &stats.bird_large[&b.species],
            Body::BirdMedium(b) => &stats.bird_medium[&b.species],
            Body::Crustacean(b) => &stats.crustacean[&b.species],
            Body::Dragon(b) => &stats.dragon[&b.species],
            Body::FishMedium(b) => &stats.fish_medium[&b.species],
            Body::FishSmall(b) => &stats.fish_small[&b.species],
            Body::Golem(b) => &stats.golem[&b.species],
            Body::QuadrupedLow(b) => &stats.quadruped_low[&b.species],
            Body::QuadrupedMedium(b) => &stats.quadruped_medium[&b.species],
            Body::QuadrupedSmall(b) => &stats.quadruped_small[&b.species],
            Body::Theropod(b) => &stats.theropod[&b.species],
            Body::Humanoid(b) => return Err(Rowless::Humanoid(b)),
            Body::Object(b) => return Err(Rowless::Object(b)),
            Body::Item(b) => return Err(Rowless::Item(b)),
            Body::Ship(b) => return Err(Rowless::Ship(b)),
            Body::Plugin(b) => return Err(Rowless::Plugin(b)),
        })
    }
}

lazy_static! {
    /// The shipped `assets/common/body_stats.ron`.
    ///
    /// `load_expect` panics at start-up if the file is missing a species, has
    /// an unknown key, or fails to parse — which is the whole point. It is
    /// loaded once; `read()` afterwards is a `parking_lot` read lock in
    /// hot-reloading (dev) builds and a plain pointer dereference in every
    /// build with `hot-reloading` off (`default-publish`, i.e. every release),
    /// because `assets_manager` only wraps the value in a lock when something
    /// can actually replace it.
    static ref BODY_STATS: AssetHandle<Ron<BodyStats>> = Ron::load_expect("common.body_stats");
}

/// The shipped per-species balance table.
///
/// Hold the guard across a batch of lookups rather than calling this per
/// species in a loop.
#[inline]
pub fn body_stats() -> common_assets::AssetReadGuard<Ron<BodyStats>> { BODY_STATS.read() }

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks every species of every body kind and checks the shipped asset is
    /// not merely parseable but sane. The "is every species present" half is
    /// serde's job — if a field were missing, `body_stats()` would have
    /// panicked before this line.
    #[test]
    fn shipped_body_stats_asset_is_sane() {
        let stats = body_stats();

        macro_rules! check {
            ($module:ident) => {
                for species in $module::ALL_SPECIES {
                    let body = Body::from($module::Body {
                        species,
                        body_type: $module::ALL_BODY_TYPES[0],
                    });
                    let row = body
                        .stats_row(&stats.0)
                        .unwrap_or_else(|_| panic!("{species:?} has no stats row"));
                    assert!(
                        row.mass.is_finite() && row.mass > 0.0,
                        "{}::{species:?} has mass {} — mass divides in the physics code",
                        stringify!($module),
                        row.mass,
                    );
                    assert!(
                        row.base_health > 0,
                        "{}::{species:?} has 0 base_health — it would spawn dead",
                        stringify!($module),
                    );
                    assert!(
                        row.base_poise > 0,
                        "{}::{species:?} has 0 base_poise",
                        stringify!($module),
                    );
                }
            };
        }

        check!(arthropod);
        check!(biped_large);
        check!(biped_small);
        check!(bird_large);
        check!(bird_medium);
        check!(crustacean);
        check!(dragon);
        check!(fish_medium);
        check!(fish_small);
        check!(golem);
        check!(quadruped_low);
        check!(quadruped_medium);
        check!(quadruped_small);
        check!(theropod);
    }

    /// The getters must actually be wired to the asset. Without this, deleting
    /// the table and hardcoding a number back into `mod.rs` would pass every
    /// other test in this file.
    #[test]
    fn the_getters_read_the_table() {
        let stats = body_stats();
        let bear = Body::from(quadruped_medium::Body {
            species: quadruped_medium::Species::Bear,
            body_type: quadruped_medium::BodyType::Male,
        });
        let row = *bear.stats_row(&stats.0).ok().expect("bear has a row");
        assert_eq!(bear.mass().0, row.mass);
        assert_eq!(bear.base_health(), row.base_health);
        assert_eq!(bear.base_poise(), row.base_poise);
        assert_eq!(bear.base_energy(), row.base_energy);
    }

    /// The five rowless kinds must stay rowless *and* keep answering, so that
    /// a refactor cannot quietly route one of them through an empty row.
    #[test]
    fn rowless_bodies_still_have_numbers() {
        let stats = body_stats();
        for body in [
            Body::Humanoid(humanoid::Body::random()),
            Body::Object(object::Body::Bomb),
            Body::Ship(ship::Body::DefaultAirship),
        ] {
            assert!(
                body.stats_row(&stats.0).is_err(),
                "{body:?} unexpectedly has a body_stats.ron row",
            );
            assert!(body.mass().0 > 0.0, "{body:?} has no mass");
            assert!(body.base_health() > 0, "{body:?} has no health");
        }
    }
}
