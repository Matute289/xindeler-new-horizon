//! Order- and terrain-independent RNG seeds for civ generation in the
//! authored Cromatolis world.
//!
//! # The problem this solves
//!
//! Upstream `Civs::generate` draws every site's generator seed, in order,
//! from one shared `ChaChaRng` (`GenCtx::reseed`), and the road carver
//! (`Civs::carve_track_into_terrain`) draws from that *same* stream: two
//! numbers per interior road node (the random chunk offset) plus a 32-byte
//! reseed per procedural bridge. In the authored Cromatolis world the
//! procedural route connectors run before the site loop, and their length
//! comes from an A* whose step cost reads chunk altitudes. So a terrain edit
//! anywhere on the map that changes the *number of nodes* of any connector
//! shifts the shared stream and hands every later site a different seed: a
//! heightmap re-encode that took the connectors from 1,824 to 1,821 nodes
//! re-rolled every multi-plot town at once.
//!
//! # What this module does instead (Cromatolis only)
//!
//! * **Per-site seeds.** Each site's generator RNG is seeded from
//!   `SHA-256(domain, world_seed, stable_site_key)`
//!   ([`CivSeedPolicy::site_seed`]); see [`site_seed_key`] for the key of every
//!   kind of site and how stable each one is.
//! * **Per-road RNG.** Each procedural Cromatolis connector carves with its own
//!   `ChaChaRng` seeded from `SHA-256(domain, world_seed, road_key)`
//!   ([`road_seed`]), keyed by the ordered pair of its endpoints' authored ids,
//!   so a road's node count can only ever change that road's own offsets (and
//!   the bridges it creates) -- never another road or a site.
//!
//! * **Plaza market stands** (`site::plot::plaza`) draw from a sub-RNG, so a
//!   stand attempt that an altitude truncation lets succeed or fail can't
//!   change how many numbers the rest of the town consumes.
//!
//! # Gating
//!
//! Every one of these decouplings is switched by the same predicate,
//! [`uses_derived_rngs`] (the chunk's authored-region flag): the civ pass
//! asks it of the world's first chunk, the plaza of its own chunk, and
//! authored regions are map-global, so the two always agree. Every other
//! world -- every procedural world, every non-authored map -- keeps drawing
//! from the shared stream exactly as upstream does, byte for byte
//! ([`GenCtx::site_rng`] with `None` is `reseed().rng`, pinned by a test
//! below, and
//! `site_layouts::procedural_world_civ_layer_matches_the_committed_digest` pins
//! a whole procedural world's civ layer, recorded before any of this existed).
//!
//! # Operating notes
//!
//! * **Renaming an authored id re-rolls that site** (and any connector keyed by
//!   it): the id *is* the seed. Rename only together with a reviewed re-bless.
//! * **Coordinates.** `Site::center` and the two ends of a
//!   `SiteKind::Bridge`/`SiteKind::Fortification` are chunk coordinates (the
//!   carver builds bridges from `find_path` nodes, which are chunks), so the
//!   procedural keys below are chunk-quantised: moving a crossing within the
//!   same chunks keeps its key.
//! * **Bumping a domain version** (`SITE_SEED_DOMAIN` / `ROAD_SEED_DOMAIN`)
//!   re-rolls every Cromatolis site, or every connector, once. It requires
//!   updating the pinned constants in `derived_seeds_are_pinned_constants` and
//!   re-blessing both digest files (see below). There is no per-world "seed
//!   version" setting: nothing needs two versions at once yet.
//! * **Regression guard.**
//!   `world/src/cromatolis_generation_tests/site_layouts.rs` digests every
//!   site's layout into `world/src/cromatolis_site_layout_digests.txt` and the
//!   default procedural map's civ layer into
//!   `world/src/procedural_world_civ_digest.txt`. They record **world seed 0
//!   only**: a smoke net for "this edit re-rolled N towns", not a contract
//!   about any other seed. They need the real LFS assets and are `#[ignore]`d,
//!   so they run locally or on the VPS, never on GitHub CI:
//!   `VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release --lib
//!   -- --ignored site_layouts::`. After a deliberate change, re-record with
//!   `XINDELER_BLESS_SITE_LAYOUTS=1` on the same command; the reviewer must
//!   read the per-site list of changed sites the failing run printed (and
//!   commit the new file with the change), never bless blind.
//! * **Terrain experiments.** Under `cfg(test)` only, `sim::test_hooks` lets a
//!   test perturb the loaded heightmap before anything is derived from it
//!   (installed per rayon worker, so concurrent tests are unaffected);
//!   `site_layouts::perturbation_experiment_from_env` uses it to measure how
//!   many towns an edit would re-roll before making it for real.
//!
//! # Why SHA-256
//!
//! The seed must be identical on every machine, every Rust version and every
//! build: a save's world is regenerated from its seed on each server start.
//! `std`'s `DefaultHasher` is explicitly unstable across releases, and
//! `FxHash` is not designed to spread a short key over 32 bytes. `sha2` is
//! already a dependency of this crate (authored raster checks), is a fixed,
//! standardised function, and produces exactly the 32 bytes `ChaChaRng`
//! takes as a seed. Every field is length-prefixed so no two distinct
//! (domain, key) pairs can serialise to the same bytes. The cost (about a
//! hundred hashes per world) is negligible.

