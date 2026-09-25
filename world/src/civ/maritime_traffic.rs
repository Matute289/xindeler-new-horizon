//! Maritime traffic policy for Cromatolis's authored routes
//! (`AuthoredCromatolisMaritimeRoutes` in the parent module): a computed
//! default per route, correctable by an optional hand-authored override
//! file.
//!
//! Nothing in `Civs::generate` calls into this module yet -- the loader,
//! validator and merge function are complete and tested, but the resolved
//! result is not wired into worldgen or RTSim. That is future work (a
//! maritime control-plane resource mirroring `Airships`); this module's job
//! is only to make the data loadable, valid and queryable.
#![expect(dead_code)]

use super::AuthoredCromatolisMaritimeRoutes;
use crate::util::{DHashMap, seed_expan};
use common::assets::{BoxedError, FileAsset, load_ron};
use fxhash::FxHasher64;
use rand::{SeedableRng, prelude::*};
use rand_chacha::ChaChaRng;
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::HashSet,
    hash::{Hash, Hasher},
};
use tracing::warn;

/// A route's distinction between "rutas costeras" (many small stops,
/// slow, cheap, small hulls) and "largas y directas a ciudades
/// importantes" (a direct out-and-back run to a major settlement, no
/// chaining). Decides both the ship's hull class and whether it chains
/// with adjacent routes (both future consumers -- this module only
/// computes and stores the tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum MaritimeRouteTier {
    LongHaul,
    Coastal,
}

/// What a maritime route's traffic carries. `Passengers` has no consumer
/// yet -- it exists so a future boarding UI has data to read rather than a
/// schema to migrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum MaritimeCargoKind {
    Cargo,
    Passengers,
}

/// A route's `length_m` at or above which it defaults to `LongHaul` rather
/// than `Coastal`.
const LONG_HAUL_MIN_LENGTH_M: f32 = 8_000.0;

/// Inclusive bounds on the computed default ship count per route.
const MIN_SHIPS_PER_ROUTE: u8 = 4;
const MAX_SHIPS_PER_ROUTE: u8 = 8;

/// The computed default tier for a route of the given length: `LongHaul`
/// at or above [`LONG_HAUL_MIN_LENGTH_M`], `Coastal` below it.
pub(crate) fn default_maritime_tier(length_m: f32) -> MaritimeRouteTier {
    if length_m >= LONG_HAUL_MIN_LENGTH_M {
        MaritimeRouteTier::LongHaul
    } else {
        MaritimeRouteTier::Coastal
    }
}

/// The computed default cargo mix for a route: every route carries both,
/// today unconditionally.
pub(crate) fn default_maritime_carries() -> Vec<MaritimeCargoKind> {
    vec![MaritimeCargoKind::Cargo, MaritimeCargoKind::Passengers]
}

/// The computed default ship count for a route: a random roll in
/// `[MIN_SHIPS_PER_ROUTE, MAX_SHIPS_PER_ROUTE]`, deterministic from the
/// route id and the world seed so it is stable across restarts (the same
/// route always rolls the same count for a given world, and re-rolling
/// never happens on reload).
pub(crate) fn default_maritime_ship_count(route_id: &str, world_seed: u32) -> u8 {
    let mut hasher = FxHasher64::default();
    route_id.hash(&mut hasher);
    let id_hash = hasher.finish() as u32;
    let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(seed_expan::diffuse_mult(&[
        world_seed, id_hash,
    ])));
    rng.random_range(MIN_SHIPS_PER_ROUTE..=MAX_SHIPS_PER_ROUTE)
}

/// The resolved traffic policy for one maritime route: the computed
/// default, corrected by an override if the override file names this
/// route id.
///
/// This is fully derived -- recomputable at any time from the authored
/// route graph, the optional override file and the world seed, all of
/// which are either static assets or already-persisted world state. A
/// future consumer must NOT persist this into a save file or rtsim
/// `Data`; recompute it at load time instead, the same way
/// `establish_authored_cromatolis_maritime_routes` already treats the
/// route graph itself as re-derivable rather than saved. Only the
/// concrete stateful objects a future control plane creates from this
/// policy (e.g. individual ship actors, their live position/cargo) are
/// genuine save data.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedMaritimeRouteTraffic {
    pub(crate) tier: MaritimeRouteTier,
    pub(crate) carries: Vec<MaritimeCargoKind>,
    pub(crate) ships: u8,
}

