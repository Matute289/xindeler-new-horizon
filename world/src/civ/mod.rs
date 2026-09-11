#![expect(dead_code)]

pub mod airship_travel;
mod econ;

#[cfg(feature = "airship_maps")]
pub mod airship_route_map;

use crate::{
    Index, IndexRef, Land,
    civ::airship_travel::Airships,
    config::CONFIG,
    sim::WorldSim,
    site::{self, Site as WorldSite, SiteKind, SitesGenMeta, namegen::NameGen},
    util::{DHashMap, NEIGHBORS, attempt, seed_expan},
};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    astar::Astar,
    calendar::Calendar,
    path::Path,
    spiral::Spiral2d,
    store::{Id, Store},
    terrain::{BiomeKind, CoordinateConversions, MapSizeLg, TerrainChunkSize, uniform_idx_as_vec2},
    vol::RectVolSize,
};
use common_base::prof_span;
use core::{fmt, hash::BuildHasherDefault, ops::Range};
use fxhash::FxHasher64;
use rand::{SeedableRng, prelude::*};
use rand_chacha::ChaChaRng;
use serde::Deserialize;
use std::borrow::Cow;
use tracing::{debug, info, warn};
use vek::*;

fn initial_civ_count(map_size_lg: MapSizeLg) -> u32 {
    // NOTE: since map_size_lg's dimensions must fit in a u16, we can safely add
    // them here.
    //
    // NOTE: 48 at "default" scale of 10 × 10 chunk bits (1024 × 1024 chunks).
    let cnt = (3 << (map_size_lg.vec().x + map_size_lg.vec().y)) >> 16;
    cnt.max(1) // we need at least one civ in order to generate a starting site
}

#[derive(Default)]
pub struct Civs {
    pub civs: Store<Civ>,
    pub places: Store<Place>,
    pub pois: Store<PointOfInterest>,

    pub tracks: Store<Track>,
    /// We use this hasher (FxHasher64) because
    /// (1) we don't care about DDOS attacks (ruling out SipHash);
    /// (2) we care about determinism across computers (ruling out AAHash);
    /// (3) we have 8-byte keys (for which FxHash is fastest).
    pub track_map: DHashMap<Id<Site>, DHashMap<Id<Site>, Id<Track>>>,

    pub bridges: DHashMap<Vec2<i32>, (Vec2<i32>, Id<Site>)>,

    pub sites: Store<Site>,
    pub airships: Airships,
}

// ---------------------------------------------------------------------------
// Authored Cromatolis settlements, landmarks & the site-pin fallback table.
//
// - The source of truth is `xindeler-open-world`; this engine only consumes a
//   compact runtime copy of it (`world.map.cromatolis_v0_sites`/`_landmarks`/
//   `_landmark_profiles`), so the game never depends on that other working tree
//   at runtime.
// - Loading is gated on `SimChunk::authored_region_id ==
//   Some(CROMATOLIS_V0_REGION_ID)` (the region registry `world/src/sim`
//   maintains), not a literal asset-name check, and never panics on
//   invalid/missing data -- it warns and falls back to procedural generation
//   instead (same posture as the terrain/water/biome loaders this crate already
//   uses).
// - `AuthoredCromatolisLandmarkProfiles` is ported as data + validation only.
//   Its physical-template/style/material fields describe a real bespoke
//   landmark renderer that exists in the reference engine
//   (`site::plot::CromatolisLandmark`, ~470 lines) but is explicitly NOT ported
//   here -- that is real future per-family generator work, out of this change's
//   scope. The profile is carried on `AuthoredLandmarkMeta` for a future
//   consumer; today every landmark still generates through the existing generic
//   `SiteKind` (`GiantTree`/`Citadel`/`ChapelSite`).
// - The real "site-pin" abstraction this adds is
//   `resolve_settlement_site_kind`: a small table read from
//   `settlement_template_contract.ron`'s family list (category+size ->
//   `xindeler_old_fallback`), replacing what used to be a 2-line hardcoded
//   `match`. Every family's fallback still resolves to `SiteKind::Camp` or
//   `SiteKind::Refactor` today -- that's the honest, measured baseline, not a
//   placeholder bug: no real per-family physical generator exists yet.
// ---------------------------------------------------------------------------

/// Authored settlement data for Cromatolis. The source-of-truth asset is
/// generated from `xindeler-open-world`, while the engine consumes this
/// compact runtime copy so it never depends on another working tree.
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisSettlements {
    schema: String,
    coordinate_space: String,
    settlements: Vec<AuthoredCromatolisSettlement>,
}

impl FileAsset for AuthoredCromatolisSettlements {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisSettlements {
    fn validate(&self, map_size: MapSizeLg) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_settlements.v1";
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
        let mut locations = std::collections::HashSet::new();
        let mut capitals = 0;
        let mut eligible_starts = 0;
        for settlement in &self.settlements {
            if settlement.id.is_empty() || !ids.insert(settlement.id.as_str()) {
                return Err(format!(
                    "duplicate or empty settlement id {}",
                    settlement.id
                ));
            }
            let location = settlement.center.to_chunk_pos(map_size);
            if !locations.insert((location.x, location.y)) {
                return Err(format!(
                    "duplicate settlement location ({}, {})",
                    location.x, location.y
                ));
            }
            if !settlement.population.is_valid() {
                return Err(format!("invalid population metadata for {}", settlement.id));
            }
            if settlement.category == AuthoredSettlementCategory::Capital {
                capitals += 1;
            }
            eligible_starts += usize::from(settlement.start_eligible);
        }

        if capitals != 1 {
            return Err(format!("expected exactly one capital, got {capitals}"));
        }
        if eligible_starts == 0 {
            return Err("no settlement is eligible as a player start".to_string());
        }
        Ok(())
    }
}

/// First physical landmark batch for Cromatolis. This remains separate from
/// settlements because landmark templates and future interactions evolve on
/// a different cadence from civilian population data.
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisLandmarks {
    schema: String,
    coordinate_space: String,
    landmarks: Vec<AuthoredCromatolisLandmark>,
}

impl FileAsset for AuthoredCromatolisLandmarks {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisLandmarks {
    fn validate(&self, map_size: MapSizeLg) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_landmarks.v1";
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
        let mut locations = std::collections::HashSet::new();
        for landmark in &self.landmarks {
            if landmark.id.is_empty() || !ids.insert(landmark.id.as_str()) {
                return Err(format!("duplicate or empty landmark id {}", landmark.id));
            }
            let location = landmark.center.to_chunk_pos(map_size);
            if !locations.insert((location.x, location.y)) {
                return Err(format!(
                    "duplicate landmark location ({}, {})",
                    location.x, location.y
                ));
            }
        }
        Ok(())
    }
}

/// Geometry profiles for authored Cromatolis landmarks, ported as data +
/// validation only (see the module-level note above -- the renderer that
/// would consume these is explicitly out of this change's scope).
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisLandmarkProfiles {
    schema: String,
    coordinate_space: String,
    entries: Vec<AuthoredLandmarkProfile>,
}

impl FileAsset for AuthoredCromatolisLandmarkProfiles {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisLandmarkProfiles {
    fn validate(&self, landmarks: &AuthoredCromatolisLandmarks) -> Result<(), String> {
        use AuthoredLandmarkPhysicalTemplate as Template;
        use AuthoredLandmarkStyle as Style;

        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_landmark_profiles.v2";
        if self.schema != EXPECTED_SCHEMA {
            return Err(format!(
                "expected schema {EXPECTED_SCHEMA}, got {}",
                self.schema
            ));
        }
        if self.coordinate_space != "inherits_landmark_center" {
            return Err(format!(
                "expected inherited landmark coordinate space, got {}",
                self.coordinate_space
            ));
        }

        let mut profiles = std::collections::HashSet::new();
        for profile in &self.entries {
            if profile.site_id.is_empty() || !profiles.insert(profile.site_id.as_str()) {
                return Err(format!(
                    "duplicate or empty landmark profile id {}",
                    profile.site_id
                ));
            }
            if !(4..=96).contains(&profile.footprint_radius)
                || !(12..=320).contains(&profile.height)
                || profile.light_range < 0
            {
                return Err(format!(
                    "invalid geometry dimensions for {}",
                    profile.site_id
                ));
            }

            let Some(landmark) = landmarks
                .landmarks
                .iter()
                .find(|landmark| landmark.id == profile.site_id)
            else {
                return Err(format!(
                    "profile references unknown landmark {}",
                    profile.site_id
                ));
            };
            let valid_kind = matches!(
                (landmark.kind, profile.physical_template, profile.style),
                (
                    AuthoredLandmarkKind::TreeOfLife | AuthoredLandmarkKind::TreeOfSouls,
                    Template::GiantTree,
                    Style::AncientCanopy
                ) | (
                    AuthoredLandmarkKind::BlackTower,
                    Template::BlackTower,
                    Style::CrownedSpire
                ) | (
                    AuthoredLandmarkKind::Lighthouse,
                    Template::Lighthouse,
                    Style::KeeperHouse | Style::StoneBeacon | Style::BandedHarbour
                ) | (
                    AuthoredLandmarkKind::ArchWrightTemple,
                    Template::Chapel,
                    Style::MonumentalChapel
                ) | (
                    AuthoredLandmarkKind::Harbour,
                    Template::Harbour,
                    Style::RiverPort
                )
            );
            if !valid_kind {
                return Err(format!(
                    "profile type does not match landmark {}",
                    profile.site_id
                ));
            }
        }

        if profiles.len() != landmarks.landmarks.len()
            || landmarks
                .landmarks
                .iter()
                .any(|landmark| !profiles.contains(landmark.id.as_str()))
        {
            return Err(
                "every authored landmark must have exactly one physical profile".to_string(),
            );
        }
        Ok(())
    }