use super::{GenCtx, SEED_SKIP, Site};
use crate::site::SiteKind;
use rand::{SeedableRng, prelude::*};
use rand_chacha::ChaChaRng;
use sha2::{Digest, Sha256};
use vek::Vec2;

/// The single gate for every RNG decoupling in this module's doc: true in an
/// authored region, false everywhere else.
pub(crate) fn uses_derived_rngs(chunk: &crate::sim::SimChunk) -> bool {
    chunk.authored_cromatolis_v0
}

/// Domain separator for per-site seeds. Bump the version suffix only as a
/// deliberate, documented re-roll of every Cromatolis site.
const SITE_SEED_DOMAIN: &str = "xindeler.civ.site-seed.v1";
/// Domain separator for per-road (procedural connector) RNGs.
const ROAD_SEED_DOMAIN: &str = "xindeler.civ.road-seed.v1";

/// `SHA-256(len(domain) || domain || world_seed || len(key) || key)`, all
/// integers little-endian.
fn derive_seed(domain: &str, world_seed: u32, key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(world_seed.to_le_bytes());
    hasher.update((key.len() as u64).to_le_bytes());
    hasher.update(key.as_bytes());
    hasher.finalize().into()
}

/// The stable identity a site's seed is derived from.
///
/// Stability properties, by origin:
///
/// * **Authored settlement / landmark / bridge / fortification** -- keyed by
///   the authored id (e.g. `settlement:site.kalthis`). Fully independent of
///   terrain, of generation order and of every other site: the seed changes
///   only if the id itself is renamed in the authored data.
/// * **Procedural bridge** (created by the road carver where a connector jumps
///   a water/cliff gap) -- keyed by its two end chunks, ordered, e.g.
///   `procedural-bridge:89,219:94,219`. Independent of generation order and of
///   how many nodes any road has, but not of terrain *at the crossing*: if an
///   edit moves the road so it crosses the gap elsewhere, the bridge is
///   genuinely a different bridge (different place, different key, different
///   name). Its creation index, its slot in `Civs::sites` and the shared stream
///   are deliberately not used, because all three move whenever any earlier
///   road gains or loses a bridge.
/// * **Any other procedural site** (unreachable in Cromatolis today, kept total
///   so a future kind can't silently fall back to the shared stream) -- keyed
///   by kind and centre chunk. Same caveat: stable unless the site itself
///   moves.
pub(crate) fn site_seed_key(site: &Site) -> String {
    if let Some(settlement) = site.authored.as_ref() {
        return format!("settlement:{}", settlement.id);
    }
    if let Some(landmark) = site.authored_landmark.as_ref() {
        return format!("landmark:{}", landmark.id);
    }
    if let Some(bridge) = site.authored_bridge.as_ref() {
        return format!("bridge:{}", bridge.id);
    }
    if let Some(fortification) = site.authored_fortification.as_ref() {
        return format!("fortification:{}", fortification.id);
    }
    match site.kind {
        SiteKind::Bridge(a, b) => {
            let (lo, hi) = ordered_pair(a, b);
            format!("procedural-bridge:{},{}:{},{}", lo.x, lo.y, hi.x, hi.y)
        },
        SiteKind::Fortification(a, b) => {
            let (lo, hi) = ordered_pair(a, b);
            format!(
                "procedural-fortification:{},{}:{},{}",
                lo.x, lo.y, hi.x, hi.y
            )
        },
        kind => format!(
            "procedural:{}:{},{}",
            procedural_kind_tag(kind),
            site.center.x,
            site.center.y
        ),
    }
}

