//! Authored Cromatolis interior geometry, driven directly by
//! `interior_graphs.ron` / `interior_places.ron` rather than any generic
//! procedural cave or `Site` placement pipeline.
//!
//! ## Architecture
//!
//! A hybrid of two existing engine techniques, adapted rather than reused
//! verbatim, because neither fits this data as-is:
//! - The "named room, explicit position" pattern used elsewhere for placing a
//!   sequence of prefab rooms (see `world/src/site/plot/dwarven_mine.rs` and
//!   `render_prefab` in `world/src/site/generation.rs`) -- generalized here to
//!   read positions from RON instead of Rust literals. No bespoke prefab voxel
//!   art exists for these rooms, so "room placement" here means carving a
//!   correctly-shaped, correctly-connected cavity directly with primitives, not
//!   stamping a `PrefabStructure`.
//! - The tunnel math in `world/src/layer/cave.rs` (a quadratic spline between
//!   two points, plus a distance-based radius falloff) -- adapted to connect
//!   two **authored, fixed** points instead of two hashed procedural nodes. The
//!   underlying primitives (`common::terrain::{river_spline_coeffs,
//!   quadratic_nearest_point}`) are literally the same ones that tunnel math
//!   calls; that module's own `Tunnel` type stays untouched since it is tightly
//!   coupled to hashed-node generation and isn't a good fit to import directly.
//!
//! Like the other authored-Cromatolis-content added elsewhere in this crate
//! (`world/src/civ/mod.rs`'s `establish_authored_cromatolis_*` functions,
//! `world/src/layer/mod.rs`'s `authored_cromatolis_path_profile`), this only
//! ever activates for chunks with `SimChunk::authored_cromatolis_v0` set, so
//! it can't affect any other map or upstream civ generation.
//!
//! ## Scope
//!
//! Both authored interiors, `interior.the_undercompact` and
//! `interior.kharvun_reach`, are enabled (see [`ENABLED_INTERIOR_IDS`]). The
//! `sealed_stone_gate` connection (`the_undercompact` only -- no such
//! connection exists in `kharvun_reach`) is built as a **physical structure
//! only**: real approach corridors on both sides, then a solid stone plug at
//! the seal plane. No puzzle/interaction logic exists here; enabling that is
//! a separate, deliberately not-yet-made decision.
//!
//! `kharvun_reach`'s submerged "respiradero" surface access
//! (`access.kharvun_polder_respiradero`) has no authored surface pixel in
//! the source data (still pending upstream). This module never needed one:
//! only the interior's designated *entry* access anchors the whole layout
//! (see [`anchor_for`]), and the respiradero level still gets a real,
//! carved position from the connection-graph walk regardless -- it just
//! doesn't (yet) have a literal surface opening tied to an exact pixel.
//!
//! No NPCs, combat, loot, or boss content is placed by this module --
//! physical space only.

use crate::{
    Canvas, CanvasInfo,
    util::{FastNoise2d, sampler::Sampler},
};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    terrain::{
        Block, BlockKind, CoordinateConversions, MapSizeLg, TerrainChunkSize,
        quadratic_nearest_point, river_spline_coeffs,
    },
    vol::RectVolSize,
};
use hashbrown::HashMap;
use serde::Deserialize;
use std::{borrow::Cow, collections::VecDeque, f32::consts::TAU, sync::OnceLock};
use tracing::warn;
use vek::*;

/// Interiors this module actually carves. Extending this list is how a
/// follow-up change would enable another authored interior once its own
/// build-order decisions are settled -- the rest of the pipeline below is
/// fully data-driven and doesn't need to change.
const ENABLED_INTERIOR_IDS: &[&str] = &["interior.the_undercompact", "interior.kharvun_reach"];

const INTERIOR_GRAPHS_ASSET: &str = "world.map.cromatolis_v0_interior_graphs";
const INTERIOR_PLACES_ASSET: &str = "world.map.cromatolis_v0_interior_places";
const SITES_ASSET: &str = "world.map.cromatolis_v0_sites";

/// Same authored-pixel raster `world/src/layer/mod.rs`'s route/road code
/// uses (`CROMATOLIS_SOURCE_MAP_SIZE`); duplicated here rather than shared
/// across the module boundary, because the two call sites want independent
/// small helpers (`wpos -> pixel` there, `pixel -> wpos` here) and that
/// isn't worth coupling through a shared private constant for one f32 pair.
const CROMATOLIS_SOURCE_MAP_SIZE: Vec2<f32> = Vec2::new(2048.0, 1536.0);

/// Minimum rock cover kept between any carved interior surface and the real
/// terrain surface above it, so a shallow level can never accidentally
/// punch a hole to the sky.
const SURFACE_MARGIN: f32 = 4.0;
/// Distance (in blocks) over which a carved edge fades in, so rooms/tunnels
/// don't have a razor-sharp boundary.
const EDGE_SOFTNESS: f32 = 3.0;
/// Vertical quantization step used for terraced traversal kinds, so they
/// read as stepped terraces rather than a smooth ramp.
const TERRACE_STEP: f64 = 4.0;
/// Half-width (in `t`, 0..1 along the connection) of the solid plug built
/// for sealed connections.
const GATE_PLUG_HALF_T: f64 = 0.05;

// ---------------------------------------------------------------------
// RON data model. Every "categorical" field in the source data
// (`medium`/`generation`/`traversal`/`scale`/...) is authored as a plain
// quoted string, not a RON enum literal (confirmed by reading the source
// files directly -- e.g. `medium: "air"`, not `medium: Air`). So these are
// parsed as `String` here and classified into real Rust enums during
// validation, with a clear error on anything unrecognized, rather than
// deserialized straight into an enum (which would fail on quoted input).
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InteriorGraphsAsset {
    schema: String,
    interiors: Vec<InteriorGraph>,
}

impl FileAsset for InteriorGraphsAsset {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Debug, Deserialize)]
struct InteriorGraph {
    id: String,
    entry_level_id: String,
    surface_accesses: Vec<SurfaceAccess>,
    /// The level id an adventure's narrative expects to start the player
    /// at (e.g. an escape-from-prison opening). Not consumed by any
    /// gameplay/spawn logic here -- validated only, so that level's
    /// reachability is confirmed as this module's geometry is built.
    #[serde(default)]
    adventure_start_level_id: Option<String>,
    levels: Vec<Level>,
    connections: Vec<Connection>,
    #[serde(default)]
    water_features: Vec<WaterFeature>,
}

