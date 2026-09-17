//! Generic, size-class-scaled cave generation for Cromatolis's authored
//! cave-location catalog: ~279 hand-marked points, each carrying a
//! `size_class` (`Giant`/`Large`/`Medium`/`Small`) but no bespoke design.
//!
//! This is deliberately *not* the same shape as `cromatolis_interior.rs`
//! (sibling module): that module carves two fully hand-authored, named-room
//! interior graphs. This module carves a single generic "hub chamber +
//! radiating branch tunnels" shape at every other authored point, sized by
//! `size_class`, with zero named content.
//!
//! ## Why not `cave.rs`'s `Tunnel` type directly
//!
//! This module reuses `world/src/layer/cave.rs`'s actual generation
//! primitives rather than inventing a new cave-shape algorithm. In
//! practice that means the same technique `cave.rs::Tunnel` is built on --
//! a quadratic spline between two points (`common::terrain::{
//! river_spline_coeffs, quadratic_nearest_point}`) with a distance-based
//! radius falloff -- not the literal `cave.rs::Tunnel` type, whose fields
//! are private and whose sizing (`MIN_RADIUS`/`MAX_RADIUS`, noise-driven)
//! is baked for *hashed, continuous* generation across the whole map. That
//! is exactly the same call `cromatolis_interior.rs` already made for the
//! same reason (see its own module doc) when adapting cave-tunnel math to
//! **authored, fixed** points instead of hashed procedural nodes; this
//! module makes the identical call for the same reason, one level simpler
//! (no named rooms/levels, just a hub + branches).
//!
//! `cave.rs::Node` (`wpos`, `depth`) *is* reused conceptually: each
//! generated cave's hub and branch tips are exactly that -- a world
//! position plus a depth below the local surface -- just resolved from an
//! authored catalog point instead of a hashed cell.
//!
//! ## Determinism
//!
//! Branch angle/curve jitter is derived from the feature's own stable
//! string id (see [`fnv1a_unit`]), not from `cave.rs`'s hash-grid
//! (`RandomField`/cell coordinates). These are discrete authored
//! placements, not a continuous noise field, so there is no cell grid to
//! hash against in the first place.
//!
//! ## Scope
//!
//! Like `cromatolis_interior.rs`, this only ever activates for chunks with
//! `SimChunk::authored_cromatolis_v0` set. The two catalog points
//! cross-referenced to two already-authored, bespoke interiors elsewhere in
//! this crate (`cave.thurnak_entrance`, `cave.kharvun_vent`) are excluded
//! outright -- see [`EXCLUDED_FEATURE_IDS`] -- so this module's generic
//! geometry never collides with that already-carved, hand-authored
//! content.
//!
//! Geometry is still all this module *invents*. The one piece of content it
//! places is the authored mineral set each catalog entry carries (the
//! `minerals` field of `cromatolis_v0_cave_features.ron`) -- see
//! [`apply_minerals_to_floor`]. That exists because the engine's only other
//! source of Iron/Coal/Cobalt/Silver and every gem is `cave.rs`'s procedural
//! generator, which deliberately does **not** run in Cromatolis; without it
//! the region's 281 authored caves contribute nothing to the mineral
//! economy. No NPC or loot population -- that stays out. (This supersedes
//! the module's earlier "no content population" promise, which was written
//! before there was an authored mineral field to honour.)
//!
//! Two things worth knowing before tuning the authored abundances:
//!
//! - **They are a ceiling, not the realized spawn rate.** Everything placed
//!   here flows into `canvas.rtsim_resource_blocks` and is then subject to
//!   rtsim's depletion roll in `World::generate_chunk`, exactly as `cave.rs`'s
//!   procedural ore is. A depleted world yields less than the catalog says, by
//!   design.
//! - **`Velorite`/`VeloriteFrag` account to rtsim's `Loot` bucket, not
//!   `Ore`/`Gem`** (`Block::get_rtsim_resource` has no explicit arm for them,
//!   so they fall through to the `default_loot_spec` catch-all). They are still
//!   minable and still authored deliberately; just don't expect them to move
//!   the ore/gem depletion pools.

use crate::{
    Canvas, CanvasInfo, Land,
    util::{RandomField, SQUARE_4},
};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    comp::tool::ToolKind,
    terrain::{
        Block, CoordinateConversions, MapSizeLg, SpriteKind, TerrainChunkSize,
        quadratic_nearest_point, river_spline_coeffs,
    },
    vol::RectVolSize,
};
use serde::Deserialize;
use std::{borrow::Cow, f32::consts::TAU};
use tracing::warn;
use vek::*;

const CAVE_FEATURES_ASSET: &str = "world.map.cromatolis_v0_cave_features";

/// Two catalog points cross-referenced (via the source catalog's own
/// connection metadata) to two already-authored, bespoke interiors elsewhere
/// in this crate. They are exported unfiltered by the upstream catalog
/// export by design -- this module is what must skip them, so a generic
/// procedural cave never visually collides with the already-carved
/// authored content there.
const EXCLUDED_FEATURE_IDS: &[&str] = &["cave.thurnak_entrance", "cave.kharvun_vent"];

/// Minimum rock cover kept between a carved cave surface and the real
/// terrain surface above it, same role as `cromatolis_interior.rs`'s
/// constant of the same name.
const SURFACE_MARGIN: f32 = 4.0;
/// Distance (in blocks) over which a carved edge fades in.
const EDGE_SOFTNESS: f32 = 3.0;

// ---------------------------------------------------------------------
// Size-class scaling. Named constants (not magic numbers inline) so the
// per-class size progression is visible and independently tunable.
// ---------------------------------------------------------------------

const GIANT_BRANCH_COUNT: usize = 6;
const LARGE_BRANCH_COUNT: usize = 4;
const MEDIUM_BRANCH_COUNT: usize = 2;
const SMALL_BRANCH_COUNT: usize = 1;

/// Radius of the central chamber every generated cave has at its authored
/// anchor point.
const GIANT_HUB_RADIUS: f32 = 26.0;
const LARGE_HUB_RADIUS: f32 = 19.0;
const MEDIUM_HUB_RADIUS: f32 = 13.0;
const SMALL_HUB_RADIUS: f32 = 8.0;

/// Radius at a branch tunnel's far tip (tapers from the hub radius down to
/// this along the tunnel).
const GIANT_BRANCH_RADIUS: f32 = 16.0;
const LARGE_BRANCH_RADIUS: f32 = 12.0;
const MEDIUM_BRANCH_RADIUS: f32 = 8.0;
const SMALL_BRANCH_RADIUS: f32 = 5.0;

/// Horizontal distance from the hub to each branch tip.
const GIANT_BRANCH_LENGTH: f32 = 95.0;
const LARGE_BRANCH_LENGTH: f32 = 68.0;
const MEDIUM_BRANCH_LENGTH: f32 = 42.0;
const SMALL_BRANCH_LENGTH: f32 = 22.0;

/// Vertical clearance (ceiling above floor) of the hub and every branch.
const GIANT_HEADROOM: f32 = 24.0;
const LARGE_HEADROOM: f32 = 18.0;
const MEDIUM_HEADROOM: f32 = 12.0;
const SMALL_HEADROOM: f32 = 7.0;

/// How far below the local surface the hub's floor sits.
const GIANT_DEPTH: i32 = 55;
const LARGE_DEPTH: i32 = 42;
const MEDIUM_DEPTH: i32 = 30;
const SMALL_DEPTH: i32 = 18;

/// Branch floor is kept within this many blocks of the hub's floor, so a
/// branch tip landing over sharply different terrain still reads as part
/// of one connected cave system rather than an absurd vertical shaft.
const MAX_BRANCH_FLOOR_DRIFT: i32 = 24;

// ---------------------------------------------------------------------
// Authored minerals.
// ---------------------------------------------------------------------

