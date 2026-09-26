//! The shoreline search that makes a waterfront plot placeable at all, and
//! the pass that claims its footprint.
//!
//! This lives in its own module rather than inside `site/mod.rs` for the same
//! reason `cromatolis_generation_tests.rs` lives outside `lib.rs`:
//! `site/mod.rs` is upstream-owned and upstream's `master` keeps editing it,
//! so a several-hundred-line fork-only block bolted into the middle of it is
//! the noisiest possible shape for the monthly merge. All that is added to
//! upstream's own file is the placement pass's call inside `generate_city`
//! and one promoted constant.
//!
//! # Why a new search is needed at all
//!
//! The shoreline is the one place the site generator is structurally
//! guaranteed never to build, and the guarantee is enforced in **three
//! independent places** — so a patch aimed at one of them appears to work
//! and then fails on a different settlement:
//!
//! * `Site::demarcate_obstacles` pre-stamps the waterline as
//!   `TileKind::Hazard(HazardKind::Water)` once, before any plot exists, and
//!   `SQUARE_4` dilates that stamp *inland*, so the first non-hazard tile
//!   already sits roughly 6–18 blocks back from the real waterline. A deck
//!   sized from the first hazard tile ends on dry sand.
//! * `TileGrid::grow_aabr` refuses to grow into a hazard tile — via
//!   `Tile::is_empty`, **not** `Tile::is_obstacle`, so patching `is_obstacle`
//!   would change nothing.
//! * `TileGrid::find_near` caps its spiral at 70 blocks (~11 tiles) from the
//!   seed, so a search seeded from a road node cannot see the shore of a
//!   settlement whose centre sits further back than that — measured on the
//!   authored region, settlement origins sit 12–39 tiles from their own
//!   waterfront.
//!
//! None of the three is loosened here. `HazardKind` has two producers and
//! three consumers repo-wide and is doing real work for every other plot in
//! every settlement in the game; relaxing it would let houses creep into the
//! surf world-wide for one plot's benefit. **The port asks for an exception,
//! not a policy change.**
//!
//! What makes the exception legal is that `Site::blit_aabr` writes tiles
//! *unconditionally* — no emptiness check, no hazard check. Claiming water
//! tiles has always been allowed; only *finding* them was forbidden. So this
//! module replaces the search (`shore_frontier` in place of `find_near`'s
//! spiral) and leaves the write path completely alone.
//!
//! # The shape of the answer
//!
//! A naval port is the one plot that has to satisfy two contradictory
//! adjacency requirements at once: its landward edge must touch a road, or
//! nobody can reach it, and its seaward edge must touch navigable water, or
//! no hull can moor. No single `grow_aabr` call can produce that, because the
//! two conditions live on opposite sides of a band the function refuses to
//! enter. So the footprint is two rectangles sharing an edge:
//!
//! ```text
//!          inland                                 seaward
//!    +-------------------+      +---------------------------------+
//!    |  APRON            |      |  DECK                           |
//!    |  ordinary land    |<---->|  claimed hazard + water tiles   |
//!    |  aabr, found by   |hinge |  (blitted, never grown)         |
//!    |  `grow_aabr`      |      |                                 |
//!    +-------------------+      +---------------------------------+
//! ```
//!
//! The apron is found the ordinary way, so it inherits every existing
//! exclusion — houses, plazas, fields, hills — for free. The deck is a
//! fixed-dimension rectangle projected from the apron's seaward face along
//! the shore normal: there is nothing to search, the tiles are claimed.
//!
//! **Nothing here renders geometry.** The apron and deck are tile-grid claims
//! only; the plot kind and the voxel geometry that fill them are a separate
//! concern, and until that plot exists these tiles carry no `plot` id, so
//! `Site::render` (which iterates plots) draws nothing for them.
//!
//! That is not the same as "changes nothing in the world", and the difference
//! is worth knowing before walking one of these settlements.
//! `Site::spawn_rules` treats any tile that is not `is_natural()` as a warp
//! anchor, so claiming the footprint lowers `max_warp` and clears
//! `spawn_rules.trees` in a `SQUARE_9` ring around it: the shoreline strip
//! stops being warped and stops growing trees. There is no altitude flattening
//! (the deck carries no `hard_alt` and neither rectangle carries a `plot`, so
//! nothing calls `prefer_alt`), so that is the whole of the visible effect —
//! almost certainly wanted, but it is the thing to look at first when someone
//! walks one of the thirteen.
//!
//! # A note on where the tier numbers should end up
//!
//! The scoring weights, scales and caps below describe the *search* and belong
//! in code: a designer cannot set them meaningfully without the algorithm in
//! front of them, and two of them ([`SHORE_FRONTIER_MARGIN`] and
//! [`SHORE_MAX_DECK_GAP`]) are derived from other engine constants, so moving
//! them to a data file would let a silently incoherent combination ship.
//!
//! [`PortClass::apron_dims`] and [`PortClass::deck_dims`] are a different
//! thing: a per-tier table of how big each kind of port is, which is a designer
//! question rather than an algorithm parameter. They stay here for now because
//! nothing else consumes them yet and they are the numbers this search was
//! measured against — but once the plot kind lands and berth counts and finger
//! pier dimensions need somewhere to live too, the table wants to be a RON
//! asset with these values as the defaults it overrides. Recorded so the
//! decision does not have to be re-argued from scratch.

use super::*;
use std::{collections::VecDeque, fmt};
use tracing::warn;

// ---------------------------------------------------------------------------
// Scoring weights and scales.
//
// These are named constants with a note on what each one prevents, rather
// than magic numbers, because they are expected to be retuned from what an
// in-client walkthrough actually shows: the main risk this search carries is
// producing piers in silly places, and a bare `3.0` buried in an expression
// costs a re-read every time that happens.
//
// The score is a *cost*: lower is better.
// ---------------------------------------------------------------------------

/// Weight on `d_road`. Prevents a pier that is geometrically perfect and
/// unreachable — a deck at the bottom of an untouched stretch of coast with
/// no road within walking distance, which reads as scenery rather than
/// infrastructure and which NPC pathing can never use.
const SHORE_W_D_ROAD: f32 = 3.0;

/// Weight on `water_run`. Prevents a pier into a puddle: a river-mouth notch,
/// a one-chunk pond, or the inland side of a sand spit all produce
/// `Hazard(Water)` tiles that look exactly like open sea to a single-tile
/// adjacency test. The run is the only term that distinguishes them.
const SHORE_W_WATER_RUN: f32 = 4.0;

/// Weight on `alt_var`. Prevents a pier at a cliff foot. The hazard band
/// happily runs along the base of a sea cliff, and an apron grown there is
/// buildable by every existing rule while being unreachable on foot and
/// visually absurd.
const SHORE_W_ALT_VAR: f32 = 6.0;

/// Weight on `d_centre`. Prevents a port a mile down the coast from the
/// settlement it belongs to. Without it a wide, deep, empty bay several
/// hundred metres away outscores the town's own narrower waterfront every
/// time — which is exactly what an unweighted version of this scan did when
/// it was first measured.
const SHORE_W_D_CENTRE: f32 = 2.5;

/// Reference scale for `d_road`, in tiles (24 tiles = 144 blocks). Not a
/// cutoff — the term keeps rising past it, so a candidate 40 tiles from a
/// road is still ranked worse than one at 30.
const SHORE_D_ROAD_SCALE: f32 = 24.0;