    fn profile_for(&self, site_id: &str) -> Option<AuthoredLandmarkProfile> {
        self.entries
            .iter()
            .find(|profile| profile.site_id == site_id)
            .cloned()
    }
}

/// Logical route edges connecting authored Cromatolis settlements. This
/// reuses the existing civ `Track`/`Path` pathfinding system directly (see
/// `Civs::establish_authored_cromatolis_routes`) rather than inventing a
/// parallel travel graph -- the physical road/route raster remains a
/// separate terrain layer, this is only the economy/RTSim travel edge list.
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisRouteGraph {
    schema: String,
    coordinate_space: String,
    routes: Vec<AuthoredCromatolisRoute>,
}

impl FileAsset for AuthoredCromatolisRouteGraph {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisRouteGraph {
    fn validate(&self, map_size: MapSizeLg) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_route_graph.v1";
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
        for route in &self.routes {
            if route.id.is_empty() || !ids.insert(route.id.as_str()) {
                return Err(format!("duplicate or empty route id {}", route.id));
            }
            if route.start_site_id == route.end_site_id || route.points.len() < 2 {
                return Err(format!("invalid route edge {}", route.id));
            }
            if route
                .points
                .iter()
                .map(|point| point.to_chunk_pos(map_size))
                .collect::<std::collections::HashSet<_>>()
                .len()
                < 2
            {
                return Err(format!("route {} collapses to one chunk", route.id));
            }
        }
        if self.routes.is_empty() {
            return Err("route graph contains no edges".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisRoute {
    id: String,
    start_site_id: String,
    end_site_id: String,
    points: Vec<AuthoredMapPoint>,
}

/// Physical bridges are independent of the route graph: a route describes
/// travel, this asset owns the elevated crossing that preserves the water
/// beneath it (bridges never fill water in to make room for themselves).
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisBridges {
    schema: String,
    coordinate_space: String,
    bridges: Vec<AuthoredCromatolisBridge>,
}

impl FileAsset for AuthoredCromatolisBridges {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisBridges {
    fn validate(&self, map_size: MapSizeLg) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_bridges.v1";
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
        if self.bridges.len() != 12 {
            return Err(format!(
                "expected 12 authored bridge crossings, got {}",
                self.bridges.len()
            ));
        }

        let mut ids = std::collections::HashSet::new();
        let mut spans = std::collections::HashSet::new();
        for bridge in &self.bridges {
            if bridge.id.is_empty() || !ids.insert(bridge.id.as_str()) {
                return Err(format!("duplicate or empty bridge id {}", bridge.id));
            }
            let start = bridge.start.to_chunk_pos(map_size);
            let end = bridge.end.to_chunk_pos(map_size);
            if start == end {
                return Err(format!("bridge {} collapses to one chunk", bridge.id));
            }
            let span = if start.x < end.x || (start.x == end.x && start.y <= end.y) {
                (start, end)
            } else {
                (end, start)
            };
            if !spans.insert(span) {
                return Err(format!("duplicate bridge span for {}", bridge.id));
            }
            if !bridge.dimensions_are_valid() {
                return Err(format!("invalid authored dimensions for {}", bridge.id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisBridge {
    id: String,
    name: String,
    style: AuthoredBridgeStyle,
    start: AuthoredMapPoint,
    end: AuthoredMapPoint,
    deck_width_m: f32,
    deck_clearance_m: f32,
    deck_thickness_m: f32,
}

impl AuthoredCromatolisBridge {
    fn dimensions_are_valid(&self) -> bool {
        self.deck_width_m.is_finite()
            && self.deck_clearance_m.is_finite()
            && self.deck_thickness_m.is_finite()
            && self.deck_width_m >= 4.0
            && self.deck_clearance_m >= 2.0
            && (2.0..=3.0).contains(&self.deck_thickness_m)
    }

    fn design(&self) -> site::AuthoredBridgeDesign {
        let dimensions = || {
            (
                self.deck_width_m.round() as i32,
                self.deck_clearance_m.round() as i32,
                self.deck_thickness_m.round() as i32,
            )
        };
        match self.style {
            AuthoredBridgeStyle::GrandStoneIron => {
                let (deck_width, clearance, deck_thickness) = dimensions();
                site::AuthoredBridgeDesign::GrandStoneIron {
                    deck_width,
                    clearance,
                    deck_thickness,
                }
            },
            AuthoredBridgeStyle::StoneArch => {
                let (deck_width, clearance, deck_thickness) = dimensions();
                site::AuthoredBridgeDesign::StoneArch {
                    deck_width,
                    clearance,
                    deck_thickness,
                }
            },
            AuthoredBridgeStyle::TimberFootbridge => {
                let (deck_width, clearance, deck_thickness) = dimensions();
                site::AuthoredBridgeDesign::TimberFootbridge {
                    deck_width,
                    clearance,
                    deck_thickness,
                }
            },
            AuthoredBridgeStyle::NaturalStoneEarth => {
                let (deck_width, clearance, deck_thickness) = dimensions();
                site::AuthoredBridgeDesign::NaturalStoneEarth {
                    deck_width,
                    clearance,
                    deck_thickness,
                }
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredBridgeStyle {
    GrandStoneIron,
    StoneArch,
    TimberFootbridge,
    NaturalStoneEarth,
}

/// The bridge data remains authored and exportable, but physical bridge
/// sites stay disabled until their isolated visual previews are approved
/// (matches `xindeler-old`'s current live behavior, ported unchanged -- this
/// is not this row's call to make). A developer can materialize exactly one
/// bridge with `XINDELER_CROMATOLIS_BRIDGE_PREVIEW=bridge.<id>` (or `all`)
/// for review.
fn cromatolis_authored_bridge_preview() -> Option<String> {
    std::env::var("XINDELER_CROMATOLIS_BRIDGE_PREVIEW")
        .ok()
        .filter(|preview| !preview.trim().is_empty())
}

fn bridge_is_selected_for_preview(preview: &str, bridge_id: &str) -> bool {
    preview == "all" || preview == bridge_id
}

#[derive(Debug, Clone)]
struct AuthoredBridgeMeta {
    #[expect(dead_code)]
    id: String,
    name: String,
    design: site::AuthoredBridgeDesign,
}

/// Authored linear defensive fortifications (walls + gates) for Cromatolis.
/// Unlike every other `cromatolis_v0_*` authored asset, this source uses raw
/// source-map **pixel** coordinates (`coordinate_space:
/// "source_pixels_xy_top_left_origin"`), not the usual 0-1 normalized space
/// -- normalization has to be computed against this asset's own `source_map`
/// dimensions (see `normalize_point`), never a hardcoded literal.
#[derive(Debug, Deserialize)]
struct AuthoredCromatolisFortifications {
    schema: String,
    coordinate_space: String,
    source_map: AuthoredFortificationSourceMap,
    #[expect(dead_code)]
    notes: Vec<String>,
    fortifications: Vec<AuthoredCromatolisFortification>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct AuthoredFortificationSourceMap {
    width_px: u32,
    height_px: u32,
}

impl FileAsset for AuthoredCromatolisFortifications {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl AuthoredCromatolisFortifications {
    /// Takes `map_size` and fails hard on chunk-quantized start/end collapse
    /// or duplicate spans, the same as `AuthoredCromatolisBridges::validate`
    /// -- so the failure mode is consistent between the two loaders instead
    /// of fortifications deferring that check to a per-item skip-and-warn at
    /// establishment time.
    fn validate(&self, map_size: MapSizeLg) -> Result<(), String> {
        const EXPECTED_SCHEMA: &str = "xindeler_open_world.authored_fortifications.v1";
        const EXPECTED_COORDINATE_SPACE: &str = "source_pixels_xy_top_left_origin";

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
        if self.source_map.width_px < 2 || self.source_map.height_px < 2 {
            return Err(format!(
                "invalid fortification source_map dimensions {}x{}",
                self.source_map.width_px, self.source_map.height_px
            ));
        }

        let mut wall_ids = std::collections::HashSet::new();
        let mut gate_ids = std::collections::HashSet::new();
        let mut spans = std::collections::HashSet::new();
        for fortification in &self.fortifications {
            if fortification.id.is_empty() || !wall_ids.insert(fortification.id.as_str()) {
                return Err(format!(
                    "duplicate or empty fortification id {}",
                    fortification.id
                ));
            }
            if fortification.start.x == fortification.end.x
                && fortification.start.y == fortification.end.y
            {
                return Err(format!(
                    "fortification {} collapses to a single point",
                    fortification.id
                ));
            }
            let start = self
                .normalize_point(fortification.start)
                .to_chunk_pos(map_size);
            let end = self
                .normalize_point(fortification.end)
                .to_chunk_pos(map_size);
            if start == end {
                return Err(format!(
                    "fortification {} collapses to a single chunk",
                    fortification.id
                ));
            }
            let span = if start.x < end.x || (start.x == end.x && start.y <= end.y) {
                (start, end)
            } else {
                (end, start)
            };
            if !spans.insert(span) {
                return Err(format!(
                    "duplicate fortification span for {}",
                    fortification.id
                ));
            }
            if !fortification.depth_m.is_finite()
                || fortification.depth_m <= 0.0
                || !fortification.height_m.is_finite()
                || fortification.height_m <= 0.0
            {
                return Err(format!("invalid wall dimensions for {}", fortification.id));
            }
            if fortification.gates.is_empty() {
                return Err(format!("fortification {} has no gates", fortification.id));
            }
            for gate in &fortification.gates {
                if gate.id.is_empty() || !gate_ids.insert(gate.id.as_str()) {
                    return Err(format!("duplicate or empty gate id {}", gate.id));
                }
                if !gate.clear_width_m.is_finite()
                    || gate.clear_width_m <= 0.0
                    || !gate.height_m.is_finite()
                    || gate.height_m <= 0.0
                {
                    return Err(format!("invalid gate dimensions for {}", gate.id));
                }
            }
        }
        Ok(())
    }

    /// Normalizes a raw source-map pixel point against this asset's own
    /// `source_map` dimensions, per the coordinate-space contract in the
    /// module-level doc comment above. Never hardcode `2048`/`1536` --
    /// always read them from here.
    fn normalize_point(&self, point: AuthoredPixelPoint) -> AuthoredMapPoint {
        AuthoredMapPoint {
            x: point.x / (self.source_map.width_px as f32 - 1.0).max(1.0),
            y: point.y / (self.source_map.height_px as f32 - 1.0).max(1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct AuthoredPixelPoint {
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisFortification {
    id: String,
    name: String,
    #[expect(dead_code)]
    material: String,
    start: AuthoredPixelPoint,
    end: AuthoredPixelPoint,
    depth_m: f32,
    height_m: f32,
    gates: Vec<AuthoredCromatolisGate>,
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisGate {
    id: String,
    #[expect(dead_code)]
    material: String,
    center: AuthoredPixelPoint,
    clear_width_m: f32,
    height_m: f32,
    /// The gate's physical open/closed state. This is static, locked-in
    /// content confirmed against lore/settlement data -- deliberately not a
    /// day/night schedule (none of these gates need one) -- but, unlike the
    /// bridge style enum, it's plain authored data on the RON record itself
    /// rather than a second, hand-synced source of truth in Rust.
    default_open: bool,
}

#[derive(Debug, Clone)]
struct AuthoredFortificationMeta {
    #[expect(dead_code)]
    id: String,
    name: String,
    design: site::AuthoredFortificationDesign,
}

impl AuthoredCromatolisFortification {
    /// Converts to the generic renderer's design type. Gate positions are
    /// stored as a fraction `t` of the wall's own start->end span, computed
    /// once here in the authored pixel space, so they land exactly on the
    /// wall regardless of chunk-position rounding.
    fn meta(&self) -> AuthoredFortificationMeta {
        let delta = Vec2::new(self.end.x - self.start.x, self.end.y - self.start.y);
        let len_sq = delta.x * delta.x + delta.y * delta.y;
        AuthoredFortificationMeta {
            id: self.id.clone(),
            name: self.name.clone(),
            design: site::AuthoredFortificationDesign {
                depth: self.depth_m.round() as i32,
                height: self.height_m.round() as i32,
                gates: self
                    .gates
                    .iter()
                    .map(|gate| {
                        let gate_delta =
                            Vec2::new(gate.center.x - self.start.x, gate.center.y - self.start.y);
                        let t = if len_sq > 0.0 {
                            ((gate_delta.x * delta.x + gate_delta.y * delta.y) / len_sq)
                                .clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        site::AuthoredFortificationGateDesign {
                            t,
                            clear_width: gate.clear_width_m.round() as i32,
                            height: gate.height_m.round() as i32,
                            open: gate.default_open,
                        }
                    })
                    .collect(),
            },
        }
    }
}

/// Geometry data for a manually placed Cromatolis landmark. Data-only port
/// (see the module-level note above): describes what a future bespoke
/// renderer would need, but nothing in this repo consumes it yet.
#[derive(Debug, Clone, Deserialize)]
struct AuthoredLandmarkProfile {
    site_id: String,
    physical_template: AuthoredLandmarkPhysicalTemplate,
    style: AuthoredLandmarkStyle,
    #[expect(dead_code)]
    material: AuthoredLandmarkMaterial,
    footprint_radius: i32,
    height: i32,
    #[expect(dead_code)]
    keeper_house: bool,
    light_range: i32,
    #[expect(dead_code)]
    #[serde(default)]
    facing: AuthoredLandmarkFacing,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredLandmarkPhysicalTemplate {
    GiantTree,
    BlackTower,
    Lighthouse,
    Chapel,
    Harbour,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredLandmarkStyle {
    AncientCanopy,
    CrownedSpire,
    KeeperHouse,
    StoneBeacon,
    BandedHarbour,
    MonumentalChapel,
    RiverPort,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredLandmarkMaterial {
    LivingWood,
    BlackStone,
    PaleStone,
    WeatheredStone,
    PaintedMasonry,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
enum AuthoredLandmarkFacing {
    #[default]
    North,
    South,
    East,
    West,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
enum AuthoredSettlementCategory {
    Capital,
    City,
    Town,
    Village,
    Hamlet,
    Inn,
    Post,
}

impl AuthoredSettlementCategory {
    /// The lowercase id `settlement_template_contract.ron`'s
    /// `template_families[].category` field uses for this category.
    const fn contract_key(self) -> &'static str {
        match self {
            Self::Capital => "capital",
            Self::City => "city",
            Self::Town => "town",
            Self::Village => "village",
            Self::Hamlet => "hamlet",
            Self::Inn => "inn",
            Self::Post => "post",
        }
    }

    /// The `Camp`/`Refactor` split used before the data-driven table
    /// existed -- kept as the safety-net default for when the contract
    /// can't be loaded at all, or has no family covering a given
    /// category+size (see `resolve_settlement_site_kind`).
    const fn default_site_kind(self) -> SiteKind {
        match self {
            Self::Inn | Self::Post => SiteKind::Camp,
            Self::Capital | Self::City | Self::Town | Self::Village | Self::Hamlet => {
                SiteKind::Refactor
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
enum AuthoredLandmarkKind {
    TreeOfLife,
    TreeOfSouls,
    BlackTower,
    Lighthouse,
    ArchWrightTemple,
    Harbour,
}

/// The *generic* engine `SiteKind` a landmark procedurally generates as.
/// Distinct from `AuthoredLandmarkPhysicalTemplate` above, which describes
/// the (unported) bespoke renderer's physical shape -- this only ever
/// resolves to pre-existing, non-Cromatolis-specific generators.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredLandmarkTemplate {
    GiantTree,
    Citadel,
    ChapelSite,
}

impl AuthoredLandmarkTemplate {
    const fn site_kind(self) -> SiteKind {
        match self {
            Self::GiantTree => SiteKind::GiantTree,
            Self::Citadel => SiteKind::Citadel,
            Self::ChapelSite => SiteKind::ChapelSite,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
enum AuthoredSettlementSize {
    VeryLarge,
    Large,
    Medium,
    Small,
    Minimal,
}

impl AuthoredSettlementSize {
    const fn city_scale(self) -> f32 {
        match self {
            Self::VeryLarge => 0.95,
            Self::Large => 0.55,
            Self::Medium => 0.28,
            Self::Small => 0.11,
            Self::Minimal => 0.03,
        }
    }

    /// The lowercase id `settlement_template_contract.ron`'s
    /// `template_families[].supported_sizes` entries use for this size.
    const fn contract_key(self) -> &'static str {
        match self {
            Self::VeryLarge => "very_large",
            Self::Large => "large",
            Self::Medium => "medium",
            Self::Small => "small",
            Self::Minimal => "minimal",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct AuthoredSettlementPopulation {
    tag: AuthoredSettlementPopulationTag,
    peoples: Vec<AuthoredSettlementPeople>,
    #[serde(default)]
    future_peoples: Vec<String>,
}

impl AuthoredSettlementPopulation {
    fn is_valid(&self) -> bool {
        if self.peoples.is_empty() {
            return false;
        }
        let unique = self
            .peoples
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique.len() != self.peoples.len() {
            return false;
        }
        let future_unique = self
            .future_peoples
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if future_unique.len() != self.future_peoples.len()
            || self.future_peoples.iter().any(|people| people.is_empty())
        {
            return false;
        }
        self.tag
            .expected_peoples()
            .is_none_or(|expected| self.peoples == expected)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum AuthoredSettlementPopulationTag {
    Human,
    Elven,
    Dwarf,
    Orc,
    Goblins,
    Dhampirs,
    HumanoidMix,
    SuperMix,
    DarkMix,
    Custom,
}

impl AuthoredSettlementPopulationTag {
    const fn expected_peoples(self) -> Option<&'static [AuthoredSettlementPeople]> {
        use AuthoredSettlementPeople::{Dhampirs, Dwarf, Elven, Goblins, Human, Orc};
        match self {
            Self::Human => Some(&[Human]),
            Self::Elven => Some(&[Elven]),
            Self::Dwarf => Some(&[Dwarf]),
            Self::Orc => Some(&[Orc]),
            Self::Goblins => Some(&[Goblins]),
            Self::Dhampirs => Some(&[Dhampirs]),
            Self::HumanoidMix => Some(&[Human, Elven, Dwarf]),
            Self::SuperMix => Some(&[Elven, Human, Dwarf, Orc, Goblins]),
            Self::DarkMix => Some(&[Orc, Goblins, Dhampirs]),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
enum AuthoredSettlementPeople {
    Human,
    Elven,
    Dwarf,
    Orc,
    Goblins,
    Dhampirs,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct AuthoredMapPoint {
    /// Normalized against the source map, whose origin is top-left.
    x: f32,
    y: f32,
}

impl AuthoredMapPoint {
    fn to_chunk_pos(self, map_size: MapSizeLg) -> Vec2<i32> {
        let size = map_size.chunks();
        let x = (self.x.clamp(0.0, 1.0) * (size.x.saturating_sub(1)) as f32).round() as i32;
        // Sim coordinates grow northward; authored map pixels grow southward.
        let y = ((1.0 - self.y.clamp(0.0, 1.0)) * (size.y.saturating_sub(1)) as f32).round() as i32;
        Vec2::new(x, y)
    }
}

/// Loads the Cromatolis source map canvas's pixel dimensions from the same
/// authored asset every other reader of this figure already uses --
/// `AuthoredCromatolisFortifications::source_map` -- rather than a second,
/// independently-hardcoded `2048x1536` constant. The asset cache (see
/// `AssetExt::load_owned`) makes repeated calls cheap. Returns `None` if the
/// asset fails to load.
pub fn cromatolis_source_pixels() -> Option<Vec2<u32>> {
    AuthoredCromatolisFortifications::load_owned("world.map.cromatolis_v0_fortifications")
        .ok()
        .map(|fortifications| {
            Vec2::new(
                fortifications.source_map.width_px,
                fortifications.source_map.height_px,
            )
        })
}

/// Converts a raw pixel coordinate on the Cromatolis source map canvas
/// (`0..source_pixels`, top-left origin) into a world chunk-center
/// position, using the exact same normalize-then-
/// [`AuthoredMapPoint::to_chunk_pos`] pattern every authored Cromatolis
/// asset loader above already uses -- so a raw QA pixel coordinate lands on
/// the same world position an authored feature at that pixel would.
/// `source_pixels` is the canvas's own pixel dimensions -- see
/// [`cromatolis_source_pixels`] -- never a hardcoded literal, same rule
/// `AuthoredCromatolisFortifications::normalize_point` already follows.
///
/// Used by the `/cromatolis_goto` admin command (`server/src/cmd.rs`).
/// Returns `None` if the pixel falls outside the canvas.
pub fn cromatolis_source_pixel_to_wpos(
    source_pixel: Vec2<f32>,
    source_pixels: Vec2<u32>,
    map_size: MapSizeLg,
) -> Option<Vec2<i32>> {
    if source_pixels.x < 2 || source_pixels.y < 2 {
        return None;
    }

    let max_x = (source_pixels.x - 1) as f32;
    let max_y = (source_pixels.y - 1) as f32;
    if !(0.0..=max_x).contains(&source_pixel.x) || !(0.0..=max_y).contains(&source_pixel.y) {
        return None;
    }

    let point = AuthoredMapPoint {
        x: source_pixel.x / max_x,
        y: source_pixel.y / max_y,
    };
    Some(point.to_chunk_pos(map_size).cpos_to_wpos_center())
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisSettlement {
    id: String,
    name: String,
    category: AuthoredSettlementCategory,
    size: AuthoredSettlementSize,
    population: AuthoredSettlementPopulation,
    center: AuthoredMapPoint,
    requires_capital_castle: bool,
    #[serde(default = "default_start_eligible")]
    start_eligible: bool,
}

const fn default_start_eligible() -> bool { true }

#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisLandmark {
    id: String,
    name: String,
    kind: AuthoredLandmarkKind,
    template: AuthoredLandmarkTemplate,
    center: AuthoredMapPoint,
}

#[derive(Debug, Clone)]
struct AuthoredSettlementMeta {
    id: String,
    name: String,
    category: AuthoredSettlementCategory,
    size: AuthoredSettlementSize,
    #[expect(dead_code)]
    population: AuthoredSettlementPopulation,
    #[expect(dead_code)]
    requires_capital_castle: bool,
    start_eligible: bool,
}

#[derive(Debug, Clone)]
struct AuthoredLandmarkMeta {
    #[expect(dead_code)]
    id: String,
    name: String,
    #[expect(dead_code)]
    kind: AuthoredLandmarkKind,
    /// Carried through for a future bespoke-renderer consumer (see the
    /// module-level note above); nothing reads this yet.
    #[expect(dead_code)]
    profile: Option<AuthoredLandmarkProfile>,
}

/// `settlement_template_contract.ron`'s own schema id.
const SETTLEMENT_TEMPLATE_CONTRACT_SCHEMA: &str =
    "xindeler_open_world.settlement_template_contract.v1";

/// The design contract that defines Cromatolis's 15+ settlement families.
/// This is the data half of the real `AuthoredSitePin` abstraction:
/// `resolve_settlement_site_kind` reads `template_families` to turn a
/// settlement's category+size into a `SiteKind`, instead of a hardcoded
/// match. Only the fields that resolution needs are captured here --
/// `culture_overlays`/`site_overrides` and the per-family
/// `terrain_requirement`/`terrain_adaptation`/`districts` aren't consumed by
/// any generator yet, and RON deserialization ignores the unrecognized
/// fields rather than erroring on them.
#[derive(Debug, Deserialize)]
struct SettlementTemplateContract {
    schema: String,
    template_families: Vec<SettlementTemplateFamily>,
}

#[derive(Debug, Deserialize)]
struct SettlementTemplateFamily {
    id: String,
    category: String,
    supported_sizes: Vec<String>,
    /// The `SiteKind` family name currently falls back to for this family
    /// (`"camp"`, `"refactor_city"`, or `"refactor_city_with_castle"`). See
    /// `site_kind_for_fallback_name`.
    xindeler_old_fallback: String,
}

impl FileAsset for SettlementTemplateContract {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl SettlementTemplateContract {
    fn validate(&self) -> Result<(), String> {
        if self.schema != SETTLEMENT_TEMPLATE_CONTRACT_SCHEMA {
            return Err(format!(
                "expected schema {SETTLEMENT_TEMPLATE_CONTRACT_SCHEMA}, got {}",
                self.schema
            ));
        }
        if self.template_families.is_empty() {
            return Err("settlement template contract has no families".to_string());
        }
        Ok(())
    }
}

/// Turns `settlement_template_contract.ron`'s
/// `template_families[].xindeler_old_fallback` string into the `SiteKind`
/// it names. Every family currently names one of these two -- there is no
/// real per-family generator yet (see the module-level note above) -- but
/// resolving by name (rather than assuming) means the day a family's
/// fallback changes to something else, this returns `None` and the caller
/// applies the same safe default it would for a missing family, instead of
/// silently mis-resolving.
fn site_kind_for_fallback_name(name: &str) -> Option<SiteKind> {
    match name {
        "camp" => Some(SiteKind::Camp),
        "refactor_city" | "refactor_city_with_castle" => Some(SiteKind::Refactor),
        _ => None,
    }
}

/// The real `AuthoredSitePin` abstraction (see the module-level note above):
/// resolves a settlement's `SiteKind` from `settlement_template_contract.ron`'s
/// family table instead of a hardcoded match. Falls back to
/// `AuthoredSettlementCategory::default_site_kind` (the same `Camp`/`Refactor`
/// split used before this table existed) whenever the contract is
/// unavailable, has no family for this category+size, or names a fallback
/// this engine doesn't recognize -- never panics, never silently drops the
/// settlement.
fn resolve_settlement_site_kind(
    contract: Option<&SettlementTemplateContract>,
    category: AuthoredSettlementCategory,
    size: AuthoredSettlementSize,
) -> SiteKind {
    contract
        .and_then(|contract| {
            contract.template_families.iter().find(|family| {
                family.category == category.contract_key()
                    && family
                        .supported_sizes
                        .iter()
                        .any(|s| s == size.contract_key())
            })
        })
        .and_then(|family| site_kind_for_fallback_name(&family.xindeler_old_fallback))
        .unwrap_or_else(|| category.default_site_kind())
}

// Change this to get rid of particularly horrid seeds
const SEED_SKIP: u8 = 5;
const POI_THINNING_DIST_SQRD: i32 = 300;

pub struct GenCtx<'a, R: Rng> {
    sim: &'a mut WorldSim,
    rng: R,
}

struct ProximitySpec {
    location: Vec2<i32>,
    min_distance: Option<i32>,
    max_distance: Option<i32>,
}

impl ProximitySpec {
    pub fn satisfied_by(&self, site: Vec2<i32>) -> bool {
        let distance_squared = site.distance_squared(self.location);
        let min_ok = self
            .min_distance
            .map(|mind| distance_squared > (mind * mind))
            .unwrap_or(true);
        let max_ok = self
            .max_distance
            .map(|maxd| distance_squared < (maxd * maxd))
            .unwrap_or(true);
        min_ok && max_ok
    }

    pub fn avoid(location: Vec2<i32>, min_distance: i32) -> Self {
        ProximitySpec {
            location,
            min_distance: Some(min_distance),
            max_distance: None,
        }
    }

    pub fn be_near(location: Vec2<i32>, max_distance: i32) -> Self {
        ProximitySpec {
            location,
            min_distance: None,
            max_distance: Some(max_distance),
        }
    }
}

struct ProximityRequirementsBuilder {
    all_of: Vec<ProximitySpec>,
    any_of: Vec<ProximitySpec>,
}

impl ProximityRequirementsBuilder {
    pub fn finalize(self, world_dims: &Aabr<i32>) -> ProximityRequirements {
        let location_hint = self.location_hint(world_dims);
        ProximityRequirements {
            all_of: self.all_of,
            any_of: self.any_of,
            location_hint,
        }
    }

    fn location_hint(&self, world_dims: &Aabr<i32>) -> Aabr<i32> {
        let bounding_box_of_point = |point: Vec2<i32>, max_distance: i32| Aabr {
            min: Vec2 {
                x: point.x - max_distance,
                y: point.y - max_distance,
            },
            max: Vec2 {
                x: point.x + max_distance,
                y: point.y + max_distance,
            },
        };
        let any_of_hint = self
            .any_of
            .iter()
            .fold(None, |acc, spec| match spec.max_distance {
                None => acc,
                Some(max_distance) => {
                    let bounding_box_of_new_point =
                        bounding_box_of_point(spec.location, max_distance);
                    match acc {
                        None => Some(bounding_box_of_new_point),
                        Some(acc) => Some(acc.union(bounding_box_of_new_point)),
                    }
                },
            })
            .map(|hint| hint.intersection(*world_dims))
            .unwrap_or_else(|| world_dims.to_owned());

        self.all_of
            .iter()
            .fold(any_of_hint, |acc, spec| match spec.max_distance {
                None => acc,
                Some(max_distance) => {
                    let bounding_box_of_new_point =
                        bounding_box_of_point(spec.location, max_distance);
                    acc.intersection(bounding_box_of_new_point)
                },
            })
    }

    pub fn new() -> Self {
        Self {
            all_of: Vec::new(),
            any_of: Vec::new(),
        }
    }

    pub fn avoid_all_of(
        mut self,
        locations: impl Iterator<Item = Vec2<i32>>,
        distance: i32,
    ) -> Self {
        let specs = locations.map(|loc| ProximitySpec::avoid(loc, distance));
        self.all_of.extend(specs);
        self
    }

    pub fn close_to_one_of(
        mut self,
        locations: impl Iterator<Item = Vec2<i32>>,
        distance: i32,
    ) -> Self {
        let specs = locations.map(|loc| ProximitySpec::be_near(loc, distance));
        self.any_of.extend(specs);
        self
    }
}

struct ProximityRequirements {
    all_of: Vec<ProximitySpec>,
    any_of: Vec<ProximitySpec>,
    location_hint: Aabr<i32>,
}

impl ProximityRequirements {
    pub fn satisfied_by(&self, site: Vec2<i32>) -> bool {
        if self.location_hint.contains_point(site) {
            let all_of_compliance = self.all_of.iter().all(|spec| spec.satisfied_by(site));
            let any_of_compliance =
                self.any_of.is_empty() || self.any_of.iter().any(|spec| spec.satisfied_by(site));
            all_of_compliance && any_of_compliance
        } else {
            false
        }
    }
}

impl<R: Rng> GenCtx<'_, R> {
    pub fn reseed(&mut self) -> GenCtx<'_, impl Rng + use<R>> {
        let mut entropy = self.rng.random::<[u8; 32]>();
        entropy[0] = entropy[0].wrapping_add(SEED_SKIP); // Skip bad seeds
        GenCtx {
            sim: self.sim,
            rng: ChaChaRng::from_seed(entropy),
        }
    }
}

#[derive(Debug)]
pub enum WorldCivStage {
    /// Civilization creation, how many out of how many civilizations have been
    /// generated yet
    CivCreation(u32, u32),
    SiteGeneration,
}

impl Civs {
    pub fn generate(
        seed: u32,
        sim: &mut WorldSim,
        index: &mut Index,
        calendar: Option<&Calendar>,
        report_stage: &dyn Fn(WorldCivStage),
    ) -> Self {
        prof_span!("Civs::generate");
        let mut this = Self::default();
        let rng = ChaChaRng::from_seed(seed_expan::rng_state(seed));
        let name_rng = rng.clone();
        let mut name_ctx = GenCtx { sim, rng: name_rng };
        if index.features().peak_naming {
            info!("starting peak naming");
            this.name_peaks(&mut name_ctx);
        }
        if index.features().biome_naming {
            info!("starting biome naming");
            this.name_biomes(&mut name_ctx);
        }

        let initial_civ_count = initial_civ_count(sim.map_size_lg());

        // Region-scoped (not a literal asset-name check): only Cromatolis
        // ships these authored settlement/landmark pins today, gated the
        // same way the terrain/water/biome authored layers already are.
        let authored_cromatolis = sim.chunks.first().is_some_and(|chunk| {
            chunk.authored_region_id == Some(crate::sim::CROMATOLIS_V0_REGION_ID)
        });
        let authored_settlements = if authored_cromatolis {
            match AuthoredCromatolisSettlements::load_owned("world.map.cromatolis_v0_sites") {
                Ok(settlements) => match settlements.validate(sim.map_size_lg()) {
                    Ok(()) => Some(settlements),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate Cromatolis authored settlements; using procedural \
                             sites"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis authored settlements; using procedural sites"
                    );
                    None
                },
            }
        } else {
            None
        };
        let authored_landmarks = if authored_cromatolis {
            match AuthoredCromatolisLandmarks::load_owned("world.map.cromatolis_v0_landmarks") {
                Ok(landmarks) => match landmarks.validate(sim.map_size_lg()) {
                    Ok(()) => Some(landmarks),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate Cromatolis authored landmarks; continuing without \
                             them"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis authored landmarks; continuing without them"
                    );
                    None
                },
            }
        } else {
            None
        };
        let authored_landmark_profiles = if authored_cromatolis {
            match AuthoredCromatolisLandmarkProfiles::load_owned(
                "world.map.cromatolis_v0_landmark_profiles",
            ) {
                Ok(profiles) => match authored_landmarks.as_ref() {
                    Some(landmarks) => match profiles.validate(landmarks) {
                        Ok(()) => Some(profiles),
                        Err(err) => {
                            warn!(
                                ?err,
                                "Could not validate Cromatolis landmark profiles; continuing \
                                 without them"
                            );
                            None
                        },
                    },
                    None => None,
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis landmark profiles; continuing without them"
                    );
                    None
                },
            }
        } else {
            None
        };
        let authored_settlement_template_contract = if authored_settlements.is_some() {
            match SettlementTemplateContract::load_owned(
                "world.map.cromatolis_v0_settlement_template_contract",
            ) {
                Ok(contract) => match contract.validate() {
                    Ok(()) => Some(contract),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate the settlement template contract; falling back to \
                             the default Camp/Refactor split"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load the settlement template contract; falling back to the \
                         default Camp/Refactor split"
                    );
                    None
                },
            }
        } else {
            None
        };
        // Routes resolve their endpoints by authored settlement id, so they
        // only make sense once the settlement loader itself is available
        // (region-scoped the same way, not a literal asset-name check).
        let authored_routes = if authored_cromatolis && authored_settlements.is_some() {
            match AuthoredCromatolisRouteGraph::load_owned("world.map.cromatolis_v0_routes") {
                Ok(routes) => match routes.validate(sim.map_size_lg()) {
                    Ok(()) => Some(routes),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate Cromatolis authored route graph; continuing \
                             without RTSim routes"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis authored route graph; continuing without RTSim \
                         routes"
                    );
                    None
                },
            }
        } else {
            None
        };
        // See `cromatolis_authored_bridge_preview`'s doc comment: bridges
        // stay off in a normal run and only load when a developer opts in
        // via the env var, matching `xindeler-old`'s current live behavior.
        let authored_bridge_preview = if authored_cromatolis && authored_settlements.is_some() {
            cromatolis_authored_bridge_preview()
        } else {
            None
        };
        let authored_bridges = if authored_bridge_preview.is_some() {
            match AuthoredCromatolisBridges::load_owned("world.map.cromatolis_v0_bridges") {
                Ok(bridges) => match bridges.validate(sim.map_size_lg()) {
                    Ok(()) => Some(bridges),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate Cromatolis authored bridges; continuing without \
                             them"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis authored bridges; continuing without them"
                    );
                    None
                },
            }
        } else {
            None
        };
        // Fortifications resolve entirely from their own start/end pins, but
        // stay gated the same region-scoped way as every other authored
        // Cromatolis layer above (and, like bridges, only make sense once
        // settlements exist).
        let authored_fortifications = if authored_cromatolis && authored_settlements.is_some() {
            match AuthoredCromatolisFortifications::load_owned(
                "world.map.cromatolis_v0_fortifications",
            ) {
                Ok(fortifications) => match fortifications.validate(sim.map_size_lg()) {
                    Ok(()) => Some(fortifications),
                    Err(err) => {
                        warn!(
                            ?err,
                            "Could not validate Cromatolis authored fortifications; continuing \
                             without them"
                        );
                        None
                    },
                },
                Err(err) => {
                    warn!(
                        ?err,
                        "Could not load Cromatolis authored fortifications; continuing without \
                         them"
                    );
                    None
                },
            }
        } else {
            None
        };
        let mut ctx = GenCtx { sim, rng };

        // info!("starting cave generation");
        // this.generate_caves(&mut ctx);

        info!("starting civilisation creation");
        prof_span!(guard, "create civs");
        if let Some(settlements) = authored_settlements.as_ref() {
            this.establish_authored_cromatolis_settlements(
                &mut ctx,
                settlements,
                authored_settlement_template_contract.as_ref(),
            );
            if let Some(landmarks) = authored_landmarks.as_ref() {
                this.establish_authored_cromatolis_landmarks(
                    &mut ctx,
                    landmarks,
                    authored_landmark_profiles.as_ref(),
                );
            }
            if let Some(bridges) = authored_bridges.as_ref() {
                this.establish_authored_cromatolis_bridges(
                    &mut ctx,
                    bridges,
                    authored_bridge_preview.as_deref(),
                );
            }
            if let Some(fortifications) = authored_fortifications.as_ref() {
                this.establish_authored_cromatolis_fortifications(&mut ctx, fortifications);
            }
            if let Some(routes) = authored_routes.as_ref() {
                this.establish_authored_cromatolis_routes(&ctx, routes);
            }
            report_stage(WorldCivStage::CivCreation(1, 1));
        } else {
            for i in 0..initial_civ_count {
                prof_span!("create civ");
                debug!("Creating civilisation...");
                if this.birth_civ(&mut ctx.reseed()).is_none() {
                    warn!("Failed to find starting site for civilisation.");
                }
                report_stage(WorldCivStage::CivCreation(i, initial_civ_count));
            }
        }
        drop(guard);
        info!(
            ?initial_civ_count,
            authored_cromatolis, "all civilisations created"
        );

        report_stage(WorldCivStage::SiteGeneration);
        prof_span!(guard, "find locations and establish sites");
        let world_dims = ctx.sim.get_aabr();
        for _ in 0..if authored_settlements.is_some() {
            0
        } else {
            initial_civ_count * 3
        } {
            attempt(5, || {
                let (loc, kind) = match ctx.rng.random_range(0..116) {
                    0..=4 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.tree_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::GiantTree,
                        )?,
                        SiteKind::GiantTree,
                    ),
                    5..=15 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.gnarling_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Gnarling,
                        )?,
                        SiteKind::Gnarling,
                    ),
                    16..=20 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.chapel_site_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::ChapelSite,
                        )?,
                        SiteKind::ChapelSite,
                    ),
                    21..=27 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.gnarling_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Adlet,
                        )?,
                        SiteKind::Adlet,
                    ),
                    28..=38 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.pirate_hideout_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::PirateHideout,
                        )?,
                        SiteKind::PirateHideout,
                    ),
                    39..=45 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.jungle_ruin_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::JungleRuin,
                        )?,
                        SiteKind::JungleRuin,
                    ),
                    46..=55 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.rock_circle_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::RockCircle,
                        )?,
                        SiteKind::RockCircle,
                    ),
                    56..=66 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.troll_cave_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::TrollCave,
                        )?,
                        SiteKind::TrollCave,
                    ),
                    67..=72 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.camp_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Camp,
                        )?,
                        SiteKind::Camp,
                    ),
                    73..=76 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.mine_site_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Haniwa,
                        )?,
                        SiteKind::Haniwa,
                    ),
                    77..=81 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.terracotta_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Terracotta,
                        )?,
                        SiteKind::Terracotta,
                    ),
                    82..=87 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.mine_site_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::DwarvenMine,
                        )?,
                        SiteKind::DwarvenMine,
                    ),
                    88..=91 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.cultist_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Cultist,
                        )?,
                        SiteKind::Cultist,
                    ),
                    92..=96 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.sahagin_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Sahagin,
                        )?,
                        SiteKind::Sahagin,
                    ),
                    97..=102 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.vampire_castle_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::VampireCastle,
                        )?,
                        SiteKind::VampireCastle,
                    ),
                    103..108 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new().finalize(&world_dims),
                            &SiteKind::GliderCourse,
                        )?,
                        SiteKind::GliderCourse,
                    ),
                    /*103..=108 => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.castle_enemies(), 40)
                                .close_to_one_of(this.towns(), 20)
                                .finalize(&world_dims),
                            &SiteKind::Castle,
                        )?,
                        SiteKind::Castle,
                    ),
                    109..=114 => (SiteKind::Citadel, (&castle_enemies, 20)),
                    */
                    _ => (
                        find_site_loc(
                            &mut ctx,
                            &ProximityRequirementsBuilder::new()
                                .avoid_all_of(this.myrmidon_enemies(), 40)
                                .finalize(&world_dims),
                            &SiteKind::Myrmidon,
                        )?,
                        SiteKind::Myrmidon,
                    ),
                };
                Some(this.establish_site(&mut ctx.reseed(), loc, |place| Site {
                    kind,
                    center: loc,
                    place,
                    site_tmp: None,
                    authored: None,
                    authored_landmark: None,
                    authored_bridge: None,
                    authored_fortification: None,
                }))
            });
        }
        drop(guard);