/// Per-column probability that one abundance tier places its sprite on a
/// carved cave floor, **before** the size-class correction below.
///
/// Reference point, computed rather than asserted: `cave.rs` gates its own
/// ore roll on `rand.chance(.., 0.007)` and then draws from a weighted table
/// (`world/src/layer/cave.rs:1568-1586`) in which `(None, 10.0)` and
/// `(Stones, 1.5)` eat 82-97% of the gate, so its *effective* rate is
/// ~6.1e-4 ore + 1.5e-4 gem = **~7.6e-4 mineral per floor column**,
/// depth-averaged (peak ~1.4e-3 in the shallow layers). So `Trace` here is
/// still well above the procedural rate and `Abundant` is roughly 12x it:
/// an authored, named-vein-grade deposit, which is the point -- `cave.rs`
/// does not run in Cromatolis at all (`cromatolis_v0_procedural_layers.ron`,
/// `caves: false`), so these 281 finite caves carry the region's entire
/// mineral economy.
///
/// `Trace` carries a slightly bigger proportional weight than the other
/// three tiers so it stays a real, findable deposit rather than fading into
/// noise as the rest scale up around it.
///
/// Calibrated against a **Medium** cave (~2.3k floor columns). Giant and
/// Large caves are 4x and 10x that area, which is what
/// [`SizeClass::mineral_scale`] exists to correct.
///
/// Kept as code constants rather than asset data on purpose: this is the
/// same side of the code/data line `cave.rs` already puts its own ore rates
/// on, and the *content* decision (which mineral, how rich, where) lives in
/// the catalog. If tuning these ever needs to happen without a recompile,
/// move all of them -- these and `cave.rs`'s -- together.
const ABUNDANT_CHANCE: f32 = 0.030;
const COMMON_CHANCE: f32 = 0.015;
const SPARSE_CHANCE: f32 = 0.006;
const TRACE_CHANCE: f32 = 0.0025;

/// Per-column density correction by cave size.
///
/// Without it the same authored Spanish word buys wildly different hauls: a
/// `Giant`'s carved floor is ~24k columns against a `Small`'s ~470, so a
/// flat per-column chance makes one `abundante` mean ~480 ore in a Giant and
/// ~9 in a Small -- a 50x swing the catalog author cannot see. Scaling the
/// big classes down keeps "abundant" meaning roughly the same *find* at
/// every size while still letting a Giant out-yield a Small several times
/// over, because it is still several times larger.
const GIANT_MINERAL_SCALE: f32 = 0.55;
const LARGE_MINERAL_SCALE: f32 = 0.40;
const MEDIUM_MINERAL_SCALE: f32 = 1.0;
const SMALL_MINERAL_SCALE: f32 = 1.0;

/// Hard ceiling on one cave's summed effective per-column mineral chance,
/// i.e. after [`SizeClass::mineral_scale`]. Nothing in the authoring catalog
/// stops a future edit from declaring six `Abundant` minerals on one entry;
/// without this, a long enough list drives the total past 1.0 and turns
/// every carved floor column into ore. Raised roughly in proportion with
/// the `*_CHANCE` tiers above rather than tied to them by an exact ratio.
///
/// [`flatten_minerals`] never truncates an over-cap list by declaration
/// order: every declared mineral that survives non-minable/duplicate
/// filtering is kept, and the whole set is compressed by one shared ratio
/// so the total lands exactly on this cap. A catalog author never loses a
/// declared mineral to list position; an over-rich cave just reads a little
/// less generous than its raw numbers suggest, uniformly across every
/// mineral it names. See
/// `flatten_minerals_compresses_an_over_cap_list_instead_of_truncating_it`
/// for the check that keeps this true.
const MAX_TOTAL_MINERAL_CHANCE: f32 = 0.055;

/// Resolution floor for every band width above.
///
/// [`mineral_for_column`] compares against `RandomField::get_f32`, which is
/// `(get(pos) % 65536) / 65536.0` -- 16 bits, so ~1.5e-5 granularity. The
/// narrowest band the current data produces is `TRACE_CHANCE *
/// LARGE_MINERAL_SCALE` = 0.001, about 65 discrete steps, so nothing
/// quantizes away today. Keep any new tier, or any reduction of the
/// `*_MINERAL_SCALE` factors, well above this: a band narrower than the
/// granularity can never be rolled at all, and would fail silently.
///
/// Kept next to the tiers it constrains rather than inside the test module,
/// because this is what a future tuner needs to read before touching them;
/// `every_abundance_tier_stays_above_the_noise_resolution_floor` enforces it.
#[cfg(test)]
const MIN_USEFUL_BAND_WIDTH: f32 = 1.0 / 65536.0;

/// Salt for the per-column mineral roll. Distinct from every other
/// `RandomField` seed used by this crate's layers so two features never
/// correlate.
const MINERAL_NOISE_SEED: u32 = 0x00C4_7E01;

/// Is this sprite something a player can actually mine out of a cave wall?
///
/// Deliberately delegates to `SpriteKind::mine_tool` rather than keeping a
/// hand-written list: that predicate is the engine's own definition of the
/// ore/gem set (`common/src/terrain/sprite/mod.rs`), it tracks any ore added
/// upstream for free, and a hand-written copy had already drifted from it in
/// both directions during review (it omitted `Lodestone` and wrongly
/// admitted `IceCrystal`, which is scenery -- no `mine_tool`, so a player
/// could never extract it and it would contribute nothing to the mineral
/// economy this whole feature exists for).
fn is_minable_mineral(kind: SpriteKind) -> bool { kind.mine_tool() == Some(ToolKind::Pick) }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum MineralAbundance {
    Abundant,
    Common,
    Sparse,
    Trace,
}

impl MineralAbundance {
    fn base_chance(self) -> f32 {
        match self {
            Self::Abundant => ABUNDANT_CHANCE,
            Self::Common => COMMON_CHANCE,
            Self::Sparse => SPARSE_CHANCE,
            Self::Trace => TRACE_CHANCE,
        }
    }
}

struct SizeProfile {
    branch_count: usize,
    hub_radius: f32,
    branch_radius: f32,
    branch_length: f32,
    headroom: f32,
    depth: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum SizeClass {
    Giant,
    Large,
    Medium,
    Small,
}

impl SizeClass {
    /// See the `*_MINERAL_SCALE` constants: corrects the per-column mineral
    /// chance for how much carved floor this class actually has.
    fn mineral_scale(self) -> f32 {
        match self {
            Self::Giant => GIANT_MINERAL_SCALE,
            Self::Large => LARGE_MINERAL_SCALE,
            Self::Medium => MEDIUM_MINERAL_SCALE,
            Self::Small => SMALL_MINERAL_SCALE,
        }
    }

    fn profile(self) -> SizeProfile {
        match self {
            Self::Giant => SizeProfile {
                branch_count: GIANT_BRANCH_COUNT,
                hub_radius: GIANT_HUB_RADIUS,
                branch_radius: GIANT_BRANCH_RADIUS,
                branch_length: GIANT_BRANCH_LENGTH,
                headroom: GIANT_HEADROOM,
                depth: GIANT_DEPTH,
            },
            Self::Large => SizeProfile {
                branch_count: LARGE_BRANCH_COUNT,
                hub_radius: LARGE_HUB_RADIUS,
                branch_radius: LARGE_BRANCH_RADIUS,
                branch_length: LARGE_BRANCH_LENGTH,
                headroom: LARGE_HEADROOM,
                depth: LARGE_DEPTH,
            },
            Self::Medium => SizeProfile {
                branch_count: MEDIUM_BRANCH_COUNT,
                hub_radius: MEDIUM_HUB_RADIUS,
                branch_radius: MEDIUM_BRANCH_RADIUS,
                branch_length: MEDIUM_BRANCH_LENGTH,
                headroom: MEDIUM_HEADROOM,
                depth: MEDIUM_DEPTH,
            },
            Self::Small => SizeProfile {
                branch_count: SMALL_BRANCH_COUNT,
                hub_radius: SMALL_HUB_RADIUS,
                branch_radius: SMALL_BRANCH_RADIUS,
                branch_length: SMALL_BRANCH_LENGTH,
                headroom: SMALL_HEADROOM,
                depth: SMALL_DEPTH,
            },
        }
    }
}

// ---------------------------------------------------------------------
// RON data model.
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CaveFeaturesAsset {
    schema: String,
    coordinate_space: String,
    features: Vec<CaveFeatureEntry>,
}

impl FileAsset for CaveFeaturesAsset {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl CaveFeaturesAsset {
    fn validate(&self) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.cave_features.v1";
        const EXPECTED_COORDINATE_SPACE: &str = "normalized_map_xy_top_left_origin";