#[derive(Debug, Deserialize)]
struct SurfaceAccess {
    #[serde(default)]
    source_pixel: Option<PixelPos>,
    entry_level_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct PixelPos {
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize)]
struct Level {
    id: String,
    floor_z_m: i32,
    ceiling_z_m: i32,
    medium: String,
    generation: String,
}

#[derive(Debug, Deserialize)]
struct Connection {
    id: String,
    from_level_id: String,
    to_level_id: String,
    traversal: String,
    #[serde(default = "default_true")]
    bidirectional: bool,
    /// Puzzle/interaction condition attached to this connection (if any);
    /// parsed for validation coverage, not yet consumed by any interaction
    /// logic.
    #[expect(dead_code, reason = "parsed for validation coverage, not yet consumed")]
    #[serde(default)]
    condition: Option<ConnectionCondition>,
}

fn default_true() -> bool { true }

#[derive(Debug, Deserialize)]
struct ConnectionCondition {
    /// Descriptive puzzle-kind string; not consumed by any interaction
    /// logic here (this module only ever builds physical geometry).
    #[serde(default)]
    #[expect(dead_code, reason = "parsed for validation coverage, not yet consumed")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct WaterFeature {
    from_level_id: String,
    to_level_id: String,
    width_m: f32,
    #[serde(default)]
    drop_m: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct InteriorPlacesAsset {
    schema: String,
    places: Vec<InteriorPlace>,
}

impl FileAsset for InteriorPlacesAsset {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Debug, Deserialize)]
struct InteriorPlace {
    id: String,
    #[expect(dead_code, reason = "parsed for validation coverage, not yet consumed")]
    interior_id: String,
    #[expect(dead_code, reason = "parsed for validation coverage, not yet consumed")]
    level_id: String,
    scale: String,
}

/// Minimal read of the authored settlement/site list -- just enough to find
/// a named site's authored position. Loaded independently of
/// `world/src/civ/mod.rs`'s own (private) settlement struct; both are just
/// readers of the same on-disk asset, cached by `assets_manager`, so
/// there's no real duplication cost.
#[derive(Debug, Deserialize)]
struct SitesAsset {
    settlements: Vec<SiteAnchor>,
}

impl FileAsset for SitesAsset {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Debug, Deserialize)]
struct SiteAnchor {
    id: String,
    center: NormalizedPoint,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct NormalizedPoint {
    x: f32,
    y: f32,
}

impl NormalizedPoint {
    /// Normalized, top-left origin; sim `y` grows northward while authored
    /// pixels grow southward, so `y` gets flipped here.
    fn to_chunk_pos(self, map_size: MapSizeLg) -> Vec2<i32> {
        let size = map_size.chunks();
        let x = (self.x.clamp(0.0, 1.0) * f32::from(size.x.saturating_sub(1))).round() as i32;
        let y =
            ((1.0 - self.y.clamp(0.0, 1.0)) * f32::from(size.y.saturating_sub(1))).round() as i32;
        Vec2::new(x, y)
    }
}

// ---------------------------------------------------------------------
// Classified enums (see the module-level note above on why these aren't
// derived `Deserialize` impls directly).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Medium {
    Air,
    Mixed,
    Water,
    Lava,
}