/// How far seaward `water_run` probes, in tiles (24 tiles = 144 blocks).
/// Also the value the term is normalised against, so a candidate with open
/// water all the way out contributes nothing to the cost.
const SHORE_WATER_RUN_PROBE: i32 = 24;

/// Reference scale for `alt_var`, in blocks of standard deviation. Roughly
/// "a slope steep enough that a flat apron would be a retaining wall".
const SHORE_ALT_VAR_SCALE: f32 = 12.0;

/// Reference scale for `d_centre`, in tiles (40 tiles = 240 blocks) — about
/// how far a real settlement's own waterfront can reasonably be from its
/// centre, measured against the 43–412 block spread observed on the authored
/// region.
const SHORE_D_CENTRE_SCALE: f32 = 40.0;

/// Side of the square lattice `alt_var` samples over the prospective apron.
/// A 4×4 lattice is 16 `get_alt_approx` calls per candidate instead of one
/// per apron tile (up to 240 for a `Harbour`); the variance of 16 samples
/// spread across the footprint is a perfectly good cliff detector, and the
/// difference is ~26k versus ~380k terrain samples per settlement.
const SHORE_ALT_SAMPLE_LATTICE: i32 = 4;

/// How many scored candidates the placement search will try to grow an apron
/// on before giving up.
///
/// Frontier candidate counts on the authored region run to a little over 1600
/// per settlement, and growth *failing* is the normal case rather than the
/// exception: the tiles flanking a waterline are usually still inside the
/// inland-dilated hazard band, so most candidates cannot host the tier's
/// apron at all. So this is a bound on pathological cases, not a budget —
/// thinning the ranking or stopping early means stopping at the first pocket
/// wide enough instead of the best one, which is exactly how a port ends up
/// down the coast from the town it serves.
///
/// The cost is bounded and small: each attempt is a handful of `grow_aabr`
/// calls over a few hundred tiles of pure grid lookups, against
/// `demarcate_obstacles`, which has already sampled real terrain nine times
/// for each of ~37k tiles of the same site.
const SHORE_MAX_CANDIDATE_ATTEMPTS: usize = 2048;

/// The widest hazard band, in tiles, the search will let a deck span.
///
/// The apron can essentially never reach the waterline: `SQUARE_4` dilates the
/// hazard stamp inland, a steep shore adds a band of hill hazard on top of
/// that, and `grow_aabr` will not enter either — so an apron a few tiles back
/// with the deck spanning the remainder is the normal outcome, not a fallback.
/// What this refuses is the pathological version, an apron set so far back
/// that its deck becomes a viaduct to nowhere.
///
/// Twelve tiles is 72 blocks, and it is a measured number rather than a
/// guessed one: on the authored region the widest band actually used is 11
/// tiles, and every value below about 10 starts costing whole settlements —
/// at 6 tiles two of the thirteen had no viable waterfront at all, and five
/// more were pushed two to four times further from their own centre, because
/// a candidate that cannot span the band loses to one further down the coast
/// that can. Raising it past 12 changes nothing, which is how we know 12 is
/// clear of the terrain rather than clamping it.
const SHORE_MAX_DECK_GAP: i32 = 12;

/// Candidates whose cost is within this band of the best one are treated as a
/// tie and one is picked at random. Purely cosmetic: it stops every
/// settlement's port from landing on the single mathematically optimal tile
/// of a symmetric shoreline, which reads mechanical. Deterministic for a
/// given world seed, since the RNG is the site's own.
const SHORE_SCORE_TIE_BAND: f32 = 0.02;

/// The water depth in blocks a deck has to reach for a port to be worth
/// placing at all: the minimum under a `Small` berth, the weakest slot any
/// tier offers.
///
/// Deliberately *not* the tier's own best berth class. Whether a particular
/// berth can take the larger hull is a per-berth question, answered where
/// berths are emitted and their measured depth recorded — gating *placement* on
/// the deeper figure would conflate "this settlement can have a port" with
/// "this settlement can take the largest hull", and would leave a town with a
/// shelving foreshore no port at all rather than a port with one small berth.
const SHORE_MIN_BERTH_DEPTH: f32 = 3.0;

/// How many tiles along a deck's centre line must sit over water at least
/// [`SHORE_MIN_BERTH_DEPTH`] deep.
///
/// Measured along the **centre line**, not over the whole rectangle, and that
/// is the whole point of the gate: a wide deck can clip a deep channel with one
/// corner while its centre runs the entire way out over sand, and an area count
/// would happily pass it. Two tiles is 12 blocks of deck over water deep enough
/// to moor against, which is the least that makes a port a port.
pub const SHORE_MIN_DEEP_TILES: i32 = 2;

/// Tiles trimmed off [`Site::OBSTACLE_SEARCH_RADIUS`] when scanning for the
/// frontier.
///
/// `demarcate_obstacles` spirals a square domain and stamps a `SQUARE_4` block
/// per hit, so its outermost ring or two is never written: every tile there
/// reads `Empty` purely because nothing evaluated it. Scanned naively, that
/// boundary looks exactly like a coastline — open ground on one side, water
/// hazard on the other — and it is the *easiest* place in the whole domain to
/// grow an apron, precisely because nothing was ever stamped to get in the
/// way. Measured, those artefacts were a third to two thirds of every
/// settlement's candidate pool and repeatedly outranked the real waterfront.
const SHORE_FRONTIER_MARGIN: u32 = 2;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The naval-port tier a `generate_city` call site can request for a
/// settlement's waterfront.
///
/// Engine-general and deliberately carrying no authoring vocabulary: it does
/// not mention any authored-map category or other private, `Deserialize`-only
/// concept, so it can cross into the generic site generator that every
/// settlement runs through without dragging a content-adapter dependency
/// upward. An authored map maps its own settlement categories onto these four
/// at its own call site, where that vocabulary belongs.
///
/// The tier drives the apron and deck dimensions the shoreline search asks for
/// ([`PortClass::apron_dims`] and [`PortClass::deck_dims`] below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortClass {
    Jetty,
    Pier,
    Quay,
    Harbour,
}

/// A circular keep-out region the shoreline search must not place a port in.
///
/// Engine-general on purpose: a centre and a radius, nothing else. Only the
/// caller knows what kind of region it is describing — an authored landmark's
/// footprint, a reserved plot, whatever a later producer of no-build regions
/// decides — and therefore only the caller can know how much clearance that
/// region deserves. So the radius arrives already padded and this module
/// neither names nor second-guesses what is inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortExclusion {
    /// World-space centre of the region.
    pub centre_wpos: Vec2<i32>,
    /// Radius in blocks, clearance included.
    pub radius: i32,
}

/// Everything a `generate_city` call site supplies for the waterfront
/// placement pass.
///
/// Bundled into one struct rather than passed as loose parameters so that
/// later additions (an authored facing, a berth-count override) can add a
/// field without another signature change rippling through every
/// `generate_city` call site in the workspace.
#[derive(Debug, Clone, Copy)]
pub struct NavalPortRequest<'a> {
    /// The tier to build. Derived from the settlement's authored category at
    /// the call site, where the authoring vocabulary belongs.
    pub class: PortClass,
    /// Regions the shoreline search must avoid; may be empty.
    pub exclusions: &'a [PortExclusion],
}