        if self.schema != EXPECTED_SCHEMA {
            return Err(format!(
                "expected schema {EXPECTED_SCHEMA}, got {}",
                self.schema
            ));
        }
        if self.coordinate_space != EXPECTED_COORDINATE_SPACE {
            return Err(format!(
                "expected coordinate space {EXPECTED_COORDINATE_SPACE}, got {}",
                self.coordinate_space
            ));
        }

        let mut ids = std::collections::HashSet::new();
        for feature in &self.features {
            if feature.id.is_empty() || !ids.insert(feature.id.as_str()) {
                return Err(format!("duplicate or empty cave feature id {}", feature.id));
            }
        }
        Ok(())
    }

    /// Whether any entry declares a mineral at all.
    ///
    /// `minerals` is `#[serde(default)]`, so the asset that predates the
    /// authoring pass loads perfectly and produces 281 ore-free caves --
    /// indistinguishable, from inside the engine, from a working feature.
    /// Used only to say so out loud at load time.
    fn declares_no_minerals_at_all(&self) -> bool {
        self.features.iter().all(|f| f.minerals.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CaveFeatureEntry {
    id: String,
    position: NormalizedPosition,
    size_class: SizeClass,
    /// Authored upstream in `xindeler-open-world`'s Cromatolis cave catalog
    /// and exported into this asset; the asset's own `notes:` header carries
    /// the exact provenance, so it isn't restated (and left to rot) here.
    /// `#[serde(default)]` so an asset predating the authoring pass -- or a
    /// hand-written test fixture -- still loads as "no mineral here" rather
    /// than failing the whole world's cave layer.
    #[serde(default)]
    minerals: Vec<CaveMineral>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct CaveMineral {
    kind: SpriteKind,
    abundance: MineralAbundance,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct NormalizedPosition {
    x: f32,
    y: f32,
}

impl NormalizedPosition {
    /// Normalized, top-left origin; sim `y` grows northward while authored
    /// pixels grow southward, so `y` gets flipped here -- identical
    /// convention to `cromatolis_interior.rs`'s `NormalizedPoint` and
    /// `civ/mod.rs`'s `AuthoredMapPoint`.
    fn to_chunk_pos(self, map_size: MapSizeLg) -> Vec2<i32> {
        let size = map_size.chunks();
        let x = (self.x.clamp(0.0, 1.0) * f32::from(size.x.saturating_sub(1))).round() as i32;
        let y =
            ((1.0 - self.y.clamp(0.0, 1.0)) * f32::from(size.y.saturating_sub(1))).round() as i32;
        Vec2::new(x, y)
    }
}

/// FNV-1a, used only to turn a stable string id into a deterministic float
/// in `[0, 1)`. Duplicated from the same technique `cromatolis_interior.rs`
/// uses (that module's copy is private to it) rather than shared across the
/// module boundary, for the same reason that module already documented for
/// its own small duplicated helpers.
fn fnv1a_unit(s: &str) -> f32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as f32 / u32::MAX as f32
}

// ---------------------------------------------------------------------
// Resolved geometry: computed once per world (cached via
// `Index::cromatolis_cave_features`), not once per process.
// ---------------------------------------------------------------------

struct HubGeom {
    anchor2d: Vec2<i32>,
    floor_z: i32,
    ceiling_z: i32,
    radius: f32,
}

struct BranchSeg {
    a: Vec3<i32>,
    b: Vec3<i32>,
    a_radius: f32,
    b_radius: f32,
    headroom: f32,
    curve: f32,
}

/// A fully-resolved, ready-to-carve generic cave. See the module doc for
/// why this doesn't reuse `cave.rs::Tunnel` directly.
pub(crate) struct GeneratedCave {
    hub: HubGeom,
    branches: Vec<BranchSeg>,
    /// Authored minerals, pre-flattened to `(sprite, cumulative chance)` so
    /// the per-column hot path is one noise sample plus a short linear scan
    /// instead of a fresh weighted-choice allocation per block.
    minerals: Vec<(SpriteKind, f32)>,
    /// Rough bounding circle (center, radius) covering every carved shape,
    /// used to cheaply skip chunks nowhere near this cave.
    bounds: (Vec2<i32>, f32),
}

impl GeneratedCave {
    /// Rough overall extent (bounding-circle radius) this generated cave
    /// reaches. Exposed for tests asserting a concrete size-class
    /// progression, not consumed by generation itself.
    #[cfg(test)]
    pub(crate) fn extent(&self) -> f32 { self.bounds.1 }
}

pub(crate) fn build_all_generated_caves(info: &CanvasInfo) -> Vec<GeneratedCave> {
    let asset = match CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET) {
        Ok(asset) => asset,
        Err(err) => {
            warn!(?err, "Failed to load Cromatolis cave features");
            return Vec::new();
        },
    };
    if let Err(err) = asset.validate() {
        warn!(%err, "Invalid Cromatolis cave features asset, skipping");
        return Vec::new();
    }
    if asset.declares_no_minerals_at_all() {
        warn!(
            "Cromatolis cave features asset declares no minerals on any of its {} entries; every \
             authored cave will generate ore-free. This is what a pre-authoring-pass asset looks \
             like, not a bug in generation.",
            asset.features.len()
        );
    }

    let map_size = info.chunks().map_size_lg();
    let land = info.land();

    asset
        .features
        .iter()
        .filter(|feature| !EXCLUDED_FEATURE_IDS.contains(&feature.id.as_str()))
        .map(|feature| build_generated_cave(feature, map_size, &land))
        .collect()
}

fn build_generated_cave(
    feature: &CaveFeatureEntry,
    map_size: MapSizeLg,
    land: &Land,
) -> GeneratedCave {
    let profile = feature.size_class.profile();
    let chunk_pos = feature.position.to_chunk_pos(map_size);
    let hub_wpos = chunk_pos.cpos_to_wpos_center();
    let hub_alt = land.get_alt_approx(hub_wpos);
    let hub_floor_z = hub_alt as i32 - profile.depth;
    let hub_ceiling_z = hub_floor_z + profile.headroom as i32;

    let hub = HubGeom {
        anchor2d: hub_wpos,
        floor_z: hub_floor_z,
        ceiling_z: hub_ceiling_z,
        radius: profile.hub_radius,
    };

    let mut branches = Vec::with_capacity(profile.branch_count);
    let mut max_reach = profile.hub_radius;
    for i in 0..profile.branch_count {
        let branch_seed = format!("{}#branch{i}", feature.id);
        let angle = (i as f32 / profile.branch_count as f32) * TAU + fnv1a_unit(&branch_seed) * 0.5;
        let offset = Vec2::new(angle.cos(), angle.sin()) * profile.branch_length;
        let tip_wpos = hub_wpos + offset.map(|e| e.round() as i32);
        let tip_alt = land.get_alt_approx(tip_wpos);
        let tip_floor_z = (tip_alt as i32 - profile.depth).clamp(
            hub_floor_z - MAX_BRANCH_FLOOR_DRIFT,
            hub_floor_z + MAX_BRANCH_FLOOR_DRIFT,
        );

        branches.push(BranchSeg {
            a: hub_wpos.with_z(hub_floor_z),
            b: tip_wpos.with_z(tip_floor_z),
            a_radius: profile.hub_radius,
            b_radius: profile.branch_radius,
            headroom: profile.headroom,
            curve: (fnv1a_unit(&branch_seed) - 0.5) * 0.6,
        });

        max_reach = max_reach.max(profile.branch_length + profile.branch_radius);
    }

    GeneratedCave {
        hub,
        branches,
        minerals: flatten_minerals(&feature.id, &feature.minerals, feature.size_class),
        bounds: (hub_wpos, max_reach + EDGE_SOFTNESS),
    }
}

/// Turn one entry's authored mineral list into the cumulative
/// `(sprite, chance)` bands [`mineral_for_column`] scans.
///
/// Degrades per-mineral rather than per-asset: a hand-edited catalog typo
/// drops that one mineral with a `warn!` and leaves the other 280 caves'
/// geometry alone. Rejecting the whole asset (the load path's only other
/// option, see [`build_all_generated_caves`]) would let one bad content
/// token delete every cave in the region, which is wildly out of proportion
/// to the mistake.
///
/// Two independent passes. The first resolves `declared` down to the valid,
/// deduplicated `(sprite, raw_chance)` set exactly as before (unminable and
/// repeated-kind entries are dropped with a `warn!`, first declared
/// abundance wins on a repeat). The second checks whether that set's total
/// already fits [`MAX_TOTAL_MINERAL_CHANCE`]; if it does, chances pass
/// through unchanged. If it does not, every one of them is scaled down by
/// the same `cap / raw_total` ratio before the cumulative bands are built,
/// so a too-rich cave reads uniformly less generous rather than losing
/// whichever minerals happen to sort last.
fn flatten_minerals(
    feature_id: &str,
    declared: &[CaveMineral],
    size_class: SizeClass,
) -> Vec<(SpriteKind, f32)> {
    let scale = size_class.mineral_scale();

    let mut resolved: Vec<(SpriteKind, f32)> = Vec::with_capacity(declared.len());
    for mineral in declared {
        if !is_minable_mineral(mineral.kind) {
            warn!(
                "Cromatolis cave {feature_id} declares {:?}, which is not minable; skipping that \
                 mineral",
                mineral.kind
            );
            continue;
        }
        if resolved.iter().any(|(kind, _)| *kind == mineral.kind) {
            warn!(
                "Cromatolis cave {feature_id} declares {:?} more than once; keeping the first \
                 abundance only",
                mineral.kind
            );
            continue;
        }
        resolved.push((mineral.kind, mineral.abundance.base_chance() * scale));
    }

    let raw_total: f32 = resolved.iter().map(|(_, chance)| *chance).sum();
    let compression = if raw_total > MAX_TOTAL_MINERAL_CHANCE {
        let ratio = MAX_TOTAL_MINERAL_CHANCE / raw_total;
        warn!(
            "Cromatolis cave {feature_id} declares {raw_total} total mineral density, above the \
             {MAX_TOTAL_MINERAL_CHANCE} cap; compressing all {} declared minerals by a factor of \
             {ratio:.3} rather than dropping any of them",
            resolved.len()
        );
        ratio
    } else {
        1.0
    };

    let mut bands = Vec::with_capacity(resolved.len());
    let mut cumulative = 0.0;
    for (kind, chance) in resolved {
        cumulative += chance * compression;
        bands.push((kind, cumulative));
    }
    bands
}

/// Pick the authored mineral for one carved floor column, if any.
///
/// One noise sample per column, compared against the cave's cumulative
/// abundance chances: the first band the sample falls into wins, and a
/// sample past the last band (the overwhelmingly common case) means bare
/// floor. Deterministic in world position, so a chunk regenerates
/// identically and two neighbouring chunks agree on the shared columns.
fn mineral_for_column(minerals: &[(SpriteKind, f32)], wpos: Vec3<i32>) -> Option<SpriteKind> {
    let total = minerals.last()?.1;
    let roll = RandomField::new(MINERAL_NOISE_SEED).get_f32(wpos);
    if roll >= total {
        return None;
    }
    minerals
        .iter()
        .find(|(_, cumulative)| roll < *cumulative)
        .map(|(kind, _)| *kind)
}

/// Replace a just-carved floor block with an ore/gem sprite when the
/// authored mineral roll hits. Mirrors how `cave.rs` places its ore: the
/// sprite lives in the first air block above the cave floor, as
/// `Block::air(sprite)`.
fn apply_minerals_to_floor(
    canvas: &mut Canvas,
    minerals: &[(SpriteKind, f32)],
    floor_pos: Vec3<i32>,
) {
    // 31 of the authored caves declare no economy at all. Bail before the
    // block lookup below so their carved floors cost nothing here, rather
    // than paying a terrain read per column only to find no bands to roll
    // against.
    if minerals.is_empty() {
        return;
    }
    // An ore sprite needs something under it. `floor_pos` is the lowest
    // floor any of this cave's shapes carved in this column, so normally the
    // block below is untouched rock -- but two authored caves can overlap,
    // and the lower one's carve would otherwise leave the upper one's sprite
    // hanging in mid-air.
    if !canvas.get(floor_pos - Vec3::unit_z()).is_solid() {
        return;
    }
    if let Some(sprite) = mineral_for_column(minerals, floor_pos) {
        canvas.set(floor_pos, Block::air(sprite));
    }
}

// ---------------------------------------------------------------------
// Per-column carving.
// ---------------------------------------------------------------------

/// The subset of one [`GeneratedCave`]'s shapes that can plausibly touch
/// the chunk currently being generated, mirroring
/// `cromatolis_interior.rs`'s `RelevantInterior` two-stage pruning (and,
/// further back, `cave.rs::apply_caves_to`'s own `SQUARE_4`-based
/// proximity filter).
struct RelevantCave<'a> {
    hub: Option<&'a HubGeom>,
    branches: Vec<&'a BranchSeg>,
    minerals: &'a [(SpriteKind, f32)],
}

pub fn apply_cromatolis_cave_features_to(canvas: &mut Canvas) {
    if !canvas.info().chunk().authored_cromatolis_v0 {
        return;
    }
    let info = canvas.info();
    let index_ref = info.index();
    let caves = index_ref
        .cromatolis_cave_features
        .get_or_init(|| build_all_generated_caves(&info));
    if caves.is_empty() {
        return;
    }

    let chunk_wpos = info.wpos();
    let chunk_size_i = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    let chunk_size = chunk_size_i.map(|e| e as f32);
    let chunk_center = chunk_wpos.map(|e| e as f32) + chunk_size / 2.0;
    let chunk_diag = (chunk_size.map(|e| e * e).sum()).sqrt() / 2.0;
    let corners_i32 = SQUARE_4.map(|rpos| chunk_wpos + rpos * chunk_size_i);
    let corners_f32 = corners_i32.map(|c| c.map(|e| e as f32));
    let corners_f64 = corners_i32.map(|c| c.map(|e| e as f64 + 0.5));

    // Two-stage pruning: first reject whole caves whose overall bounding
    // circle can't reach this chunk at all, then reject individual
    // hub/branch shapes that don't touch any of the chunk's 4 corners.
    let relevant: Vec<RelevantCave> = caves
        .iter()
        .filter(|cave| {
            chunk_center.distance(cave.bounds.0.map(|e| e as f32))
                <= cave.bounds.1 + chunk_diag + 32.0
        })
        .filter_map(|cave| {
            let hub = hub_touches_chunk(&cave.hub, &corners_f32).then_some(&cave.hub);
            let branches: Vec<&BranchSeg> = cave
                .branches
                .iter()
                .filter(|branch| branch_touches_chunk(branch, &corners_f64))
                .collect();
            if hub.is_none() && branches.is_empty() {
                None
            } else {
                Some(RelevantCave {
                    hub,
                    branches,
                    minerals: &cave.minerals,
                })
            }
        })
        .collect();
    if relevant.is_empty() {
        return;
    }

    canvas.foreach_col(|canvas, wpos2d, col| {
        let col_alt = col.alt;
        for cave in &relevant {
            // Carve every shape of this cave first, then run exactly one
            // mineral pass over the column. Doing it inside the carve calls
            // would let a branch's `Block::empty()` sweep erase a sprite the
            // hub had just placed in the overlap region, so ore density near
            // a hub would be decided by carve order instead of by the
            // authored abundance.
            let mut lowest_floor: Option<i32> = None;
            if let Some(hub) = cave.hub {
                lowest_floor = min_floor(lowest_floor, carve_hub(canvas, wpos2d, col_alt, hub));
            }
            for branch in &cave.branches {
                lowest_floor =
                    min_floor(lowest_floor, carve_branch(canvas, wpos2d, col_alt, branch));
            }
            if let Some(floor_z) = lowest_floor {
                apply_minerals_to_floor(canvas, cave.minerals, wpos2d.with_z(floor_z));
            }
        }
    });
}

fn hub_touches_chunk(hub: &HubGeom, corners: &[Vec2<f32>; 4]) -> bool {
    let max_radius = hub.radius + EDGE_SOFTNESS;
    let anchor = hub.anchor2d.map(|e| e as f32);
    corners
        .iter()
        .any(|corner| corner.distance(anchor) <= max_radius)
}

fn branch_touches_chunk(seg: &BranchSeg, corners: &[Vec2<f64>; 4]) -> bool {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let max_dist = seg.a_radius.max(seg.b_radius) as f64 + EDGE_SOFTNESS as f64 + 1.0;
    corners.iter().any(|&corner| {
        spline_sample(a2, b2, seg.curve, corner).is_some_and(|(_, dist)| dist <= max_dist)
    })
}

fn edge_weight(dist: f32, radius: f32) -> f32 { ((radius - dist) / EDGE_SOFTNESS).clamp(0.0, 1.0) }

/// Shared spline sample used by branch tunnels: a quadratic spline between
/// two fixed points, returning `t` (0 at `a2`, 1 at `b2`) and the
/// perpendicular distance from the queried point to the curve. Identical
/// technique to `cromatolis_interior.rs`'s `spline_sample` (and, beneath
/// that, `cave.rs::Tunnel`'s own spline math) -- see the module doc for why
/// it's duplicated here rather than shared.
fn spline_sample(a2: Vec2<f64>, b2: Vec2<f64>, curve: f32, point: Vec2<f64>) -> Option<(f64, f64)> {
    let ctrl_offset = ((b2 - a2) * 0.5
        + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * curve as f64)
        .map(|e| e as f32);
    let spline = river_spline_coeffs(a2, ctrl_offset, b2);
    let (t, closest, dist_sq) = quadratic_nearest_point(&spline, point, Vec2::new(a2, b2))?;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    Some((t, closest.distance(point).min(dist_sq.sqrt())))
}

/// Carve this column's slice of a hub chamber. Returns the floor `z` it
/// carved, if any, so the caller can run a single mineral pass per column.
fn carve_hub(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, hub: &HubGeom) -> Option<i32> {
    let dist = wpos2d
        .map(|e| e as f32)
        .distance(hub.anchor2d.map(|e| e as f32));
    if dist > hub.radius + EDGE_SOFTNESS {
        return None;
    }
    if edge_weight(dist, hub.radius) <= 0.0 {
        return None;
    }

    let ceiling_cap = (col_alt - SURFACE_MARGIN).floor() as i32;
    let ceiling_z = hub.ceiling_z.min(ceiling_cap);
    if ceiling_z <= hub.floor_z {
        return None;
    }

    for z in hub.floor_z..=ceiling_z {
        canvas.set(wpos2d.with_z(z), Block::empty());
    }
    Some(hub.floor_z)
}

/// Lowest of two optional carved floors -- the floor a mineral should sit
/// on when a hub and one or more branches all cross the same column.
fn min_floor(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (some, None) | (None, some) => some,
    }
}

/// Carve this column's slice of a branch tunnel. Returns the floor `z` it
/// carved, if any -- see [`carve_hub`].
fn carve_branch(
    canvas: &mut Canvas,
    wpos2d: Vec2<i32>,
    col_alt: f32,
    seg: &BranchSeg,
) -> Option<i32> {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let (t, dist) = spline_sample(a2, b2, seg.curve, wpos2d.map(|e| e as f64 + 0.5))?;
    let radius = Lerp::lerp_unclamped(seg.a_radius as f64, seg.b_radius as f64, t) as f32;
    if edge_weight(dist as f32, radius) <= 0.0 {
        return None;
    }

    let floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t);
    let ceiling_cap = (col_alt - SURFACE_MARGIN) as f64;
    let ceiling_z = (floor_z + seg.headroom as f64).min(ceiling_cap);
    if ceiling_z <= floor_z {
        return None;
    }