impl Medium {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "air" => Ok(Self::Air),
            "mixed" => Ok(Self::Mixed),
            "water" => Ok(Self::Water),
            "lava" => Ok(Self::Lava),
            other => Err(format!("unknown level medium {other:?}")),
        }
    }

    /// Fill for one column of a *level room* at height `z` within its
    /// `[floor_z, ceiling_z]` band. `Water` fills the whole band (levels
    /// with this medium are authored as fully flooded, e.g. a submerged
    /// throat). `Lava` only fills the lower fraction of the band, leaving
    /// air above -- a lava level is authored as a hazard pool with an
    /// air-side walkway over/around it, not a room full of lava a player
    /// can't stand in.
    fn room_fill(self, floor_z: i32, ceiling_z: i32, z: i32) -> Block {
        match self {
            Self::Air | Self::Mixed => Block::empty(),
            Self::Water => Block::new(BlockKind::Water, Rgb::zero()),
            Self::Lava => {
                let band = (ceiling_z - floor_z).max(1) as f32;
                let lava_top = floor_z + (band * 0.35).round() as i32;
                if z <= lava_top {
                    Block::new(BlockKind::Lava, Rgb::new(255, 65, 0))
                } else {
                    Block::empty()
                }
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    AuthoredCoreProceduralDressing,
    AuthoredGeometry,
}

impl Generation {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "authored_core_procedural_dressing" => Ok(Self::AuthoredCoreProceduralDressing),
            "authored_geometry" => Ok(Self::AuthoredGeometry),
            other => Err(format!("unknown level generation mode {other:?}")),
        }
    }

    /// A level with no procedural substitution gets zero edge jitter --
    /// clean authored skeleton only.
    fn allows_dressing(self) -> bool { self == Self::AuthoredCoreProceduralDressing }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Traversal {
    WalkDescend,
    HiddenWalkDescend,
    TerracedWalkDescend,
    BridgeLiftAndWalkDescend,
    BridgeLiftAndTerracedWalk,
    BridgeAndShoreWalk,
    MineLiftAndTerracedWalk,
    SwimAscend,
    ShoreWalkDescend,
    ProtectedLavaSidewalk,
    SealedStoneGate,
}

impl Traversal {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "walk_descend" => Ok(Self::WalkDescend),
            "hidden_walk_descend" => Ok(Self::HiddenWalkDescend),
            "terraced_walk_descend" => Ok(Self::TerracedWalkDescend),
            "bridge_lift_and_walk_descend" => Ok(Self::BridgeLiftAndWalkDescend),
            "bridge_lift_and_terraced_walk" => Ok(Self::BridgeLiftAndTerracedWalk),
            "bridge_and_shore_walk" => Ok(Self::BridgeAndShoreWalk),
            "mine_lift_and_terraced_walk" => Ok(Self::MineLiftAndTerracedWalk),
            "swim_ascend" => Ok(Self::SwimAscend),
            "shore_walk_descend" => Ok(Self::ShoreWalkDescend),
            "protected_lava_sidewalk" => Ok(Self::ProtectedLavaSidewalk),
            "sealed_stone_gate" => Ok(Self::SealedStoneGate),
            other => Err(format!("unknown connection traversal {other:?}")),
        }
    }

    fn style(self) -> TraversalStyle {
        match self {
            Self::WalkDescend => TraversalStyle {
                radius: 7.0,
                headroom: 9.0,
                terraced: false,
                bridge_deck: false,
                slope: 2.2,
            },
            Self::HiddenWalkDescend => TraversalStyle {
                radius: 4.5,
                headroom: 7.0,
                terraced: false,
                bridge_deck: false,
                slope: 2.0,
            },
            Self::TerracedWalkDescend => TraversalStyle {
                radius: 7.5,
                headroom: 9.0,
                terraced: true,
                bridge_deck: false,
                slope: 2.4,
            },
            Self::BridgeLiftAndWalkDescend | Self::BridgeAndShoreWalk => TraversalStyle {
                radius: 16.0,
                headroom: 14.0,
                terraced: false,
                bridge_deck: true,
                slope: 1.4,
            },
            Self::BridgeLiftAndTerracedWalk => TraversalStyle {
                radius: 16.0,
                headroom: 14.0,
                terraced: true,
                bridge_deck: true,
                slope: 1.4,
            },
            Self::MineLiftAndTerracedWalk => TraversalStyle {
                radius: 8.0,
                headroom: 10.0,
                terraced: true,
                bridge_deck: false,
                slope: 3.2,
            },
            Self::SwimAscend => TraversalStyle {
                radius: 5.0,
                headroom: 8.0,
                terraced: false,
                bridge_deck: false,
                slope: 1.8,
            },
            Self::ShoreWalkDescend => TraversalStyle {
                radius: 8.0,
                headroom: 9.0,
                terraced: false,
                bridge_deck: false,
                slope: 2.0,
            },
            Self::ProtectedLavaSidewalk => TraversalStyle {
                radius: 9.0,
                headroom: 10.0,
                terraced: false,
                bridge_deck: false,
                slope: 2.0,
            },
            Self::SealedStoneGate => TraversalStyle {
                radius: 6.0,
                headroom: 9.0,
                terraced: false,
                bridge_deck: false,
                slope: 1.6,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TraversalStyle {
    radius: f32,
    headroom: f32,
    terraced: bool,
    bridge_deck: bool,
    /// Horizontal blocks synthesized per vertical metre of drop when laying
    /// out levels along this connection (see [`layout_levels`]) -- lower
    /// values make for steeper, more direct connections; higher values
    /// spread the same drop over a longer, gentler run.
    slope: f32,
}

// ---------------------------------------------------------------------
// Resolved layout: absolute positions computed once (see [`layout`]) and
// reused for every chunk column, so no RON parsing or graph-walking happens
// in the per-column hot path.
// ---------------------------------------------------------------------

struct LevelGeom {
    anchor2d: Vec2<i32>,
    floor_z: i32,
    ceiling_z: i32,
    radius: f32,
    medium: Medium,
    generation: Generation,
}

struct ConnectionSeg {
    a: Vec3<i32>,
    b: Vec3<i32>,
    a_ceiling: i32,
    b_ceiling: i32,
    curve: f32,
    style: TraversalStyle,
    sealed: bool,
}

struct WaterSeg {
    a: Vec3<i32>,
    b: Vec3<i32>,
    curve: f32,
    radius: f32,
    drop_m: Option<f32>,
}

#[derive(Default)]
struct InteriorLayout {
    levels: Vec<LevelGeom>,
    connections: Vec<ConnectionSeg>,
    water: Vec<WaterSeg>,
    /// Rough bounding circle (center, radius) covering every carved shape,
    /// used to cheaply skip chunks nowhere near this interior.
    bounds: Option<(Vec2<i32>, f32)>,
}

static LAYOUT: OnceLock<Vec<InteriorLayout>> = OnceLock::new();

fn layout(info: &CanvasInfo) -> &'static [InteriorLayout] {
    LAYOUT.get_or_init(|| build_all_layouts(info))
}

fn build_all_layouts(info: &CanvasInfo) -> Vec<InteriorLayout> {
    let graphs = match InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET) {
        Ok(graphs) => graphs,
        Err(err) => {
            warn!(?err, "Failed to load Cromatolis interior graphs");
            return Vec::new();
        },
    };
    if graphs.schema != "xindeler_open_world.interior_graphs.v1" {
        warn!(schema = %graphs.schema, "Unexpected interior_graphs schema, skipping");
        return Vec::new();
    }

    // Loaded for validation coverage even though not every named place is
    // carved yet -- `the_undercompact` currently has none of its own.
    match InteriorPlacesAsset::load_owned(INTERIOR_PLACES_ASSET) {
        Ok(places) if places.schema != "xindeler_open_world.interior_places.v1" => {
            warn!(schema = %places.schema, "Unexpected interior_places schema");
        },
        Ok(places) => {
            for place in &places.places {
                if let Err(err) = parse_place_scale(&place.scale) {
                    warn!(place_id = %place.id, %err, "Invalid interior place scale");
                }
            }
        },
        Err(err) => warn!(?err, "Failed to load Cromatolis interior places"),
    }

    let map_size = info.chunks().map_size_lg();
    let world_size =
        TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);

    graphs
        .interiors
        .iter()
        .filter(|graph| ENABLED_INTERIOR_IDS.contains(&graph.id.as_str()))
        .filter_map(|graph| match build_layout(graph, map_size, world_size) {
            Ok(layout) => Some(layout),
            Err(err) => {
                warn!(interior_id = %graph.id, %err, "Failed to build authored interior layout");
                None
            },
        })
        .collect()
}

fn parse_place_scale(s: &str) -> Result<(), String> {
    match s {
        "small" | "small_expandable" | "medium" | "major" | "regional" => Ok(()),
        other => Err(format!("unknown place scale {other:?}")),
    }
}