impl<'a> NavalPortRequest<'a> {
    pub fn new(class: PortClass, exclusions: &'a [PortExclusion]) -> Self {
        Self { class, exclusions }
    }
}

/// One shoreline frontier candidate: a buildable land tile with water within
/// reach along one cardinal, across the hazard band between them.
///
/// Note what this deliberately is *not*: "a water tile with a non-hazard
/// cardinal neighbour". That is the obvious definition and it finds almost
/// nothing on real terrain. `wpos_is_hazard` classifies a steep tile as
/// `HazardKind::Hill`, and a coastline is steep, so on the authored region the
/// water blob is separated from open land by a band of *hill* hazard almost
/// everywhere — measured, an adjacency-only scan found its candidates only
/// where the demarcated domain runs out, which is an artefact of the domain
/// edge rather than a shoreline. Looking outward *across* the band from the
/// land side is what finds the real waterfront, and it hands back the width of
/// the band as a bonus, which is exactly what the deck has to span.
#[derive(Debug, Clone, Copy)]
pub struct ShoreFrontierTile {
    /// The empty land tile the apron grows from.
    pub land_tpos: Vec2<i32>,
    /// The first `Hazard(HazardKind::Water)` tile found along `outward`.
    pub water_tpos: Vec2<i32>,
    /// Unit cardinal pointing from `land_tpos` out to sea. The deck is
    /// projected along this.
    pub outward: Vec2<i32>,
    /// Hazard tiles lying between the two, i.e. the band the deck must cross.
    /// Zero when the land tile and the water tile are cardinally adjacent.
    pub band: i32,
}

/// The two-part footprint the placement pass blits.
#[derive(Debug, Clone, Copy)]
pub struct ShorePlacement {
    /// The tier this was sized for.
    pub class: PortClass,
    /// Landward half: ordinary ground, grown with `grow_aabr`, so it overlaps
    /// nothing. Min-inclusive / max-exclusive, like every other tile aabr in
    /// this module.
    pub apron: Aabr<i32>,
    /// Seaward half: claimed hazard and water tiles, projected — never grown.
    pub deck: Aabr<i32>,
    /// The shared edge: the apron's seaward face, one tile deep. This is
    /// where a ramp from apron grade to deck grade belongs.
    pub hinge: Aabr<i32>,
    /// A tile on the apron's *landward* face, chosen as the one closest to a
    /// road. Where NPCs and players enter the port from the town.
    pub door_tile: Vec2<i32>,
    /// The outward (seaward) shore normal, as a unit cardinal.
    pub outward: Vec2<i32>,
    /// Ground altitude claimed for the apron. The deck deliberately has none
    /// — see [`TileKind::Pier`].
    pub apron_hard_alt: i32,
    /// How many of the deck's seaward tiles are approach rather than the tier's
    /// own reach: the hazard band the apron could not grow across.
    ///
    /// Derivable — it is the deck's seaward extent less `class.deck_dims().h`,
    /// and `class` is right there — but kept because it is the split that
    /// matters to whatever builds the deck: a causeway or trestle running down
    /// the shore, and then the quay or pier head the berths hang off. Naming it
    /// beats re-deriving it at every call site that cares.
    pub causeway: i32,
    /// How many tiles along the deck's centre line sit over water at least
    /// [`SHORE_MIN_BERTH_DEPTH`] deep. Always at least
    /// [`SHORE_MIN_DEEP_TILES`]; a candidate that could not manage that was
    /// rejected.
    pub deck_deep_tiles: i32,
    /// The greatest water depth in blocks found along the deck's centre line.
    ///
    /// Recorded rather than left to be recomputed because it is what decides,
    /// per berth, which hull class that berth can take — and a berth whose
    /// class disagrees with the depth under it should be a loud failure
    /// rather than a hull on the seabed, which needs the measurement to
    /// have been kept.
    pub deck_max_depth: f32,
    /// What the winning candidate scored, term by term.
    pub score: ShoreScore,
}

/// Why no port could be placed. The reason is half the deliverable: the
/// placement pass logs it by name so an unviable waterfront is diagnosable
/// from a generation log instead of in the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShoreFailure {
    /// The site's tile domain contains no water hazard adjacent to any
    /// non-hazard tile at all: as far as the tile grid can see, this
    /// settlement is landlocked.
    NoFrontier,
    /// Frontier tiles exist, but none of them offered a usable landward
    /// approach — every one was inside an authored landmark's exclusion, had
    /// no empty land tile to grow from, had no road anywhere in the domain to
    /// walk from, or had a deck footprint that ran into something already
    /// built.
    NoApproach,
    /// Approaches existed and were tried, but `grow_aabr` never reached the
    /// tier's minimum apron dimensions on any of them.
    ApronTooSmall,
}