/// Hand-authored corrections to the computed maritime traffic defaults.
/// Every field is optional -- an absent field means "use the computed
/// default" -- and the file may validly contain zero overrides. See the
/// module doc comment and `resolve_maritime_traffic` for how this merges
/// with the computed defaults.
#[derive(Debug, Deserialize)]
pub(crate) struct AuthoredCromatolisMaritimeTraffic {
    schema: String,
    #[serde(default)]
    routes: Vec<MaritimeTrafficRouteOverride>,
    #[serde(default)]
    settlements: Vec<MaritimeTrafficSettlementOverride>,
    /// Schema-only today: parsed and validated (must be positive if set,
    /// see `validate()`), but no consumer enforces it yet -- there is no
    /// world-total ship count to cap until a later phase actually spawns
    /// ships. Authoring a value here has no effect until then.
    #[serde(default)]
    world_ship_budget: Option<u32>,
}

impl FileAsset for AuthoredCromatolisMaritimeTraffic {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisMaritimeTraffic {
    const EXPECTED_SCHEMA: &'static str = "xindeler.cromatolis_maritime_traffic.v1";

    /// Schema and per-record invariants only -- this does NOT cross-check
    /// route/settlement ids against the real maritime route graph, since
    /// this type has no access to it. That cross-check lives in
    /// `resolve_maritime_traffic`, which -- matching
    /// `establish_authored_cromatolis_maritime_routes`'s rule -- warns and
    /// skips a single unresolvable entry rather than failing the whole
    /// load.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != Self::EXPECTED_SCHEMA {
            return Err(format!(
                "expected schema {}, got {}",
                Self::EXPECTED_SCHEMA,
                self.schema
            ));
        }

        let mut route_ids = HashSet::new();
        for route in &self.routes {
            if route.route.is_empty() || !route_ids.insert(route.route.as_str()) {
                return Err(format!(
                    "duplicate or empty maritime traffic route override id {}",
                    route.route
                ));
            }
            if route.ships == Some(0) {
                return Err(format!(
                    "maritime traffic override for {} specifies zero ships",
                    route.route
                ));
            }
            if let Some(itinerary) = &route.itinerary {
                if itinerary.len() < 2 {
                    return Err(format!(
                        "maritime traffic itinerary override for {} needs at least 2 stops, got {}",
                        route.route,
                        itinerary.len()
                    ));
                }
                let mut stops = HashSet::new();
                if !itinerary.iter().all(|stop| stops.insert(stop.as_str())) {
                    return Err(format!(
                        "maritime traffic itinerary override for {} revisits a stop",
                        route.route
                    ));
                }
            }
        }

        let mut settlement_ids = HashSet::new();
        for settlement in &self.settlements {
            if settlement.settlement.is_empty()
                || !settlement_ids.insert(settlement.settlement.as_str())
            {
                return Err(format!(
                    "duplicate or empty maritime traffic settlement override id {}",
                    settlement.settlement
                ));
            }
        }