/// `(min, max)` in lexicographic `(x, y)` order, so a bridge or wall keys
/// the same whichever end the carver happened to reach first.
fn ordered_pair(a: Vec2<i32>, b: Vec2<i32>) -> (Vec2<i32>, Vec2<i32>) {
    if (a.x, a.y) <= (b.x, b.y) {
        (a, b)
    } else {
        (b, a)
    }
}

/// An explicit, frozen spelling per kind -- not `Debug`, whose output is
/// not a stability promise. Exhaustive on purpose: a new `SiteKind` must
/// pick its tag here.
fn procedural_kind_tag(kind: SiteKind) -> &'static str {
    match kind {
        SiteKind::Refactor => "refactor",
        SiteKind::CliffTown => "cliff_town",
        SiteKind::SavannahTown => "savannah_town",
        SiteKind::DesertCity => "desert_city",
        SiteKind::ChapelSite => "chapel_site",
        SiteKind::DwarvenMine => "dwarven_mine",
        SiteKind::CoastalTown => "coastal_town",
        SiteKind::Citadel => "citadel",
        SiteKind::Terracotta => "terracotta",
        SiteKind::GiantTree => "giant_tree",
        SiteKind::Gnarling => "gnarling",
        SiteKind::Bridge(..) => "bridge",
        SiteKind::Fortification(..) => "fortification",
        SiteKind::Adlet => "adlet",
        SiteKind::Haniwa => "haniwa",
        SiteKind::PirateHideout => "pirate_hideout",
        SiteKind::JungleRuin => "jungle_ruin",
        SiteKind::RockCircle => "rock_circle",
        SiteKind::TrollCave => "troll_cave",
        SiteKind::Camp => "camp",
        SiteKind::Cultist => "cultist",
        SiteKind::Sahagin => "sahagin",
        SiteKind::VampireCastle => "vampire_castle",
        SiteKind::GliderCourse => "glider_course",
        SiteKind::Myrmidon => "myrmidon",
    }
}

/// The 32-byte seed of one procedural Cromatolis connector's own RNG, keyed
/// by the ordered (directed) pair of its endpoints' stable site keys
/// ([`site_seed_key`], e.g. `settlement:site.itos_village`) -- what the
/// connector pass searches between (`find_path(target -> anchor)`) -- and
/// therefore independent of the path the search returns. Using the full
/// site key (not just an authored settlement id) means an endpoint without
/// one still gets a unique, stable key instead of an empty string.
pub(super) fn road_seed(world_seed: u32, from_site_key: &str, to_site_key: &str) -> [u8; 32] {
    // Length-prefix each key so no pair of keys can forge another pair's.
    // (`derive_seed` length-prefixes the key as a whole as well; the inner
    // prefixes are what separate the two keys. Dropping either would change
    // every connector's RNG, so both stay.)
    let key = format!(
        "road:{}:{from_site_key}->{}:{to_site_key}",
        from_site_key.len(),
        to_site_key.len()
    );
    derive_seed(ROAD_SEED_DOMAIN, world_seed, &key)
}

/// Whether, and from what, sites get their seed. Built once per
/// `Civs::generate`.
#[derive(Clone, Copy, Debug)]
pub(super) struct CivSeedPolicy {
    world_seed: u32,
    /// `true` only for the authored Cromatolis path.
    derived: bool,
}

impl CivSeedPolicy {
    pub(super) fn new(world_seed: u32, derived: bool) -> Self {
        Self {
            world_seed,
            derived,
        }
    }

    /// `Some(seed)` when this world derives per-site seeds, `None` when the
    /// site must keep drawing from the shared stream (every non-Cromatolis
    /// world).
    pub(super) fn site_seed(&self, site: &Site) -> Option<[u8; 32]> {
        self.derived
            .then(|| derive_seed(SITE_SEED_DOMAIN, self.world_seed, &site_seed_key(site)))
    }