impl fmt::Display for ShoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::NoFrontier => write!(f, "no frontier candidate"),
            Self::NoApproach => write!(f, "no approach"),
            Self::ApronTooSmall => write!(f, "apron too small"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier dimensions
// ---------------------------------------------------------------------------

impl PortClass {
    /// The apron's `grow_aabr` area range and minimum dimensions, in tiles,
    /// as `(area_range, Extent2::new(along_shore, inland))`.
    ///
    /// Nominal `Harbour` 24×10 (min 18×8), `Quay` 16×8 (min 12×6), `Pier`
    /// 11×6 (min 8×5), `Jetty` 7×4 (min 5×3), with `area_range` spanning
    /// minimum area up to nominal area inclusive — the same shape the airship
    /// dock's `81..82` uses.
    ///
    /// The minimum is expressed shore-relative (long side *along* the shore,
    /// short side running inland) because `grow_aabr` checks `w` and `h`
    /// against fixed axes; [`Site::find_shore_aabr`] rotates this onto the
    /// candidate's own normal.
    pub(crate) fn apron_dims(self) -> (Range<u32>, Extent2<u32>) {
        let (along, inland, nom_along, nom_inland) = match self {
            Self::Harbour => (18u32, 8u32, 24u32, 10u32),
            Self::Quay => (12, 6, 16, 8),
            Self::Pier => (8, 5, 11, 6),
            Self::Jetty => (5, 3, 7, 4),
        };
        (
            (along * inland)..(nom_along * nom_inland + 1),
            Extent2::new(along, inland),
        )
    }

    /// The deck's footprint in tiles, as `Extent2::new(along_shore,
    /// seaward)`.
    ///
    /// The seaward figure is the tier's whole reach — the quay wall's own
    /// depth *plus* the finger piers that project off it: a `Harbour` is a
    /// 4-deep quay with 9-tile fingers, a `Quay` 3 plus 9, a `Pier` a single
    /// 11-tile pier, a `Jetty` a 7-tile jetty. What is claimed here is the
    /// bounding rectangle of all of it; the quay and the gaps between the
    /// fingers are drawn inside that claim by the geometry that fills it.
    ///
    /// Every one of these comfortably exceeds the 2–4 tiles `SQUARE_4`
    /// dilates the hazard band inland, which is the point: a deck sized from
    /// the first hazard tile would stop on sand.
    pub(crate) fn deck_dims(self) -> Extent2<u32> {
        match self {
            Self::Harbour => Extent2::new(24, 13),
            Self::Quay => Extent2::new(16, 12),
            Self::Pier => Extent2::new(3, 11),
            Self::Jetty => Extent2::new(2, 7),
        }
    }
}

// ---------------------------------------------------------------------------
// The road-distance field
// ---------------------------------------------------------------------------

/// Chebyshev tile distance from every tile in the site's obstacle domain to
/// the nearest `is_road()` tile (which includes plazas).
///
/// Built once per placement attempt as a single multi-source breadth-first
/// search over the 8-neighbourhood, rather than per candidate: with up to
/// ~1600 candidates and a few hundred road tiles, the naive form is a
/// six-figure number of distance computations, whereas one BFS visits each of
/// the domain's ~37k cells exactly once. Uniform edge weights over the
/// 8-neighbourhood make BFS give the *exact* chebyshev distance, not an
/// approximation.
struct RoadDistanceField {
    radius: i32,
    side: usize,
    dist: Vec<u16>,
}

impl RoadDistanceField {
    const UNREACHABLE: u16 = u16::MAX;

    fn build(site: &Site) -> Self {
        let radius = Site::OBSTACLE_SEARCH_RADIUS as i32;
        let side = (radius * 2 + 1) as usize;
        let mut field = Self {
            radius,
            side,
            dist: vec![Self::UNREACHABLE; side * side],
        };

        // The queue can hold the whole domain, so reserve it once rather than
        // growing it through a dozen doublings and memcpys.
        let mut queue = VecDeque::with_capacity(side * side);
        for y in -radius..=radius {
            for x in -radius..=radius {
                let tpos = Vec2::new(x, y);
                if site.tiles.get(tpos).is_road()
                    && let Some(idx) = field.index(tpos)
                {
                    field.dist[idx] = 0;
                    queue.push_back((tpos, 0u16));
                }
            }
        }

        // Every edge weighs 1, so writing the distance at push time makes each
        // cell enter the queue exactly once, and the result an exact chebyshev
        // distance rather than an approximation. The distance rides along with
        // the node so popping does not have to look it up again.
        while let Some((tpos, d)) = queue.pop_front() {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let neighbour = tpos + Vec2::new(dx, dy);
                    let Some(idx) = field.index(neighbour) else {
                        continue;
                    };
                    if field.dist[idx] == Self::UNREACHABLE {
                        field.dist[idx] = d + 1;
                        queue.push_back((neighbour, d + 1));
                    }
                }
            }
        }

        field
    }

    fn index(&self, tpos: Vec2<i32>) -> Option<usize> {
        (tpos.x >= -self.radius
            && tpos.x <= self.radius
            && tpos.y >= -self.radius
            && tpos.y <= self.radius)
            .then(|| (tpos.y + self.radius) as usize * self.side + (tpos.x + self.radius) as usize)
    }

    /// Chebyshev tiles to the nearest road, or `None` if the tile is outside
    /// the domain or no road is reachable within it.
    fn get(&self, tpos: Vec2<i32>) -> Option<u16> {
        let d = self.dist[self.index(tpos)?];
        (d != Self::UNREACHABLE).then_some(d)
    }
}

/// The four scored terms of the candidate a placement was grown from, plus
/// the cost they combined into.
///
/// Kept on the placement rather than only logged, because the weights above
/// are expected to be retuned against what a walkthrough shows, and a retune
/// needs to see which term was actually deciding. Reporting only the final
/// cost would make every regression look the same.
#[derive(Debug, Clone, Copy)]
pub struct ShoreScore {
    /// Chebyshev tiles from the candidate to the nearest road or plaza tile.
    pub d_road: u16,
    /// Contiguous water tiles seaward of the candidate, capped by the probe
    /// length.
    pub water_run: i32,
    /// Standard deviation of terrain altitude over the prospective apron, in
    /// blocks.
    pub alt_var: f32,
    /// Blocks from the site origin to the candidate.
    pub d_centre_blocks: f32,
    /// The weighted sum. Lower is better.
    pub cost: f32,
}

/// A frontier candidate with its scored terms.
#[derive(Debug, Clone, Copy)]
struct ScoredCandidate {
    tile: ShoreFrontierTile,
    score: ShoreScore,
}

// ---------------------------------------------------------------------------
// The search
// ---------------------------------------------------------------------------

impl Site {
    /// Every buildable land tile in the site's own tile domain that has water
    /// within [`SHORE_MAX_DECK_GAP`] tiles across the hazard band, with the
    /// outward normal and the band's width.
    ///
    /// This is the replacement for `TileGrid::find_near`'s 70-block spiral,
    /// and it is what actually fixes the "the search cannot reach the shore"
    /// half of the problem. Two properties make it cheap enough to run
    /// unconditionally per port settlement:
    ///
    /// * **No terrain resampling.** The hazard band is already in the tile
    ///   grid, put there by `demarcate_obstacles`, which has already paid for
    ///   nine `wpos_is_hazard` samples per tile. This is a pure grid walk.
    /// * **Anchored inside the domain `demarcate_obstacles` actually
    ///   evaluated** ([`Site::OBSTACLE_SEARCH_RADIUS`], less
    ///   [`SHORE_FRONTIER_MARGIN`]), so no candidate is ever seeded on a tile
    ///   that reads `Empty` merely because nothing looked at it. A probe may
    ///   still step past that radius, which is harmless: out there
    ///   `TileGrid::get` answers with the static empty tile, so the probe ends
    ///   without finding water and no candidate can be manufactured.
    ///   `Site::shore_footprint_is_in_domain` separately keeps the chosen
    ///   footprint inside the stamped band.
    ///
    /// A land tile with water reachable along two cardinals yields two
    /// candidates, one per normal — deliberately, since the two face different
    /// water.
    pub(crate) fn shore_frontier(&self) -> Vec<ShoreFrontierTile> {
        let radius = Site::OBSTACLE_SEARCH_RADIUS - SHORE_FRONTIER_MARGIN;
        let mut frontier = Vec::with_capacity(2048);

        for tpos in Spiral2d::new().take((radius * 2 + 1).pow(2) as usize) {
            // The apron is grown from here, and `grow_aabr` refuses to start
            // anywhere but an empty tile.
            if !self.tiles.get(tpos).is_empty() {
                continue;
            }
            for &outward in CARDINALS.iter() {
                for step in 1..=SHORE_MAX_DECK_GAP + 1 {
                    let probe = tpos + outward * step;
                    let tile = self.tiles.get(probe);
                    if tile.is_water_hazard() {
                        frontier.push(ShoreFrontierTile {
                            land_tpos: tpos,
                            water_tpos: probe,
                            outward,
                            band: step - 1,
                        });
                        break;
                    }
                    // Only the hazard band may lie between the apron and the
                    // water. Open ground means the shore is not this way at
                    // all; anything built means a deck here would be blitted
                    // over a road, path or plot.
                    if !tile.is_hazard() {
                        break;
                    }
                }
            }
        }

        frontier
    }