        if self.world_ship_budget == Some(0) {
            return Err("maritime traffic world_ship_budget must be positive when set".to_string());
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MaritimeTrafficRouteOverride {
    route: String,
    #[serde(default)]
    tier: Option<MaritimeRouteTier>,
    /// Schema-only today: parsed and validated (at least 2 stops, no
    /// revisited stop, see `validate()`), but `resolve_maritime_traffic`
    /// does not read it and `ResolvedMaritimeRouteTraffic` has nowhere to
    /// put it -- the derived out-and-back chaining it would override does
    /// not exist yet (a later maritime-control-plane phase's job).
    /// Authoring a value here has no effect until then.
    #[serde(default)]
    itinerary: Option<Vec<String>>,
    #[serde(default)]
    ships: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct MaritimeTrafficSettlementOverride {
    settlement: String,
    /// Schema-only today: parsed and validated (id must be non-empty and
    /// unique, see `validate()`), but nothing derives or reads a
    /// settlement's "has a naval port" default yet -- that derivation is a
    /// later phase's job (building the actual dock/pier plot). Authoring a
    /// value here has no effect until then.
    #[serde(default)]
    naval_port: Option<bool>,
}

/// Resolves every route in `routes` (all 36 authored maritime routes, not
/// just the 15 that resolve to two real settlements -- the heuristic and
/// the override schema both apply uniformly) against the computed
/// defaults, corrected by `overrides` where present.
///
/// A single override entry whose `route` id does not exist in `routes` is
/// warned about and skipped -- matching
/// `establish_authored_cromatolis_maritime_routes`'s rule that one bad or
/// not-yet-applicable entry must never fail the whole asset -- rather than
/// rejecting the whole override file.
pub(crate) fn resolve_maritime_traffic(
    routes: &AuthoredCromatolisMaritimeRoutes,
    overrides: Option<&AuthoredCromatolisMaritimeTraffic>,
    world_seed: u32,
) -> DHashMap<String, ResolvedMaritimeRouteTraffic> {
    let real_route_ids: HashSet<&str> = routes
        .routes
        .iter()
        .map(|route| route.id.as_str())
        .collect();

    let mut override_by_route: DHashMap<&str, &MaritimeTrafficRouteOverride> = DHashMap::default();
    if let Some(overrides) = overrides {
        for route_override in &overrides.routes {
            if real_route_ids.contains(route_override.route.as_str()) {
                override_by_route.insert(route_override.route.as_str(), route_override);
            } else {
                warn!(
                    route_id = %route_override.route,
                    "Maritime traffic override references a route id absent from the \
                     authored maritime route graph; ignoring this override entry"
                );
            }
        }
    }

    routes
        .routes
        .iter()
        .map(|route| {
            let route_override = override_by_route.get(route.id.as_str()).copied();
            let tier = route_override
                .and_then(|o| o.tier)
                .unwrap_or_else(|| default_maritime_tier(route.length_m));
            let ships = route_override
                .and_then(|o| o.ships)
                .unwrap_or_else(|| default_maritime_ship_count(&route.id, world_seed));
            let resolved = ResolvedMaritimeRouteTraffic {
                tier,
                carries: default_maritime_carries(),
                ships,
            };
            (route.id.clone(), resolved)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::assets::AssetExt;

    fn real_routes() -> AuthoredCromatolisMaritimeRoutes {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_maritime_routes.ron"
        ))
        .expect("real Cromatolis maritime route graph export must parse")
    }

    fn real_traffic_override() -> AuthoredCromatolisMaritimeTraffic {
        load_ron(include_bytes!(
            "../../../assets/world/maritime/cromatolis_v0_maritime_traffic.ron"
        ))
        .expect("real Cromatolis maritime traffic override must parse")
    }

    fn parse(ron: &str) -> AuthoredCromatolisMaritimeTraffic {
        load_ron(ron.as_bytes()).expect("test override RON must parse")
    }

    // ---- default heuristics: pure logic, no assets ----

    #[test]
    fn tier_heuristic_boundary() {
        assert_eq!(default_maritime_tier(8_000.0), MaritimeRouteTier::LongHaul);
        assert_eq!(default_maritime_tier(7_999.999), MaritimeRouteTier::Coastal);
        assert_eq!(default_maritime_tier(33_410.6), MaritimeRouteTier::LongHaul);
        assert_eq!(default_maritime_tier(623.5), MaritimeRouteTier::Coastal);
    }

    #[test]
    fn carries_default_is_cargo_and_passengers() {
        assert_eq!(default_maritime_carries(), vec![
            MaritimeCargoKind::Cargo,
            MaritimeCargoKind::Passengers
        ]);
    }

    #[test]
    fn ship_count_default_is_deterministic_and_in_range() {
        for route_id in [
            "maritime.andiran__garens_town",
            "maritime.dove_city__rios_port",
            "maritime.kalthis__portland",
        ] {
            let first = default_maritime_ship_count(route_id, 42);
            let second = default_maritime_ship_count(route_id, 42);
            assert_eq!(
                first, second,
                "same route id + world seed must roll the same ship count every time"
            );
            assert!(
                (MIN_SHIPS_PER_ROUTE..=MAX_SHIPS_PER_ROUTE).contains(&first),
                "ship count {first} out of the advertised 4-8 range"
            );
        }
    }

    #[test]
    fn ship_count_default_varies_with_world_seed() {
        // Not a hard guarantee for every possible pair, but with a 5-value
        // range collisions across a handful of distinct seeds would be
        // suspicious enough to indicate the seed isn't actually being
        // mixed in.
        let counts: HashSet<u8> = (0..12)
            .map(|seed| default_maritime_ship_count("maritime.andiran__garens_town", seed))
            .collect();
        assert!(
            counts.len() > 1,
            "12 distinct world seeds all rolled the same ship count for one route"
        );
    }

    // ---- validate(): schema + per-record invariants ----

    #[test]
    fn validate_accepts_empty_file() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        traffic
            .validate()
            .expect("an override file with zero entries must be valid");
    }

    #[test]
    fn validate_rejects_wrong_schema() {
        let traffic = parse(
            r#"(
                schema: "some.other.schema.v1",
                routes: [],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicate_route_override() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.a__b", tier: Some(LongHaul)),
                    (route: "maritime.a__b", ships: Some(5)),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_ships_override() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.a__b", ships: Some(0)),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_rejects_itinerary_too_short() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.a__b", itinerary: Some(["site.a"])),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_rejects_itinerary_revisiting_a_stop() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.a__b", itinerary: Some(["site.a", "site.b", "site.a"])),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_world_ship_budget() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [],
                settlements: [],
                world_ship_budget: Some(0),
            )"#,
        );
        assert!(traffic.validate().is_err());
    }