    for z in floor_z.floor() as i32..=ceiling_z.ceil() as i32 {
        canvas.set(wpos2d.with_z(z), Block::empty());
    }
    Some(floor_z.floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_asset() -> CaveFeaturesAsset {
        CaveFeaturesAsset {
            schema: "xindeler_open_world.cave_features.v1".to_string(),
            coordinate_space: "normalized_map_xy_top_left_origin".to_string(),
            features: vec![
                CaveFeatureEntry {
                    id: "cave.test_giant".to_string(),
                    position: NormalizedPosition { x: 0.4, y: 0.4 },
                    size_class: SizeClass::Giant,
                    minerals: vec![
                        CaveMineral {
                            kind: SpriteKind::Iron,
                            abundance: MineralAbundance::Abundant,
                        },
                        CaveMineral {
                            kind: SpriteKind::Sapphire,
                            abundance: MineralAbundance::Trace,
                        },
                    ],
                },
                CaveFeatureEntry {
                    id: "cave.test_small".to_string(),
                    position: NormalizedPosition { x: 0.6, y: 0.6 },
                    size_class: SizeClass::Small,
                    minerals: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn validate_accepts_the_expected_schema_and_coordinate_space() {
        assert!(sample_asset().validate().is_ok());
    }

    fn mineral(kind: SpriteKind, abundance: MineralAbundance) -> CaveMineral {
        CaveMineral { kind, abundance }
    }

    /// The cumulative-chance table must stay strictly increasing and end at
    /// the sum of every declared tier, or `mineral_for_column`'s
    /// single-sample scan silently starves the later minerals. Exercises the
    /// real production flattening, not a hand-built copy of it.
    #[test]
    fn flatten_minerals_builds_strictly_increasing_cumulative_bands() {
        let bands = flatten_minerals(
            "cave.test",
            &[
                mineral(SpriteKind::Iron, MineralAbundance::Abundant),
                mineral(SpriteKind::Sapphire, MineralAbundance::Trace),
            ],
            SizeClass::Medium,
        );
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0], (SpriteKind::Iron, ABUNDANT_CHANCE));
        assert!(bands[0].1 < bands[1].1);
        assert!((bands[1].1 - (ABUNDANT_CHANCE + TRACE_CHANCE)).abs() < 1e-6);
    }

    /// A hand-edited catalog typo must cost that one mineral, never the
    /// whole region's cave geometry.
    #[test]
    fn flatten_minerals_drops_a_non_minable_sprite_and_keeps_the_rest() {
        let bands = flatten_minerals(
            "cave.test",
            &[
                mineral(SpriteKind::IceCrystal, MineralAbundance::Abundant),
                mineral(SpriteKind::DungeonChest0, MineralAbundance::Common),
                mineral(SpriteKind::Iron, MineralAbundance::Common),
            ],
            SizeClass::Medium,
        );
        assert_eq!(bands, vec![(SpriteKind::Iron, COMMON_CHANCE)]);
    }

    #[test]
    fn flatten_minerals_keeps_only_the_first_abundance_of_a_repeated_kind() {
        let bands = flatten_minerals(
            "cave.test",
            &[
                mineral(SpriteKind::Iron, MineralAbundance::Trace),
                mineral(SpriteKind::Iron, MineralAbundance::Abundant),
            ],
            SizeClass::Medium,
        );
        assert_eq!(bands, vec![(SpriteKind::Iron, TRACE_CHANCE)]);
    }

    /// Without the cap, a long enough authored list drives the summed
    /// chance past 1.0 and every carved floor column becomes ore. The cap
    /// must still bind the total -- it just compresses now, see
    /// `flatten_minerals_compresses_an_over_cap_list_instead_of_truncating_it`
    /// for proof it keeps every mineral rather than dropping any.
    #[test]
    fn flatten_minerals_caps_total_density() {
        let greedy: Vec<CaveMineral> = [
            SpriteKind::Iron,
            SpriteKind::Coal,
            SpriteKind::Cobalt,
            SpriteKind::Silver,
            SpriteKind::Copper,
            SpriteKind::Tin,
            SpriteKind::Gold,
            SpriteKind::Ruby,
        ]
        .into_iter()
        .map(|kind| mineral(kind, MineralAbundance::Abundant))
        .collect();
        let bands = flatten_minerals("cave.test", &greedy, SizeClass::Medium);
        assert!(bands.last().unwrap().1 <= MAX_TOTAL_MINERAL_CHANCE + f32::EPSILON);
    }

    /// The proportional-compression rule this cap relies on: an over-cap
    /// list keeps every declared mineral (none dropped by position), and
    /// their *relative* weight survives -- an `Abundant` entry stays twice
    /// an equally-scaled `Common` one -- while the summed total lands
    /// exactly on the cap.
    #[test]
    fn flatten_minerals_compresses_an_over_cap_list_instead_of_truncating_it() {
        let declared = vec![
            mineral(SpriteKind::Velorite, MineralAbundance::Abundant),
            mineral(SpriteKind::Sapphire, MineralAbundance::Abundant),
            mineral(SpriteKind::Cobalt, MineralAbundance::Abundant),
            mineral(SpriteKind::Bloodstone, MineralAbundance::Common),
        ];
        let bands = flatten_minerals("cave.test", &declared, SizeClass::Small);

        assert_eq!(
            bands.len(),
            declared.len(),
            "every declared mineral must survive compression, none dropped by list position"
        );
        assert!(
            (bands.last().unwrap().1 - MAX_TOTAL_MINERAL_CHANCE).abs() < 1e-5,
            "a compressed list's total should land exactly on the cap, got {}",
            bands.last().unwrap().1
        );

        let width = |i: usize| {
            if i == 0 {
                bands[0].1
            } else {
                bands[i].1 - bands[i - 1].1
            }
        };
        let velorite_width = width(0);
        let bloodstone_width = width(3);
        assert!(
            (velorite_width - 2.0 * bloodstone_width).abs() < 1e-6,
            "Abundant (velorite, width {velorite_width}) should stay exactly twice Common \
             (bloodstone, width {bloodstone_width}) after a uniform compression"
        );
    }

    /// Real-catalog regression for the two caves that motivated the
    /// compression rule: both keep all four declared minerals now, where
    /// the old drop-the-rest rule kept only one and two respectively.
    #[test]
    fn the_merid_stormbound_passages_keep_every_declared_mineral() {
        let asset =
            CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).expect("the real asset should load");

        for id in [
            "cave.merid_stormbound_passage_coast",
            "cave.merid_stormbound_passage_inner",
        ] {
            let feature = asset
                .features
                .iter()
                .find(|f| f.id == id)
                .unwrap_or_else(|| panic!("{id} should exist in the real catalog"));
            let bands = flatten_minerals(&feature.id, &feature.minerals, feature.size_class);
            assert_eq!(
                bands.len(),
                feature.minerals.len(),
                "{id} should keep all {} declared minerals, kept {}",
                feature.minerals.len(),
                bands.len()
            );
        }
    }

    /// A Giant cave's carved floor is ~50x a Small's, so the same authored
    /// tier must not mean the same per-column chance in both.
    #[test]
    fn bigger_size_classes_get_a_lower_per_column_mineral_chance() {
        let declared = [mineral(SpriteKind::Iron, MineralAbundance::Abundant)];
        let chance = |size| flatten_minerals("cave.test", &declared, size)[0].1;
        let giant = chance(SizeClass::Giant);
        let large = chance(SizeClass::Large);
        let medium = chance(SizeClass::Medium);
        let small = chance(SizeClass::Small);
        assert!(
            large < giant,
            "Large ({large}) should be below Giant ({giant})"
        );
        assert!(
            giant < medium,
            "Giant ({giant}) should be below Medium ({medium})"
        );
        assert_eq!(medium, small, "Medium and Small are both calibrated at 1.0");
        assert_eq!(medium, ABUNDANT_CHANCE);
    }

    /// `is_minable_mineral` must agree with the engine's own ore/gem
    /// definition -- the exact drift a hand-written allow-list produced.
    #[test]
    fn minable_predicate_tracks_the_engine_ore_set() {
        for kind in [
            SpriteKind::Iron,
            SpriteKind::Coal,
            SpriteKind::Velorite,
            SpriteKind::Lodestone,
            SpriteKind::Diamond,
        ] {
            assert!(is_minable_mineral(kind), "{kind:?} should be minable");
        }
        for kind in [
            SpriteKind::IceCrystal,
            SpriteKind::DungeonChest0,
            SpriteKind::Stones,
        ] {
            assert!(!is_minable_mineral(kind), "{kind:?} should not be minable");
        }
    }

    #[test]
    fn an_asset_with_no_minerals_anywhere_is_detectable() {
        let mut asset = sample_asset();
        assert!(!asset.declares_no_minerals_at_all());
        for feature in &mut asset.features {
            feature.minerals.clear();
        }
        assert!(asset.declares_no_minerals_at_all());
    }

    #[test]
    fn a_cave_with_no_minerals_never_places_a_sprite() {
        for x in 0..64 {
            assert_eq!(mineral_for_column(&[], Vec3::new(x, x * 7, -40)), None);
        }
    }

    /// Every column must be either bare or one of the declared minerals,
    /// both bands must actually be reachable (i.e. the roll is really
    /// weighted, not first-entry-wins), and the realized density must match
    /// the declared one -- the concrete bound the "not a loot pinata"
    /// intent actually rests on.
    #[test]
    fn mineral_rolls_respect_the_declared_abundance_order_and_density() {
        let bands = flatten_minerals(
            "cave.test",
            &[
                mineral(SpriteKind::Iron, MineralAbundance::Abundant),
                mineral(SpriteKind::Sapphire, MineralAbundance::Trace),
            ],
            SizeClass::Medium,
        );
        let (mut iron, mut sapphire, mut bare) = (0u32, 0u32, 0u32);
        for x in 0..600 {
            for y in 0..600 {
                match mineral_for_column(&bands, Vec3::new(x, y, -37)) {
                    Some(SpriteKind::Iron) => iron += 1,
                    Some(SpriteKind::Sapphire) => sapphire += 1,
                    Some(other) => panic!("unexpected sprite {other:?}"),
                    None => bare += 1,
                }
            }
        }
        assert!(iron > 0 && sapphire > 0, "both bands must be reachable");
        assert!(iron > sapphire, "Abundant must out-spawn Trace");

        let total = f64::from(iron + sapphire + bare);
        let iron_rate = f64::from(iron) / total;
        let sapphire_rate = f64::from(sapphire) / total;
        // 360k samples: the sampling error on a ~1% rate is well under 10%
        // relative, so a 25% tolerance only fires on a real regression.
        assert!(
            (iron_rate - f64::from(ABUNDANT_CHANCE)).abs() < f64::from(ABUNDANT_CHANCE) * 0.25,
            "Abundant realized at {iron_rate}, expected ~{ABUNDANT_CHANCE}"
        );
        assert!(
            (sapphire_rate - f64::from(TRACE_CHANCE)).abs() < f64::from(TRACE_CHANCE) * 0.25,
            "Trace realized at {sapphire_rate}, expected ~{TRACE_CHANCE}"
        );
    }

    #[test]
    fn validate_rejects_a_schema_mismatch() {
        let mut asset = sample_asset();
        asset.schema = "some.other.schema.v1".to_string();
        assert!(asset.validate().is_err());
    }

    #[test]
    fn validate_rejects_a_coordinate_space_mismatch() {
        let mut asset = sample_asset();
        asset.coordinate_space = "pixels".to_string();
        assert!(asset.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let mut asset = sample_asset();
        let dup = asset.features[0].clone();
        asset.features.push(dup);
        assert!(asset.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_ids() {
        let mut asset = sample_asset();
        asset.features[0].id = String::new();
        assert!(asset.validate().is_err());
    }

    #[test]
    fn excluded_feature_ids_matches_the_real_authored_cross_references() {
        assert_eq!(EXCLUDED_FEATURE_IDS, &[
            "cave.thurnak_entrance",
            "cave.kharvun_vent"
        ]);
    }

    /// Loads the real committed asset (not a hand-written fixture) so a
    /// real data error (schema drift, a bad entry) surfaces as a test
    /// failure. Requires `VELOREN_ASSETS` to point at the repo's `assets/`
    /// directory.
    #[test]
    fn real_cave_features_asset_parses_and_validates_without_panicking() {
        let asset = CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET)
            .expect("assets/world/map/cromatolis_v0_cave_features.ron should load and parse");
        asset
            .validate()
            .expect("the real authored asset should pass validation");
        assert_eq!(
            asset.features.len(),
            281,
            "expected all 281 catalog entries"
        );
        for excluded in EXCLUDED_FEATURE_IDS {
            assert!(
                asset.features.iter().any(|f| f.id == *excluded),
                "expected the real catalog to contain the cross-referenced id {excluded}"
            );
        }
    }

    /// Pins the authored mineral economy against the real installed asset.
    ///
    /// `minerals` is `#[serde(default)]`, so a reconciliation that dropped
    /// the field, or an upstream export that silently emitted empty lists,
    /// still loads and still generates all 281 caves -- just without any
    /// ore in them. `declares_no_minerals_at_all` only catches the total
    /// wipe; these numbers catch a partial one. Nothing in this repo pins
    /// the artifact's checksum, so in practice these totals are also what
    /// would catch the asset being replaced by a stale copy.
    ///
    /// Asset rows and *generated* economy are deliberately counted
    /// separately: the two `EXCLUDED_FEATURE_IDS` entries declare seven
    /// deposits between them that are never placed, so an export that
    /// quietly moved deposits onto an excluded entry would leave the asset
    /// totals untouched while shrinking the real economy.
    #[test]
    fn real_asset_mineral_totals_match_the_authored_catalog() {
        let asset = CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET)
            .expect("assets/world/map/cromatolis_v0_cave_features.ron should load and parse");

        let deposits: usize = asset.features.iter().map(|f| f.minerals.len()).sum();
        let declaring = asset
            .features
            .iter()
            .filter(|f| !f.minerals.is_empty())
            .count();

        assert_eq!(deposits, 657, "authored mineral deposit rows in the asset");
        assert_eq!(
            declaring, 250,
            "asset entries carrying at least one deposit"
        );
        assert_eq!(
            asset.features.len() - declaring,
            31,
            "asset entries that declared an empty economy"
        );
        assert!(!asset.declares_no_minerals_at_all());

        // What generation actually sees, once the two cross-referenced
        // entries are filtered out.
        let generic: Vec<&CaveFeatureEntry> = asset
            .features
            .iter()
            .filter(|f| !EXCLUDED_FEATURE_IDS.contains(&f.id.as_str()))
            .collect();
        let generic_deposits: usize = generic.iter().map(|f| f.minerals.len()).sum();
        let generic_declaring = generic.iter().filter(|f| !f.minerals.is_empty()).count();

        // Derived from the asset totals rather than pinned independently:
        // these are not independent quantities, and pinning all of them
        // means one re-export fails several assertions at once and someone
        // recomputes six numbers by hand. The seven deposits stranded on
        // the two excluded entries are the entire difference.
        let excluded_deposits: usize = asset
            .features
            .iter()
            .filter(|f| EXCLUDED_FEATURE_IDS.contains(&f.id.as_str()))
            .map(|f| f.minerals.len())
            .sum();

        assert_eq!(generic.len(), 279, "generic caves the engine generates");
        assert_eq!(
            excluded_deposits, 7,
            "deposits stranded on excluded entries"
        );
        assert_eq!(
            generic_deposits,
            deposits - excluded_deposits,
            "deposits that can actually be placed"
        );
        assert_eq!(
            generic_declaring,
            declaring - EXCLUDED_FEATURE_IDS.len(),
            "generic caves carrying a deposit"
        );
    }

    /// Every mineral the real asset names must be something a player can
    /// actually mine. A scenery sprite here would pass deserialization,
    /// pass validation, and then be dropped one warning at a time at world
    /// generation -- an authored deposit that simply never exists.
    #[test]
    fn every_real_asset_mineral_is_minable() {
        let asset =
            CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).expect("the real asset should load");
        for feature in &asset.features {
            for mineral in &feature.minerals {
                assert!(
                    is_minable_mineral(mineral.kind),
                    "{} declares {:?}, which has no mine_tool and would be skipped",
                    feature.id,
                    mineral.kind
                );
            }
        }
    }

    /// `flatten_minerals` drops a repeated `SpriteKind` within one cave's own
    /// list (keeping the first declared abundance), same as it drops a
    /// non-minable one -- pinned independently, the same way
    /// [`every_real_asset_mineral_is_minable`] pins the non-minable case, so
    /// `no_real_cave_ever_loses_a_mineral_to_the_density_cap`'s failure
    /// message can keep blaming the cap specifically without a duplicate
    /// kind ever being able to masquerade as a cap compression.
    #[test]
    fn no_real_cave_declares_the_same_mineral_kind_twice() {
        let asset =
            CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).expect("the real asset should load");
        for feature in &asset.features {
            let mut seen = std::collections::HashSet::new();
            for mineral in &feature.minerals {
                assert!(
                    seen.insert(mineral.kind),
                    "{} declares {:?} more than once; only the first declared abundance survives \
                     flatten_minerals, silently dropping the repeat",
                    feature.id,
                    mineral.kind
                );
            }
        }
    }

    /// Every abundance tier must stay wide enough to actually be rolled.
    ///
    /// `mineral_for_column` compares against `RandomField::get_f32`, which
    /// has ~1.5e-5 granularity; a band narrower than that can never be hit,
    /// and would fail silently rather than loudly. Guards
    /// [`MIN_USEFUL_BAND_WIDTH`] against a future tier below `Trace` or a
    /// smaller `*_MINERAL_SCALE`.
    #[test]
    fn every_abundance_tier_stays_above_the_noise_resolution_floor() {
        for size_class in [
            SizeClass::Giant,
            SizeClass::Large,
            SizeClass::Medium,
            SizeClass::Small,
        ] {
            for abundance in [
                MineralAbundance::Abundant,
                MineralAbundance::Common,
                MineralAbundance::Sparse,
                MineralAbundance::Trace,
            ] {
                let width = abundance.base_chance() * size_class.mineral_scale();
                assert!(
                    width > MIN_USEFUL_BAND_WIDTH * 10.0,
                    "{abundance:?} on a {size_class:?} cave is {width}, too close to the \
                     {MIN_USEFUL_BAND_WIDTH} roll granularity to be reliably placed"
                );
            }
        }
    }

    /// Catalog-wide guarantee, not just the two caves that first exposed the
    /// problem: with a compressing cap, no real entry can ever lose a
    /// mineral to list position, so this loop should never find one, for
    /// any future catalog edit, not only the ones known about today.
    #[test]
    fn no_real_cave_ever_loses_a_mineral_to_the_density_cap() {
        let asset =
            CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).expect("the real asset should load");

        let dropped: Vec<(String, usize, usize)> = asset
            .features
            .iter()
            .filter(|f| !EXCLUDED_FEATURE_IDS.contains(&f.id.as_str()))
            .filter_map(|f| {
                let kept = flatten_minerals(&f.id, &f.minerals, f.size_class).len();
                (kept < f.minerals.len()).then(|| (f.id.clone(), f.minerals.len(), kept))
            })
            .collect();

        assert_eq!(
            dropped,
            Vec::<(String, usize, usize)>::new(),
            "these caves lost a mineral to the density cap, which the compressing rule should \
             never allow (only actually-unminable or repeated-kind entries may still be dropped, \
             and those aren't counted here): {dropped:?}"
        );
    }