    /// Find a two-part waterfront footprint for a port of `class`.
    ///
    /// Best-first over the scored frontier candidates: grow the apron
    /// **inland** with the ordinary `grow_aabr` — so it inherits every
    /// existing exclusion (houses, plazas, fields, hills) for free — then
    /// project the deck **seaward** by the tier's reach, which needs no
    /// search at all because those tiles are claimed rather than found.
    ///
    /// `rng` is used only to break near-exact scoring ties (see
    /// [`SHORE_SCORE_TIE_BAND`]); the result is deterministic for a given
    /// world seed.
    pub(crate) fn find_shore_aabr(
        &self,
        land: &Land,
        rng: &mut impl Rng,
        class: PortClass,
        exclusions: &[PortExclusion],
    ) -> Result<ShorePlacement, ShoreFailure> {
        let frontier = self.shore_frontier();
        if frontier.is_empty() {
            return Err(ShoreFailure::NoFrontier);
        }

        let road_dist = RoadDistanceField::build(self);
        let (_, min_dims) = class.apron_dims();

        let mut candidates: Vec<ScoredCandidate> = frontier
            .iter()
            .filter(|candidate| {
                // The authored-landmark keep-out, applied to the seed. The
                // grown footprint is re-checked below, once its real extent
                // is known.
                !self.shore_point_is_excluded(candidate.land_tpos, exclusions)
            })
            .filter_map(|candidate| {
                self.score_shore_candidate(land, *candidate, &road_dist, min_dims)
            })
            .collect();

        if candidates.is_empty() {
            return Err(ShoreFailure::NoApproach);
        }

        // Cost first; a narrower band breaks ties, so an equally good spot is
        // not turned into a longer causeway than it has to be.
        candidates.sort_by(|a, b| {
            a.score
                .cost
                .total_cmp(&b.score.cost)
                .then(a.tile.band.cmp(&b.tile.band))
        });

        // Cosmetic tie-break, see `SHORE_SCORE_TIE_BAND`.
        let best_cost = candidates[0].score.cost;
        let tied = candidates
            .iter()
            .take_while(|c| c.score.cost - best_cost <= SHORE_SCORE_TIE_BAND)
            .count();
        if tied > 1 {
            candidates.swap(0, rng.random_range(0..tied));
        }

        let mut grew_nothing = true;
        for candidate in candidates.iter().take(SHORE_MAX_CANDIDATE_ATTEMPTS) {
            let Some(apron) = self.grow_shore_apron(candidate.tile, class) else {
                continue;
            };
            grew_nothing = false;

            let outward = candidate.tile.outward;
            let hinge = shore_face(apron, outward);
            let Some(gap) = self.shore_deck_gap(apron, outward) else {
                continue;
            };
            // The tier fixes how far the deck reaches *into water*; whatever
            // dilated band or beach strip the apron could not grow across is
            // added on top, so the reach is measured from the waterline
            // rather than from wherever the apron happened to stop.
            let tier_deck = class.deck_dims();
            let deck = project_deck(
                apron,
                outward,
                Extent2::new(tier_deck.w, tier_deck.h + gap as u32),
            );

            // Four independent, side-effect-free rejections, cheapest first:
            // two O(1) rectangle tests, then a pass over the deck's tiles, then
            // the one that touches real terrain. The order cannot change which
            // candidate is chosen, only how much is paid for the ones that are
            // thrown away — and throwing candidates away is the normal case
            // here, not the exception.
            if !self.shore_footprint_is_in_domain(apron, deck) {
                continue;
            }

            // The landmark keep-out again, now against the real footprint
            // rather than the seed. A `Harbour`'s apron plus deck runs to well
            // over a hundred blocks in each direction, so a seed that clears a
            // landmark by a few dozen proves very little on its own.
            if self.shore_footprint_is_excluded(apron, deck, exclusions) {
                continue;
            }

            // Nothing may be blitted over something already built. At the point
            // in `generate_city` where this runs only the initial plaza and its
            // road exist, so this almost never fires — but "almost never" is not
            // "cannot", and a deck silently overwriting a bridge would be a very
            // confusing bug to find later.
            if !self.shore_deck_is_clear(deck) {
                continue;
            }

            let (deep_tiles, max_depth) = self.deck_centre_line_depth(land, deck, outward);
            if deep_tiles < SHORE_MIN_DEEP_TILES {
                continue;
            }

            let door_tile = self.shore_door_tile(apron, outward, &road_dist);
            // Same idiom as `Plaza::generate`: one `get_alt_approx` at the
            // footprint's centre. The deck's own altitude needs per-corner
            // `water_level`/`alt` sampling, which belongs with the geometry
            // rather than with the tile claim.
            let apron_hard_alt =
                land.get_alt_approx(self.tile_center_wpos(shore_aabr_centre(apron))) as i32;

            return Ok(ShorePlacement {
                class,
                apron,
                deck,
                hinge,
                door_tile,
                outward,
                apron_hard_alt,
                causeway: gap,
                deck_deep_tiles: deep_tiles,
                deck_max_depth: max_depth,
                score: candidate.score,
            });
        }

        Err(if grew_nothing {
            ShoreFailure::ApronTooSmall
        } else {
            ShoreFailure::NoApproach
        })
    }

    /// Claim a waterfront footprint for a port of the requested class.
    ///
    /// Called **unconditionally** from `generate_city` immediately after
    /// `make_initial_plaza_default` and **before** the plot `Lottery` loop.
    /// Not inside the `Lottery`, for three reasons in order of weight:
    ///
    /// 1. It has to be deterministic. Whether a settlement has a port is an
    ///    authored fact, not a roll — and the airship dock's 15/134 weight over
    ///    `(size * 200)` draws would give a small settlement (6 draws) no port
    ///    most of the time.
    /// 2. The port needs first pick of the waterfront. Run after the loop, the
    ///    good shoreline aprons are already houses.
    /// 3. It needs the initial plaza to exist, because the scoring's `d_road`
    ///    term has nothing to measure against otherwise.
    ///
    /// On failure: a `warn!` naming the settlement *and* the reason, and no
    /// port. Deliberately **not** a `make_plaza` fallback like the airship
    /// dock's — that would silently convert a missing harbour into a random
    /// square, which is exactly the outcome that is impossible to notice.
    pub(crate) fn place_naval_port(
        &mut self,
        land: &Land,
        rng: &mut impl Rng,
        site_name: &str,
        request: NavalPortRequest<'_>,
    ) -> Option<ShorePlacement> {
        match self.find_shore_aabr(land, rng, request.class, request.exclusions) {
            Ok(placement) => {
                // The apron is ordinary ground, so it gets the airship dock's
                // idiom exactly: `TileKind::Building` with a `hard_alt`.
                self.blit_aabr(placement.apron, Tile {
                    kind: TileKind::Building,
                    // No plot id yet: the tiles are a claim and nothing
                    // else, which is why nothing renders here.
                    // `Site::render` iterates plots, and `render_tile` only
                    // ever draws `TileKind::Path`.
                    plot: None,
                    hard_alt: Some(placement.apron_hard_alt),
                });
                // The deck is over water: `TileKind::Pier`, and no
                // `hard_alt` — a ground-level claim would be false there.
                // `blit_aabr` writes unconditionally, which is the whole
                // reason claiming the hazard band is legal at all.
                self.blit_aabr(placement.deck, Tile {
                    kind: TileKind::Pier,
                    plot: None,
                    hard_alt: None,
                });
                // The four scoring terms are logged alongside the geometry, not
                // just the cost they summed to: when a port lands somewhere
                // surprising, the useful question is which term put it there,
                // and a single cost figure cannot answer it.
                debug!(
                    site = %site_name,
                    class = ?placement.class,
                    apron = ?placement.apron,
                    deck = ?placement.deck,
                    causeway = placement.causeway,
                    outward = ?placement.outward,
                    door_tile = ?placement.door_tile,
                    apron_hard_alt = placement.apron_hard_alt,
                    deck_deep_tiles = placement.deck_deep_tiles,
                    deck_max_depth = placement.deck_max_depth,
                    d_road = placement.score.d_road,
                    water_run = placement.score.water_run,
                    alt_var = placement.score.alt_var,
                    d_centre_blocks = placement.score.d_centre_blocks,
                    cost = placement.score.cost,
                    "placed naval port footprint"
                );
                self.naval_port = Some(placement);
                Some(placement)
            },
            Err(reason) => {
                warn!(
                    site = %site_name,
                    class = ?request.class,
                    %reason,
                    "no naval port could be placed on this settlement's waterfront"
                );
                None
            },
        }
    }