        // Tick
        //=== old economy is gone

        // Place sites in world
        prof_span!(guard, "Place sites in world");
        let mut cnt = 0;
        let mut gen_meta = SitesGenMeta::new(seed);
        for sim_site in this.sites.values_mut() {
            cnt += 1;
            let wpos = sim_site
                .center
                .map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
                    e * sz as i32 + sz as i32 / 2
                });

            let mut rng = ctx.reseed().rng;
            let site = index.sites.insert({
                let index_ref = IndexRef {
                    colors: &index.colors(),
                    features: &index.features(),
                    index,
                };
                let generated_site = match &sim_site.kind {
                    SiteKind::Refactor => {
                        let size = sim_site.authored.as_ref().map_or_else(
                            || Lerp::lerp(0.03, 1.0, rng.random_range(0.0..1f32).powi(5)),
                            |authored| authored.size.city_scale(),
                        );
                        WorldSite::generate_city(
                            &Land::from_sim(ctx.sim),
                            index_ref,
                            &mut rng,
                            wpos,
                            size,
                            calendar,
                            &mut gen_meta,
                        )
                    },
                    SiteKind::GliderCourse => WorldSite::generate_glider_course(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                    ),
                    SiteKind::CliffTown => WorldSite::generate_cliff_town(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::SavannahTown => WorldSite::generate_savannah_town(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::CoastalTown => WorldSite::generate_coastal_town(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::PirateHideout => {
                        WorldSite::generate_pirate_hideout(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::JungleRuin => {
                        WorldSite::generate_jungle_ruin(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::RockCircle => {
                        WorldSite::generate_rock_circle(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },

                    SiteKind::TrollCave => {
                        WorldSite::generate_troll_cave(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Camp => {
                        WorldSite::generate_camp(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::DesertCity => WorldSite::generate_desert_city(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::GiantTree => {
                        WorldSite::generate_giant_tree(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Gnarling => {
                        WorldSite::generate_gnarling(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::DwarvenMine => {
                        WorldSite::generate_mine(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::ChapelSite => {
                        WorldSite::generate_chapel_site(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Terracotta => WorldSite::generate_terracotta(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::Citadel => {
                        WorldSite::generate_citadel(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Bridge(a, b) => {
                        let mut bridge_site = WorldSite::generate_bridge(
                            &Land::from_sim(ctx.sim),
                            index_ref,
                            &mut rng,
                            *a,
                            *b,
                            sim_site
                                .authored_bridge
                                .as_ref()
                                .map(|bridge| bridge.design),
                        );
                        if let Some(authored) = sim_site.authored_bridge.as_ref() {
                            bridge_site = bridge_site.with_name(authored.name.clone());
                        }

                        // Update the path connecting to the bridge to line up better.
                        if let Some(bridge) =
                            bridge_site
                                .plots
                                .values()
                                .find_map(|plot| match &plot.kind {
                                    site::PlotKind::Bridge(bridge) => Some(bridge),
                                    _ => None,
                                })
                        {
                            let mut update_offset = |original: Vec2<i32>, new: Vec2<i32>| {
                                let chunk = original.wpos_to_cpos();
                                if let Some(c) = ctx.sim.get_mut(chunk) {
                                    c.path.0.offset = (new - chunk.cpos_to_wpos_center())
                                        .map(|e| e.clamp(-16, 16) as i8);
                                }
                            };

                            update_offset(bridge.original_start, bridge.start.xy());
                            update_offset(bridge.original_end, bridge.end.xy());
                        }
                        bridge_site.demarcate_obstacles(&Land::from_sim(ctx.sim));
                        bridge_site
                    },
                    SiteKind::Fortification(a, b) => {
                        let land = Land::from_sim(ctx.sim);
                        let mut fortification_site = WorldSite::generate_fortification(
                            &land,
                            &mut rng,
                            *a,
                            *b,
                            sim_site
                                .authored_fortification
                                .as_ref()
                                .map(|fortification| fortification.design.clone()),
                        );
                        if let Some(authored) = sim_site.authored_fortification.as_ref() {
                            fortification_site =
                                fortification_site.with_name(authored.name.clone());
                        }
                        fortification_site.demarcate_obstacles(&land);
                        fortification_site
                    },
                    SiteKind::Adlet => WorldSite::generate_adlet(
                        &Land::from_sim(ctx.sim),
                        &mut rng,
                        wpos,
                        index_ref,
                    ),
                    SiteKind::Haniwa => {
                        WorldSite::generate_haniwa(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Cultist => {
                        WorldSite::generate_cultist(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                    SiteKind::Myrmidon => WorldSite::generate_myrmidon(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                        &mut gen_meta,
                    ),
                    SiteKind::Sahagin => WorldSite::generate_sahagin(
                        &Land::from_sim(ctx.sim),
                        index_ref,
                        &mut rng,
                        wpos,
                    ),
                    SiteKind::VampireCastle => {
                        WorldSite::generate_vampire_castle(&Land::from_sim(ctx.sim), &mut rng, wpos)
                    },
                };
                if let Some(name) = sim_site.authored_name() {
                    generated_site.with_name(name.to_string())
                } else {
                    generated_site
                }
            });
            sim_site.site_tmp = Some(site);
            let site_ref = &index.sites[site];

            let radius_chunks =
                (site_ref.radius() / TerrainChunkSize::RECT_SIZE.x as f32).ceil() as usize;
            for pos in Spiral2d::new()
                .map(|offs| sim_site.center + offs)
                .take((radius_chunks * 2).pow(2))
            {
                ctx.sim.get_mut(pos).map(|chunk| chunk.sites.push(site));
            }
            debug!(?sim_site.center, "Placed site at location");
        }
        drop(guard);
        info!(?cnt, "all sites placed");
        gen_meta.log();

        //this.display_info();

        // remember neighbor information in economy
        for (s1, val) in this.track_map.iter() {
            if let Some(index1) = this.sites.get(*s1).site_tmp {
                for (s2, t) in val.iter() {
                    if let Some(index2) = this.sites.get(*s2).site_tmp
                        && index.sites.get(index1).do_economic_simulation()
                        && index.sites.get(index2).do_economic_simulation()
                    {
                        let cost = this.tracks.get(*t).path.len();
                        index
                            .sites
                            .get_mut(index1)
                            .economy_mut()
                            .add_neighbor(index2, cost);
                        index
                            .sites
                            .get_mut(index2)
                            .economy_mut()
                            .add_neighbor(index1, cost);
                    }
                }
            }
        }

        prof_span!(guard, "generate airship routes");
        this.airships.generate_airship_routes(ctx.sim, index);
        drop(guard);

        // TODO: this looks optimizable

        // collect natural resources
        prof_span!(guard, "collect natural resources");
        let sites = &mut index.sites;
        (0..ctx.sim.map_size_lg().chunks_len()).for_each(|posi| {
            let chpos = uniform_idx_as_vec2(ctx.sim.map_size_lg(), posi);
            let wpos = chpos.map(|e| e as i64) * TerrainChunkSize::RECT_SIZE.map(|e| e as i64);
            let closest_site = (*sites)
                .iter_mut()
                .filter(|s| !matches!(s.1.kind, Some(crate::site::SiteKind::Myrmidon)))
                .min_by_key(|(_id, s)| s.origin.map(|e| e as i64).distance_squared(wpos));
            if let Some((_id, s)) = closest_site
                && s.do_economic_simulation()
            {
                let distance_squared = s.origin.map(|e| e as i64).distance_squared(wpos);
                s.economy_mut()
                    .add_chunk(ctx.sim.get(chpos).unwrap(), distance_squared);
            }
        });
        drop(guard);

        sites.iter_mut().for_each(|(_, s)| {
            if let Some(econ) = s.economy.as_mut() {
                econ.cache_economy()
            }
        });

        this
    }

    pub fn place(&self, id: Id<Place>) -> &Place { self.places.get(id) }

    pub fn sites(&self) -> impl Iterator<Item = &Site> + '_ { self.sites.values() }

    #[expect(dead_code)]
    fn display_info(&self) {
        for (id, civ) in self.civs.iter() {
            println!("# Civilisation {:?}", id);
            println!("Name: <unnamed>");
            println!("Homeland: {:#?}", self.places.get(civ.homeland));
        }

        for (id, site) in self.sites.iter() {
            println!("# Site {:?}", id);
            println!("{:#?}", site);
        }
    }

    /// Return the direct track between two places, bool if the track should be
    /// reversed or not
    pub fn track_between(&self, a: Id<Site>, b: Id<Site>) -> Option<(Id<Track>, bool)> {
        self.track_map
            .get(&a)
            .and_then(|dests| Some((*dests.get(&b)?, false)))
            .or_else(|| {
                self.track_map
                    .get(&b)
                    .and_then(|dests| Some((*dests.get(&a)?, true)))
            })
    }

    /// Return an iterator over a site's neighbors
    pub fn neighbors(&self, site: Id<Site>) -> impl Iterator<Item = Id<Site>> + '_ {
        let to = self
            .track_map
            .get(&site)
            .map(|dests| dests.keys())
            .into_iter()
            .flatten();
        let fro = self
            .track_map
            .iter()
            .filter(move |(_, dests)| dests.contains_key(&site))
            .map(|(p, _)| p);
        to.chain(fro).filter(move |p| **p != site).copied()
    }

    /// Find the cheapest route between two places
    fn route_between(&self, a: Id<Site>, b: Id<Site>) -> Option<(Path<Id<Site>>, f32)> {
        let heuristic = move |p: &Id<Site>| {
            (self
                .sites
                .get(*p)
                .center
                .distance_squared(self.sites.get(b).center) as f32)
                .sqrt()
        };
        let transition =
            |a: Id<Site>, b: Id<Site>| self.tracks.get(self.track_between(a, b).unwrap().0).cost;
        let neighbors = |p: &Id<Site>| {
            let p = *p;
            self.neighbors(p)
                .map(move |neighbor| (neighbor, transition(p, neighbor)))
        };
        let satisfied = |p: &Id<Site>| *p == b;
        // We use this hasher (FxHasher64) because
        // (1) we don't care about DDOS attacks (ruling out SipHash);
        // (2) we care about determinism across computers (ruling out AAHash);
        // (3) we have 8-byte keys (for which FxHash is fastest).
        let mut astar = Astar::new(100, a, BuildHasherDefault::<FxHasher64>::default());
        astar.poll(100, heuristic, neighbors, satisfied).into_path()
    }

    fn birth_civ(&mut self, ctx: &mut GenCtx<impl Rng>) -> Option<Id<Civ>> {
        // TODO: specify SiteKind based on where a suitable location is found
        let kind = match ctx.rng.random_range(0..64) {
            0..=8 => SiteKind::CliffTown,
            9..=17 => SiteKind::DesertCity,
            18..=23 => SiteKind::SavannahTown,
            24..=33 => SiteKind::CoastalTown,
            _ => SiteKind::Refactor,
        };
        let world_dims = ctx.sim.get_aabr();
        let avoid_town_enemies = ProximityRequirementsBuilder::new()
            .avoid_all_of(self.town_enemies(), 60)
            .finalize(&world_dims);
        let loc = (0..100)
            .flat_map(|_| {
                find_site_loc(ctx, &avoid_town_enemies, &kind).and_then(|loc| {
                    town_attributes_of_site(loc, ctx.sim)
                        .map(|town_attrs| (loc, town_attrs.score()))
                })
            })
            // Compare just a few different potential locations (produces diversity)
            .take(4)
            .reduce(|a, b| if a.1 > b.1 { a } else { b })?
            .0;

        let site = self.establish_site(ctx, loc, |place| Site {
            kind,
            site_tmp: None,
            center: loc,
            place,
            authored: None,
            authored_landmark: None,
            authored_bridge: None,
            authored_fortification: None,
            /* most economic members have moved to site/Economy */
            /* last_exports: Stocks::from_default(0.0),
             * export_targets: Stocks::from_default(0.0),
             * //trade_states: Stocks::default(), */
        });

        let civ = self.civs.insert(Civ {
            capital: site,
            homeland: self.sites.get(site).place,
        });

        Some(civ)
    }

    fn establish_place(
        &mut self,
        _ctx: &mut GenCtx<impl Rng>,
        loc: Vec2<i32>,
        _area: Range<usize>,
    ) -> Id<Place> {
        self.places.insert(Place { center: loc })
    }

    /// Adds lake POIs and names them
    fn name_biomes(&mut self, ctx: &mut GenCtx<impl Rng>) {
        prof_span!("name_biomes");
        let map_size_lg = ctx.sim.map_size_lg();
        let world_size = map_size_lg.chunks();
        let mut biomes: Vec<(common::terrain::BiomeKind, Vec<usize>)> = Vec::new();
        let mut explored = vec![false; world_size.x as usize * world_size.y as usize];
        let mut to_floodfill = Vec::new();
        let mut to_explore = Vec::new();
        // TODO: have start point in center and ignore ocean?
        let start_point = 0;
        to_explore.push(start_point);

        while let Some(exploring) = to_explore.pop() {
            if explored[exploring] {
                continue;
            }
            to_floodfill.push(exploring);
            // Should always be a chunk on the map
            let biome = ctx.sim.chunks[exploring].get_biome();
            let mut filled = Vec::new();

            while let Some(filling) = to_floodfill.pop() {
                explored[filling] = true;
                filled.push(filling);
                for neighbour in common::terrain::neighbors(map_size_lg, filling) {
                    if explored[neighbour] {
                        continue;
                    }
                    let n_biome = ctx.sim.chunks[neighbour].get_biome();
                    if n_biome == biome {
                        to_floodfill.push(neighbour);
                    } else {
                        to_explore.push(neighbour);
                    }
                }
            }

            biomes.push((biome, filled));
        }

        prof_span!("after flood fill");
        let mut biome_count = 0;
        for biome in biomes {
            let name = match biome.0 {
                common::terrain::BiomeKind::Lake if biome.1.len() as u32 > 200 => Some(format!(
                    "{} {}",
                    ["Lake", "Loch"].choose_mut(&mut ctx.rng).unwrap(),
                    NameGen::location(&mut ctx.rng).generate_lake_custom()
                )),
                common::terrain::BiomeKind::Lake if biome.1.len() as u32 > 10 => Some(format!(
                    "{} {}",
                    NameGen::location(&mut ctx.rng).generate_lake_custom(),
                    ["Pool", "Well", "Pond"].choose_mut(&mut ctx.rng).unwrap()
                )),
                common::terrain::BiomeKind::Grassland if biome.1.len() as u32 > 750 => {
                    Some(format!(
                        "{} {}",
                        [
                            NameGen::location(&mut ctx.rng).generate_grassland_engl(),
                            NameGen::location(&mut ctx.rng).generate_grassland_custom()
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap(),
                        [
                            "Grasslands",
                            "Plains",
                            "Meadows",
                            "Fields",
                            "Heath",
                            "Hills",
                            "Prairie",
                            "Lowlands",
                            "Steppe",
                            "Downs",
                            "Greens",
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                    ))
                },
                common::terrain::BiomeKind::Ocean if biome.1.len() as u32 > 750 => Some(format!(
                    "{} {}",
                    [
                        NameGen::location(&mut ctx.rng).generate_ocean_engl(),
                        NameGen::location(&mut ctx.rng).generate_ocean_custom()
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap(),
                    ["Sea", "Bay", "Gulf", "Deep", "Depths", "Ocean", "Blue",]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                )),
                common::terrain::BiomeKind::Mountain if biome.1.len() as u32 > 750 => {
                    Some(format!(
                        "{} {}",
                        [
                            NameGen::location(&mut ctx.rng).generate_mountain_engl(),
                            NameGen::location(&mut ctx.rng).generate_mountain_custom()
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap(),
                        [
                            "Mountains",
                            "Range",
                            "Reach",
                            "Massif",
                            "Rocks",
                            "Cliffs",
                            "Peaks",
                            "Heights",
                            "Bluffs",
                            "Ridge",
                            "Canyon",
                            "Plateau",
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                    ))
                },
                common::terrain::BiomeKind::Snowland if biome.1.len() as u32 > 750 => {
                    Some(format!(
                        "{} {}",
                        [
                            NameGen::location(&mut ctx.rng).generate_snowland_engl(),
                            NameGen::location(&mut ctx.rng).generate_snowland_custom()
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap(),
                        [
                            "Snowlands",
                            "Glacier",
                            "Tundra",
                            "Drifts",
                            "Snowfields",
                            "Hills",
                            "Downs",
                            "Uplands",
                            "Highlands",
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                    ))
                },
                common::terrain::BiomeKind::Desert if biome.1.len() as u32 > 750 => Some(format!(
                    "{} {}",
                    [
                        NameGen::location(&mut ctx.rng).generate_desert_engl(),
                        NameGen::location(&mut ctx.rng).generate_desert_custom()
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap(),
                    [
                        "Desert", "Sands", "Sandsea", "Drifts", "Dunes", "Droughts", "Flats",
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap()
                )),
                common::terrain::BiomeKind::Swamp if biome.1.len() as u32 > 200 => Some(format!(
                    "{} {}",
                    NameGen::location(&mut ctx.rng).generate_swamp_engl(),
                    [
                        "Swamp",
                        "Swamps",
                        "Swamplands",
                        "Marsh",
                        "Marshlands",
                        "Morass",
                        "Mire",
                        "Bog",
                        "Wetlands",
                        "Fen",
                        "Moors",
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap()
                )),
                common::terrain::BiomeKind::Jungle if biome.1.len() as u32 > 85 => Some(format!(
                    "{} {}",
                    [
                        NameGen::location(&mut ctx.rng).generate_jungle_engl(),
                        NameGen::location(&mut ctx.rng).generate_jungle_custom()
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap(),
                    [
                        "Jungle",
                        "Rainforest",
                        "Greatwood",
                        "Wilds",
                        "Wildwood",
                        "Tangle",
                        "Tanglewood",
                        "Bush",
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap()
                )),
                common::terrain::BiomeKind::Forest if biome.1.len() as u32 > 750 => Some(format!(
                    "{} {}",
                    [
                        NameGen::location(&mut ctx.rng).generate_forest_engl(),
                        NameGen::location(&mut ctx.rng).generate_forest_custom()
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap(),
                    ["Forest", "Woodlands", "Woods", "Glades", "Grove", "Weald",]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                )),
                common::terrain::BiomeKind::Savannah if biome.1.len() as u32 > 750 => {
                    Some(format!(
                        "{} {}",
                        [
                            NameGen::location(&mut ctx.rng).generate_savannah_engl(),
                            NameGen::location(&mut ctx.rng).generate_savannah_custom()
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap(),
                        [
                            "Savannah",
                            "Shrublands",
                            "Sierra",
                            "Prairie",
                            "Lowlands",
                            "Flats",
                        ]
                        .choose_mut(&mut ctx.rng)
                        .unwrap()
                    ))
                },
                common::terrain::BiomeKind::Taiga if biome.1.len() as u32 > 750 => Some(format!(
                    "{} {}",
                    [
                        NameGen::location(&mut ctx.rng).generate_taiga_engl(),
                        NameGen::location(&mut ctx.rng).generate_taiga_custom()
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap(),
                    [
                        "Forest",
                        "Woodlands",
                        "Woods",
                        "Timberlands",
                        "Highlands",
                        "Uplands",
                    ]
                    .choose_mut(&mut ctx.rng)
                    .unwrap()
                )),
                _ => None,
            };
            if let Some(name) = name {
                // find average center of the biome
                let center = biome
                    .1
                    .iter()
                    .map(|b| {
                        uniform_idx_as_vec2(map_size_lg, *b).as_::<f32>() / biome.1.len() as f32
                    })
                    .sum::<Vec2<f32>>()
                    .as_::<i32>();
                // Select the point closest to the center
                let idx = *biome
                    .1
                    .iter()
                    .min_by_key(|&b| center.distance_squared(uniform_idx_as_vec2(map_size_lg, *b)))
                    .unwrap();
                let id = self.pois.insert(PointOfInterest {
                    name,
                    loc: uniform_idx_as_vec2(map_size_lg, idx),
                    kind: PoiKind::Biome(biome.1.len() as u32),
                });
                for chunk in biome.1 {
                    ctx.sim.chunks[chunk].poi = Some(id);
                }
                biome_count += 1;
            }
        }

        info!(?biome_count, "all biomes named");
    }

    /// Adds mountain POIs and name them
    fn name_peaks(&mut self, ctx: &mut GenCtx<impl Rng>) {
        prof_span!("name_peaks");
        let map_size_lg = ctx.sim.map_size_lg();
        const MIN_MOUNTAIN_ALT: f32 = 600.0;
        const MIN_MOUNTAIN_CHAOS: f32 = 0.35;
        let rng = &mut ctx.rng;
        let sim_chunks = &ctx.sim.chunks;
        let peaks = sim_chunks
            .iter()
            .enumerate()
            .filter(|(posi, chunk)| {
                let neighbor_alts_max = common::terrain::neighbors(map_size_lg, *posi)
                    .map(|i| sim_chunks[i].alt as u32)
                    .max();
                chunk.alt > MIN_MOUNTAIN_ALT
                    && chunk.chaos > MIN_MOUNTAIN_CHAOS
                    && neighbor_alts_max.is_some_and(|n_alt| chunk.alt as u32 > n_alt)
            })
            .map(|(posi, chunk)| {
                (
                    posi,
                    uniform_idx_as_vec2(map_size_lg, posi),
                    (chunk.alt - CONFIG.sea_level) as u32,
                )
            })
            .collect::<Vec<(usize, Vec2<i32>, u32)>>();
        let mut num_peaks = 0;
        let mut removals = vec![false; peaks.len()];
        for (i, peak) in peaks.iter().enumerate() {
            for (k, n_peak) in peaks.iter().enumerate() {
                // If the difference in position of this peak and another is
                // below a threshold and this peak's altitude is lower, remove the
                // peak from the list
                if i != k
                    && (peak.1).distance_squared(n_peak.1) < POI_THINNING_DIST_SQRD
                    && peak.2 <= n_peak.2
                {
                    // Remove this peak
                    // This cannot panic as `removals` is the same length as `peaks`
                    // i is the index in `peaks`
                    removals[i] = true;
                }
            }
        }
        peaks
            .iter()
            .enumerate()
            .filter(|&(i, _)| !removals[i])
            .for_each(|(_, (_, loc, alt))| {
                num_peaks += 1;
                self.pois.insert(PointOfInterest {
                    name: {
                        let name = NameGen::location(rng).generate();
                        if *alt < 1000 {
                            match rng.random_range(0..6) {
                                0 => format!("{} Bluff", name),
                                1 => format!("{} Crag", name),
                                _ => format!("{} Hill", name),
                            }
                        } else {
                            match rng.random_range(0..8) {
                                0 => format!("{}'s Peak", name),
                                1 => format!("{} Peak", name),
                                2 => format!("{} Summit", name),
                                _ => format!("Mount {}", name),
                            }
                        }
                    },
                    kind: PoiKind::Peak(*alt),
                    loc: *loc,
                });
            });
        info!(?num_peaks, "all peaks named");
    }

    /// Places every authored Cromatolis settlement as a `Site`, resolving
    /// each one's `SiteKind` through `resolve_settlement_site_kind` (the
    /// data-driven category+size -> generator table) and reprojecting its
    /// requested location onto the nearest dry compatibility cell (see
    /// `project_authored_settlement_location`). The capital settlement
    /// becomes this world's sole `Civ`; if none is marked as capital
    /// (already rejected by `AuthoredCromatolisSettlements::validate`, but
    /// checked again here defensively), no civilisation is created.
    fn establish_authored_cromatolis_settlements(
        &mut self,
        ctx: &mut GenCtx<impl Rng>,
        authored: &AuthoredCromatolisSettlements,
        contract: Option<&SettlementTemplateContract>,
    ) {
        info!(
            settlement_count = authored.settlements.len(),
            "Applying authored Cromatolis settlements"
        );

        let mut used_ids = std::collections::HashSet::new();
        let mut used_locations = std::collections::HashSet::new();
        let mut capital = None;

        for settlement in &authored.settlements {
            let requested_loc = settlement.center.to_chunk_pos(ctx.sim.map_size_lg());
            let loc = project_authored_settlement_location(requested_loc, ctx.sim);
            if loc != requested_loc {
                info!(
                    site_id = %settlement.id,
                    ?requested_loc,
                    ?loc,
                    "Reprojected Cromatolis settlement onto a dry compatibility cell"
                );
            }
            if !used_ids.insert(settlement.id.as_str()) {
                warn!(site_id = %settlement.id, "Skipping duplicate authored Cromatolis settlement id");
                continue;
            }
            if !used_locations.insert((loc.x, loc.y)) {
                warn!(site_id = %settlement.id, ?loc, "Skipping authored settlement with a duplicate map location");
                continue;
            }
            let kind = resolve_settlement_site_kind(contract, settlement.category, settlement.size);
            let metadata = AuthoredSettlementMeta {
                id: settlement.id.clone(),
                name: settlement.name.clone(),
                category: settlement.category,
                size: settlement.size,
                population: settlement.population.clone(),
                requires_capital_castle: settlement.requires_capital_castle,
                start_eligible: settlement.start_eligible,
            };
            let site = self.establish_site(ctx, loc, |place| Site {
                kind,
                site_tmp: None,
                center: loc,
                place,
                authored: Some(metadata),
                authored_landmark: None,
                authored_bridge: None,
                authored_fortification: None,
            });

            debug!(
                site_id = %settlement.id,
                site_name = %settlement.name,
                ?loc,
                ?kind,
                ?settlement.category,
                ?settlement.size,
                population_tag = ?settlement.population.tag,
                "Established authored Cromatolis settlement"
            );

            if settlement.category == AuthoredSettlementCategory::Capital {
                capital = Some(site);
            }
        }

        if let Some(capital) = capital {
            self.civs.insert(Civ {
                capital,
                homeland: self.sites.get(capital).place,
            });
        } else {
            warn!(
                "Cromatolis authored settlements contain no capital; no civilisation was created"
            );
        }
    }

    /// Places every authored Cromatolis landmark as a `Site`, using the
    /// landmark's own `template.site_kind()` (one of the pre-existing
    /// generic `GiantTree`/`Citadel`/`ChapelSite` generators -- not a
    /// bespoke per-landmark renderer). Landmark locations are used exactly
    /// as authored, unlike settlements: they don't get reprojected onto a
    /// dry compatibility cell.
    fn establish_authored_cromatolis_landmarks(
        &mut self,
        ctx: &mut GenCtx<impl Rng>,
        authored: &AuthoredCromatolisLandmarks,
        profiles: Option<&AuthoredCromatolisLandmarkProfiles>,
    ) {
        info!(
            landmark_count = authored.landmarks.len(),
            "Applying authored Cromatolis landmarks"
        );

        let mut used_ids = std::collections::HashSet::new();
        let mut used_locations = self
            .sites
            .values()
            .map(|site| (site.center.x, site.center.y))
            .collect::<std::collections::HashSet<_>>();

        for landmark in &authored.landmarks {
            let loc = landmark.center.to_chunk_pos(ctx.sim.map_size_lg());
            if !used_ids.insert(landmark.id.as_str()) {
                warn!(landmark_id = %landmark.id, "Skipping duplicate authored Cromatolis landmark id");
                continue;
            }
            if !used_locations.insert((loc.x, loc.y)) {
                warn!(landmark_id = %landmark.id, ?loc, "Skipping authored landmark with a duplicate map location");
                continue;
            }

            let metadata = AuthoredLandmarkMeta {
                id: landmark.id.clone(),
                name: landmark.name.clone(),
                kind: landmark.kind,
                profile: profiles.and_then(|profiles| profiles.profile_for(&landmark.id)),
            };
            self.establish_site(ctx, loc, |place| Site {
                kind: landmark.template.site_kind(),
                site_tmp: None,
                center: loc,
                place,
                authored: None,
                authored_landmark: Some(metadata),
                authored_bridge: None,
                authored_fortification: None,
            });
            debug!(
                landmark_id = %landmark.id,
                landmark_name = %landmark.name,
                ?loc,
                ?landmark.kind,
                ?landmark.template,
                "Established authored Cromatolis landmark"
            );
        }
    }

    /// Establishes logical travel edges between authored Cromatolis
    /// settlements as real `Track`s in `self.tracks`/`self.track_map` --
    /// literally the same pathfinding/travel system procedural civs already
    /// use (see the module-level note above), not a parallel structure. Must
    /// run after `establish_authored_cromatolis_settlements`, since routes
    /// resolve their endpoints by authored settlement id.
    fn establish_authored_cromatolis_routes(
        &mut self,
        ctx: &GenCtx<impl Rng>,
        routes: &AuthoredCromatolisRouteGraph,
    ) {
        let sites_by_authored_id = self
            .sites
            .iter()
            .filter_map(|(site_id, site)| {
                site.authored
                    .as_ref()
                    .map(|metadata| (metadata.id.as_str(), site_id))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut connected_pairs = std::collections::HashSet::new();
        let mut established = 0;

        for route in &routes.routes {
            let Some(&start) = sites_by_authored_id.get(route.start_site_id.as_str()) else {
                warn!(route_id = %route.id, start = %route.start_site_id, "Authored route has no start settlement");
                continue;
            };
            let Some(&end) = sites_by_authored_id.get(route.end_site_id.as_str()) else {
                warn!(route_id = %route.id, end = %route.end_site_id, "Authored route has no end settlement");
                continue;
            };
            let pair = if route.start_site_id < route.end_site_id {
                (route.start_site_id.as_str(), route.end_site_id.as_str())
            } else {
                (route.end_site_id.as_str(), route.start_site_id.as_str())
            };
            if !connected_pairs.insert(pair) {
                debug!(route_id = %route.id, "Skipping duplicate authored route edge");
                continue;
            }

            let mut path = route
                .points
                .iter()
                .map(|point| point.to_chunk_pos(ctx.sim.map_size_lg()))
                .collect::<Vec<_>>();
            path[0] = self.sites.get(start).center;
            let last = path.len() - 1;
            path[last] = self.sites.get(end).center;
            path.dedup();
            if path.len() < 2 {
                warn!(route_id = %route.id, "Skipping authored route collapsed to one chunk");
                continue;
            }
            let cost = path
                .windows(2)
                .map(|points| points[0].as_::<f32>().distance(points[1].as_()))
                .sum::<f32>()
                .max(1.0);
            let track = self.tracks.insert(Track {
                cost,
                path: Path { nodes: path },
            });
            self.track_map.entry(start).or_default().insert(end, track);
            established += 1;
        }
        info!(
            established,
            "Established authored Cromatolis RTSim route edges"
        );
    }

    /// Places authored Cromatolis bridge crossings as real `Bridge` sites,
    /// additive to the generic procedural bridge generator via an
    /// `Option<AuthoredBridgeDesign>` design parameter. `preview` mirrors
    /// `xindeler-old`'s current live gating: bridges stay disabled unless a
    /// developer opts a specific bridge (or `all`) in for review, via
    /// `XINDELER_CROMATOLIS_BRIDGE_PREVIEW`.
    fn establish_authored_cromatolis_bridges(
        &mut self,
        ctx: &mut GenCtx<impl Rng>,
        authored: &AuthoredCromatolisBridges,
        preview: Option<&str>,
    ) {
        info!(
            bridge_count = authored.bridges.len(),
            "Applying authored Cromatolis bridges"
        );
        for bridge in &authored.bridges {
            if let Some(preview) = preview
                && !bridge_is_selected_for_preview(preview, &bridge.id)
            {
                continue;
            }
            let start = bridge.start.to_chunk_pos(ctx.sim.map_size_lg());
            let end = bridge.end.to_chunk_pos(ctx.sim.map_size_lg());
            if self.bridges.contains_key(&start) || self.bridges.contains_key(&end) {
                warn!(bridge_id = %bridge.id, ?start, ?end, "Skipping authored bridge with an occupied endpoint");
                continue;
            }
            let center = (start + end) / 2;
            let metadata = AuthoredBridgeMeta {
                id: bridge.id.clone(),
                name: bridge.name.clone(),
                design: bridge.design(),
            };
            let site = self.establish_site(ctx, center, |place| Site {
                kind: SiteKind::Bridge(start, end),
                site_tmp: None,
                center,
                place,
                authored: None,
                authored_landmark: None,
                authored_bridge: Some(metadata),
                authored_fortification: None,
            });
            self.bridges.insert(start, (end, site));
            self.bridges.insert(end, (start, site));
            debug!(bridge_id = %bridge.id, bridge_name = %bridge.name, ?start, ?end, "Established authored Cromatolis bridge");
        }
    }

    /// Places authored Cromatolis defensive fortifications (walls + gates) as
    /// real `Fortification` sites. Unlike settlements/cities, these never run
    /// `establish_site`'s neighbor-pathfinding-search block: like `Bridge`,
    /// every authored Cromatolis fortification sits on an
    /// `authored_cromatolis_v0` chunk, so `establish_site` returns before
    /// that block runs at all (see its early `chunk.authored_cromatolis_v0`
    /// return) -- `SiteKind::Fortification` isn't even in that block's match
    /// arms, so this holds by construction either way.
    /// Assumes `authored.validate(ctx.sim.map_size_lg())` already succeeded
    /// (checked by the caller before this runs, same as bridges) -- chunk
    /// collapse and duplicate spans are validation failures for the whole
    /// batch, not a per-item skip here.
    fn establish_authored_cromatolis_fortifications(
        &mut self,
        ctx: &mut GenCtx<impl Rng>,
        authored: &AuthoredCromatolisFortifications,
    ) {
        info!(
            fortification_count = authored.fortifications.len(),
            "Applying authored Cromatolis fortifications"
        );
        for fortification in &authored.fortifications {
            let start = authored
                .normalize_point(fortification.start)
                .to_chunk_pos(ctx.sim.map_size_lg());
            let end = authored
                .normalize_point(fortification.end)
                .to_chunk_pos(ctx.sim.map_size_lg());
            let center = (start + end) / 2;
            let metadata = fortification.meta();
            self.establish_site(ctx, center, |place| Site {
                kind: SiteKind::Fortification(start, end),
                site_tmp: None,
                center,
                place,
                authored: None,
                authored_landmark: None,
                authored_bridge: None,
                authored_fortification: Some(metadata),
            });
            debug!(fortification_id = %fortification.id, fortification_name = %fortification.name, ?start, ?end, "Established authored Cromatolis fortification");
        }
    }

    fn establish_site(
        &mut self,
        ctx: &mut GenCtx<impl Rng>,
        loc: Vec2<i32>,
        site_fn: impl FnOnce(Id<Place>) -> Site,
    ) -> Id<Site> {
        prof_span!("establish_site");
        const SITE_AREA: Range<usize> = 1..4; //64..256;

        fn establish_site(
            civs: &mut Civs,
            ctx: &mut GenCtx<impl Rng>,
            loc: Vec2<i32>,
            site_fn: impl FnOnce(Id<Place>) -> Site,
        ) -> Id<Site> {
            let place = match ctx.sim.get(loc).and_then(|site| site.place) {
                Some(place) => place,
                None => civs.establish_place(ctx, loc, SITE_AREA),
            };

            civs.sites.insert(site_fn(place))
        }

        let site = establish_site(self, ctx, loc, site_fn);
        if ctx
            .sim
            .get(loc)
            .is_some_and(|chunk| chunk.authored_cromatolis_v0)
        {
            return site;
        }

        // Find neighbors
        // Note, the maximum distance that I have so far observed not hitting the
        // iteration limit in `find_path` is 364. So I think this is a reasonable
        // limit (although the relationship between distance and pathfinding iterations
        // can be a bit variable). Note, I have seen paths reach the iteration limit
        // with distances as small as 137, so this certainly doesn't catch all
        // cases that would fail.
        const MAX_NEIGHBOR_DISTANCE: f32 = 400.0;
        let mut nearby = self
            .sites
            .iter()
            .filter(|&(id, _)| id != site)
            .filter(|(_, p)| {
                matches!(
                    p.kind,
                    SiteKind::Refactor
                        | SiteKind::CliffTown
                        | SiteKind::SavannahTown
                        | SiteKind::CoastalTown
                        | SiteKind::DesertCity
                )
            })
            .map(|(id, p)| (id, (p.center.distance_squared(loc) as f32).sqrt()))
            .filter(|(_, dist)| *dist < MAX_NEIGHBOR_DISTANCE)
            .collect::<Vec<_>>();
        nearby.sort_by_key(|(_, dist)| *dist as i32);

        if let SiteKind::Refactor
        | SiteKind::CliffTown
        | SiteKind::SavannahTown
        | SiteKind::CoastalTown
        | SiteKind::DesertCity = self.sites[site].kind
        {
            for (nearby, _) in nearby.into_iter().take(4) {
                prof_span!("for nearby");
                // Find a route using existing paths
                //
                // If the novel path isn't efficient compared to this, don't use it
                let max_novel_cost = self
                    .route_between(site, nearby)
                    .map_or(f32::MAX, |(_, route_cost)| route_cost / 3.0);

                let start = loc;
                let end = self.sites.get(nearby).center;
                // Find a novel path.
                let get_bridge = |start| self.bridges.get(&start).map(|(end, _)| *end);
                if let Some((path, cost)) = find_path(ctx, get_bridge, start, end, max_novel_cost) {
                    // Write the track to the world as a path
                    for locs in path.nodes().windows(3) {
                        if let Some((i, _)) = NEIGHBORS
                            .iter()
                            .enumerate()
                            .find(|(_, dir)| **dir == locs[0] - locs[1])
                        {
                            ctx.sim.get_mut(locs[0]).unwrap().path.0.neighbors |=
                                1 << ((i as u8 + 4) % 8);
                            ctx.sim.get_mut(locs[1]).unwrap().path.0.neighbors |= 1 << (i as u8);
                        }

                        if let Some((i, _)) = NEIGHBORS
                            .iter()
                            .enumerate()
                            .find(|(_, dir)| **dir == locs[2] - locs[1])
                        {
                            ctx.sim.get_mut(locs[2]).unwrap().path.0.neighbors |=
                                1 << ((i as u8 + 4) % 8);

                            ctx.sim.get_mut(locs[1]).unwrap().path.0.neighbors |= 1 << (i as u8);
                            ctx.sim.get_mut(locs[1]).unwrap().path.0.offset = Vec2::new(
                                ctx.rng.random_range(-16..17),
                                ctx.rng.random_range(-16..17),
                            );
                        } else if !self.bridges.contains_key(&locs[1]) {
                            let center = (locs[1] + locs[2]) / 2;
                            let id =
                                establish_site(self, &mut ctx.reseed(), center, move |place| {
                                    Site {
                                        kind: SiteKind::Bridge(locs[1], locs[2]),
                                        site_tmp: None,
                                        center,
                                        place,
                                        authored: None,
                                        authored_landmark: None,
                                        authored_bridge: None,
                                        authored_fortification: None,
                                    }
                                });
                            self.bridges.insert(locs[1], (locs[2], id));
                            self.bridges.insert(locs[2], (locs[1], id));
                        }
                        /*
                        let to_prev_idx = NEIGHBORS
                            .iter()
                            .enumerate()
                            .find(|(_, dir)| **dir == (locs[0] - locs[1]).map(|e| e.signum()))
                            .expect("Track locations must be neighbors")
                            .0;

                        let to_next_idx = NEIGHBORS
                            .iter()
                            .enumerate()
                            .find(|(_, dir)| **dir == (locs[2] - locs[1]).map(|e| e.signum()))
                            .expect("Track locations must be neighbors")
                            .0;

                        ctx.sim.get_mut(locs[0]).unwrap().path.0.neighbors |=
                            1 << ((to_prev_idx as u8 + 4) % 8);
                        ctx.sim.get_mut(locs[2]).unwrap().path.0.neighbors |=
                            1 << ((to_next_idx as u8 + 4) % 8);
                        let mut chunk = ctx.sim.get_mut(locs[1]).unwrap();
                        chunk.path.0.neighbors |=
                            (1 << (to_prev_idx as u8)) | (1 << (to_next_idx as u8));
                        */
                    }

                    // Take note of the track
                    let track = self.tracks.insert(Track { cost, path });
                    self.track_map
                        .entry(site)
                        .or_default()
                        .insert(nearby, track);
                }
            }
        }

        site
    }

    fn gnarling_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().filter_map(|s| match s.kind {
            SiteKind::GiantTree => None,
            _ => Some(s.center),
        })
    }

    fn adlet_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn haniwa_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn chapel_site_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn mine_site_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn terracotta_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn cultist_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn myrmidon_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn vampire_castle_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn tree_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn castle_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().filter_map(|s| {
            if s.is_settlement() {
                None
            } else {
                Some(s.center)
            }
        })
    }

    fn jungle_ruin_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn town_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().filter_map(|s| match s.kind {
            SiteKind::Citadel => None,
            _ => Some(s.center),
        })
    }

    fn towns(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().filter_map(|s| {
            if s.is_settlement() {
                Some(s.center)
            } else {
                None
            }
        })
    }

    fn pirate_hideout_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn sahagin_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn rock_circle_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn troll_cave_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }

    fn camp_enemies(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.sites().map(|s| s.center)
    }
}

/// Attempt to find a path between two locations
fn find_path(
    ctx: &mut GenCtx<impl Rng>,
    get_bridge: impl Fn(Vec2<i32>) -> Option<Vec2<i32>>,
    a: Vec2<i32>,
    b: Vec2<i32>,
    max_path_cost: f32,
) -> Option<(Path<Vec2<i32>>, f32)> {
    prof_span!("find_path");
    const MAX_PATH_ITERS: usize = 100_000;
    let sim = &ctx.sim;
    // NOTE: If heuristic overestimates the actual cost, then A* is not guaranteed
    // to produce the least-cost path (since it will explore partially based on
    // the heuristic).
    // TODO: heuristic can be larger than actual cost, since existing bridges cost
    // 1.0 (after the 1.0 that is added to everthting), but they can cover
    // multiple chunks.
    let heuristic = move |l: &Vec2<i32>| (l.distance_squared(b) as f32).sqrt();
    let neighbors = |l: &Vec2<i32>| {
        let l = *l;
        let bridge = get_bridge(l);
        let potential = walk_in_all_dirs(sim, bridge, l);
        potential
            .into_iter()
            .filter_map(|p| p.map(|(node, cost)| (node, cost + 1.0)))
    };
    let satisfied = |l: &Vec2<i32>| *l == b;
    // We use this hasher (FxHasher64) because
    // (1) we don't care about DDOS attacks (ruling out SipHash);
    // (2) we care about determinism across computers (ruling out AAHash);
    // (3) we have 8-byte keys (for which FxHash is fastest).
    let mut astar = Astar::new(
        MAX_PATH_ITERS,
        a,
        BuildHasherDefault::<FxHasher64>::default(),
    )
    .with_max_cost(max_path_cost);
    astar
        .poll(MAX_PATH_ITERS, heuristic, neighbors, satisfied)
        .into_path()
}

/// Return Some if travel between a location and a chunk next to it is permitted
/// If permitted, the approximate relative const of traversal is given
// (TODO: by whom?)
/// Return tuple: (final location, cost)
///
/// For efficiency, this computes for all 8 directions at once.
fn walk_in_all_dirs(
    sim: &WorldSim,
    bridge: Option<Vec2<i32>>,
    a: Vec2<i32>,
) -> [Option<(Vec2<i32>, f32)>; 8] {
    let mut potential = [None; 8];

    let adjacents = NEIGHBORS.map(|dir| a + dir);

    let Some(a_chunk) = sim.get(a) else {
        return potential;
    };
    let mut chunks = [None; 8];
    for i in 0..8 {
        if loc_suitable_for_walking(sim, adjacents[i]) {
            chunks[i] = sim.get(adjacents[i]);
        }
    }

    for i in 0..8 {
        let Some(b_chunk) = chunks[i] else { continue };

        let hill_cost = ((b_chunk.alt - a_chunk.alt).abs() / 5.0).powi(2);
        let water_cost = (b_chunk.water_alt - b_chunk.alt + 8.0).clamped(0.0, 8.0) * 3.0; // Try not to path swamps / tidal areas
        let wild_cost = if b_chunk.path.0.is_way() {
            0.0 // Traversing existing paths has no additional cost!
        } else {
            3.0 // + (1.0 - b_chunk.tree_density) * 20.0 // Prefer going through forests, for aesthetics
        };

        let cost = 1.0 + hill_cost + water_cost + wild_cost;
        potential[i] = Some((adjacents[i], cost));
    }

    // Look for potential bridge spots in the cardinal directions if
    // `loc_suitable_for_wallking` was false for the adjacent chunk.
    for (i, &dir) in NEIGHBORS.iter().enumerate() {
        let is_cardinal_dir = dir.x == 0 || dir.y == 0;
        if is_cardinal_dir && potential[i].is_none() {
            // if we can skip over unsuitable area with a bridge
            potential[i] = (4..=5).find_map(|i| {
                loc_suitable_for_walking(sim, a + dir * i)
                    .then(|| (a + dir * i, 120.0 + (i - 4) as f32 * 10.0))
            });
        }
    }

    // If current position is a bridge, skip to its destination.
    if let Some(p) = bridge {
        let dir = (p - a).map(|e| e.signum());
        if let Some((dir_index, _)) = NEIGHBORS
            .iter()
            .enumerate()
            .find(|(_, n_dir)| **n_dir == dir)
        {
            potential[dir_index] = Some((p, (p - a).map(|e| e.abs()).reduce_max() as f32));
        }
    }

    potential
}

/// Return true if a position is suitable for walking on
fn loc_suitable_for_walking(sim: &WorldSim, loc: Vec2<i32>) -> bool {
    if sim.get(loc).is_some() {
        NEIGHBORS.iter().all(|n| {
            sim.get(loc + *n)
                .is_some_and(|chunk| !chunk.river.near_water())
        })
    } else {
        false
    }
}

/// Attempt to search for a location that's suitable for site construction
// FIXME when a `close_to_one_of` requirement is passed in, we should start with
// just the chunks around those locations instead of random sampling the entire
// map
fn find_site_loc(
    ctx: &mut GenCtx<impl Rng>,
    proximity_reqs: &ProximityRequirements,
    site_kind: &SiteKind,
) -> Option<Vec2<i32>> {
    prof_span!("find_site_loc");
    const MAX_ATTEMPTS: usize = 10000;
    let mut loc = None;
    let location_hint = proximity_reqs.location_hint;
    for _ in 0..MAX_ATTEMPTS {
        let test_loc = loc.unwrap_or_else(|| {
            Vec2::new(
                ctx.rng
                    .random_range(location_hint.min.x..location_hint.max.x),
                ctx.rng
                    .random_range(location_hint.min.y..location_hint.max.y),
            )
        });

        let is_suitable_loc = site_kind.is_suitable_loc(test_loc, ctx.sim);
        if is_suitable_loc && proximity_reqs.satisfied_by(test_loc) {
            if site_kind.exclusion_radius_clear(ctx.sim, test_loc) {
                return Some(test_loc);
            }

            // If the current location is suitable and meets proximity requirements,
            // try nearby spot downhill.
            loc = ctx.sim.get(test_loc).and_then(|c| c.downhill);
        }
    }

    debug!("Failed to place site {:?}.", site_kind);
    None
}

fn town_attributes_of_site(loc: Vec2<i32>, sim: &WorldSim) -> Option<TownSiteAttributes> {
    sim.get(loc).map(|chunk| {
        const RESOURCE_RADIUS: i32 = 1;
        let mut river_chunks = 0;
        let mut lake_chunks = 0;
        let mut ocean_chunks = 0;
        let mut rock_chunks = 0;
        let mut tree_chunks = 0;
        let mut farmable_chunks = 0;
        let mut farmable_needs_irrigation_chunks = 0;
        let mut land_chunks = 0;
        for x in (-RESOURCE_RADIUS)..RESOURCE_RADIUS {
            for y in (-RESOURCE_RADIUS)..RESOURCE_RADIUS {
                let check_loc = loc + Vec2::new(x, y).cpos_to_wpos();
                sim.get(check_loc).map(|c| {
                    if num::abs(chunk.alt - c.alt) < 200.0 {
                        if c.river.is_river() {
                            river_chunks += 1;
                        }
                        if c.river.is_lake() {
                            lake_chunks += 1;
                        }
                        if c.river.is_ocean() {
                            ocean_chunks += 1;
                        }
                        if c.tree_density > 0.3 {
                            tree_chunks += 1;
                        }
                        if c.rockiness < 0.4 && c.temp > CONFIG.snow_temp {
                            if c.surface_veg > 0.35 {
                                farmable_chunks += 1;
                            } else {
                                match c.get_biome() {
                                    common::terrain::BiomeKind::Savannah => {
                                        farmable_needs_irrigation_chunks += 1
                                    },
                                    common::terrain::BiomeKind::Desert => {
                                        farmable_needs_irrigation_chunks += 1
                                    },
                                    _ => (),
                                }
                            }
                        }
                        if !c.river.is_river() && !c.river.is_lake() && !c.river.is_ocean() {
                            land_chunks += 1;
                        }
                    }
                    // Mining is different since presumably you dig into the hillside
                    if c.rockiness > 0.7 && c.alt - chunk.alt > -10.0 {
                        rock_chunks += 1;
                    }
                });
            }
        }
        let has_river = river_chunks > 1;
        let has_lake = lake_chunks > 1;
        let vegetation_implies_potable_water = chunk.tree_density > 0.3
            && !matches!(chunk.get_biome(), common::terrain::BiomeKind::Swamp);
        let has_many_rocks = chunk.rockiness > 1.2;
        let warm_or_firewood = chunk.temp > CONFIG.snow_temp || tree_chunks > 2;
        let has_potable_water =
            { has_river || (has_lake && chunk.alt > 100.0) || vegetation_implies_potable_water };
        let has_building_materials = tree_chunks > 0
            || rock_chunks > 0
            || chunk.temp > CONFIG.tropical_temp && (has_river || has_lake);
        let water_rich = lake_chunks + river_chunks > 2;
        let can_grow_rice = water_rich
            && chunk.humidity + 1.0 > CONFIG.jungle_hum
            && chunk.temp + 1.0 > CONFIG.tropical_temp;
        let farming_score = if can_grow_rice {
            farmable_chunks * 2
        } else {
            farmable_chunks
        } + if water_rich {
            farmable_needs_irrigation_chunks
        } else {
            0
        };
        let fish_score = lake_chunks + ocean_chunks;
        let food_score = farming_score + fish_score;
        let mining_score = if tree_chunks > 1 { rock_chunks } else { 0 };
        let forestry_score = if has_river { tree_chunks } else { 0 };
        let trading_score = std::cmp::min(std::cmp::min(land_chunks, ocean_chunks), river_chunks);
        TownSiteAttributes {
            food_score,
            mining_score,
            forestry_score,
            trading_score,
            heating: warm_or_firewood,
            potable_water: has_potable_water,
            building_materials: has_building_materials,
            aquifer: has_many_rocks,
        }
    })
}

pub struct TownSiteAttributes {
    food_score: i32,
    mining_score: i32,
    forestry_score: i32,
    trading_score: i32,
    heating: bool,
    potable_water: bool,
    building_materials: bool,
    aquifer: bool,
}

impl TownSiteAttributes {
    pub fn score(&self) -> f32 {
        1.5 * (self.food_score as f32 + 1.0).log2()
            + 2.0 * (self.forestry_score as f32 + 1.0).log2()
            + (self.mining_score as f32 + 1.0).log2()
            + (self.trading_score as f32 + 1.0).log2()
    }
}

/// The authored map is edited at a much higher resolution than the engine's
/// compatibility grid. Preserve the authored pin, but do not let that
/// reduction place a settlement centre in a river or lake cell.
fn project_authored_settlement_location(requested: Vec2<i32>, sim: &WorldSim) -> Vec2<i32> {
    const SEARCH_RADIUS: i32 = 12;
    nearest_authored_settlement_location(requested, SEARCH_RADIUS, |location| {
        authored_settlement_has_dry_buffer(location, sim)
    })
    .unwrap_or(requested)
}

fn nearest_authored_settlement_location(
    requested: Vec2<i32>,
    search_radius: i32,
    is_suitable: impl Fn(Vec2<i32>) -> bool,
) -> Option<Vec2<i32>> {
    let mut candidates = Vec::with_capacity(((search_radius * 2 + 1).pow(2)) as usize);
    for dy in -search_radius..=search_radius {
        for dx in -search_radius..=search_radius {
            let location = requested + Vec2::new(dx, dy);
            candidates.push((location, dx * dx + dy * dy, dy.abs(), dx.abs()));
        }
    }
    candidates.sort_by_key(|(_, distance_sq, dy, dx)| (*distance_sq, *dy, *dx));

    candidates
        .into_iter()
        .map(|(location, _, _, _)| location)
        .find(|&location| is_suitable(location))
}

fn authored_settlement_has_dry_buffer(location: Vec2<i32>, sim: &WorldSim) -> bool {
    (-1..=1).all(|dy| {
        (-1..=1).all(|dx| {
            sim.get(location + Vec2::new(dx, dy))
                .is_some_and(|chunk| !chunk.is_underwater())
        })
    })
}

#[derive(Debug)]
pub struct Civ {
    capital: Id<Site>,
    homeland: Id<Place>,
}

#[derive(Debug)]
pub struct Place {
    pub center: Vec2<i32>,
    /* act sort of like territory with sites belonging to it
     * nat_res/NaturalResources was moved to Economy
     *    nat_res: NaturalResources, */
}

pub struct Track {
    /// Cost of using this track relative to other paths. This cost is an
    /// arbitrary unit and doesn't make sense unless compared to other track
    /// costs.
    pub cost: f32,
    path: Path<Vec2<i32>>,
}

impl Track {
    pub fn path(&self) -> &Path<Vec2<i32>> { &self.path }
}

#[derive(Debug)]
pub struct Site {
    pub kind: SiteKind,
    // TODO: Remove this field when overhauling
    pub site_tmp: Option<Id<crate::site::Site>>,
    pub center: Vec2<i32>,
    pub place: Id<Place>,
    /// Present iff this site was established from an authored Cromatolis
    /// settlement pin (as opposed to procedural civ generation).
    authored: Option<AuthoredSettlementMeta>,
    /// Present iff this site was established from an authored Cromatolis
    /// landmark pin.
    authored_landmark: Option<AuthoredLandmarkMeta>,
    /// Present iff this site was established from an authored Cromatolis
    /// bridge crossing (as opposed to the generic procedural bridge
    /// generator).
    authored_bridge: Option<AuthoredBridgeMeta>,
    /// Present iff this site was established from an authored Cromatolis
    /// defensive fortification (wall + gates) pin.
    authored_fortification: Option<AuthoredFortificationMeta>,
}

impl Site {
    /// The authored map owns the starting-site policy for its settlements.
    pub fn is_authored_starting_settlement(&self) -> bool {
        self.authored
            .as_ref()
            .is_some_and(|settlement| settlement.start_eligible)
    }

    /// Whether player-start selection should consider this site at all.
    /// Procedural sites (not authored) are always eligible -- the authored
    /// map only ever *restricts* the pool, it never adds eligibility a
    /// procedural site wouldn't already have. An authored settlement is
    /// eligible unless its own `start_eligible: false` explicitly excludes
    /// it (e.g. a settlement whose lore/terrain makes it unsuitable, such as
    /// a permanently stormy or volcanic hamlet).
    pub fn is_eligible_as_starting_site(&self) -> bool {
        self.authored
            .as_ref()
            .is_none_or(|settlement| settlement.start_eligible)
    }

    /// The authored name for this site, if it was established from an
    /// authored settlement or landmark pin rather than procedural
    /// generation.
    fn authored_name(&self) -> Option<&str> {
        self.authored
            .as_ref()
            .map(|settlement| settlement.name.as_str())
            .or_else(|| {
                self.authored_landmark
                    .as_ref()
                    .map(|landmark| landmark.name.as_str())
            })
    }

    /// Test-only mutator. `authored` is private so other crate modules can't
    /// build/edit it directly; this lets a cross-module integration test
    /// (see `crate::tests` in `lib.rs`) exercise player-start exclusion
    /// against a real, fully generated authored settlement -- picking one
    /// out of a real generated world and flipping its eligibility -- instead
    /// of a synthetic stand-in with no real plots/terrain behind it. Does
    /// nothing on a site that isn't an authored settlement.
    #[cfg(test)]
    pub(crate) fn set_start_eligible_for_test(&mut self, start_eligible: bool) {
        if let Some(authored) = self.authored.as_mut() {
            authored.start_eligible = start_eligible;
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "{:?}", self.kind)?;

        Ok(())
    }
}

impl SiteKind {
    pub fn is_suitable_loc(&self, loc: Vec2<i32>, sim: &WorldSim) -> bool {
        let on_land = || -> bool {
            if let Some(chunk) = sim.get(loc) {
                !chunk.river.is_ocean()
                    && !chunk.river.is_lake()
                    && !chunk.river.is_river()
                    && !chunk.is_underwater()
                    && !matches!(
                        chunk.get_biome(),
                        common::terrain::BiomeKind::Lake | common::terrain::BiomeKind::Ocean
                    )
            } else {
                false
            }
        };
        let on_flat_terrain = || -> bool {
            sim.get_gradient_approx(loc)
                .map(|grad| grad < 1.0)
                .unwrap_or(false)
        };

        sim.get(loc).is_some_and(|chunk| {
            let suitable_for_town = || -> bool {
                let attributes = town_attributes_of_site(loc, sim);
                attributes.is_some_and(|attributes| {
                    // aquifer and has_many_rocks was added to make mesa clifftowns suitable for towns
                    (attributes.potable_water || (attributes.aquifer && matches!(self, SiteKind::CliffTown)))
                        && attributes.building_materials
                        && attributes.heating
                        // Because of how the algorithm for site towns work, they have to start on land.
                        && on_land()
                })
            };
            match self {
                SiteKind::Gnarling => {
                    on_land()
                        && on_flat_terrain()
                        && (-0.3..0.4).contains(&chunk.temp)
                        && chunk.tree_density > 0.75
                },
                SiteKind::Adlet => chunk.temp < -0.2 && chunk.cliff_height > 25.0,
                SiteKind::DwarvenMine => {
                    matches!(chunk.get_biome(), BiomeKind::Forest | BiomeKind::Desert)
                        && !chunk.near_cliffs()
                        && !chunk.river.near_water()
                        && on_flat_terrain()
                },
                SiteKind::Haniwa => {
                    on_land()
                        && on_flat_terrain()
                        && (-0.3..0.4).contains(&chunk.temp)
                },
                SiteKind::GiantTree => {
                    on_land()
                        && on_flat_terrain()
                        && chunk.tree_density > 0.4
                        && (-0.3..0.4).contains(&chunk.temp)
                },
                SiteKind::Citadel => true,
                SiteKind::CliffTown => {
                    chunk.temp >= CONFIG.desert_temp
                        && chunk.cliff_height > 40.0
                        && chunk.rockiness > 1.2
                        && suitable_for_town()
                },
                SiteKind::GliderCourse => {
                    chunk.alt > 1400.0
                },
                SiteKind::SavannahTown => {
                    matches!(chunk.get_biome(), BiomeKind::Savannah)
                        && !chunk.near_cliffs()
                        && !chunk.river.near_water()
                        && suitable_for_town()
                },
                SiteKind::CoastalTown => {
                    (2.0..3.5).contains(&(chunk.water_alt - CONFIG.sea_level))
                        && suitable_for_town()
                },
                SiteKind::PirateHideout => {
                    (0.5..3.5).contains(&(chunk.water_alt - CONFIG.sea_level))
                },
                SiteKind::Sahagin => {
                    matches!(chunk.get_biome(), BiomeKind::Ocean)
                    && (40.0..45.0).contains(&(CONFIG.sea_level - chunk.alt))
                },
                SiteKind::JungleRuin => {
                    matches!(chunk.get_biome(), BiomeKind::Jungle)
                },
                SiteKind::RockCircle => !chunk.near_cliffs() && !chunk.river.near_water(),
                SiteKind::TrollCave => {
                    !chunk.near_cliffs()
                        && on_flat_terrain()
                        && !chunk.river.near_water()
                        && chunk.temp < 0.6
                },
                SiteKind::Camp => {
                    !chunk.near_cliffs() && on_flat_terrain() && !chunk.river.near_water()
                },
                SiteKind::DesertCity => {
                    (0.9..1.0).contains(&chunk.temp) && !chunk.near_cliffs() && suitable_for_town()
                        && on_land()
                        && !chunk.river.near_water()
                },
                SiteKind::ChapelSite => {
                    matches!(chunk.get_biome(), BiomeKind::Ocean)
                        && CONFIG.sea_level < chunk.alt + 1.0
                },
                SiteKind::Terracotta => {
                    (0.9..1.0).contains(&chunk.temp)
                        && on_land()
                        && (chunk.water_alt - CONFIG.sea_level) > 50.0
                        && on_flat_terrain()
                        && !chunk.river.near_water()
                        && !chunk.near_cliffs()
                },
                SiteKind::Myrmidon => {
                    (0.9..1.0).contains(&chunk.temp)
                        && on_land()
                        && (chunk.water_alt - CONFIG.sea_level) > 50.0
                        && on_flat_terrain()
                        && !chunk.river.near_water()
                        && !chunk.near_cliffs()
                },
                SiteKind::Cultist => on_land() && chunk.temp < 0.5 && chunk.near_cliffs(),
                SiteKind::VampireCastle => on_land() && chunk.temp <= -0.8 && chunk.near_cliffs(),
                SiteKind::Refactor => suitable_for_town(),
                SiteKind::Bridge(_, _) => true,
                // Placement is driven entirely by the authored start/end
                // pins (see `establish_authored_cromatolis_fortifications`),
                // same as `Bridge` above.
                SiteKind::Fortification(_, _) => true,
            }
        })
    }

    pub fn exclusion_radius(&self) -> i32 {
        // FIXME: Provide specific values for each individual SiteKind
        match self {
            SiteKind::Myrmidon => 7,
            _ => 8, // This is just an arbitrary value
        }
    }

    pub fn exclusion_radius_clear(&self, sim: &WorldSim, loc: Vec2<i32>) -> bool {
        let radius = self.exclusion_radius();
        for x in (-radius)..radius {
            for y in (-radius)..radius {
                let check_loc = loc + Vec2::new(x, y);
                if sim.get(check_loc).is_some_and(|c| !c.sites.is_empty()) {
                    return false;
                }
            }
        }
        true
    }
}

impl Site {
    pub fn is_dungeon(&self) -> bool {
        matches!(
            self.kind,
            SiteKind::Adlet
                | SiteKind::Gnarling
                | SiteKind::ChapelSite
                | SiteKind::Terracotta
                | SiteKind::Haniwa
                | SiteKind::Myrmidon
                | SiteKind::DwarvenMine
                | SiteKind::Cultist
                | SiteKind::Sahagin
                | SiteKind::VampireCastle
        )
    }

    pub fn is_settlement(&self) -> bool {
        matches!(
            self.kind,
            SiteKind::Refactor
                | SiteKind::CliffTown
                | SiteKind::DesertCity
                | SiteKind::SavannahTown
                | SiteKind::CoastalTown
        )
    }

    pub fn is_bridge(&self) -> bool { matches!(self.kind, SiteKind::Bridge(_, _)) }

    pub fn is_fortification(&self) -> bool { matches!(self.kind, SiteKind::Fortification(_, _)) }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct PointOfInterest {
    pub name: String,
    pub kind: PoiKind,
    pub loc: Vec2<i32>,
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum PoiKind {
    /// Peak stores the altitude
    Peak(u32),
    /// Lake stores a metric relating to size
    Biome(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn empty_proximity_requirements() {
        let world_dims = Aabr {
            min: Vec2 { x: 0, y: 0 },
            max: Vec2 {
                x: 200_i32,
                y: 200_i32,
            },
        };
        let reqs = ProximityRequirementsBuilder::new().finalize(&world_dims);
        assert!(reqs.satisfied_by(Vec2 { x: 0, y: 0 }));
    }

    #[test]
    fn avoid_proximity_requirements() {
        let world_dims = Aabr {
            min: Vec2 {
                x: -200_i32,
                y: -200_i32,
            },
            max: Vec2 {
                x: 200_i32,
                y: 200_i32,
            },
        };
        let reqs = ProximityRequirementsBuilder::new()
            .avoid_all_of(vec![Vec2 { x: 0, y: 0 }].into_iter(), 10)
            .finalize(&world_dims);
        assert!(reqs.satisfied_by(Vec2 { x: 8, y: -8 }));
        assert!(!reqs.satisfied_by(Vec2 { x: -1, y: 1 }));
    }

    #[test]
    fn near_proximity_requirements() {
        let world_dims = Aabr {
            min: Vec2 {
                x: -200_i32,
                y: -200_i32,
            },
            max: Vec2 {
                x: 200_i32,
                y: 200_i32,
            },
        };
        let reqs = ProximityRequirementsBuilder::new()
            .close_to_one_of(vec![Vec2 { x: 0, y: 0 }].into_iter(), 10)
            .finalize(&world_dims);
        assert!(reqs.satisfied_by(Vec2 { x: 1, y: -1 }));
        assert!(!reqs.satisfied_by(Vec2 { x: -8, y: 8 }));
    }

    #[test]
    fn complex_proximity_requirements() {
        let a_site = Vec2 { x: 572, y: 724 };
        let world_dims = Aabr {
            min: Vec2 { x: 0, y: 0 },
            max: Vec2 {
                x: 1000_i32,
                y: 1000_i32,
            },
        };
        let reqs = ProximityRequirementsBuilder::new()
            .close_to_one_of(vec![a_site].into_iter(), 60)
            .avoid_all_of(vec![a_site].into_iter(), 40)
            .finalize(&world_dims);
        assert!(reqs.satisfied_by(Vec2 { x: 572, y: 774 }));
        assert!(!reqs.satisfied_by(a_site));
    }

    #[test]
    fn location_hint() {
        let reqs = ProximityRequirementsBuilder::new().close_to_one_of(
            vec![Vec2 { x: 1, y: 0 }, Vec2 { x: 13, y: 12 }].into_iter(),
            10,
        );
        let expected = Aabr {
            min: Vec2 { x: 0, y: 0 },
            max: Vec2 { x: 23, y: 22 },
        };
        let map_dims = Aabr {
            min: Vec2 { x: 0, y: 0 },
            max: Vec2 { x: 200, y: 300 },
        };
        assert_eq!(expected, reqs.location_hint(&map_dims));
    }

    // ---- Authored Cromatolis settlements/landmarks: loaders ----

    fn real_settlements() -> AuthoredCromatolisSettlements {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_sites.ron"
        ))
        .expect("real Cromatolis settlements export must parse")
    }

    fn real_landmarks() -> AuthoredCromatolisLandmarks {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_landmarks.ron"
        ))
        .expect("real Cromatolis landmarks export must parse")
    }

    fn real_landmark_profiles() -> AuthoredCromatolisLandmarkProfiles {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_landmark_profiles.ron"
        ))
        .expect("real Cromatolis landmark profiles export must parse")
    }

    fn real_routes() -> AuthoredCromatolisRouteGraph {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_routes.ron"
        ))
        .expect("real Cromatolis route graph export must parse")
    }

    fn real_bridges() -> AuthoredCromatolisBridges {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_bridges.ron"
        ))
        .expect("real Cromatolis bridges export must parse")
    }

    fn real_fortifications() -> AuthoredCromatolisFortifications {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_fortifications.ron"
        ))
        .expect("real Cromatolis fortifications export must parse")
    }

    fn real_settlement_template_contract() -> SettlementTemplateContract {
        load_ron(include_bytes!(
            "../../../assets/world/map/cromatolis_v0_settlement_template_contract.ron"
        ))
        .expect("real settlement template contract must parse")
    }

    fn synthetic_map_size() -> MapSizeLg { MapSizeLg::new(Vec2::new(10, 10)).unwrap() }

    #[test]
    fn cromatolis_authored_settlements_parse_and_validate_real_export_without_panicking() {
        let settlements = real_settlements();
        assert_eq!(
            settlements.schema,
            "xindeler_open_world.authored_settlements.v1"
        );
        let map_size = synthetic_map_size();
        settlements
            .validate(map_size)
            .expect("real Cromatolis settlements must be valid at runtime scale");

        // Real, measured numbers as of this export -- not the round figure
        // an earlier design pass estimated before the source data's last
        // update.
        assert_eq!(settlements.settlements.len(), 62);

        let ids = settlements
            .settlements
            .iter()
            .map(|settlement| settlement.id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), settlements.settlements.len());

        let capitals = settlements
            .settlements
            .iter()
            .filter(|settlement| settlement.category == AuthoredSettlementCategory::Capital)
            .count();
        assert_eq!(capitals, 1);

        let mut by_category: std::collections::HashMap<AuthoredSettlementCategory, usize> =
            std::collections::HashMap::new();
        for settlement in &settlements.settlements {
            *by_category.entry(settlement.category).or_default() += 1;
        }
        assert_eq!(
            by_category.get(&AuthoredSettlementCategory::Capital),
            Some(&1)
        );
        assert_eq!(by_category.get(&AuthoredSettlementCategory::City), Some(&7));
        assert_eq!(
            by_category.get(&AuthoredSettlementCategory::Town),
            Some(&17)
        );
        assert_eq!(
            by_category.get(&AuthoredSettlementCategory::Village),
            Some(&20)
        );
        assert_eq!(
            by_category.get(&AuthoredSettlementCategory::Hamlet),
            Some(&5)
        );
        assert_eq!(by_category.get(&AuthoredSettlementCategory::Inn), Some(&4));
        assert_eq!(by_category.get(&AuthoredSettlementCategory::Post), Some(&8));
    }

    #[test]
    fn cromatolis_authored_landmarks_parse_and_validate_real_export_without_panicking() {
        let landmarks = real_landmarks();
        assert_eq!(
            landmarks.schema,
            "xindeler_open_world.authored_landmarks.v1"
        );
        let map_size = synthetic_map_size();
        landmarks
            .validate(map_size)
            .expect("real Cromatolis landmarks must be valid at runtime scale");

        // Real, measured numbers as of this export -- not the round figure
        // an earlier design pass estimated before the source data's last
        // update (which added the two river-port Harbour landmarks).
        assert_eq!(landmarks.landmarks.len(), 16);

        let mut by_kind: std::collections::HashMap<AuthoredLandmarkKind, usize> =
            std::collections::HashMap::new();
        for landmark in &landmarks.landmarks {
            *by_kind.entry(landmark.kind).or_default() += 1;
        }
        assert_eq!(by_kind.get(&AuthoredLandmarkKind::TreeOfLife), Some(&1));
        assert_eq!(by_kind.get(&AuthoredLandmarkKind::TreeOfSouls), Some(&1));
        assert_eq!(by_kind.get(&AuthoredLandmarkKind::BlackTower), Some(&2));
        assert_eq!(by_kind.get(&AuthoredLandmarkKind::Lighthouse), Some(&9));
        assert_eq!(
            by_kind.get(&AuthoredLandmarkKind::ArchWrightTemple),
            Some(&1)
        );
        assert_eq!(by_kind.get(&AuthoredLandmarkKind::Harbour), Some(&2));
    }

    #[test]
    fn cromatolis_landmark_profiles_cover_every_real_landmark_without_panicking() {
        let landmarks = real_landmarks();
        let profiles = real_landmark_profiles();
        profiles
            .validate(&landmarks)
            .expect("every real landmark must have exactly one valid physical profile");
        assert_eq!(profiles.entries.len(), landmarks.landmarks.len());
    }

    // ---- Authored Cromatolis routes/bridges: loaders ----

    #[test]
    fn cromatolis_authored_routes_parse_and_validate_real_export_without_panicking() {
        let routes = real_routes();
        assert_eq!(routes.schema, "xindeler_open_world.authored_route_graph.v1");
        let map_size = synthetic_map_size();
        routes
            .validate(map_size)
            .expect("real Cromatolis route graph must be valid at runtime scale");

        // Real, measured count as of this export -- verified against the
        // current `xindeler-open-world` export, not the design pass's
        // earlier estimate (see the module-level note above and this row's
        // spec §3/§6 for the "verify against the live export" rule).
        assert_eq!(routes.routes.len(), 49);

        let ids = routes
            .routes
            .iter()
            .map(|route| route.id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), routes.routes.len());
    }

    #[test]
    fn cromatolis_authored_route_endpoints_resolve_against_real_settlements() {
        let routes = real_routes();
        let settlements = real_settlements();
        let settlement_ids = settlements
            .settlements
            .iter()
            .map(|settlement| settlement.id.as_str())
            .collect::<HashSet<_>>();

        for route in &routes.routes {
            assert!(
                settlement_ids.contains(route.start_site_id.as_str()),
                "route {} references unknown start settlement {}",
                route.id,
                route.start_site_id
            );
            assert!(
                settlement_ids.contains(route.end_site_id.as_str()),
                "route {} references unknown end settlement {}",
                route.id,
                route.end_site_id
            );
        }
    }

    #[test]
    fn cromatolis_authored_bridges_parse_and_validate_real_export_without_panicking() {
        let bridges = real_bridges();
        assert_eq!(bridges.schema, "xindeler_open_world.authored_bridges.v1");
        let map_size = synthetic_map_size();
        bridges
            .validate(map_size)
            .expect("real Cromatolis bridges must be valid at runtime scale");

        // The contract requires exactly 12 -- `validate` already enforces
        // this, this assertion documents the number for readers.
        assert_eq!(bridges.bridges.len(), 12);

        let ids = bridges
            .bridges
            .iter()
            .map(|bridge| bridge.id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), bridges.bridges.len());
    }

    #[test]
    fn cromatolis_authored_bridge_design_maps_real_dimensions() {
        let bridges = real_bridges();
        let kalthis_duren = bridges
            .bridges
            .iter()
            .find(|bridge| bridge.id == "bridge.kalthis_duren")
            .expect("the named Kalthis-Duren crossing must be present in the real export");

        // Trust the RON over any prior markdown draft (see this row's spec
        // §3): the real export uses 17m width / 12m clearance.
        assert_eq!(kalthis_duren.deck_width_m, 17.0);
        assert_eq!(kalthis_duren.deck_clearance_m, 12.0);
        assert!(matches!(
            kalthis_duren.design(),
            site::AuthoredBridgeDesign::GrandStoneIron {
                deck_width: 17,
                clearance: 12,
                deck_thickness: 3,
            }
        ));

        for bridge in &bridges.bridges {
            assert!(
                bridge.dimensions_are_valid(),
                "bridge {} has invalid dimensions",
                bridge.id
            );
        }
    }

    #[test]
    fn bridge_preview_selection_matches_by_id_or_all() {
        assert!(bridge_is_selected_for_preview(
            "bridge.kalthis_duren",
            "bridge.kalthis_duren"
        ));
        assert!(!bridge_is_selected_for_preview(
            "bridge.kalthis_duren",
            "bridge.sapphire_loch"
        ));
        assert!(bridge_is_selected_for_preview(
            "all",
            "bridge.sapphire_loch"
        ));
    }

    #[test]
    fn invalid_authored_settlement_schema_fails_validation_without_panicking() {
        let mut settlements = real_settlements();
        settlements.schema = "invalid".to_string();
        assert!(settlements.validate(synthetic_map_size()).is_err());
    }

    // ---- Authored Cromatolis fortifications: loader + gate physical state ----

    #[test]
    fn cromatolis_authored_fortifications_parse_and_validate_real_export_without_panicking() {
        let fortifications = real_fortifications();
        assert_eq!(
            fortifications.schema,
            "xindeler_open_world.authored_fortifications.v1"
        );
        assert_eq!(
            fortifications.coordinate_space,
            "source_pixels_xy_top_left_origin"
        );
        fortifications
            .validate(synthetic_map_size())
            .expect("real Cromatolis fortifications must validate");

        assert_eq!(fortifications.fortifications.len(), 3);
        let ids = fortifications
            .fortifications
            .iter()
            .map(|fortification| fortification.id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), fortifications.fortifications.len());
    }

    #[test]
    fn invalid_authored_fortification_schema_fails_validation_without_panicking() {
        let mut fortifications = real_fortifications();
        fortifications.schema = "invalid".to_string();
        assert!(fortifications.validate(synthetic_map_size()).is_err());
    }

    #[test]
    fn invalid_authored_fortification_coordinate_space_fails_validation_without_panicking() {
        let mut fortifications = real_fortifications();
        fortifications.coordinate_space = "normalized_map_xy_top_left_origin".to_string();
        assert!(fortifications.validate(synthetic_map_size()).is_err());
    }

    #[test]
    fn fortification_chunk_collapse_fails_validation_without_panicking() {
        let mut fortifications = real_fortifications();
        // Force the first fortification's start/end to normalize to the same
        // chunk at this map size, without them being literally the same
        // pixel point (that's the separate raw-pixel collapse check above).
        fortifications.fortifications[0].end = fortifications.fortifications[0].start;
        fortifications.fortifications[0].end.x += 1.0;
        assert!(fortifications.validate(synthetic_map_size()).is_err());
    }

    #[test]
    fn duplicate_fortification_span_fails_validation_without_panicking() {
        let mut fortifications = real_fortifications();
        let duplicate = fortifications.fortifications[0].clone();
        fortifications
            .fortifications
            .push(AuthoredCromatolisFortification {
                id: "site.duplicate_span_stone".to_string(),
                ..duplicate
            });
        assert!(fortifications.validate(synthetic_map_size()).is_err());
    }

    #[test]
    fn fortification_pixel_point_normalizes_against_its_own_source_map() {
        let fortifications = real_fortifications();
        // Formula from the loader's contract: never a hardcoded 2048/1536 --
        // always read from the asset's own `source_map`.
        let normalized = fortifications.normalize_point(AuthoredPixelPoint { x: 0.0, y: 0.0 });
        assert_eq!(normalized.x, 0.0);
        assert_eq!(normalized.y, 0.0);

        let normalized = fortifications.normalize_point(AuthoredPixelPoint {
            x: fortifications.source_map.width_px as f32 - 1.0,
            y: fortifications.source_map.height_px as f32 - 1.0,
        });
        assert_eq!(normalized.x, 1.0);
        assert_eq!(normalized.y, 1.0);
    }

    // ---- `/cromatolis_goto` pixel-to-world conversion (COW-9) ----

    #[test]
    fn cromatolis_source_pixels_reads_real_fortifications_source_map() {
        let fortifications = real_fortifications();
        let source_pixels =
            cromatolis_source_pixels().expect("real fortifications asset must load");
        assert_eq!(source_pixels.x, fortifications.source_map.width_px);
        assert_eq!(source_pixels.y, fortifications.source_map.height_px);
    }

    #[test]
    fn cromatolis_source_pixel_to_wpos_matches_fortification_loader_reference_point() {
        let fortifications = real_fortifications();
        // The fortifications asset is the one authored asset that already
        // uses raw pixel coordinates (like `/cromatolis_goto`'s input)
        // rather than pre-normalized 0..1 points -- so its own reference
        // point doubles as a real, already-committed cross-check with no
        // reconstruction rounding involved. Its `source_map` is also the
        // canonical source of the canvas dimensions, read here the same way
        // `cromatolis_source_pixels` reads it, never a hardcoded literal.
        let source_pixels = Vec2::new(
            fortifications.source_map.width_px,
            fortifications.source_map.height_px,
        );

        let reference = fortifications
            .fortifications
            .iter()
            .find(|fortification| fortification.id == "site.northwall_stone")
            .expect("reference fortification must exist in the real export");

        let map_size = synthetic_map_size();
        let expected = fortifications
            .normalize_point(reference.start)
            .to_chunk_pos(map_size)
            .cpos_to_wpos_center();

        let got = cromatolis_source_pixel_to_wpos(
            Vec2::new(reference.start.x, reference.start.y),
            source_pixels,
            map_size,
        )
        .expect("reference pixel must be within the source canvas");

        assert_eq!(got, expected);
    }

    #[test]
    fn cromatolis_source_pixel_to_wpos_rejects_out_of_range_pixels_without_panicking() {
        let map_size = synthetic_map_size();
        let source_pixels = Vec2::new(2048_u32, 1536_u32);
        assert!(
            cromatolis_source_pixel_to_wpos(Vec2::new(-1.0, 0.0), source_pixels, map_size)
                .is_none()
        );
        assert!(
            cromatolis_source_pixel_to_wpos(Vec2::new(0.0, -1.0), source_pixels, map_size)
                .is_none()
        );
        assert!(
            cromatolis_source_pixel_to_wpos(
                Vec2::new(source_pixels.x as f32, 0.0),
                source_pixels,
                map_size
            )
            .is_none()
        );
        assert!(
            cromatolis_source_pixel_to_wpos(
                Vec2::new(0.0, source_pixels.y as f32),
                source_pixels,
                map_size
            )
            .is_none()
        );
        // In-range corners must succeed.
        assert!(
            cromatolis_source_pixel_to_wpos(Vec2::new(0.0, 0.0), source_pixels, map_size).is_some()
        );
        assert!(
            cromatolis_source_pixel_to_wpos(
                Vec2::new((source_pixels.x - 1) as f32, (source_pixels.y - 1) as f32),
                source_pixels,
                map_size
            )
            .is_some()
        );
    }

    /// Locks in the per-gate physical open/closed state confirmed against
    /// lore/settlement data: the Freelands gate (anchored near
    /// `site.evercross`) stays open, the other two stay closed. The state
    /// itself lives on the RON record (`default_open`), same as a bridge's
    /// `style` field -- this test just confirms the real export still
    /// carries the expected values and that the metadata pipeline the
    /// renderer actually consumes (`AuthoredCromatolisFortification::meta`)
    /// passes it through unchanged. See `plot::fortification`'s own tests
    /// for the render-level half (turning `open` into an actual passable
    /// gap).
    #[test]
    fn cromatolis_gate_physical_state_matches_the_locked_in_policy_for_all_three_real_gates() {
        let fortifications = real_fortifications();
        let expected_open: std::collections::HashMap<&str, bool> = [
            ("gate.northwall_black_iron", true),
            ("gate.greenhwall_black_iron", false),
            ("gate.eastwall_black_iron", false),
        ]
        .into_iter()
        .collect();

        let mut seen = std::collections::HashSet::new();
        for fortification in &fortifications.fortifications {
            assert_eq!(
                fortification.gates.len(),
                1,
                "fortification {} is expected to have exactly one gate in this export",
                fortification.id
            );
            let gate = &fortification.gates[0];
            let expected = *expected_open.get(gate.id.as_str()).unwrap_or_else(|| {
                panic!(
                    "gate {} is not one of the three real, locked-in gates",
                    gate.id
                )
            });
            assert_eq!(
                gate.default_open, expected,
                "gate {} physical state does not match the locked-in policy",
                gate.id
            );
            // Also confirm the metadata pipeline the renderer actually
            // consumes (`AuthoredCromatolisFortification::meta`) carries the
            // same state through.
            let design_gate = fortification
                .meta()
                .design
                .gates
                .into_iter()
                .next()
                .expect("fortification must produce exactly one gate design");
            assert_eq!(design_gate.open, expected);
            seen.insert(gate.id.clone());
        }
        assert_eq!(seen.len(), 3, "all three real gates must be exercised");
    }

    #[test]
    fn invalid_authored_landmark_profiles_reject_a_mismatched_physical_template() {
        let landmarks = real_landmarks();
        let mut profiles = real_landmark_profiles();
        // Swap the first entry's template to one no landmark kind allows.
        profiles.entries[0].physical_template = AuthoredLandmarkPhysicalTemplate::Chapel;
        profiles.entries[0].style = AuthoredLandmarkStyle::MonumentalChapel;
        assert!(profiles.validate(&landmarks).is_err());
    }

    #[test]
    fn authored_map_point_flips_source_y_axis() {
        let map_size = synthetic_map_size();
        assert_eq!(
            AuthoredMapPoint { x: 0.0, y: 0.0 }.to_chunk_pos(map_size),
            Vec2::new(0, 1023)
        );
        assert_eq!(
            AuthoredMapPoint { x: 1.0, y: 1.0 }.to_chunk_pos(map_size),
            Vec2::new(1023, 0)
        );
    }

    // ---- Positioning ----

    #[test]
    fn nearest_authored_settlement_location_keeps_a_valid_pin_or_finds_nearest_match() {
        let requested = Vec2::new(100, 100);
        assert_eq!(
            nearest_authored_settlement_location(requested, 2, |location| location == requested),
            Some(requested)
        );
        assert_eq!(
            nearest_authored_settlement_location(requested, 2, |location| location
                == Vec2::new(101, 100)),
            Some(Vec2::new(101, 100))
        );
        assert_eq!(
            nearest_authored_settlement_location(requested, 2, |_| false),
            None
        );
    }

    #[test]
    fn project_authored_settlement_location_falls_back_to_requested_when_nothing_qualifies() {
        let requested = Vec2::new(50, 50);
        // No `WorldSim` chunks exist at all outside index 0, so every probed
        // location around `requested` fails the dry-buffer check and the
        // projection must fall back to the literal requested position
        // instead of panicking or wandering off.
        let sim = WorldSim::empty();
        assert_eq!(
            project_authored_settlement_location(requested, &sim),
            requested
        );
    }

    // ---- The data-driven category+size -> generator table ----

    #[test]
    fn settlement_template_contract_parses_and_validates_real_export_without_panicking() {
        let contract = real_settlement_template_contract();
        contract
            .validate()
            .expect("real settlement template contract must be valid");
        assert!(!contract.template_families.is_empty());
    }

    #[test]
    fn resolve_settlement_site_kind_covers_every_real_settlement_via_the_contract_table() {
        let settlements = real_settlements();
        let contract = real_settlement_template_contract();

        for settlement in &settlements.settlements {
            // Must resolve through a real family match in the contract (not
            // silently fall through to the category default) for every
            // settlement actually in the export -- this is the "fallback
            // behaviour is explicitly tested, not incidental" requirement.
            let matched_family = contract.template_families.iter().any(|family| {
                family.category == settlement.category.contract_key()
                    && family
                        .supported_sizes
                        .iter()
                        .any(|s| s == settlement.size.contract_key())
            });
            assert!(
                matched_family,
                "settlement {} (category {:?}, size {:?}) has no matching family in the \
                 settlement template contract",
                settlement.id, settlement.category, settlement.size
            );

            let kind =
                resolve_settlement_site_kind(Some(&contract), settlement.category, settlement.size);
            // Honest baseline: every family's current fallback is one of
            // these two generic generators -- no real per-family physical
            // generator exists yet.
            assert!(matches!(kind, SiteKind::Camp | SiteKind::Refactor));
            if matches!(
                settlement.category,
                AuthoredSettlementCategory::Inn | AuthoredSettlementCategory::Post
            ) {
                assert_eq!(kind, SiteKind::Camp);
            } else {
                assert_eq!(kind, SiteKind::Refactor);
            }
        }
    }

    #[test]
    fn resolve_settlement_site_kind_falls_back_to_the_default_split_without_a_contract() {
        // No contract at all (e.g. failed to load) -- every category must
        // still resolve to the safe Camp/Refactor default, never panic.
        for category in [
            AuthoredSettlementCategory::Capital,
            AuthoredSettlementCategory::City,
            AuthoredSettlementCategory::Town,
            AuthoredSettlementCategory::Village,
            AuthoredSettlementCategory::Hamlet,
            AuthoredSettlementCategory::Inn,
            AuthoredSettlementCategory::Post,
        ] {
            let kind = resolve_settlement_site_kind(None, category, AuthoredSettlementSize::Medium);
            assert_eq!(kind, category.default_site_kind());
        }
    }

    #[test]
    fn resolve_settlement_site_kind_falls_back_when_no_family_matches_category_and_size() {
        let contract = real_settlement_template_contract();
        // `city_very_large`/`city_large`/`city_medium`/`city_small` exist,
        // but no family supports a `Minimal` city in the real contract --
        // this exercises the "family list exists but doesn't cover this
        // exact category+size" branch of the fallback, not just "no
        // contract at all".
        let no_family_covers_city_minimal = !contract.template_families.iter().any(|family| {
            family.category == "city" && family.supported_sizes.iter().any(|s| s == "minimal")
        });
        assert!(
            no_family_covers_city_minimal,
            "test assumption stale: the contract now defines a city/minimal family"
        );

        let kind = resolve_settlement_site_kind(
            Some(&contract),
            AuthoredSettlementCategory::City,
            AuthoredSettlementSize::Minimal,
        );
        assert_eq!(kind, SiteKind::Refactor);
    }

    #[test]
    fn resolve_settlement_site_kind_falls_back_on_an_unrecognized_fallback_name() {
        let contract = SettlementTemplateContract {
            schema: SETTLEMENT_TEMPLATE_CONTRACT_SCHEMA.to_string(),
            template_families: vec![SettlementTemplateFamily {
                id: "test_family".to_string(),
                category: "inn".to_string(),
                supported_sizes: vec!["minimal".to_string()],
                xindeler_old_fallback: "some_future_bespoke_inn_generator".to_string(),
            }],
        };
        let kind = resolve_settlement_site_kind(
            Some(&contract),
            AuthoredSettlementCategory::Inn,
            AuthoredSettlementSize::Minimal,
        );
        // Inn's safe default is Camp; an unrecognized fallback name must not
        // panic or silently resolve to something else.
        assert_eq!(kind, SiteKind::Camp);
    }

    #[test]
    fn site_kind_for_fallback_name_recognizes_every_name_the_real_contract_uses() {
        let contract = real_settlement_template_contract();
        for family in &contract.template_families {
            assert!(
                site_kind_for_fallback_name(&family.xindeler_old_fallback).is_some(),
                "family {} names an unrecognized fallback {}",
                family.id,
                family.xindeler_old_fallback
            );
        }
    }

    // ---- Site helper methods ----

    #[test]
    fn authored_starting_settlement_policy_reads_the_authored_flag() {
        fn settlement_site(start_eligible: bool) -> Site {
            Site {
                kind: SiteKind::Refactor,
                site_tmp: None,
                center: Vec2::zero(),
                place: Id::new(0),
                authored: Some(AuthoredSettlementMeta {
                    id: "test".to_string(),
                    name: "Test".to_string(),
                    category: AuthoredSettlementCategory::Village,
                    size: AuthoredSettlementSize::Small,
                    population: AuthoredSettlementPopulation {
                        tag: AuthoredSettlementPopulationTag::Human,
                        peoples: vec![AuthoredSettlementPeople::Human],
                        future_peoples: Vec::new(),
                    },
                    requires_capital_castle: false,
                    start_eligible,
                }),
                authored_landmark: None,
                authored_bridge: None,
                authored_fortification: None,
            }
        }

        let eligible = settlement_site(true);
        assert!(eligible.is_authored_starting_settlement());
        assert_eq!(eligible.authored_name(), Some("Test"));

        let not_eligible = settlement_site(false);
        assert!(!not_eligible.is_authored_starting_settlement());

        let procedural = Site {
            kind: SiteKind::Refactor,
            site_tmp: None,
            center: Vec2::zero(),
            place: Id::new(0),
            authored: None,
            authored_landmark: None,
            authored_bridge: None,
            authored_fortification: None,
        };
        assert!(!procedural.is_authored_starting_settlement());
        assert_eq!(procedural.authored_name(), None);
    }

    // ---- Heavy, real-terrain-backed tests: require the real Cromatolis LFS
    // assets pulled locally. Not run automated (same precedent as
    // `site::economy::context::tests::test_economy0`/`sim::tests`'s
    // `cromatolis_world_*_regression_against_real_lfs_assets`). Recommended:
    // `cargo test -p xindeler-world -- --ignored cromatolis` ----

    fn generate_cromatolis_world() -> WorldSim {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        WorldSim::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        )
    }

    #[test]
    #[ignore]
    fn known_settlement_projects_within_dry_buffer_radius_against_real_terrain() {
        let sim = generate_cromatolis_world();
        let settlements = real_settlements();
        let map_size = sim.map_size_lg();

        let kalthis = settlements
            .settlements
            .iter()
            .find(|settlement| settlement.id == "site.kalthis")
            .expect("the capital settlement must be present in the real export");
        let requested = kalthis.center.to_chunk_pos(map_size);
        let projected = project_authored_settlement_location(requested, &sim);

        assert!(
            authored_settlement_has_dry_buffer(projected, &sim),
            "projected settlement location must have a dry 3x3 buffer against real terrain"
        );
        let dx = (projected.x - requested.x) as f64;
        let dy = (projected.y - requested.y) as f64;
        let distance = (dx * dx + dy * dy).sqrt();
        assert!(
            distance <= 12.0 * std::f64::consts::SQRT_2,
            "projected location moved {distance} chunks from the requested pin, further than the \
             12-chunk search radius allows"
        );
    }

    #[test]
    #[ignore]
    fn civs_generate_places_every_authored_cromatolis_settlement_and_landmark() {
        let mut sim = generate_cromatolis_world();
        let mut index = crate::index::Index::new(0);
        let civs = crate::civ::Civs::generate(0, &mut sim, &mut index, None, &|_| {});

        let settlement_sites = civs.sites().filter(|site| site.authored.is_some()).count();
        let landmark_sites = civs
            .sites()
            .filter(|site| site.authored_landmark.is_some())
            .count();
        assert_eq!(settlement_sites, 62);
        assert_eq!(landmark_sites, 16);
        assert_eq!(
            civs.civs.iter().count(),
            1,
            "exactly one civilisation must be created, rooted at the authored capital"
        );
    }

    #[test]
    #[ignore]
    fn civs_generate_establishes_every_authored_cromatolis_route_as_a_real_track() {
        let mut sim = generate_cromatolis_world();
        let mut index = crate::index::Index::new(0);
        let civs = crate::civ::Civs::generate(0, &mut sim, &mut index, None, &|_| {});

        // Routes have no preview gate (unlike bridges): they're established
        // unconditionally whenever authored settlements exist, so a normal
        // `Civs::generate` run already exercises
        // `establish_authored_cromatolis_routes`.
        let sites_by_authored_id = civs
            .sites
            .iter()
            .filter_map(|(id, site)| site.authored.as_ref().map(|meta| (meta.id.as_str(), id)))
            .collect::<std::collections::HashMap<_, _>>();

        let routes = real_routes();
        for route in &routes.routes {
            let start = *sites_by_authored_id
                .get(route.start_site_id.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "route {} start settlement {} missing from generated civs",
                        route.id, route.start_site_id
                    )
                });
            let end = *sites_by_authored_id
                .get(route.end_site_id.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "route {} end settlement {} missing from generated civs",
                        route.id, route.end_site_id
                    )
                });
            assert!(
                civs.track_between(start, end).is_some(),
                "route {} did not resolve to a real Track between its authored settlements",
                route.id
            );
        }
    }

    #[test]
    #[ignore]
    fn civs_bridge_preview_places_every_real_authored_bridge() {
        let mut sim = generate_cromatolis_world();
        let mut index = crate::index::Index::new(0);
        let mut civs = crate::civ::Civs::generate(0, &mut sim, &mut index, None, &|_| {});

        // Bridges stay gated behind `XINDELER_CROMATOLIS_BRIDGE_PREVIEW` in a
        // normal run (see `cromatolis_authored_bridge_preview`'s doc
        // comment). Exercise the establishment logic directly here, the
        // same way a developer's `... =all` preview run would, instead of
        // mutating process-global env state in a test.
        let bridges = real_bridges();
        let rng = ChaChaRng::from_seed(seed_expan::rng_state(0));
        let mut ctx = GenCtx { sim: &mut sim, rng };
        civs.establish_authored_cromatolis_bridges(&mut ctx, &bridges, Some("all"));

        let placed_bridge_names = civs
            .sites
            .iter()
            .filter_map(|(_, site)| site.authored_bridge.as_ref())
            .map(|meta| meta.name.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            placed_bridge_names.len(),
            12,
            "all 12 real authored bridges must be placed as real sites"
        );
        for bridge in &bridges.bridges {
            assert!(
                placed_bridge_names.contains(bridge.name.as_str()),
                "bridge {} ({}) was not placed",
                bridge.id,
                bridge.name
            );
        }

        // The bridge endpoints stay at their authored source coordinates
        // (`establish_authored_cromatolis_bridges` never reprojects them the
        // way settlements are, and never fills the water beneath -- the
        // renderer only ever adds sparse piers/ramps, see
        // `render_grand_stone_iron`/`render_authored_low_span`).
        for bridge in &bridges.bridges {
            let expected_start = bridge.start.to_chunk_pos(sim.map_size_lg());
            let expected_end = bridge.end.to_chunk_pos(sim.map_size_lg());
            assert!(
                civs.bridges.contains_key(&expected_start)
                    && civs.bridges.contains_key(&expected_end),
                "bridge {} is not registered at its real authored source coordinates",
                bridge.id
            );
        }
    }
}