fn anchor_for(
    graph: &InteriorGraph,
    map_size: MapSizeLg,
    world_size: Vec2<f32>,
) -> Result<Vec2<i32>, String> {
    let entry_access = graph
        .surface_accesses
        .iter()
        .find(|access| access.entry_level_id == graph.entry_level_id)
        .ok_or_else(|| {
            format!(
                "no surface_access targets entry_level_id {}",
                graph.entry_level_id
            )
        })?;

    if let Some(pixel) = entry_access.source_pixel {
        return Ok(wpos_for_source_pixel(pixel, world_size));
    }

    // No pixel authored for this interior's entry: fall back to its parent
    // settlement's position (e.g. a district access built into an existing
    // city rather than its own map pin).
    let sites = SitesAsset::load_owned(SITES_ASSET)
        .map_err(|err| format!("failed to load {SITES_ASSET}: {err}"))?;
    let anchor_site = sites
        .settlements
        .iter()
        .find(|site| site.id == "site.cutstone_city")
        .ok_or_else(|| "site.cutstone_city not found in the authored site list".to_string())?;
    let chunk_pos = anchor_site.center.to_chunk_pos(map_size);
    let wpos = chunk_pos.cpos_to_wpos_center();
    // Offset the district doorway away from the settlement's own footprint.
    // A precise tie-in to that settlement's own site plot is separate,
    // future integration work.
    Ok(wpos + Vec2::new(96, -64))
}

fn wpos_for_source_pixel(pixel: PixelPos, world_size: Vec2<f32>) -> Vec2<i32> {
    let x = (pixel.x as f32 / (CROMATOLIS_SOURCE_MAP_SIZE.x - 1.0)) * world_size.x;
    let y = (1.0 - pixel.y as f32 / (CROMATOLIS_SOURCE_MAP_SIZE.y - 1.0)) * world_size.y;
    Vec2::new(x.round() as i32, y.round() as i32)
}

/// FNV-1a, used only to turn a stable string id into a deterministic float
/// in `[0, 1)`. Deliberately not `std`'s `DefaultHasher` (not an API
/// stability guarantee worth leaning on for world-gen determinism) and not
/// the engine's position-keyed random-field hash (there's no position yet
/// at the point this is used -- it's what produces one).
fn fnv1a_unit(s: &str) -> f32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as f32 / u32::MAX as f32
}

fn build_layout(
    graph: &InteriorGraph,
    map_size: MapSizeLg,
    world_size: Vec2<f32>,
) -> Result<InteriorLayout, String> {
    let levels_by_id: HashMap<&str, &Level> =
        graph.levels.iter().map(|l| (l.id.as_str(), l)).collect();
    if !levels_by_id.contains_key(graph.entry_level_id.as_str()) {
        return Err(format!(
            "entry_level_id {} is not one of this interior's levels",
            graph.entry_level_id
        ));
    }

    let anchor = anchor_for(graph, map_size, world_size)?;
    let positions = layout_levels(graph, &levels_by_id, anchor)?;

    if let Some(start_id) = &graph.adventure_start_level_id
        && !positions.contains_key(start_id.as_str())
    {
        return Err(format!(
            "adventure_start_level_id {start_id} is not reachable from entry_level_id"
        ));
    }

    let mut levels = Vec::with_capacity(graph.levels.len());
    for level in &graph.levels {
        let medium = Medium::parse(&level.medium)?;
        let generation = Generation::parse(&level.generation)?;
        let Some(&anchor2d) = positions.get(level.id.as_str()) else {
            // Unreachable from entry_level_id via the connection graph --
            // shouldn't happen for a well-formed authored graph, but don't
            // panic on it.
            warn!(level_id = %level.id, "Level unreachable from entry_level_id, skipping");
            continue;
        };
        let band = (level.ceiling_z_m - level.floor_z_m).max(1) as f32;
        let radius = (band * 0.35).clamp(18.0, 50.0)
            + if generation == Generation::AuthoredGeometry {
                12.0
            } else {
                0.0
            };
        levels.push(LevelGeom {
            anchor2d,
            floor_z: level.floor_z_m,
            ceiling_z: level.ceiling_z_m,
            radius,
            medium,
            generation,
        });
    }

    let mut connections = Vec::with_capacity(graph.connections.len());
    for conn in &graph.connections {
        let (Some(&from2d), Some(&to2d)) = (
            positions.get(conn.from_level_id.as_str()),
            positions.get(conn.to_level_id.as_str()),
        ) else {
            warn!(connection_id = %conn.id, "Connection references an unreachable level, skipping");
            continue;
        };
        let from = levels_by_id[conn.from_level_id.as_str()];
        let to = levels_by_id[conn.to_level_id.as_str()];
        let traversal = Traversal::parse(&conn.traversal)?;
        connections.push(ConnectionSeg {
            a: from2d.with_z(from.floor_z_m),
            b: to2d.with_z(to.floor_z_m),
            a_ceiling: from.ceiling_z_m,
            b_ceiling: to.ceiling_z_m,
            curve: (fnv1a_unit(&conn.id) - 0.5) * 0.6,
            style: traversal.style(),
            sealed: traversal == Traversal::SealedStoneGate,
        });
    }

    let mut water = Vec::with_capacity(graph.water_features.len());
    for feature in &graph.water_features {
        let (Some(&from2d), Some(&to2d)) = (
            positions.get(feature.from_level_id.as_str()),
            positions.get(feature.to_level_id.as_str()),
        ) else {
            warn!(
                from = %feature.from_level_id,
                to = %feature.to_level_id,
                "Water feature references an unreachable level, skipping"
            );
            continue;
        };
        let from = levels_by_id[feature.from_level_id.as_str()];
        let to = levels_by_id[feature.to_level_id.as_str()];
        water.push(WaterSeg {
            a: from2d.with_z(from.floor_z_m),
            b: to2d.with_z(to.floor_z_m),
            curve: (fnv1a_unit(&feature.from_level_id) - 0.5) * 0.4,
            radius: (feature.width_m / 2.0 + 1.0).max(3.0),
            drop_m: feature.drop_m,
        });
    }

    let bounds = compute_bounds(&levels, &connections, &water);

    Ok(InteriorLayout {
        levels,
        connections,
        water,
        bounds,
    })
}