    /// Score one frontier candidate, or reject it outright if it has no road
    /// anywhere in the domain to be reached from.
    fn score_shore_candidate(
        &self,
        land: &Land,
        tile: ShoreFrontierTile,
        road_dist: &RoadDistanceField,
        min_dims: Extent2<u32>,
    ) -> Option<ScoredCandidate> {
        let d_road = road_dist.get(tile.land_tpos)?;
        let water_run = self.shore_water_run(tile);
        let alt_var = self.shore_apron_alt_variance(land, tile, min_dims);
        let d_centre_blocks = self
            .tile_center_wpos(tile.land_tpos)
            .as_::<f32>()
            .distance(self.origin.as_::<f32>());

        let cost = SHORE_W_D_ROAD * (f32::from(d_road) / SHORE_D_ROAD_SCALE)
            + SHORE_W_WATER_RUN * (1.0 - water_run as f32 / SHORE_WATER_RUN_PROBE as f32)
            + SHORE_W_ALT_VAR * (alt_var / SHORE_ALT_VAR_SCALE)
            + SHORE_W_D_CENTRE * (d_centre_blocks / (SHORE_D_CENTRE_SCALE * TILE_SIZE as f32));

        Some(ScoredCandidate {
            tile,
            score: ShoreScore {
                d_road,
                water_run,
                alt_var,
                d_centre_blocks,
                cost,
            },
        })
    }

    /// Contiguous water-hazard tiles from the frontier tile outward along the
    /// shore normal, capped at [`SHORE_WATER_RUN_PROBE`].
    ///
    /// Walks from the *water* tile along the *outward* normal. Getting either
    /// of those backwards produces a run of zero everywhere, since by
    /// construction the other direction is dry land on the first step.
    fn shore_water_run(&self, tile: ShoreFrontierTile) -> i32 {
        (0..SHORE_WATER_RUN_PROBE)
            .take_while(|step| {
                self.tiles
                    .get(tile.water_tpos + tile.outward * *step)
                    .is_water_hazard()
            })
            .count() as i32
    }

    /// Standard deviation, in blocks, of the terrain altitude over the
    /// prospective apron — the cliff-foot rejector.
    ///
    /// Sampled on a [`SHORE_ALT_SAMPLE_LATTICE`]² lattice over a box of the
    /// tier's *minimum* apron dimensions laid inland from the candidate,
    /// which is the footprint actually being promised.
    fn shore_apron_alt_variance(
        &self,
        land: &Land,
        tile: ShoreFrontierTile,
        min_dims: Extent2<u32>,
    ) -> f32 {
        let along_axis = if tile.outward.x != 0 {
            Vec2::new(0, 1)
        } else {
            Vec2::new(1, 0)
        };
        let along_len = min_dims.w as i32;
        let inland_len = min_dims.h as i32;
        let inland = -tile.outward;

        let lattice = SHORE_ALT_SAMPLE_LATTICE;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut n = 0u32;
        for i in 0..lattice {
            for j in 0..lattice {
                // Spread the lattice across the box's interior rather than
                // pinning samples to its corners.
                let a = (along_len * (2 * i + 1)) / (2 * lattice) - along_len / 2;
                let b = (inland_len * (2 * j + 1)) / (2 * lattice);
                let tpos = tile.land_tpos + along_axis * a + inland * b;
                let alt = f64::from(land.get_alt_approx(self.tile_center_wpos(tpos)));
                sum += alt;
                sum_sq += alt * alt;
                n += 1;
            }
        }

        let n = f64::from(n);
        let mean = sum / n;
        (sum_sq / n - mean * mean).max(0.0).sqrt() as f32
    }

    /// Grow the apron from a frontier candidate.
    ///
    /// The minimum dimensions are rotated onto the candidate's own normal, so
    /// the long side runs *along* the shore. If that orientation does not fit,
    /// the transposed one is tried too: a yard turned 90° is still a yard, the
    /// deck simply hinges off its shorter edge.
    ///
    /// Growth seaward is impossible by construction — the tile on the other
    /// side of `land_tpos` is a hazard, so `grow_aabr` can never take that
    /// step — which is what keeps the apron's seaward face on the frontier,
    /// where the hinge belongs. The frontier already offers candidates at
    /// every depth into the shore, so there is nothing to walk here: a spot
    /// further back is a different, separately scored candidate.
    fn grow_shore_apron(&self, tile: ShoreFrontierTile, class: PortClass) -> Option<Aabr<i32>> {
        let (area_range, min_dims) = class.apron_dims();
        let shore_relative = if tile.outward.x != 0 {
            // Water to the east or west: the shore runs along y.
            Extent2::new(min_dims.h, min_dims.w)
        } else {
            Extent2::new(min_dims.w, min_dims.h)
        };
        let transposed = Extent2::new(shore_relative.h, shore_relative.w);

        self.tiles
            .grow_aabr(tile.land_tpos, area_range.clone(), shore_relative)
            .or_else(|_| self.tiles.grow_aabr(tile.land_tpos, area_range, transposed))
            .ok()
    }

    /// Tiles between the apron's seaward face and the first water tile along
    /// the shore normal, or `None` if there is no water within
    /// [`SHORE_MAX_DECK_GAP`] or something already built is in the way.
    ///
    /// Re-measured from the grown apron rather than reused from the candidate,
    /// because `grow_aabr` grows in every direction it can: the face can end
    /// up a tile or two further back than the seed it started from, and the
    /// deck has to span what is actually there.
    fn shore_deck_gap(&self, apron: Aabr<i32>, outward: Vec2<i32>) -> Option<i32> {
        let start = shore_aabr_centre(shore_face(apron, outward));
        let mut gap = 0;
        while gap <= SHORE_MAX_DECK_GAP {
            let tile = self.tiles.get(start + outward * (gap + 1));
            if tile.is_water_hazard() {
                return Some(gap);
            }
            // The band and bare ground are both fine to span; a road, path or
            // plot is not, since the deck is blitted straight over it.
            if !tile.is_hazard() && !tile.is_empty() {
                return None;
            }
            gap += 1;
        }
        None
    }

