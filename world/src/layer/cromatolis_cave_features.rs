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
//! No content/NPC/loot population -- physical cave geometry only.

use crate::{Canvas, CanvasInfo, Land, util::SQUARE_4};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    terrain::{
        Block, CoordinateConversions, MapSizeLg, TerrainChunkSize, quadratic_nearest_point,
        river_spline_coeffs,
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
}

#[derive(Debug, Clone, Deserialize)]
struct CaveFeatureEntry {
    id: String,
    position: NormalizedPosition,
    size_class: SizeClass,
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
        bounds: (hub_wpos, max_reach + EDGE_SOFTNESS),
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
                Some(RelevantCave { hub, branches })
            }
        })
        .collect();
    if relevant.is_empty() {
        return;
    }

    canvas.foreach_col(|canvas, wpos2d, col| {
        let col_alt = col.alt;
        for cave in &relevant {
            if let Some(hub) = cave.hub {
                carve_hub(canvas, wpos2d, col_alt, hub);
            }
            for branch in &cave.branches {
                carve_branch(canvas, wpos2d, col_alt, branch);
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

fn carve_hub(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, hub: &HubGeom) {
    let dist = wpos2d
        .map(|e| e as f32)
        .distance(hub.anchor2d.map(|e| e as f32));
    if dist > hub.radius + EDGE_SOFTNESS {
        return;
    }
    if edge_weight(dist, hub.radius) <= 0.0 {
        return;
    }

    let ceiling_cap = (col_alt - SURFACE_MARGIN).floor() as i32;
    let ceiling_z = hub.ceiling_z.min(ceiling_cap);
    if ceiling_z <= hub.floor_z {
        return;
    }

    for z in hub.floor_z..=ceiling_z {
        canvas.set(wpos2d.with_z(z), Block::empty());
    }
}

fn carve_branch(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, seg: &BranchSeg) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let Some((t, dist)) = spline_sample(a2, b2, seg.curve, wpos2d.map(|e| e as f64 + 0.5)) else {
        return;
    };
    let radius = Lerp::lerp_unclamped(seg.a_radius as f64, seg.b_radius as f64, t) as f32;
    if edge_weight(dist as f32, radius) <= 0.0 {
        return;
    }

    let floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t);
    let ceiling_cap = (col_alt - SURFACE_MARGIN) as f64;
    let ceiling_z = (floor_z + seg.headroom as f64).min(ceiling_cap);
    if ceiling_z <= floor_z {
        return;
    }

    for z in floor_z.floor() as i32..=ceiling_z.ceil() as i32 {
        canvas.set(wpos2d.with_z(z), Block::empty());
    }
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
                },
                CaveFeatureEntry {
                    id: "cave.test_small".to_string(),
                    position: NormalizedPosition { x: 0.6, y: 0.6 },
                    size_class: SizeClass::Small,
                },
            ],
        }
    }

    #[test]
    fn validate_accepts_the_expected_schema_and_coordinate_space() {
        assert!(sample_asset().validate().is_ok());
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
                .generate_chunk(index_ref, chunk_pos, None, || false, None)
                .expect("chunk generation must not fail for a real, in-bounds Cromatolis chunk");
        }
    }
}