fn compute_bounds(
    levels: &[LevelGeom],
    connections: &[ConnectionSeg],
    water: &[WaterSeg],
) -> Option<(Vec2<i32>, f32)> {
    let mut points: Vec<(Vec2<i32>, f32)> = Vec::new();
    for level in levels {
        points.push((level.anchor2d, level.radius));
    }
    for conn in connections {
        points.push((conn.a.xy(), conn.style.radius));
        points.push((conn.b.xy(), conn.style.radius));
    }
    for w in water {
        points.push((w.a.xy(), w.radius));
        points.push((w.b.xy(), w.radius));
    }
    if points.is_empty() {
        return None;
    }
    let sum: Vec2<i64> = points
        .iter()
        .map(|(p, _)| p.map(i64::from))
        .fold(Vec2::zero(), |a, b| a + b);
    let center = (sum / points.len() as i64).map(|e| e as i32);
    let radius = points
        .iter()
        .map(|(p, r)| p.map(|e| e as f32).distance(center.map(|e| e as f32)) + r)
        .fold(0.0_f32, f32::max);
    Some((center, radius))
}

/// Walks the (undirected, since every authored connection is bidirectional)
/// connection graph breadth-first from `entry_level_id`, assigning each
/// newly-reached level a synthetic horizontal position: its parent's
/// position offset by a deterministic direction (hashed from the
/// connecting edge's id, so it's stable across runs) and a distance derived
/// from the vertical drop between the two levels and that connection's
/// traversal slope, so steep drops naturally get a longer horizontal run
/// instead of an absurdly steep tunnel.
fn layout_levels(
    graph: &InteriorGraph,
    levels_by_id: &HashMap<&str, &Level>,
    anchor: Vec2<i32>,
) -> Result<HashMap<String, Vec2<i32>>, String> {
    let mut neighbors: HashMap<&str, Vec<(&str, &Connection)>> = HashMap::new();
    for conn in &graph.connections {
        if !levels_by_id.contains_key(conn.from_level_id.as_str())
            || !levels_by_id.contains_key(conn.to_level_id.as_str())
        {
            return Err(format!(
                "connection {} references an unknown level id",
                conn.id
            ));
        }
        neighbors
            .entry(conn.from_level_id.as_str())
            .or_default()
            .push((conn.to_level_id.as_str(), conn));
        if conn.bidirectional {
            neighbors
                .entry(conn.to_level_id.as_str())
                .or_default()
                .push((conn.from_level_id.as_str(), conn));
        }
    }

    let mut positions: HashMap<String, Vec2<i32>> = HashMap::new();
    positions.insert(graph.entry_level_id.clone(), anchor);
    let mut queue = VecDeque::new();
    queue.push_back(graph.entry_level_id.clone());

    while let Some(current_id) = queue.pop_front() {
        let Some(current_level) = levels_by_id.get(current_id.as_str()) else {
            continue;
        };
        let current_pos = positions[&current_id];
        let Some(edges) = neighbors.get(current_id.as_str()) else {
            continue;
        };
        for (next_id, conn) in edges {
            if positions.contains_key(*next_id) {
                continue;
            }
            let Some(next_level) = levels_by_id.get(*next_id) else {
                continue;
            };
            let traversal = Traversal::parse(&conn.traversal)?;
            let drop = (current_level.floor_z_m - next_level.floor_z_m).unsigned_abs() as f32;
            let step = (drop * traversal.style().slope).clamp(48.0, 420.0);
            let angle = fnv1a_unit(&conn.id) * TAU;
            let offset = Vec2::new(angle.cos(), angle.sin()) * step;
            let next_pos = current_pos + offset.map(|e| e.round() as i32);
            positions.insert((*next_id).to_string(), next_pos);
            queue.push_back((*next_id).to_string());
        }
    }

    Ok(positions)
}

// ---------------------------------------------------------------------
// Per-column carving.
// ---------------------------------------------------------------------

pub fn apply_cromatolis_interiors_to(canvas: &mut Canvas) {
    if !canvas.info().chunk().authored_cromatolis_v0 {
        return;
    }
    let interiors = layout(&canvas.info());
    if interiors.is_empty() {
        return;
    }

    // Cheap chunk-level rejection before touching any column: skip
    // interiors whose bounding circle can't possibly reach this chunk.
    let chunk_wpos = canvas.info().wpos();
    let chunk_size = TerrainChunkSize::RECT_SIZE.map(|e| e as f32);
    let chunk_center = chunk_wpos.map(|e| e as f32) + chunk_size / 2.0;
    let chunk_diag = (chunk_size.map(|e| e * e).sum()).sqrt() / 2.0;
    let relevant: Vec<&InteriorLayout> = interiors
        .iter()
        .filter(|interior| match interior.bounds {
            Some((center, radius)) => {
                chunk_center.distance(center.map(|e| e as f32)) <= radius + chunk_diag + 32.0
            },
            None => false,
        })
        .collect();
    if relevant.is_empty() {
        return;
    }

    canvas.foreach_col(|canvas, wpos2d, col| {
        let col_alt = col.alt;
        for interior in &relevant {
            for level in &interior.levels {
                carve_level_room(canvas, wpos2d, col_alt, level);
            }
            for conn in &interior.connections {
                carve_connection(canvas, wpos2d, col_alt, conn);
            }
            for water in &interior.water {
                carve_water(canvas, wpos2d, col_alt, water);
            }
            for conn in &interior.connections {
                if conn.sealed {
                    plug_sealed_gate(canvas, wpos2d, conn);
                }
            }
        }
    });
}

fn edge_weight(dist: f32, radius: f32) -> f32 { ((radius - dist) / EDGE_SOFTNESS).clamp(0.0, 1.0) }

fn carve_level_room(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, level: &LevelGeom) {
    let dist = wpos2d
        .map(|e| e as f32)
        .distance(level.anchor2d.map(|e| e as f32));

    let mut radius = level.radius;
    if level.generation.allows_dressing() {
        // Additive-only jitter: never shrinks the guaranteed authored
        // skeleton, only adds an irregular fringe on top of it.
        let jitter = FastNoise2d::new(9001)
            .get(wpos2d.map(|e| e as f64 / 48.0))
            .max(0.0);
        radius += jitter * 10.0;
    }

    if edge_weight(dist, radius) <= 0.0 {
        return;
    }

    let ceiling_cap = (col_alt - SURFACE_MARGIN).floor() as i32;
    let ceiling_z = level.ceiling_z.min(ceiling_cap);
    if ceiling_z <= level.floor_z {
        return;
    }

    for z in level.floor_z..=ceiling_z {
        canvas.set(
            wpos2d.with_z(z),
            level.medium.room_fill(level.floor_z, level.ceiling_z, z),
        );
    }
}