    /// The authored `minerals` field is `#[serde(default)]` so that the
    /// asset that predates the authoring pass still loads. That makes a
    /// silent deserialization failure invisible against the real asset, so
    /// the round trip is pinned against an inline fixture instead: the exact
    /// shape the upstream authoring exporter emits, including an entry
    /// that deliberately declares none (sewers, necropolises and guild
    /// hideouts do) and one that omits the field entirely.
    #[test]
    fn minerals_round_trip_from_the_exported_ron_shape() {
        let asset: CaveFeaturesAsset = load_ron(
            br#"(
                schema: "xindeler_open_world.cave_features.v1",
                coordinate_space: "normalized_map_xy_top_left_origin",
                features: [
                    (
                        id: "cave.with_minerals",
                        position: (x: 0.5, y: 0.5),
                        size_class: Giant,
                        minerals: [
                            (kind: Iron, abundance: Abundant),
                            (kind: Sapphire, abundance: Trace),
                        ],
                    ),
                    (
                        id: "cave.declared_empty",
                        position: (x: 0.5, y: 0.5),
                        size_class: Medium,
                        minerals: [],
                    ),
                    (
                        id: "cave.field_absent",
                        position: (x: 0.5, y: 0.5),
                        size_class: Small,
                    ),
                ],
            )"#
            .as_slice(),
        )
        .expect("the exporter's RON shape must deserialize");
        asset.validate().expect("fixture should validate");