    /// The minimum building count this site's generated layout must reach
    /// ([`MIN_BUILDINGS_BY_SIZE`]), or `None` if it isn't held to one: only
    /// authored settlements of a banded category, and only where seeds are
    /// derived at all.
    pub(super) fn layout_band(&self, site: &Site) -> Option<usize> {
        if !self.derived {
            return None;
        }
        let settlement = site.authored.as_ref()?;
        min_buildings_for(
            settlement.category.contract_key(),
            settlement.size.contract_key(),
        )
    }

    /// The seed of retry `attempt` (1-based) of a site whose first layout
    /// fell below its band: same stable key, a distinct derived suffix, so
    /// the sequence of retries is as reproducible as the first draw.
    pub(super) fn layout_retry_seed(&self, site: &Site, attempt: u32) -> [u8; 32] {
        derive_seed(
            SITE_SEED_DOMAIN,
            self.world_seed,
            &format!("{}#layout-retry-{attempt}", site_seed_key(site)),
        )
    }
}

impl<R: Rng> GenCtx<'_, R> {
    /// The RNG one site's generator draws from.
    ///
    /// `Some(seed)` (Cromatolis) seeds it directly and does **not** touch the
    /// shared stream. `None` is exactly `self.reseed().rng` -- same draw from
    /// the shared stream, same `SEED_SKIP` tweak -- spelled out here only
    /// because `reseed`'s return type is opaque and cannot be named next to a
    /// `ChaChaRng`. `site_rng_none_is_bit_identical_to_reseed` pins the two
    /// together.
    pub(super) fn site_rng(&mut self, derived_seed: Option<[u8; 32]>) -> ChaChaRng {
        match derived_seed {
            Some(seed) => ChaChaRng::from_seed(seed),
            None => {
                let mut entropy = self.rng.random::<[u8; 32]>();
                entropy[0] = entropy[0].wrapping_add(SEED_SKIP);
                ChaChaRng::from_seed(entropy)
            },
        }
    }

    /// A context over the same `WorldSim` that draws from `rng` instead of
    /// the shared stream (used to give one road its own RNG).
    pub(super) fn with_rng<R2: Rng>(&mut self, rng: R2) -> GenCtx<'_, R2> {
        GenCtx { sim: self.sim, rng }
    }
}

/// Minimum building count (plots that are not plazas, roads, farm fields or
/// bridges, see [`building_count`]) an authored settlement of each authored
/// size must reach; a first layout below it is re-drawn
/// ([`keep_layout_within_band`]). Judgement values: each sits a little under
/// the smallest count any settlement of that size reached on the roll the
/// old shared-stream seeds gave at world seed 0 (very large 60 bar Kalthis,
/// large 40, medium 23, small 7), so only a clearly starved roll trips it.
/// A stop-gap floor, not a size model: settlements of one category should
/// eventually be *sized* by their authored footprint, not re-drawn.
///
/// TODO: these are balance numbers and belong in data, next to the sizes
/// they describe -- the authored settlement template contract RON, keyed by
/// `AuthoredSettlementSize::contract_key()` -- not in code.
pub(crate) const MIN_BUILDINGS_BY_SIZE: &[(&str, usize)] = &[
    ("very_large", 45),
    ("large", 40),
    ("medium", 20),
    ("small", 6),
    ("minimal", 1),
];
/// Settlement categories the bands apply to. Inns and posts are single
/// structures and are never re-drawn.
pub(crate) const SIZE_BANDED_CATEGORIES: &[&str] =
    &["capital", "city", "town", "village", "hamlet"];
/// Upper bound on re-draws per settlement, so a site whose terrain can't fit
/// its band costs a bounded amount of generation time.
pub(crate) const MAX_LAYOUT_RETRIES: u32 = 16;

/// The band for an authored `(category, size)` pair, by contract keys.
pub(crate) fn min_buildings_for(category: &str, size: &str) -> Option<usize> {
    if !SIZE_BANDED_CATEGORIES.contains(&category) {
        return None;
    }
    MIN_BUILDINGS_BY_SIZE
        .iter()
        .find(|(s, _)| *s == size)
        .map(|(_, n)| *n)
}