/// Shared spline sample used by connections and water features: a
/// quadratic spline between two fixed points, returning `t` (0 at `a2`, 1
/// at `b2`) and the perpendicular distance from the queried point to the
/// curve.
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

fn carve_connection(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, seg: &ConnectionSeg) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let Some((t, dist)) = spline_sample(a2, b2, seg.curve, wpos2d.map(|e| e as f64 + 0.5)) else {
        return;
    };
    let radius = seg.style.radius as f64;
    let weight = edge_weight(dist as f32, seg.style.radius);
    if weight <= 0.0 {
        return;
    }

    let mut floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t);
    if seg.style.terraced {
        floor_z = (floor_z / TERRACE_STEP).round() * TERRACE_STEP;
    }
    let ceiling_lerp =
        Lerp::lerp_unclamped(seg.a_ceiling as f64, seg.b_ceiling as f64, t).max(floor_z + 4.0);
    let ceiling_cap = (col_alt - SURFACE_MARGIN) as f64;
    let ceiling_z = (floor_z + seg.style.headroom as f64)
        .min(ceiling_lerp)
        .min(ceiling_cap);
    if ceiling_z <= floor_z {
        return;
    }

    // Connections always carve a dry/air passage: a traversal path is a
    // route a player walks (or swims a short stretch of, per its
    // `traversal` kind), never a room-sized hazard fill -- any actual
    // liquid along the way comes from a `WaterSeg` layered independently
    // over the same span, and a lava-medium level's hazard pool stays
    // confined to that level's own room (see `Medium::room_fill`), never
    // spilling into the dry connections in and out of it.
    for z in floor_z.floor() as i32..=ceiling_z.ceil() as i32 {
        canvas.set(wpos2d.with_z(z), Block::empty());
    }

    if seg.style.bridge_deck && dist <= radius * 0.35 {
        let deck_z = ((floor_z + ceiling_z) * 0.5) as i32;
        let deck = Block::new(BlockKind::Rock, Rgb::new(90, 82, 78));
        canvas.set(wpos2d.with_z(deck_z), deck);
        canvas.set(wpos2d.with_z(deck_z - 1), deck);
    }
}

/// Fills a solid stone plug across the full cross-section of `seg` at its
/// midpoint, guaranteeing a sealed connection stays physically blocked
/// regardless of any carving `carve_connection` already did there. No
/// puzzle/interaction logic.
fn plug_sealed_gate(canvas: &mut Canvas, wpos2d: Vec2<i32>, seg: &ConnectionSeg) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let Some((t, dist)) = spline_sample(a2, b2, seg.curve, wpos2d.map(|e| e as f64 + 0.5)) else {
        return;
    };
    if (t - 0.5).abs() > GATE_PLUG_HALF_T {
        return;
    }
    if dist > (seg.style.radius as f64) + 1.0 {
        return;
    }

    let floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t) as i32 - 1;
    let ceiling_z = Lerp::lerp_unclamped(seg.a_ceiling as f64, seg.b_ceiling as f64, t) as i32 + 1;
    let plug = Block::new(BlockKind::Rock, Rgb::new(60, 55, 60));
    for z in floor_z..=ceiling_z {
        canvas.set(wpos2d.with_z(z), plug);
    }
}