    /// How many tiles along the deck's centre line, walking seaward, sit over
    /// water at least [`SHORE_MIN_BERTH_DEPTH`] deep, and the greatest depth
    /// found along it.
    ///
    /// Measured against the terrain (`water_alt - alt` on the sim chunk), not
    /// against the tile grid's hazard band: the band is dilated inland and a
    /// steep shore adds hill hazard on top of it, so a deck standing entirely
    /// on the beach would pass a grid-only test. Chunk-granular, which
    /// quantises to about five tiles — good enough to tell "this deck ends in
    /// water" from "this deck ends on sand", which is the question being
    /// asked.
    ///
    /// Along the centre line rather than over the whole rectangle, for the
    /// reason on [`SHORE_MIN_DEEP_TILES`]. Its length is also why this is cheap
    /// enough to sit in the candidate loop: a dozen or two chunk lookups per
    /// attempt rather than one per deck tile.
    fn deck_centre_line_depth(
        &self,
        land: &Land,
        deck: Aabr<i32>,
        outward: Vec2<i32>,
    ) -> (i32, f32) {
        let reach = if outward.x != 0 {
            deck.size().w
        } else {
            deck.size().h
        };
        let start = shore_aabr_centre(shore_face(deck, -outward));
        let mut deep = 0;
        let mut max_depth = 0.0f32;
        for step in 0..reach {
            if let Some(chunk) = land.get_chunk_wpos(self.tile_center_wpos(start + outward * step))
            {
                let depth = chunk.water_alt - chunk.alt;
                max_depth = max_depth.max(depth);
                if depth >= SHORE_MIN_BERTH_DEPTH {
                    deep += 1;
                }
            }
        }
        (deep, max_depth)
    }

    /// Whether the whole footprint lies inside the domain
    /// `demarcate_obstacles` actually evaluated.
    ///
    /// Outside it every tile reads `Empty` purely because nothing was ever
    /// stamped there, so `grow_aabr` succeeds trivially and the apron can
    /// land on an un-evaluated lake or cliff face. Without this check a
    /// search that is otherwise well behaved drifts to the edge of its own
    /// domain, because that is where growth is easiest.
    fn shore_footprint_is_in_domain(&self, apron: Aabr<i32>, deck: Aabr<i32>) -> bool {
        let r = Site::OBSTACLE_SEARCH_RADIUS as i32 - 1;
        [apron, deck].iter().all(|aabr| {
            aabr.min.x >= -r && aabr.min.y >= -r && aabr.max.x <= r + 1 && aabr.max.y <= r + 1
        })
    }

    /// Whether every tile of a prospective deck is either empty or a hazard,
    /// i.e. nothing already built is about to be overwritten.
    fn shore_deck_is_clear(&self, deck: Aabr<i32>) -> bool {
        aabr_tiles(deck).all(|tpos| {
            let tile = self.tiles.get(tpos);
            tile.is_empty() || tile.is_hazard()
        })
    }

    /// Whether a single tile's centre falls inside any exclusion region.
    fn shore_point_is_excluded(&self, tpos: Vec2<i32>, exclusions: &[PortExclusion]) -> bool {
        if exclusions.is_empty() {
            return false;
        }
        let wpos = self.tile_center_wpos(tpos).as_::<f32>();
        exclusions.iter().any(|exclusion| {
            let keep_out = exclusion.radius as f32;
            wpos.distance_squared(exclusion.centre_wpos.as_::<f32>()) <= keep_out * keep_out
        })
    }

    /// Whether the grown apron or the projected deck reaches into any
    /// exclusion region.
    fn shore_footprint_is_excluded(
        &self,
        apron: Aabr<i32>,
        deck: Aabr<i32>,
        exclusions: &[PortExclusion],
    ) -> bool {
        if exclusions.is_empty() {
            return false;
        }
        let to_wpos = |aabr: Aabr<i32>| Aabr {
            min: self.tile_wpos(aabr.min).as_::<f32>(),
            max: self.tile_wpos(aabr.max).as_::<f32>(),
        };
        [to_wpos(apron), to_wpos(deck)].iter().any(|footprint| {
            exclusions.iter().any(|exclusion| {
                let keep_out = exclusion.radius as f32;
                let centre = exclusion.centre_wpos.as_::<f32>();
                footprint.projected_point(centre).distance_squared(centre) <= keep_out * keep_out
            })
        })
    }

    /// The apron's landward-face tile closest to a road — where the port is
    /// entered from the town.
    fn shore_door_tile(
        &self,
        apron: Aabr<i32>,
        outward: Vec2<i32>,
        road_dist: &RoadDistanceField,
    ) -> Vec2<i32> {
        let landward = shore_face(apron, -outward);
        aabr_tiles(landward)
            .min_by_key(|tpos| {
                road_dist
                    .get(*tpos)
                    .unwrap_or(RoadDistanceField::UNREACHABLE)
            })
            .unwrap_or_else(|| shore_aabr_centre(apron))
    }
}

// ---------------------------------------------------------------------------
// Aabr helpers
// ---------------------------------------------------------------------------

/// The one-tile-deep face of `aabr` on the `dir` side. Min-inclusive,
/// max-exclusive.
fn shore_face(aabr: Aabr<i32>, dir: Vec2<i32>) -> Aabr<i32> {
    if dir.x > 0 {
        Aabr {
            min: Vec2::new(aabr.max.x - 1, aabr.min.y),
            max: aabr.max,
        }
    } else if dir.x < 0 {
        Aabr {
            min: aabr.min,
            max: Vec2::new(aabr.min.x + 1, aabr.max.y),
        }
    } else if dir.y > 0 {
        Aabr {
            min: Vec2::new(aabr.min.x, aabr.max.y - 1),
            max: aabr.max,
        }
    } else {
        Aabr {
            min: aabr.min,
            max: Vec2::new(aabr.max.x, aabr.min.y + 1),
        }
    }
}

/// Project a deck of `dims` (along-shore × seaward) off the seaward face of
/// `apron`, centred on that face and flush with it.
fn project_deck(apron: Aabr<i32>, outward: Vec2<i32>, dims: Extent2<u32>) -> Aabr<i32> {
    let along = dims.w as i32;
    let seaward = dims.h as i32;
    let face = shore_face(apron, outward);

    if outward.x != 0 {
        let centre_y = (face.min.y + face.max.y) / 2;
        let min_y = centre_y - along / 2;
        let (min_x, max_x) = if outward.x > 0 {
            (apron.max.x, apron.max.x + seaward)
        } else {
            (apron.min.x - seaward, apron.min.x)
        };
        Aabr {
            min: Vec2::new(min_x, min_y),
            max: Vec2::new(max_x, min_y + along),
        }
    } else {
        let centre_x = (face.min.x + face.max.x) / 2;
        let min_x = centre_x - along / 2;
        let (min_y, max_y) = if outward.y > 0 {
            (apron.max.y, apron.max.y + seaward)
        } else {
            (apron.min.y - seaward, apron.min.y)
        };
        Aabr {
            min: Vec2::new(min_x, min_y),
            max: Vec2::new(min_x + along, max_y),
        }
    }
}