    #[test]
    fn validate_accepts_a_real_looking_override() {
        let traffic = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.mazon_town__rios_port", tier: Some(LongHaul)),
                    (route: "maritime.dove_city__kalitos", itinerary: Some([
                        "site.dove_city", "site.kalitos", "site.hita",
                    ])),
                    (route: "maritime.kalthis__portland", ships: Some(2)),
                ],
                settlements: [
                    (settlement: "site.ravenfair", naval_port: Some(true)),
                ],
                world_ship_budget: None,
            )"#,
        );
        traffic
            .validate()
            .expect("this override shape must be valid");
    }

    // ---- resolve_maritime_traffic(): the merge ----

    #[test]
    fn resolve_with_no_override_uses_computed_defaults_for_every_route() {
        let routes = real_routes();
        let resolved = resolve_maritime_traffic(&routes, None, 1234);

        assert_eq!(resolved.len(), routes.routes.len());
        for route in &routes.routes {
            let entry = resolved
                .get(&route.id)
                .unwrap_or_else(|| panic!("resolved map missing route {}", route.id));
            assert_eq!(entry.tier, default_maritime_tier(route.length_m));
            assert_eq!(entry.carries, default_maritime_carries());
            assert_eq!(entry.ships, default_maritime_ship_count(&route.id, 1234));
        }
    }

    #[test]
    fn resolve_applies_a_real_override_and_leaves_other_routes_at_their_default() {
        let routes = real_routes();
        let overrides = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    // The default heuristic already calls this Coastal
                    // (7786.9m); flip it to prove the override wins.
                    (route: "maritime.mazon_town__rios_port", tier: Some(LongHaul)),
                    (route: "maritime.kalthis__portland", ships: Some(2)),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        overrides.validate().expect("test override must be valid");

        let resolved = resolve_maritime_traffic(&routes, Some(&overrides), 1234);

        let overridden_tier = &resolved["maritime.mazon_town__rios_port"];
        assert_eq!(overridden_tier.tier, MaritimeRouteTier::LongHaul);
        assert_eq!(
            overridden_tier.ships,
            default_maritime_ship_count("maritime.mazon_town__rios_port", 1234),
            "the ship count wasn't overridden here, so it must still be the computed default"
        );

        let overridden_ships = &resolved["maritime.kalthis__portland"];
        assert_eq!(overridden_ships.ships, 2);
        assert_eq!(
            overridden_ships.tier,
            default_maritime_tier(623.5),
            "the tier wasn't overridden here, so it must still be the computed default"
        );

        // An unrelated route must be entirely unaffected.
        let untouched = &resolved["maritime.andiran__garens_town"];
        assert_eq!(untouched.tier, default_maritime_tier(3_572.5));
        assert_eq!(
            untouched.ships,
            default_maritime_ship_count("maritime.andiran__garens_town", 1234)
        );
    }

    #[test]
    fn resolve_skips_an_override_for_a_route_id_that_does_not_exist() {
        let routes = real_routes();
        let overrides = parse(
            r#"(
                schema: "xindeler.cromatolis_maritime_traffic.v1",
                routes: [
                    (route: "maritime.does_not_exist", tier: Some(LongHaul)),
                ],
                settlements: [],
                world_ship_budget: None,
            )"#,
        );
        overrides.validate().expect("test override must be valid");

        // Must not panic, and must resolve every real route to its
        // computed default since the only override entry is unresolvable.
        let resolved = resolve_maritime_traffic(&routes, Some(&overrides), 1234);
        assert_eq!(resolved.len(), routes.routes.len());
        for route in &routes.routes {
            let entry = &resolved[&route.id];
            assert_eq!(entry.tier, default_maritime_tier(route.length_m));
        }
    }

    // ---- the real shipped files ----

    #[test]
    fn real_maritime_traffic_override_parses_and_validates() {
        let traffic = real_traffic_override();
        traffic
            .validate()
            .expect("the real shipped maritime traffic override must be valid");
    }

    #[test]
    fn real_maritime_traffic_override_ships_empty() {
        let traffic = real_traffic_override();
        assert!(
            traffic.routes.is_empty() && traffic.settlements.is_empty(),
            "COW-24 Phase 2b ships this file empty; a non-empty file here should still be valid, \
             but this test's whole point is confirming the empty-file case works against the real \
             shipped asset -- if this fails because real overrides were added, replace it with a \
             narrower assertion instead of deleting it"
        );
    }

    #[test]
    fn real_maritime_routes_resolve_against_computed_defaults_without_panicking() {
        let routes = real_routes();
        let overrides = real_traffic_override();
        overrides
            .validate()
            .expect("the real shipped maritime traffic override must be valid");

        let resolved = resolve_maritime_traffic(&routes, Some(&overrides), 987_654_321);
        assert_eq!(resolved.len(), routes.routes.len());
    }

    /// COW-24, measured: of the 15 authored maritime routes connecting two
    /// real settlements, the `length_m >= 8_000.0` heuristic yields 4
    /// `LongHaul` and 11 `Coastal` -- the island crossings plus the one
    /// long mainland run. Across all 36 authored routes (including the 21
    /// `External`/`Waypoint`-ending ones the heuristic also applies to
    /// uniformly), the split is 8 `LongHaul` and 28 `Coastal`. This test
    /// pins those numbers so a future change to the heuristic or the
    /// export is a deliberate, visible decision rather than a silent
    /// drift.
    #[test]
    fn real_route_tier_split_matches_measured_counts() {
        let routes = real_routes();
        let resolved = resolve_maritime_traffic(&routes, None, 0);

        let site_to_site_ids: HashSet<&str> = routes
            .routes
            .iter()
            .filter(|route| route.stops.iter().filter_map(|s| s.site_id()).count() == 2)
            .map(|route| route.id.as_str())
            .collect();
        assert_eq!(
            site_to_site_ids.len(),
            15,
            "expected 15 Site<->Site maritime routes in the real export"
        );

        let (site_long_haul, site_coastal): (Vec<_>, Vec<_>) = resolved
            .iter()
            .filter(|(id, _)| site_to_site_ids.contains(id.as_str()))
            .partition(|(_, traffic)| traffic.tier == MaritimeRouteTier::LongHaul);
        assert_eq!(site_long_haul.len(), 4);
        assert_eq!(site_coastal.len(), 11);

        let (all_long_haul, all_coastal): (Vec<_>, Vec<_>) = resolved
            .values()
            .partition(|traffic| traffic.tier == MaritimeRouteTier::LongHaul);
        assert_eq!(all_long_haul.len(), 8);
        assert_eq!(all_coastal.len(), 28);
    }

    #[test]
    fn real_maritime_traffic_override_loads_through_the_asset_manager() {
        let traffic = AuthoredCromatolisMaritimeTraffic::load_owned(
            "world.maritime.cromatolis_v0_maritime_traffic",
        )
        .expect("the shipped maritime traffic override must be loadable by asset id");
        traffic
            .validate()
            .expect("the shipped maritime traffic override must be valid");
    }
}