fn carve_water(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, seg: &WaterSeg) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let point = wpos2d.map(|e| e as f64 + 0.5);

    if let Some((t, dist)) = spline_sample(a2, b2, seg.curve, point)
        && edge_weight(dist as f32, seg.radius) > 0.0
    {
        let surface_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t);
        let depth = (seg.radius * 0.6).clamp(3.0, 10.0) as f64;
        let cap = (col_alt - SURFACE_MARGIN) as f64;
        let top = surface_z.min(cap);
        let bottom = top - depth;
        if top > bottom {
            let fill = Block::new(BlockKind::Water, Rgb::zero());
            for z in bottom.floor() as i32..=top.ceil() as i32 {
                canvas.set(wpos2d.with_z(z), fill);
            }
        }
    }

    // Waterfall: a narrow vertical water column dropping `drop_m` blocks at
    // the downstream endpoint.
    if let Some(drop_m) = seg.drop_m {
        let dist_to_b = wpos2d
            .map(|e| e as f32)
            .distance(seg.b.xy().map(|e| e as f32));
        let fall_radius = (seg.radius * 0.5).max(2.0);
        if dist_to_b <= fall_radius {
            let fill = Block::new(BlockKind::Water, Rgb::zero());
            let top = (seg.b.z as f32 + drop_m).min(col_alt - SURFACE_MARGIN) as i32;
            for z in seg.b.z..=top {
                canvas.set(wpos2d.with_z(z), fill);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::vol::ReadVol;

    fn sample_graph() -> InteriorGraph {
        InteriorGraph {
            id: "interior.test".to_string(),
            entry_level_id: "level.a".to_string(),
            surface_accesses: vec![SurfaceAccess {
                source_pixel: None,
                entry_level_id: "level.a".to_string(),
            }],
            adventure_start_level_id: Some("level.gated".to_string()),
            levels: vec![
                Level {
                    id: "level.a".to_string(),
                    floor_z_m: 100,
                    ceiling_z_m: 140,
                    medium: "air".to_string(),
                    generation: "authored_core_procedural_dressing".to_string(),
                },
                Level {
                    id: "level.b".to_string(),
                    floor_z_m: 40,
                    ceiling_z_m: 80,
                    medium: "mixed".to_string(),
                    generation: "authored_geometry".to_string(),
                },
                Level {
                    id: "level.gated".to_string(),
                    floor_z_m: 0,
                    ceiling_z_m: 30,
                    medium: "air".to_string(),
                    generation: "authored_geometry".to_string(),
                },
            ],
            connections: vec![
                Connection {
                    id: "connection.a_b".to_string(),
                    from_level_id: "level.a".to_string(),
                    to_level_id: "level.b".to_string(),
                    traversal: "walk_descend".to_string(),
                    bidirectional: true,
                    condition: None,
                },
                Connection {
                    id: "connection.b_gated".to_string(),
                    from_level_id: "level.b".to_string(),
                    to_level_id: "level.gated".to_string(),
                    traversal: "sealed_stone_gate".to_string(),
                    bidirectional: true,
                    condition: Some(ConnectionCondition {
                        kind: "two_hidden_mechanisms".to_string(),
                    }),
                },
            ],
            water_features: vec![WaterFeature {
                from_level_id: "level.a".to_string(),
                to_level_id: "level.b".to_string(),
                width_m: 4.0,
                drop_m: Some(6.0),
            }],
        }
    }

    #[test]
    fn medium_and_generation_parse_known_values() {
        assert_eq!(Medium::parse("air").unwrap(), Medium::Air);
        assert_eq!(Medium::parse("mixed").unwrap(), Medium::Mixed);
        assert_eq!(Medium::parse("water").unwrap(), Medium::Water);
        assert_eq!(Medium::parse("lava").unwrap(), Medium::Lava);
        assert!(Medium::parse("gas").is_err());

        assert_eq!(
            Generation::parse("authored_geometry").unwrap(),
            Generation::AuthoredGeometry
        );
        assert!(Generation::parse("bogus").is_err());
    }

    #[test]
    fn only_procedural_dressing_generation_allows_edge_jitter() {
        assert!(Generation::AuthoredCoreProceduralDressing.allows_dressing());
        assert!(!Generation::AuthoredGeometry.allows_dressing());
    }

    #[test]
    fn traversal_parses_every_kind_seen_in_real_data() {
        for kind in [
            "walk_descend",
            "hidden_walk_descend",
            "terraced_walk_descend",
            "bridge_lift_and_walk_descend",
            "bridge_lift_and_terraced_walk",
            "bridge_and_shore_walk",
            "mine_lift_and_terraced_walk",
            "swim_ascend",
            "shore_walk_descend",
            "protected_lava_sidewalk",
            "sealed_stone_gate",
        ] {
            assert!(
                Traversal::parse(kind).is_ok(),
                "traversal kind {kind} failed to parse"
            );
        }
    }

    #[test]
    fn layout_levels_reaches_every_level_from_entry() {
        let graph = sample_graph();
        let levels_by_id: HashMap<&str, &Level> =
            graph.levels.iter().map(|l| (l.id.as_str(), l)).collect();
        let positions = layout_levels(&graph, &levels_by_id, Vec2::new(1000, 1000)).unwrap();
        assert_eq!(positions.len(), graph.levels.len());
        for level in &graph.levels {
            assert!(positions.contains_key(&level.id));
        }
        // Entry stays exactly at the anchor.
        assert_eq!(positions["level.a"], Vec2::new(1000, 1000));
        // Every other level is offset (not stacked on the anchor).
        assert_ne!(positions["level.b"], positions["level.a"]);
        assert_ne!(positions["level.gated"], positions["level.b"]);
    }

    #[test]
    fn sealed_connection_is_flagged_sealed_and_others_are_not() {
        let graph = sample_graph();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(&graph, map_size, world_size).unwrap();
        assert_eq!(layout.levels.len(), 3);
        assert_eq!(layout.connections.len(), 2);

        let sealed_count = layout.connections.iter().filter(|c| c.sealed).count();
        assert_eq!(
            sealed_count, 1,
            "exactly the sealed connection should be flagged"
        );

        let open_count = layout.connections.iter().filter(|c| !c.sealed).count();
        assert_eq!(open_count, 1);
    }

    #[test]
    fn plug_sealed_gate_blocks_the_midpoint_column() {
        let graph = sample_graph();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(&graph, map_size, world_size).unwrap();
        let sealed = layout.connections.iter().find(|c| c.sealed).unwrap();

        let a2 = sealed.a.xy().map(|e| e as f64 + 0.5);
        let b2 = sealed.b.xy().map(|e| e as f64 + 0.5);
        let ctrl_offset = ((b2 - a2) * 0.5
            + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * sealed.curve as f64)
            .map(|e| e as f32);
        let spline = river_spline_coeffs(a2, ctrl_offset, b2);
        // Evaluate the actual curve (not the straight a-b line) at t=0.5.
        let midpoint = spline.x * 0.25 + spline.y * 0.5 + spline.z;

        let (t, dist) = spline_sample(a2, b2, sealed.curve, midpoint).unwrap();
        assert!(
            (t - 0.5).abs() < 0.15,
            "midpoint sample should land near t=0.5, got {t}"
        );
        assert!(dist < sealed.style.radius as f64 + 1.0);
    }

    #[test]
    fn bounds_cover_every_level_and_connection() {
        let graph = sample_graph();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(&graph, map_size, world_size).unwrap();
        let (center, radius) = layout.bounds.expect("non-empty layout has bounds");
        for level in &layout.levels {
            assert!(
                level
                    .anchor2d
                    .map(|e| e as f32)
                    .distance(center.map(|e| e as f32))
                    <= radius + 1.0
            );
        }
    }

    #[test]
    fn build_layout_errors_on_unknown_medium() {
        let mut graph = sample_graph();
        graph.levels[0].medium = "nitrogen".to_string();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        assert!(build_layout(&graph, map_size, world_size).is_err());
    }

    #[test]
    fn enabled_interior_ids_contains_both_authored_interiors() {
        assert_eq!(ENABLED_INTERIOR_IDS, &[
            "interior.the_undercompact",
            "interior.kharvun_reach"
        ]);
    }

    /// Loads the real authored asset (not a hand-written fixture) and
    /// builds a full layout for every interior it defines -- both the
    /// enabled one and the not-yet-enabled one -- so a real data error
    /// (an unknown traversal/medium string, an unreachable level, a
    /// dangling connection endpoint) surfaces as a test failure. Requires
    /// `VELOREN_ASSETS` to point at the repo's `assets/` directory.
    #[test]
    fn real_interior_graphs_asset_builds_a_layout_for_every_interior() {
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET)
            .expect("assets/world/map/cromatolis_v0_interior_graphs.ron should load and parse");
        assert_eq!(graphs.schema, "xindeler_open_world.interior_graphs.v1");
        assert_eq!(
            graphs.interiors.len(),
            2,
            "expected exactly the two authored interiors"
        );

        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);

        for graph in &graphs.interiors {
            let layout = build_layout(graph, map_size, world_size)
                .unwrap_or_else(|err| panic!("interior {} failed to build: {err}", graph.id));
            assert_eq!(
                layout.levels.len(),
                graph.levels.len(),
                "every level in {} should resolve to a position",
                graph.id
            );
            assert!(layout.bounds.is_some());
        }
    }

    #[test]
    fn real_undercompact_layout_has_exactly_one_sealed_connection() {
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let graph = graphs
            .interiors
            .iter()
            .find(|g| g.id == "interior.the_undercompact")
            .expect("interior.the_undercompact should be present in the authored data");

        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(graph, map_size, world_size).unwrap();

        assert_eq!(
            layout.levels.len(),
            8,
            "the_undercompact has 8 authored levels"
        );
        let sealed: Vec<_> = layout.connections.iter().filter(|c| c.sealed).collect();
        assert_eq!(
            sealed.len(),
            1,
            "exactly the deep-compact gate should be sealed"
        );

        let authored_geometry_count = layout
            .levels
            .iter()
            .filter(|l| l.generation == Generation::AuthoredGeometry)
            .count();
        assert_eq!(
            authored_geometry_count, 2,
            "the sealed gate room and the finale are the only authored_geometry levels"
        );
    }

    #[test]
    fn real_kharvun_reach_layout_reaches_every_level_including_the_adventure_start() {
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let graph = graphs
            .interiors
            .iter()
            .find(|g| g.id == "interior.kharvun_reach")
            .expect("interior.kharvun_reach should be present in the authored data");

        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(graph, map_size, world_size).unwrap();

        assert_eq!(
            layout.levels.len(),
            13,
            "kharvun_reach has 13 authored levels"
        );
        assert_eq!(
            graph.connections.len(),
            12,
            "kharvun_reach has 12 authored connections in the real data (the design docs say 11 \
             -- data is source of truth)"
        );
        assert_eq!(
            layout.connections.len(),
            12,
            "every connection should resolve (none reference an unreachable level)"
        );
        assert_eq!(
            layout.connections.iter().filter(|c| c.sealed).count(),
            0,
            "kharvun_reach has no sealed_stone_gate connection"
        );

        // The submerged "respiradero" access has no authored surface pixel
        // (still pending upstream) -- confirm that level still resolves to
        // a real position via the connection-graph walk regardless.
        let levels_by_id: HashMap<&str, &Level> =
            graph.levels.iter().map(|l| (l.id.as_str(), l)).collect();
        let positions = layout_levels(graph, &levels_by_id, Vec2::new(2000, 2000)).unwrap();
        assert!(
            positions.contains_key("level.kharvun_polder_respiradero"),
            "the respiradero level should still resolve to a position despite its surface access \
             having no authored pixel"
        );

        // "Out of the Abyss" will reference the adventure start by this id.
        assert!(
            positions.contains_key("level.kharvun_prison_depths"),
            "the adventure-start level must be reachable from entry_level_id"
        );
        assert_eq!(
            graph.adventure_start_level_id.as_deref(),
            Some("level.kharvun_prison_depths")
        );
    }

    #[test]
    fn real_interior_places_asset_parses_and_every_scale_is_known() {
        let places = InteriorPlacesAsset::load_owned(INTERIOR_PLACES_ASSET)
            .expect("assets/world/map/cromatolis_v0_interior_places.ron should load and parse");
        assert_eq!(places.schema, "xindeler_open_world.interior_places.v1");
        assert_eq!(
            places.places.len(),
            8,
            "expected all 8 authored named places"
        );
        for place in &places.places {
            parse_place_scale(&place.scale)
                .unwrap_or_else(|err| panic!("place {}: {err}", place.id));
        }
    }

    /// Full-world smoke test: generates the real Cromatolis map and real
    /// chunks at the Undercompact's entry level and Kharvun Reach's
    /// adventure-start level (the latter reached only after walking the
    /// full 13-level connection chain, so this also confirms the layout
    /// stays within real map bounds), then confirms at least one block
    /// inside each level's authored z-band came back non-solid. Requires
    /// the real Cromatolis LFS assets pulled locally (`git lfs pull`
    /// against the VPS store), matching this crate's existing precedent
    /// for heavy real-terrain tests. Recommended command:
    /// `cargo test -p xindeler-world real_world_chunk_generation -- --ignored`
    #[test]
    #[ignore]
    fn real_world_chunk_generation_carves_both_interiors_entry_levels_without_panicking() {
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

        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let map_size = world.sim().map_size_lg();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);

        let assert_level_carved = |interior_id: &str, level_id: &str| {
            let graph = graphs
                .interiors
                .iter()
                .find(|g| g.id == interior_id)
                .unwrap();
            let level = graph.levels.iter().find(|l| l.id == level_id).unwrap();
            let layout = build_layout(graph, map_size, world_size).unwrap();
            let level_geom = layout
                .levels
                .iter()
                .zip(&graph.levels)
                .find(|(_, l)| l.id == level_id)
                .map(|(geom, _)| geom)
                .expect("level should have resolved a position");
            let anchor = level_geom.anchor2d;

            let chunk_pos = anchor.wpos_to_cpos();
            let chunk_wpos2d = chunk_pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
            let local = anchor - chunk_wpos2d;

            let (chunk, _supplement) = world
                .generate_chunk(index_ref, chunk_pos, None, || false, None)
                .expect("chunk generation must not fail for a real, in-bounds Cromatolis chunk");

            let carved_any = (level.floor_z_m..=level.ceiling_z_m).any(|z| {
                chunk
                    .get(Vec3::new(local.x, local.y, z))
                    .is_ok_and(|block| !block.is_filled())
            });
            assert!(
                carved_any,
                "expected at least one carved (non-solid) block inside {level_id}'s authored \
                 z-band near its resolved anchor"
            );
        };

        // The Undercompact's entry level (also its BFS anchor).
        assert_level_carved("interior.the_undercompact", "level.thurnak_gate_market");
        // Kharvun Reach's adventure-start level -- deep, far from the
        // surface anchor via many chained connection offsets, so this also
        // confirms the layout stays in-bounds for a real, large map.
        assert_level_carved("interior.kharvun_reach", "level.kharvun_prison_depths");
    }
}