/// Plots that are buildings: everything but plazas, roads, farm fields and
/// bridges.
pub(crate) fn building_count(site: &crate::site::Site) -> usize {
    use crate::site::PlotKind;
    site.plots()
        .filter(|plot| {
            !matches!(
                plot.kind(),
                PlotKind::Plaza(_)
                    | PlotKind::Road(_)
                    | PlotKind::FarmField(_)
                    | PlotKind::Bridge(_)
            )
        })
        .count()
}

/// Keeps `first` if it reaches `band`; otherwise re-draws with `retry(1)`,
/// `retry(2)`, ... up to [`MAX_LAYOUT_RETRIES`] and keeps the first attempt
/// that does. If none does, keeps the largest attempt (the earliest on a
/// tie) and warns. A site already within its band is never re-drawn, so a
/// terrain edit can only bring retries into play for a town whose own first
/// draw it pushed below the band.
///
/// Generic over the attempt type so the caller can carry per-attempt state
/// (the generated site plus its own generation statistics) and so the
/// selection rule is unit-testable without generating a town. `buildings`
/// measures an attempt; `site_key` is only called to label a log line.
pub(super) fn keep_layout_within_band<T>(
    first: T,
    band: Option<usize>,
    buildings: impl Fn(&T) -> usize,
    site_key: impl Fn() -> String,
    mut retry: impl FnMut(u32) -> T,
) -> T {
    let Some(min) = band else {
        return first;
    };
    let first_count = buildings(&first);
    if first_count >= min {
        return first;
    }
    let (mut best, mut best_count) = (first, first_count);
    for attempt in 1..=MAX_LAYOUT_RETRIES {
        let candidate = retry(attempt);
        let count = buildings(&candidate);
        if count >= min {
            tracing::info!(
                site_key = %site_key(), attempt, first_count, count, min,
                "Re-drew a settlement layout that fell below its size band"
            );
            return candidate;
        }
        if count > best_count {
            (best, best_count) = (candidate, count);
        }
    }
    tracing::warn!(
        site_key = %site_key(), first_count, best_count, min,
        retries = MAX_LAYOUT_RETRIES,
        "No settlement layout reached its size band; keeping the largest attempt"
    );
    best
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            AuthoredLandmarkKind, AuthoredLandmarkMeta, AuthoredSettlementCategory,
            AuthoredSettlementMeta, AuthoredSettlementPeople, AuthoredSettlementPopulation,
            AuthoredSettlementPopulationTag, AuthoredSettlementSize,
        },
        *,
    };
    use crate::sim::WorldSim;
    use common::store::Id;

    fn bare_site(kind: SiteKind, center: Vec2<i32>) -> Site {
        Site {
            kind,
            site_tmp: None,
            center,
            place: Id::new(0),
            authored: None,
            authored_landmark: None,
            authored_bridge: None,
            authored_fortification: None,
        }
    }

    fn settlement(id: &str, center: Vec2<i32>) -> Site {
        let mut site = bare_site(SiteKind::Refactor, center);
        site.authored = Some(AuthoredSettlementMeta {
            id: id.to_string(),
            name: id.to_string(),
            category: AuthoredSettlementCategory::Town,
            size: AuthoredSettlementSize::Medium,
            population: AuthoredSettlementPopulation {
                tag: AuthoredSettlementPopulationTag::Human,
                peoples: vec![AuthoredSettlementPeople::Human],
                future_peoples: Vec::new(),
            },
            requires_capital_castle: false,
            start_eligible: true,
        });
        site
    }

    fn hex(seed: [u8; 32]) -> String { seed.iter().map(|b| format!("{b:02x}")).collect() }

    /// Golden values (cross-checked against Python's `hashlib.sha256` over
    /// the same byte layout): if these change, every Cromatolis site and
    /// connector re-rolls. Only ever update them together with a deliberate
    /// domain version bump.
    #[test]
    fn derived_seeds_are_pinned_constants() {
        assert_eq!(
            hex(derive_seed(SITE_SEED_DOMAIN, 0, "settlement:site.kalthis")),
            "cc1be8c8abee13137a2338269257d71b3ab735e71a2bda8f4486fef9241c6bb3",
        );
        assert_eq!(
            hex(road_seed(
                0,
                "settlement:site.bronze_shore",
                "settlement:site.kalthis"
            )),
            "71f07160decb57ea2ffc4ff32ba58689aa9e7104c0f80499a6fc8564d366cd44",
        );
    }

    #[test]
    fn site_seed_depends_on_world_seed_and_key_only() {
        let kalthis = settlement("site.kalthis", Vec2::new(10, 10));
        let moved_kalthis = settlement("site.kalthis", Vec2::new(500, 3));
        let duren = settlement("site.duren", Vec2::new(10, 10));
        let policy = CivSeedPolicy::new(7, true);
        // Position is irrelevant for an authored site; identity is all.
        assert_eq!(policy.site_seed(&kalthis), policy.site_seed(&moved_kalthis));
        assert_ne!(policy.site_seed(&kalthis), policy.site_seed(&duren));
        assert_ne!(
            policy.site_seed(&kalthis),
            CivSeedPolicy::new(8, true).site_seed(&kalthis)
        );
        // Upstream / non-Cromatolis worlds never derive.
        assert_eq!(CivSeedPolicy::new(7, false).site_seed(&kalthis), None);
    }

    #[test]
    fn site_seed_keys_are_disjoint_across_origins() {
        let mut landmark = bare_site(SiteKind::GiantTree, Vec2::new(1, 2));
        landmark.authored_landmark = Some(AuthoredLandmarkMeta {
            id: "site.kalthis".to_string(),
            name: String::new(),
            kind: AuthoredLandmarkKind::TreeOfLife,
            profile: None,
        });
        assert_eq!(
            site_seed_key(&settlement("site.kalthis", Vec2::new(1, 2))),
            "settlement:site.kalthis"
        );
        assert_eq!(site_seed_key(&landmark), "landmark:site.kalthis");
    }

    #[test]
    fn procedural_keys_ignore_endpoint_order_and_creation_order() {
        let (a, b) = (Vec2::new(94, 219), Vec2::new(89, 219));
        let ab = bare_site(SiteKind::Bridge(a, b), (a + b) / 2);
        let ba = bare_site(SiteKind::Bridge(b, a), (a + b) / 2);
        assert_eq!(site_seed_key(&ab), "procedural-bridge:89,219:94,219");
        assert_eq!(site_seed_key(&ab), site_seed_key(&ba));
        assert_eq!(
            site_seed_key(&bare_site(SiteKind::Camp, Vec2::new(3, -4))),
            "procedural:camp:3,-4"
        );
    }

    #[test]
    fn road_seeds_depend_on_direction_world_seed_and_both_ids() {
        let ab = road_seed(0, "site.a", "site.b");
        assert_ne!(ab, road_seed(0, "site.b", "site.a"));
        assert_ne!(ab, road_seed(1, "site.a", "site.b"));
        // Ids containing the separator can't forge another pair's key.
        assert_ne!(
            road_seed(0, "site.a->x", "y"),
            road_seed(0, "site.a", "x->y")
        );
    }

    /// The core property, asset-free: carving a road with *more* nodes (more
    /// draws from its own RNG) changes neither another road's draws nor any
    /// site seed -- where the old shared stream shifted all of them.
    #[test]
    fn a_roads_node_count_cannot_shift_another_road_or_any_site() {
        let mut sim = WorldSim::empty();
        let sites = [
            settlement("site.kalthis", Vec2::new(5, 5)),
            settlement("site.duren", Vec2::new(9, 5)),
        ];
        let policy = CivSeedPolicy::new(0, true);

        let run = |sim: &mut WorldSim, road_a_nodes: usize| {
            let mut ctx = GenCtx {
                sim,
                rng: ChaChaRng::from_seed([3; 32]),
            };
            let mut offsets = Vec::new();
            for (from, to, nodes) in [("site.a", "site.b", road_a_nodes), ("site.c", "site.d", 10)]
            {
                let mut road = ctx.with_rng(ChaChaRng::from_seed(road_seed(0, from, to)));
                // The 2 draws per interior node `carve_track_into_terrain` makes.
                let drawn: Vec<(i32, i32)> = (0..nodes)
                    .map(|_| {
                        (
                            road.rng.random_range(-16..17),
                            road.rng.random_range(-16..17),
                        )
                    })
                    .collect();
                offsets.push(drawn);
            }
            let site_draws: Vec<u64> = sites
                .iter()
                .map(|site| ctx.site_rng(policy.site_seed(site)).random::<u64>())
                .collect();
            (offsets, site_draws)
        };

        let (short_offsets, short_sites) = run(&mut sim, 20);
        let (long_offsets, long_sites) = run(&mut sim, 23);
        assert_eq!(short_offsets[1], long_offsets[1], "road B must not move");
        assert_eq!(short_sites, long_sites, "no site seed may move");
        assert_eq!(short_offsets[0][..], long_offsets[0][..20]);
    }

    #[test]
    fn size_bands_cover_every_banded_category_and_size() {
        assert_eq!(min_buildings_for("city", "large"), Some(40));
        assert_eq!(min_buildings_for("capital", "very_large"), Some(45));
        assert_eq!(min_buildings_for("inn", "minimal"), None);
        assert_eq!(min_buildings_for("post", "minimal"), None);
        for &category in SIZE_BANDED_CATEGORIES {
            for size in ["very_large", "large", "medium", "small", "minimal"] {
                assert!(min_buildings_for(category, size).is_some());
            }
        }
    }

    /// Runs `keep_layout_within_band` over plain numbers ("buildings" =
    /// the value itself) with scripted retries, returning the kept value and
    /// the retry attempts that were requested.
    fn pick(first: usize, band: Option<usize>, retries: &[usize]) -> (usize, Vec<u32>) {
        let mut asked = Vec::new();
        let kept = keep_layout_within_band(
            first,
            band,
            |n: &usize| *n,
            || "test".to_string(),
            |attempt| {
                asked.push(attempt);
                retries.get(attempt as usize - 1).copied().unwrap_or(0)
            },
        );
        (kept, asked)
    }

    #[test]
    fn layout_band_keeps_an_in_band_first_draw_without_retrying() {
        assert_eq!(pick(50, Some(40), &[99]), (50, vec![]));
        assert_eq!(pick(40, Some(40), &[99]), (40, vec![]));
    }

    #[test]
    fn layout_band_without_a_band_returns_the_first_draw() {
        assert_eq!(pick(3, None, &[99]), (3, vec![]));
    }

    #[test]
    fn layout_band_takes_the_first_retry_that_reaches_the_band() {
        assert_eq!(pick(10, Some(40), &[20, 41, 90]), (41, vec![1, 2]));
    }

    #[test]
    fn layout_band_falls_back_to_the_largest_attempt_earliest_on_a_tie() {
        // Retries 2 and 4 tie at 30; nothing reaches 40: keep the earliest 30
        // after trying every retry. A tagged value tells the two 30s apart.
        let mut script = vec![(0usize, 0u32); MAX_LAYOUT_RETRIES as usize];
        script[1] = (30, 2);
        script[3] = (30, 4);
        let mut asked = 0;
        let kept = keep_layout_within_band(
            (10usize, 0u32),
            Some(40),
            |(n, _): &(usize, u32)| *n,
            || "test".to_string(),
            |attempt| {
                asked += 1;
                script[attempt as usize - 1]
            },
        );
        assert_eq!(kept, (30, 2));
        assert_eq!(asked, MAX_LAYOUT_RETRIES);
        // A first draw no retry beats is kept.
        assert_eq!(pick(25, Some(40), &[]).0, 25);
    }

    /// Gating guarantee: the non-derived path is byte-for-byte upstream's
    /// `reseed()`, so every procedural / non-Cromatolis world is unchanged.
    #[test]
    fn site_rng_none_is_bit_identical_to_reseed() {
        let mut sim_a = WorldSim::empty();
        let mut sim_b = WorldSim::empty();
        let mut a = GenCtx {
            sim: &mut sim_a,
            rng: ChaChaRng::from_seed([42; 32]),
        };
        let mut b = GenCtx {
            sim: &mut sim_b,
            rng: ChaChaRng::from_seed([42; 32]),
        };
        for _ in 0..16 {
            let mut upstream = a.reseed().rng;
            let mut ours = b.site_rng(None);
            let x: [u64; 8] = upstream.random();
            let y: [u64; 8] = ours.random();
            assert_eq!(x, y);
        }
        // And both left the shared stream in the same state.
        assert_eq!(a.rng.random::<u64>(), b.rng.random::<u64>());
    }
}