        assert_eq!(asset.features[0].minerals.len(), 2);
        assert_eq!(asset.features[0].minerals[0].kind, SpriteKind::Iron);
        assert_eq!(
            asset.features[0].minerals[0].abundance,
            MineralAbundance::Abundant
        );
        assert_eq!(
            asset.features[0].minerals[1].abundance,
            MineralAbundance::Trace
        );
        assert!(asset.features[1].minerals.is_empty());
        assert!(asset.features[2].minerals.is_empty());
    }

    /// Exercises the exact filter predicate `build_all_generated_caves`
    /// applies to the real catalog before any `GeneratedCave` is ever built
    /// -- so this confirms the two cross-referenced points can never reach
    /// cave generation, without needing a real `CanvasInfo`/`WorldSim`.
    #[test]
    fn excluded_ids_are_filtered_out_of_the_real_catalog_before_generation() {
        let asset = CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).unwrap();
        let generated_ids: Vec<&str> = asset
            .features
            .iter()
            .filter(|feature| !EXCLUDED_FEATURE_IDS.contains(&feature.id.as_str()))
            .map(|f| f.id.as_str())
            .collect();
        for excluded in EXCLUDED_FEATURE_IDS {
            assert!(
                !generated_ids.contains(excluded),
                "{excluded} must never reach cave generation"
            );
        }
        assert_eq!(
            generated_ids.len(),
            asset.features.len() - EXCLUDED_FEATURE_IDS.len(),
            "exactly the two cross-referenced points should be filtered out"
        );
    }

    #[test]
    fn invalid_schema_fails_validation_without_panicking() {
        let mut asset = sample_asset();
        asset.schema = "bogus".to_string();
        // Must return an Err, not panic -- mirrors this codebase's other
        // authored-Cromatolis loaders' hard "never panic on bad data" rule.
        assert!(asset.validate().is_err());
    }

    #[test]
    fn missing_asset_fails_to_load_without_panicking() {
        // Exercises the "missing/unparseable asset" path that
        // `build_all_generated_caves` falls back to `Vec::new()` on -- see
        // its `Err(err) => { warn!(...); Vec::new() }` arm above.
        let missing = CaveFeaturesAsset::load_owned("world.map.this_asset_does_not_exist");
        assert!(missing.is_err());
    }

    /// Builds a `GeneratedCave` directly from a size class's own profile,
    /// bypassing `build_generated_cave`'s `Land`-based altitude lookup
    /// (not available without a real `WorldSim`) so the size-scaling math
    /// itself can be exercised as pure geometry.
    fn generated_cave_for(size_class: SizeClass, feature_id: &str) -> GeneratedCave {
        let profile = size_class.profile();
        let hub_wpos = Vec2::new(1000, 1000);
        let hub_floor_z = 0;
        let hub = HubGeom {
            anchor2d: hub_wpos,
            floor_z: hub_floor_z,
            ceiling_z: hub_floor_z + profile.headroom as i32,
            radius: profile.hub_radius,
        };
        let mut branches = Vec::with_capacity(profile.branch_count);
        let mut max_reach = profile.hub_radius;
        for i in 0..profile.branch_count {
            let branch_seed = format!("{feature_id}#branch{i}");
            let angle =
                (i as f32 / profile.branch_count as f32) * TAU + fnv1a_unit(&branch_seed) * 0.5;
            let offset = Vec2::new(angle.cos(), angle.sin()) * profile.branch_length;
            let tip_wpos = hub_wpos + offset.map(|e| e.round() as i32);
            branches.push(BranchSeg {
                a: hub_wpos.with_z(hub_floor_z),
                b: tip_wpos.with_z(hub_floor_z),
                a_radius: profile.hub_radius,
                b_radius: profile.branch_radius,
                headroom: profile.headroom,
                curve: (fnv1a_unit(&branch_seed) - 0.5) * 0.6,
            });
            max_reach = max_reach.max(profile.branch_length + profile.branch_radius);
        }
        GeneratedCave {
            hub,
            branches,
            minerals: Vec::new(),
            bounds: (hub_wpos, max_reach + EDGE_SOFTNESS),
        }
    }

    #[test]
    fn bigger_size_classes_produce_a_larger_extent_than_smaller_ones() {
        let giant = generated_cave_for(SizeClass::Giant, "cave.giant").extent();
        let large = generated_cave_for(SizeClass::Large, "cave.large").extent();
        let medium = generated_cave_for(SizeClass::Medium, "cave.medium").extent();
        let small = generated_cave_for(SizeClass::Small, "cave.small").extent();

        assert!(
            giant > large,
            "giant ({giant}) should exceed large ({large})"
        );
        assert!(
            large > medium,
            "large ({large}) should exceed medium ({medium})"
        );
        assert!(
            medium > small,
            "medium ({medium}) should exceed small ({small})"
        );
    }

    #[test]
    fn every_generated_cave_has_a_non_empty_extent_and_shape() {
        for size_class in [
            SizeClass::Giant,
            SizeClass::Large,
            SizeClass::Medium,
            SizeClass::Small,
        ] {
            let cave = generated_cave_for(size_class, "cave.any");
            assert!(
                cave.extent() > 0.0,
                "{size_class:?} should have a positive extent"
            );
            assert!(
                !cave.branches.is_empty() || cave.hub.radius > 0.0,
                "{size_class:?} should generate at least a hub or a branch"
            );
        }
    }

    /// Full-world smoke test: generates the real Cromatolis map and
    /// confirms (a) the two catalog points cross-referenced to already-
    /// authored bespoke interiors are filtered out before any generation
    /// happens, and (b) a real non-excluded point of every size class
    /// resolves to a real chunk without panicking. Requires the real
    /// Cromatolis LFS assets pulled locally, same precedent as
    /// `cromatolis_interior.rs`'s own ignored full-world test.
    /// `cargo test -p xindeler-world cromatolis_cave_features:: -- --ignored`
    #[test]
    #[ignore]
    fn real_world_excludes_the_two_cross_referenced_points_and_generates_others() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let map_size = world.sim().map_size_lg();

        let asset = CaveFeaturesAsset::load_owned(CAVE_FEATURES_ASSET).unwrap();
        asset.validate().unwrap();

        // The generated-cave list itself must never contain the excluded
        // ids' geometry -- confirmed structurally: `build_all_generated_caves`
        // filters `EXCLUDED_FEATURE_IDS` out before `build_generated_cave`
        // ever runs on them, and every generated cave's hub position is
        // deterministic from its source feature's chunk position, so no
        // excluded feature's chunk can end up carved by this code path.
        for excluded_id in EXCLUDED_FEATURE_IDS {
            assert!(
                asset.features.iter().any(|f| &f.id == excluded_id),
                "expected the real catalog to still contain {excluded_id}"
            );
        }

        // A real non-excluded point of each size class should resolve to a
        // real chunk without panicking.
        for size_class in [
            SizeClass::Giant,
            SizeClass::Large,
            SizeClass::Medium,
            SizeClass::Small,
        ] {
            let feature = asset
                .features
                .iter()
                .find(|f| {
                    f.size_class == size_class && !EXCLUDED_FEATURE_IDS.contains(&f.id.as_str())
                })
                .unwrap_or_else(|| {
                    panic!("expected at least one non-excluded {size_class:?} point")
                });
            let chunk_pos = feature.position.to_chunk_pos(map_size);
            world
                .generate_chunk(index_ref, chunk_pos, None, || false, None, None)
                .expect("chunk generation must not fail for a real, in-bounds Cromatolis chunk");
        }
    }
}