/// Centre tile of a max-exclusive tile aabr.
fn shore_aabr_centre(aabr: Aabr<i32>) -> Vec2<i32> { (aabr.min + aabr.max - 1) / 2 }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;
    use rand::SeedableRng;
    use rand_chacha::ChaChaRng;

    #[test]
    fn pier_tiles_are_obstacles_but_not_buildings_or_ground() {
        let pier = Tile::free(TileKind::Pier);
        assert!(pier.is_obstacle(), "a pier deck must exclude other plots");
        assert!(!pier.is_building());
        assert!(
            !pier.is_natural(),
            "a claimed pier deck is not natural terrain"
        );
        assert!(!pier.is_road());
        assert!(
            !pier.is_empty(),
            "a pier deck must never read as empty, or `grow_aabr` would grow into it"
        );
        assert!(pier.is_pier());
        assert_eq!(pier.hard_alt, None);
        assert_eq!(TileKind::Pier.to_string(), "Pier");
    }

    #[test]
    fn every_tier_deck_crosses_the_dilated_hazard_band() {
        // `SQUARE_4` dilates the water hazard 2 tiles inland, and the
        // chunk-granular `river.near_water()` test can widen that to ~4. A
        // deck shorter than that would be claimed entirely within the band
        // and end on dry sand.
        const WORST_CASE_DILATION: u32 = 4;
        for class in [
            PortClass::Jetty,
            PortClass::Pier,
            PortClass::Quay,
            PortClass::Harbour,
        ] {
            let reach = class.deck_dims().h;
            assert!(
                reach > WORST_CASE_DILATION,
                "{class:?}'s deck reach of {reach} tiles does not clear the hazard dilation"
            );
        }
    }

    #[test]
    fn apron_area_range_admits_the_nominal_footprint_and_starts_at_the_minimum() {
        for (class, min_dims, nominal) in [
            (PortClass::Harbour, (18u32, 8u32), (24u32, 10u32)),
            (PortClass::Quay, (12, 6), (16, 8)),
            (PortClass::Pier, (8, 5), (11, 6)),
            (PortClass::Jetty, (5, 3), (7, 4)),
        ] {
            let (range, dims) = class.apron_dims();
            assert_eq!((dims.w, dims.h), min_dims, "{class:?} minimum dimensions");
            assert_eq!(range.start, min_dims.0 * min_dims.1);
            assert!(
                range.contains(&(nominal.0 * nominal.1)),
                "{class:?}'s area range must reach its nominal footprint"
            );
        }
    }

    #[test]
    fn deck_projects_seaward_of_the_apron_in_every_cardinal() {
        let apron = Aabr {
            min: Vec2::new(-4, -3),
            max: Vec2::new(4, 3),
        };
        for outward in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let dims = PortClass::Pier.deck_dims();
            let deck = project_deck(apron, outward, dims);
            assert_eq!(
                deck.size().product(),
                (dims.w * dims.h) as i32,
                "deck lost tiles projecting {outward:?}"
            );
            // No overlap with the apron, and flush with its seaward face
            // rather than skipping a tile.
            let overlap = deck.intersection(apron);
            assert!(
                overlap.size().w <= 0 || overlap.size().h <= 0,
                "deck overlaps the apron projecting {outward:?}"
            );
            let hinge = shore_face(apron, outward);
            let gap = if outward.x > 0 {
                deck.min.x - hinge.max.x
            } else if outward.x < 0 {
                hinge.min.x - deck.max.x
            } else if outward.y > 0 {
                deck.min.y - hinge.max.y
            } else {
                hinge.min.y - deck.max.y
            };
            assert_eq!(gap, 0, "deck is not flush with the hinge for {outward:?}");
        }
    }

    /// An unviable waterfront must leave the settlement bit-for-bit as it
    /// would have been with no port requested at all -- in particular it must
    /// *not* fall back to a plaza the way the airship dock does, since that
    /// would silently turn a missing harbour into a random square.
    ///
    /// `Land::empty()` is exactly that unviable case, and not by accident:
    /// with no sim behind it, `wpos_is_hazard` answers
    /// `Some(HazardKind::Water)` for every position, so `demarcate_obstacles`
    /// stamps the *entire* domain as a water hazard and there is no buildable
    /// land tile anywhere for the frontier scan to anchor to. The real-asset
    /// placement test covers the viable half.
    ///
    /// A caller-supplied, not entropy-seeded, RNG is what makes the two runs
    /// deterministic and therefore actually comparable (unlike full chunk
    /// generation, which reseeds its own RNG from entropy per call).
    ///
    /// Note the scope this pins, which is narrower than it may read: `None` and
    /// *failure* both draw nothing from the RNG, so the plot `Lottery` that
    /// follows sees an untouched stream and the site really is bit-for-bit
    /// unchanged. A *successful* placement can draw once, to break a scoring
    /// tie, and from there the `Lottery` diverges — which is expected, since
    /// that settlement's geometry is meant to differ anyway. What this test
    /// guarantees is that nothing diverges when no port is built.
    #[test]
    fn an_unviable_waterfront_places_no_port_and_no_fallback() {
        fn generate_with(naval_port: Option<NavalPortRequest<'_>>) -> Site {
            let index = Index::new(0);
            let index_ref = IndexRef {
                colors: &index.colors(),
                features: &index.features(),
                biome_profiles: &index.biome_profiles(),
                index: &index,
            };
            let mut gen_meta = SitesGenMeta::new(0);
            let mut rng = ChaChaRng::from_seed([7u8; 32]);
            Site::generate_city(
                &Land::empty(),
                index_ref,
                &mut rng,
                Vec2::zero(),
                0.5,
                None,
                &mut gen_meta,
                naval_port,
            )
        }

        let without_port = generate_with(None);
        // The largest tier, so the comparison is against the biggest
        // footprint that could possibly have been claimed.
        let with_port = generate_with(Some(NavalPortRequest::new(PortClass::Harbour, &[])));

        assert_eq!(
            without_port.tiles.bounds, with_port.tiles.bounds,
            "requesting a naval port class changed the generated tile grid's bounds"
        );
        for x in without_port.tiles.bounds.min.x..=without_port.tiles.bounds.max.x {
            for y in without_port.tiles.bounds.min.y..=without_port.tiles.bounds.max.y {
                let tpos = Vec2::new(x, y);
                // `Tile` has no `Debug` impl, so this can't be `assert_eq!`.
                assert!(
                    without_port.tiles.get_known(tpos) == with_port.tiles.get_known(tpos),
                    "tile {tpos:?} differs between naval_port: None and naval_port: Some(_)"
                );
            }
        }
        assert_eq!(
            without_port.plots.values().count(),
            with_port.plots.values().count(),
            "requesting a naval port class changed the generated plot count"
        );
        assert_eq!(without_port.plazas.len(), with_port.plazas.len());
        assert_eq!(without_port.roads.len(), with_port.roads.len());

        // And the reason, stated rather than inferred from the absence of a
        // difference.
        let mut rng = ChaChaRng::from_seed([7u8; 32]);
        assert_eq!(
            with_port
                .find_shore_aabr(&Land::empty(), &mut rng, PortClass::Harbour, &[])
                .err(),
            Some(ShoreFailure::NoFrontier),
            "a domain that is water hazard end to end has no waterfront to find"
        );
    }

    #[test]
    fn shore_failure_reasons_are_named_not_numbered() {
        // The placement pass logs these strings verbatim, and a generation
        // log is grepped for them.
        assert_eq!(
            ShoreFailure::NoFrontier.to_string(),
            "no frontier candidate"
        );
        assert_eq!(ShoreFailure::NoApproach.to_string(), "no approach");
        assert_eq!(ShoreFailure::ApronTooSmall.to_string(), "apron too small");
    }
}
