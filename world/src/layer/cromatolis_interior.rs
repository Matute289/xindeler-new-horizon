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
    Canvas, IndexRef,
    layer::authored_voids::{
        CapsuleShape, CapsuleSpan, DiscShape, EDGE_SOFTNESS, ProceduralContact, SURFACE_MARGIN,
        chunk_query_rect, dist_to_rect, spline_reaches_rect,
    },
    sim::WorldSim,
    util::{FastNoise2d, sampler::Sampler},
};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    terrain::{
        Block, BlockKind, CoordinateConversions, MapSizeLg, SpriteKind, TerrainChunkSize,
        quadratic_nearest_point, river_spline_coeffs,
    },
    vol::RectVolSize,
};
use hashbrown::HashMap;
use serde::Deserialize;
use std::{borrow::Cow, collections::VecDeque, f32::consts::TAU};
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

// `SURFACE_MARGIN` (minimum rock cover kept between any carved interior
// surface and the real terrain surface above it, so a shallow level can never
// punch a hole to the sky) and `EDGE_SOFTNESS` (the distance over which a
// carved edge fades in) are imported from `authored_voids` rather than
// declared here: the authored-void protection index is defined as the volume
// these carves produce, so the two must never drift apart.
/// Upper bound on how far `carve_level_room`'s procedural-dressing edge
/// jitter can ever push a room's radius outward. Used both to cheaply
/// reject a column before paying for the noise sample, and to size the
/// chunk-corner pruning check in [`level_touches_chunk`] so it never
/// under-counts a jittered room's true reach.
const MAX_ROOM_JITTER: f32 = 10.0;
/// Vertical quantization step used for terraced traversal kinds, so they
/// read as stepped terraces rather than a smooth ramp.
const TERRACE_STEP: f64 = 4.0;
/// Half-width (in `t`, 0..1 along the connection) of the solid plug built
/// for sealed connections.
const GATE_PLUG_HALF_T: f64 = 0.05;

// ---------------------------------------------------------------------
// COW-7b: the Undercompact gate's two-lever puzzle. Only the antechamber's
// physical geometry (its room, and the sealed connection's plug) is this
// module's concern -- world-gen places the two lever sprites (this module),
// and [`undercompact_gate_antechamber_world_geometry`] lets server-side
// runtime code (lever activation, restart recovery) resolve the same
// positions fresh, without hardcoding coordinates. No puzzle *state* lives
// here; that is `rtsim::data::undercompact_gate`.
// ---------------------------------------------------------------------

/// The authored id of the antechamber level added between
/// `level.thurnak_lower_mines` and `level.undercompact_threshold`.
const GATE_ANTECHAMBER_LEVEL_ID: &str = "level.undercompact_gate_antechamber";
/// The authored id of the sealed connection carrying the gate's plug --
/// unchanged by COW-7b, only its `from_level_id` moved to the antechamber.
const GATE_SEALED_CONNECTION_ID: &str = "connection.deep_compact_gate";
const UNDERCOMPACT_INTERIOR_ID: &str = "interior.the_undercompact";

/// Fraction of the antechamber room's own radius the two levers are offset
/// from its center, flanking opposite walls.
const GATE_LEVER_OFFSET_FRAC: f32 = 0.6;

/// The two lever world positions inside the antechamber room, derived
/// purely from the room's own resolved geometry -- never a hardcoded
/// coordinate. Called identically by world-gen (to place the sprites, see
/// `carve_gate_levers`) and by the public runtime accessor below (to
/// recognize which lever an interaction hit), so the two can never
/// disagree.
fn antechamber_lever_positions(center2d: Vec2<i32>, radius: f32, floor_z: i32) -> [Vec3<i32>; 2] {
    let offset = (radius * GATE_LEVER_OFFSET_FRAC).round() as i32;
    [
        Vec3::new(center2d.x - offset, center2d.y, floor_z),
        Vec3::new(center2d.x + offset, center2d.y, floor_z),
    ]
}

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
    /// The authored site id ([`SiteAnchor::id`]) this interior's entry is
    /// physically built into or under (e.g. a city district's access) --
    /// used by [`anchor_for`] as the fallback anchor when the entry's own
    /// `surface_access` carries no authored pixel of its own.
    parent_surface_site_id: String,
    entry_level_id: String,
    surface_accesses: Vec<SurfaceAccess>,
    /// The level id an adventure's narrative expects to start the player
    /// at (e.g. an escape-from-prison opening). Not consumed by any
    /// gameplay/spawn logic here -- validated only, so that level's
    /// reachability is confirmed as this module's geometry is built.
    #[serde(default)]
    adventure_start_level_id: Option<String>,
    /// Opt-in escape-compass configuration. Absent (the default) means this
    /// interior has no navigation graph built for it at all.
    #[serde(default)]
    escape_guidance: Option<EscapeGuidanceCfg>,
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
    /// A capability the player must have to use this access at all (e.g.
    /// `"underwater_breathing_or_short_dive"` on a submerged one). Escape
    /// guidance excludes a gated access from its exit set unless the
    /// interior sets [`EscapeGuidanceCfg::allow_gated_exits`].
    #[serde(default)]
    required_capability: Option<String>,
}

/// How, and whether, escape guidance activates inside one interior.
///
/// `activation_kind` follows this module's standing convention for every
/// categorical field (see the RON data-model comment above): a plain quoted
/// string classified by an explicit [`EscapeActivation::parse`] that errors
/// on anything unrecognized, **not** a RON enum literal. The flag list is a
/// separate field for the same reason -- it keeps the authored form a plain
/// string plus a plain list, with no enum-variant syntax anywhere.
#[derive(Debug, Deserialize)]
struct EscapeGuidanceCfg {
    /// `"manual"` | `"always"` | `"narrative_flags"`.
    activation_kind: String,
    /// Only meaningful for `"narrative_flags"`: the narrative variable ids
    /// that activate guidance while any one of them is non-zero.
    #[serde(default)]
    activation_flags: Vec<String>,
    /// Override the exit set with an explicit list of level ids. Default
    /// (`None`) = every level named by a `surface_accesses` entry, subject
    /// to `allow_gated_exits`.
    #[serde(default)]
    exits: Option<Vec<String>>,
    /// Whether guidance may use a route that demands something of the
    /// player: a `surface_access` carrying a `required_capability`, or a
    /// `connection` carrying a `condition`.
    ///
    /// Defaults to `false`. Gating only the exit would be a half-measure --
    /// the capability an exit demands is typically demanded again by the
    /// connection leading to it, so excluding the exit while still routing
    /// through the gated tunnel just moves the hazard one edge inward.
    #[serde(default)]
    allow_gated_routes: bool,
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
    /// Puzzle/interaction condition attached to this connection (if any).
    /// Its *presence* marks the connection as one that demands something of
    /// the player, which routing honours; the condition's own `kind` is
    /// still not interpreted by any interaction logic here.
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
    /// The authored level id (e.g. `level.undercompact_gate_antechamber`).
    /// Only consulted by carve-time code that needs to recognize one
    /// specific, named authored room (see [`GATE_ANTECHAMBER_LEVEL_ID`]) --
    /// every other level is carved generically and never inspects this.
    id: String,
    anchor2d: Vec2<i32>,
    floor_z: i32,
    ceiling_z: i32,
    radius: f32,
    medium: Medium,
    generation: Generation,
}

#[derive(Clone)]
struct ConnectionSeg {
    /// The authored connection id (e.g. `connection.deep_compact_gate`).
    /// Only consulted by the COW-7b public geometry accessor below, which
    /// needs to find this one specific sealed connection by name.
    id: String,
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

/// A fully-resolved, ready-to-carve interior. Computed once per world (see
/// `Index::cromatolis_interiors`, which lazily builds and caches this the
/// first time a chunk needs it), not once per process -- a `static` here
/// would silently keep reusing the first world's layout if a binary ever
/// called `World::generate` more than once in the same process (a batch
/// export/preview tool, a multi-world test harness), which is not an
/// invariant this module wants to rely on.
#[derive(Default)]
pub(crate) struct InteriorLayout {
    /// The authored interior id (e.g. `interior.the_undercompact`). Lets a
    /// consumer of the cached `Vec<InteriorLayout>` (see
    /// `Index::cromatolis_interiors`) pick out one specific interior without
    /// re-deriving it from the raw RON graph.
    id: String,
    levels: Vec<LevelGeom>,
    connections: Vec<ConnectionSeg>,
    water: Vec<WaterSeg>,
    /// Rough bounding circle (center, radius) covering every carved shape,
    /// used to cheaply skip chunks nowhere near this interior.
    bounds: Option<(Vec2<i32>, f32)>,
    /// Read-only navigation view of this same geometry, built only when the
    /// interior authored an `escape_guidance` block. `None` for every
    /// interior that did not opt in, which is why no cost is paid for one.
    pub(crate) nav: Option<InteriorNavGraph>,
}

/// The procedural-contact policy every shape an authored interior contributes
/// carries, hard-coded rather than authored.
///
/// The mechanism is present for interiors; the *authored field* deliberately is
/// not. Interiors already solve what `Connect` solves -- they author real
/// surface accesses, with optional capability gating, so they never need a
/// procedural tunnel to be reachable -- and both would choose `Seal` anyway
/// (the one room measured as penetrated by a procedural tunnel is a built
/// market, and a hash-placed hole in its wall is not a design anyone would
/// accept). Adding an unused field to a schema with a strict validator, for two
/// features and zero demand, is speculative generality.
///
/// **It is also load-bearing, not merely a default.** Both shape accessors
/// below deliberately *over*-approximate what the carve produces, which is only
/// sound for a shape that gets dilated. Promoting this to an authored field
/// means revisiting both of them in the same change, not just adding a
/// `#[serde(default)]`.
const INTERIOR_PROCEDURAL_CONTACT: ProceduralContact = ProceduralContact::Seal;

impl InteriorLayout {
    /// The authored interior id (e.g. `interior.the_undercompact`).
    pub(crate) fn id(&self) -> &str { &self.id }

    /// This interior's rooms as protection shapes for the authored-void index,
    /// paired with their policy, mirroring what [`carve_level_room`] carves.
    ///
    /// The radius includes [`MAX_ROOM_JITTER`] for every room the authored
    /// generation kind lets the carve dress, because that jitter is
    /// additive-only and the dressed fringe is real carved space -- the same
    /// reason [`level_touches_chunk`] widens by it before pruning. It does
    /// *not* subtract the carve's own edge slack, so the reported radius is an
    /// upper bound: sound only under [`INTERIOR_PROCEDURAL_CONTACT`].
    pub(crate) fn void_discs(&self) -> impl Iterator<Item = (DiscShape, ProceduralContact)> + '_ {
        self.levels.iter().map(|level| {
            (
                DiscShape {
                    centre: level.anchor2d,
                    radius: level.radius
                        + if level.generation.allows_dressing() {
                            MAX_ROOM_JITTER
                        } else {
                            0.0
                        },
                    floor_z: level.floor_z,
                    ceiling_z: level.ceiling_z,
                },
                INTERIOR_PROCEDURAL_CONTACT,
            )
        })
    }

    /// This interior's connections and water features as protection shapes,
    /// paired with their policy, mirroring what [`carve_connection`] and
    /// [`carve_water`] carve -- including each segment's `curve`, so the
    /// protected volume follows the same bowed spline rather than the straight
    /// chord between its endpoints, and each one's vertical anchor, so a water
    /// body's whole band drops with the surface cap the way [`carve_water`]
    /// drops it.
    ///
    /// Three approximations, all sound only under
    /// [`INTERIOR_PROCEDURAL_CONTACT`], because a dilated shape has 19 blocks
    /// of slack to absorb them while an undilated one would have to match
    /// exactly:
    ///
    /// * a connection's ceiling is reported as `floor + headroom`, without the
    ///   per-column lerped authored ceiling that can lower it further (an
    ///   *over*-approximation);
    /// * a terraced connection's floor quantization is not reproduced, and
    ///   `.round()` can put the carved floor up to half a terrace step *below*
    ///   the un-terraced floor reported here (an *under*-approximation, the
    ///   only one, and well inside the margin).
    ///
    /// The narrow waterfall column a water feature can also carve is **not**
    /// here -- it is its own disc, see [`Self::void_waterfall_discs`].
    pub(crate) fn void_capsules(
        &self,
    ) -> impl Iterator<Item = (CapsuleShape, ProceduralContact)> + '_ {
        let connections = self.connections.iter().map(|seg| CapsuleShape {
            a: seg.a,
            b: seg.b,
            r_a: seg.style.radius,
            r_b: seg.style.radius,
            curve: seg.curve,
            span: CapsuleSpan::AboveFloor {
                headroom: seg.style.headroom,
            },
        });
        let water = self.water.iter().map(|seg| CapsuleShape {
            a: seg.a,
            b: seg.b,
            r_a: seg.radius,
            r_b: seg.radius,
            curve: seg.curve,
            span: CapsuleSpan::BelowSurface {
                depth: water_depth(seg.radius),
            },
        });
        connections
            .chain(water)
            .map(|capsule| (capsule, INTERIOR_PROCEDURAL_CONTACT))
    }

    /// The narrow vertical water columns [`carve_water`] raises at the
    /// downstream end of a water feature that authored a `drop_m`, as
    /// protection shapes paired with their policy.
    ///
    /// A waterfall is *not* part of its segment's capsule: the capsule follows
    /// the water surface along the segment, and the fall is a separate column
    /// standing at one endpoint and rising `drop_m` above it. A tall one
    /// therefore reaches well outside the capsule it belongs to, and left
    /// unindexed it would be the one authored volume a procedural tunnel could
    /// cross without the guard noticing.
    ///
    /// It maps exactly onto a [`DiscShape`], because the carve is a plain
    /// cylinder: the same radius test, floor at the segment's endpoint `z`,
    /// ceiling `drop_m` above it, and the same surface cap the disc already
    /// applies. Two boundary details differ from the carve by less than a
    /// block -- the carve includes its exact radius and truncates the capped
    /// ceiling toward zero where the disc floors it -- both of which the
    /// 19-block seal dilation absorbs many times over, and interior shapes are
    /// always sealed.
    pub(crate) fn void_waterfall_discs(
        &self,
    ) -> impl Iterator<Item = (DiscShape, ProceduralContact)> + '_ {
        self.water.iter().filter_map(|seg| {
            let drop_m = seg.drop_m?;
            Some((
                DiscShape {
                    centre: seg.b.xy(),
                    radius: waterfall_radius(seg.radius),
                    floor_z: seg.b.z,
                    ceiling_z: seg.b.z + drop_m.ceil() as i32,
                },
                INTERIOR_PROCEDURAL_CONTACT,
            ))
        })
    }
}

// ---------------------------------------------------------------------
// Navigation view.
//
// A read-only projection of an already-resolved `InteriorLayout` into a
// small node/edge graph, plus a precomputed "cost to the nearest exit"
// label per node. Built at most once per world alongside the layout it
// projects, and thereafter read (never rebuilt) by low-cadence server code
// that needs to answer "which way is out from here" for one position.
//
// Nothing here is authored: every position comes from the same anchors and
// connection splines world-gen already carved, so the graph can never
// describe a route the geometry does not actually contain.
// ---------------------------------------------------------------------

/// Upper bound on nodes in a single interior's navigation graph. Locating a
/// position in the graph is a linear scan over nodes, so this bounds that
/// scan's cost; it is validated at load so no interior can silently grow
/// past it.
const MAX_NAV_NODES: usize = 256;

/// `NavNode::hops_to_exit` is a `u8`, and the longest possible route
/// through `MAX_NAV_NODES` rooms has one fewer hop than there are rooms, so
/// raising the node cap past this point requires widening that field too.
const _: () = assert!(MAX_NAV_NODES - 1 <= u8::MAX as usize);

/// Extra multiplier applied to a connection's length when it is traversed
/// *upward*, so a long climb is not treated as equivalent to the same
/// distance of level walking. Scaled by the connection's own authored
/// traversal style (a gentler `slope` spreads the same drop over a longer,
/// easier run), never a bare per-connection constant.
const CLIMB_PENALTY: f32 = 1.35;

/// What decides whether escape guidance is active for a given player inside
/// one interior. Classified from [`EscapeGuidanceCfg::activation_kind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EscapeActivation {
    /// Never active on its own; only an explicit per-player grant turns it
    /// on. The default for an interior that wants guidance to exist but be
    /// driven entirely by gameplay code.
    Manual,
    /// Active for anyone inside the interior.
    Always,
    /// Active while any one of these narrative variable ids is non-zero.
    /// The ids are *not* resolved here -- this crate has no business
    /// loading a gameplay manifest, and this loader runs long before any
    /// character exists. They are checked against the real manifest by the
    /// consumer, at a point where both are in hand.
    NarrativeFlags(Vec<String>),
}

impl EscapeActivation {
    /// Same `parse(&str, ..)` shape as [`Medium::parse`],
    /// [`Traversal::parse`] and [`Generation::parse`]: classify the authored
    /// string and error on anything unrecognized.
    fn parse(kind: &str, flags: &[String]) -> Result<Self, String> {
        match kind {
            "manual" | "always" => {
                // Silently ignoring a filled-in list would let an author who
                // forgot to change the kind ship guidance that is
                // permanently on, or permanently off, with no complaint.
                if !flags.is_empty() {
                    return Err(format!(
                        "activation_kind {kind:?} takes no activation_flags, but {} were given",
                        flags.len()
                    ));
                }
                Ok(if kind == "manual" {
                    Self::Manual
                } else {
                    Self::Always
                })
            },
            "narrative_flags" => {
                if flags.is_empty() {
                    return Err("activation_kind \"narrative_flags\" needs a non-empty \
                                activation_flags list"
                        .to_string());
                }
                if let Some(bad) = flags
                    .iter()
                    .find(|id| id.trim().is_empty() || id.trim() != id.as_str())
                {
                    return Err(format!("malformed activation flag id {bad:?}"));
                }
                Ok(Self::NarrativeFlags(flags.to_vec()))
            },
            other => Err(format!("unknown escape activation_kind {other:?}")),
        }
    }
}

/// One room in the navigation graph.
#[derive(Debug)]
pub struct NavNode {
    /// The authored level id (e.g. `level.kharvun_mycelial_basin`).
    pub level_id: String,
    /// The room's own resolved centre, at floor height.
    pub centre: Vec3<i32>,
    pub floor_z: i32,
    pub ceiling_z: i32,
    pub radius: f32,
    /// Indices into [`InteriorNavGraph::edges`] for every edge touching
    /// this node.
    pub incident: Vec<u16>,
    /// Whether a surface access reaches the world from this room.
    pub is_exit: bool,
    /// Traversal cost to the nearest reachable exit. `None` when no exit is
    /// reachable from here at all.
    pub cost_to_exit: Option<f32>,
    /// Edge count along that same route, for callers that want a coarse
    /// "how much further" figure without re-deriving it.
    pub hops_to_exit: Option<u8>,
}

/// One tunnel in the navigation graph.
#[derive(Debug)]
pub struct NavEdge {
    /// The authored connection id (e.g.
    /// `connection.kharvun_ash_forks_to_prison`).
    pub connection_id: String,
    pub a: u16,
    pub b: u16,
    /// Where this tunnel meets node `a`'s room wall, and node `b`'s. A
    /// connection's carved endpoints both sit on their rooms' centres, so
    /// steering toward a centre would point at the middle of the room the
    /// player is already standing in; these perimeter points are where the
    /// tunnel physically leaves each room.
    pub portal_a: Vec3<f32>,
    pub portal_b: Vec3<f32>,
    /// The carved tunnel's own half-width, so a consumer can tell whether a
    /// position is inside this tunnel rather than merely near its line.
    pub tunnel_radius: f32,
    /// The authored bow of this tunnel, and its two carved endpoints --
    /// exactly the inputs `carve_connection` used, kept so a consumer can
    /// locate a position against the real centreline instead of a chord.
    pub curve: f32,
    pub end_a: Vec3<i32>,
    pub end_b: Vec3<i32>,
    /// Traversal cost from `a` to `b`.
    pub cost_ab: f32,
    /// Traversal cost from `b` to `a`. Differs from `cost_ab` whenever the
    /// two rooms are at different heights, since climbing costs more than
    /// descending.
    pub cost_ba: f32,
    /// A sealed gate: real, carved, and impassable until whatever governs
    /// it is satisfied.
    pub sealed: bool,
    /// Whether this tunnel can be traversed `b` -> `a` as well as
    /// `a` -> `b`. Mirrors the authored `bidirectional` flag, which the
    /// sibling level-placement walk also honours -- a one-way drop must
    /// never be offered as a way back up.
    pub bidirectional: bool,
    /// Whether this tunnel carries an authored `condition` (a capability
    /// check, a puzzle). Excluded from routing unless the interior sets
    /// `allow_gated_routes`.
    pub conditional: bool,
}

impl NavEdge {
    /// The far node from `node`, or `None` if `node` is not an endpoint.
    pub fn other(&self, node: u16) -> Option<u16> {
        if node == self.a {
            Some(self.b)
        } else if node == self.b {
            Some(self.a)
        } else {
            None
        }
    }

    /// Where this tunnel leaves `node`'s room, or `None` if `node` is not an
    /// endpoint.
    pub fn portal_at(&self, node: u16) -> Option<Vec3<f32>> {
        if node == self.a {
            Some(self.portal_a)
        } else if node == self.b {
            Some(self.portal_b)
        } else {
            None
        }
    }

    /// Cost of traversing this edge starting from `node`, or `None` when
    /// `node` is not an endpoint **or** the edge cannot be walked in that
    /// direction.
    pub fn cost_from(&self, node: u16) -> Option<f32> {
        if node == self.a {
            Some(self.cost_ab)
        } else if node == self.b && self.bidirectional {
            Some(self.cost_ba)
        } else {
            None
        }
    }

    /// Whether routing may use this edge at all. `allow_gated` mirrors the
    /// interior's `allow_gated_routes`.
    pub fn is_routable(&self, allow_gated: bool) -> bool {
        !self.sealed && (allow_gated || !self.conditional)
    }
}

/// A whole interior's navigation graph.
#[derive(Debug)]
pub struct InteriorNavGraph {
    /// The authored interior id (e.g. `interior.kharvun_reach`).
    pub interior_id: String,
    pub nodes: Vec<NavNode>,
    pub edges: Vec<NavEdge>,
    /// What turns guidance on inside this interior.
    pub activation: EscapeActivation,
    /// Bounding circle covering every carved shape -- the cheapest possible
    /// first rejection for "is this position even in here".
    pub bounds: (Vec2<i32>, f32),
}

impl InteriorNavGraph {
    /// The node whose room contains `wpos`, if any. `slack` widens the test
    /// so a player pressed against a wall, or standing on the floor of a
    /// room whose carved surface sits a little below `floor_z`, still
    /// resolves to that room.
    pub fn node_containing(&self, wpos: Vec3<f32>, slack: f32) -> Option<u16> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                wpos.z >= n.floor_z as f32 - slack && wpos.z <= n.ceiling_z as f32 + slack
            })
            .filter_map(|(i, n)| {
                let d = wpos.xy().distance(n.centre.xy().map(|e| e as f32));
                (d <= n.radius + slack).then_some((i as u16, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The tunnel containing `wpos`, with how far along it the position
    /// sits (`0.0` at `edge.a`, `1.0` at `edge.b`).
    ///
    /// Rooms are tens of blocks across while tunnels run for hundreds, so a
    /// player inside an interior is usually in a tunnel, not a room; a
    /// consumer that only resolved rooms would lose track of them for most
    /// of a traversal. Measured against the same spline `carve_connection`
    /// carved, via the same helper.
    pub fn edge_containing(&self, wpos: Vec3<f32>, slack: f32) -> Option<(u16, f64)> {
        let point = wpos.xy().map(|e| e as f64);
        self.edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let a2 = e.end_a.xy().map(|c| c as f64 + 0.5);
                let b2 = e.end_b.xy().map(|c| c as f64 + 0.5);
                let (t, dist) = spline_sample(a2, b2, e.curve, point)?;
                // Vertically, the tunnel runs between its two endpoints.
                let z = e.end_a.z as f32 + (e.end_b.z - e.end_a.z) as f32 * t as f32;
                ((dist <= (e.tunnel_radius + slack) as f64)
                    && (wpos.z - z).abs() <= e.tunnel_radius + slack)
                    .then_some((i as u16, t, dist))
            })
            .min_by(|x, y| x.2.total_cmp(&y.2))
            .map(|(i, t, _)| (i, t))
    }

    /// Whether `wpos` is inside this interior's bounding circle at all.
    pub fn within_bounds(&self, wpos: Vec3<f32>) -> bool {
        let (centre, radius) = self.bounds;
        wpos.xy().distance_squared(centre.map(|e| e as f32)) <= radius * radius
    }
}

/// Builds the navigation view for one interior, given the geometry already
/// resolved for it.
///
/// `positions` is the same level-id -> anchor map the layout was built from;
/// `levels_by_id` the same authored level table. Returns `Err` on an
/// authoring mistake that would make guidance meaningless (an unknown exit
/// id, no reachable exit at all, or more nodes than the linear-scan lookup
/// is sized for), so a bad edit fails loudly at load instead of producing an
/// arrow that points nowhere.
fn build_nav_graph(
    graph: &InteriorGraph,
    cfg: &EscapeGuidanceCfg,
    levels: &[LevelGeom],
    segments: &[ConnectionSeg],
    bounds: (Vec2<i32>, f32),
) -> Result<InteriorNavGraph, String> {
    if levels.len() > MAX_NAV_NODES {
        return Err(format!(
            "interior has {} navigable levels, over the {MAX_NAV_NODES} supported",
            levels.len()
        ));
    }

    let activation = EscapeActivation::parse(&cfg.activation_kind, &cfg.activation_flags)?;

    let index_of: HashMap<&str, u16> = levels
        .iter()
        .enumerate()
        .map(|(i, l)| (l.id.as_str(), i as u16))
        .collect();

    // Exits: either the authored override, or every level a surface access
    // reaches -- minus capability-gated ones unless the interior opts in.
    let mut exits: Vec<u16> = Vec::new();
    match &cfg.exits {
        Some(ids) => {
            for id in ids {
                let i = *index_of.get(id.as_str()).ok_or_else(|| {
                    format!("escape_guidance exits names unknown or unreachable level {id}")
                })?;
                if !exits.contains(&i) {
                    exits.push(i);
                }
            }
        },
        None => {
            for access in &graph.surface_accesses {
                if access.required_capability.is_some() && !cfg.allow_gated_routes {
                    continue;
                }
                // A surface access pointing at a level the layout could not
                // place is skipped rather than fatal: `build_layout` already
                // warned about that level, and the remaining exits are still
                // usable.
                if let Some(&i) = index_of.get(access.entry_level_id.as_str())
                    && !exits.contains(&i)
                {
                    exits.push(i);
                }
            }
        },
    }
    if exits.is_empty() {
        return Err(
            "escape_guidance is authored but no exit level resolved (check surface_accesses, \
             allow_gated_exits, or the exits override)"
                .to_string(),
        );
    }

    let mut nodes: Vec<NavNode> = levels
        .iter()
        .enumerate()
        .map(|(i, l)| NavNode {
            level_id: l.id.clone(),
            centre: l.anchor2d.with_z(l.floor_z),
            floor_z: l.floor_z,
            ceiling_z: l.ceiling_z,
            radius: l.radius,
            incident: Vec::new(),
            is_exit: exits.contains(&(i as u16)),
            cost_to_exit: None,
            hops_to_exit: None,
        })
        .collect();

    let segment_of: HashMap<&str, &ConnectionSeg> =
        segments.iter().map(|s| (s.id.as_str(), s)).collect();

    let mut edges: Vec<NavEdge> = Vec::with_capacity(graph.connections.len());
    for conn in &graph.connections {
        let (Some(&a), Some(&b)) = (
            index_of.get(conn.from_level_id.as_str()),
            index_of.get(conn.to_level_id.as_str()),
        ) else {
            // Same tolerance `build_layout` applies: a connection touching a
            // level that could not be placed is skipped, not fatal.
            continue;
        };
        if a == b {
            continue;
        }
        // The carved segment is the source of truth for this tunnel's real
        // shape; a connection with no segment was skipped by the geometry
        // pass, so there is nothing to navigate.
        let Some(seg) = segment_of.get(conn.id.as_str()) else {
            continue;
        };
        if edges.len() >= u16::MAX as usize {
            return Err(format!(
                "interior has more than {} navigable connections",
                u16::MAX
            ));
        }

        let length = connection_length(seg);
        // A gentler authored slope means the same drop is spread over a
        // longer, easier run, so it should feel *less* punishing per metre
        // of climb, not more.
        let climb = 1.0 + (CLIMB_PENALTY - 1.0) / seg.style.slope.max(1.0);
        let dz = nodes[b as usize].floor_z - nodes[a as usize].floor_z;
        // A level tunnel is symmetric; only a real rise is penalised, and
        // only in the direction that actually climbs it.
        let (cost_ab, cost_ba) = match dz.signum() {
            1 => (length * climb, length),
            -1 => (length, length * climb),
            _ => (length, length),
        };

        let idx = edges.len() as u16;
        edges.push(NavEdge {
            connection_id: conn.id.clone(),
            a,
            b,
            portal_a: portal_on_connection(seg, true, nodes[a as usize].radius),
            portal_b: portal_on_connection(seg, false, nodes[b as usize].radius),
            tunnel_radius: seg.style.radius,
            curve: seg.curve,
            end_a: seg.a,
            end_b: seg.b,
            cost_ab,
            cost_ba,
            sealed: seg.sealed,
            bidirectional: conn.bidirectional,
            conditional: conn.condition.is_some(),
        });
        nodes[a as usize].incident.push(idx);
        nodes[b as usize].incident.push(idx);
    }

    // Labels are computed once per world, but a sealed gate can be opened at
    // runtime, and nothing here would notice. No opted-in interior has one
    // today; say so loudly if that ever changes rather than shipping a
    // silently stale label.
    if let Some(sealed) = edges.iter().find(|e| e.sealed) {
        warn!(
            interior_id = %graph.id,
            connection_id = %sealed.connection_id,
            "Escape guidance in an interior with a sealed connection: its route labels are \
             computed once and will not reflect the gate being opened"
        );
    }

    label_cost_to_exit(&mut nodes, &edges, &exits, cfg.allow_gated_routes);

    // A pocket of levels with no routable way out is a real authoring
    // outcome, not an impossible one (the sealed-gate branch in this
    // module's own tests is exactly that), so it is reported rather than
    // rejected: guidance still works everywhere else, and a player inside
    // the pocket simply gets no arrow instead of a wrong one.
    let stranded: Vec<&str> = nodes
        .iter()
        .filter(|n| n.cost_to_exit.is_none())
        .map(|n| n.level_id.as_str())
        .collect();
    if !stranded.is_empty() {
        warn!(
            interior_id = %graph.id,
            levels = ?stranded,
            "Levels have no routable path to any escape exit"
        );
    }

    Ok(InteriorNavGraph {
        interior_id: graph.id.clone(),
        nodes,
        edges,
        activation,
        bounds,
    })
}

/// Where the tunnel joining `from` and `to` meets `from`'s room wall.
///
/// Derived from the two resolved anchors rather than from the placement
/// angle `layout_levels` used, because that angle only exists for the one
/// connection that placed a level -- a connection joining two levels that
/// were each placed via some other route has no such angle, and must still
/// get a sensible portal.
/// The quadratic the carved tunnel actually follows, in the exact form
/// [`carve_connection`] evaluates via [`spline_sample`]. Returned as
/// `(a_coef, b_coef, c)` for `P(t) = a t^2 + b t + c`.
///
/// This is read from the already-built [`ConnectionSeg`] rather than
/// re-derived from the two room centres. The distinction is load-bearing:
/// `river_spline_coeffs` puts the control offset in the *derivative* slot,
/// so the tunnel leaves its room along `ctrl_offset`, which the authored
/// per-connection `curve` bows away from the straight chord by up to
/// `atan(6 * 0.3)`, about 60 degrees. A portal placed on the chord bearing
/// would sit on the wrong arc of the room wall and point a player into
/// rock.
fn connection_spline(seg: &ConnectionSeg) -> (Vec2<f64>, Vec2<f64>, Vec2<f64>) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let ctrl_offset = ((b2 - a2) * 0.5
        + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * seg.curve as f64)
        .map(|e| e as f32);
    let spline = river_spline_coeffs(a2, ctrl_offset, b2);
    (spline.x, spline.y, spline.z)
}

fn spline_at(coeffs: (Vec2<f64>, Vec2<f64>, Vec2<f64>), t: f64) -> Vec2<f64> {
    coeffs.0 * t * t + coeffs.1 * t + coeffs.2
}

/// How far along a connection's own centreline its two ends are sampled
/// when measuring arc length and locating portals. The tunnels are single
/// quadratic arcs, so a modest fixed subdivision is well within a block.
const SPLINE_SAMPLES: usize = 64;

/// Where the carved tunnel leaves one of its rooms: the first point along
/// the real centreline that is at least `radius` from that room's centre.
///
/// `from_a` picks which end to march from. The search is capped at the
/// midpoint so the two portals of one tunnel can never cross, even where
/// two rooms sit closer together than their own radii.
fn portal_on_connection(seg: &ConnectionSeg, from_a: bool, radius: f32) -> Vec3<f32> {
    let coeffs = connection_spline(seg);
    let origin = spline_at(coeffs, if from_a { 0.0 } else { 1.0 });
    let radius = radius as f64;

    // Capped at the midpoint, from whichever end we are marching.
    let mut t_hit = 0.5;
    for i in 1..=SPLINE_SAMPLES {
        let f = (i as f64 / SPLINE_SAMPLES as f64) * 0.5;
        let t = if from_a { f } else { 1.0 - f };
        t_hit = t;
        if spline_at(coeffs, t).distance(origin) >= radius {
            break;
        }
    }

    let p = spline_at(coeffs, t_hit);
    let z = seg.a.z as f32 + (seg.b.z - seg.a.z) as f32 * t_hit as f32;
    Vec3::new(p.x as f32, p.y as f32, z)
}

/// Arc length of a connection's carved centreline, including its vertical
/// run. The straight chord between two room centres under-measures every
/// bowed tunnel, which would make curved routes look cheaper than they are.
fn connection_length(seg: &ConnectionSeg) -> f32 {
    let coeffs = connection_spline(seg);
    let mut horizontal = 0.0;
    let mut prev = spline_at(coeffs, 0.0);
    for i in 1..=SPLINE_SAMPLES {
        let t = i as f64 / SPLINE_SAMPLES as f64;
        let p = spline_at(coeffs, t);
        horizontal += p.distance(prev);
        prev = p;
    }
    let dz = (seg.b.z - seg.a.z) as f64;
    ((horizontal * horizontal + dz * dz).sqrt() as f32).max(1.0)
}

/// Labels every node with its cost, and hop count, to the nearest reachable
/// exit.
///
/// A multi-source relaxation seeded with every exit at zero. The graph is
/// undirected, so the distance found from an exit outward is also the
/// distance inward to it -- but the per-direction costs are not symmetric
/// (climbing costs more than descending), so the relaxation walks each edge
/// in the direction the *player* would travel it: outward from the exit
/// means the player is coming the other way.
fn label_cost_to_exit(nodes: &mut [NavNode], edges: &[NavEdge], exits: &[u16], allow_gated: bool) {
    // Small graphs (a couple of dozen nodes at most, validated by
    // `MAX_NAV_NODES`), so an O(V^2) scan beats the bookkeeping of a heap
    // and avoids needing a total order on f32.
    let mut best: Vec<Option<(f32, u8)>> = vec![None; nodes.len()];
    let mut settled = vec![false; nodes.len()];
    for &e in exits {
        best[e as usize] = Some((0.0, 0));
    }

    while let Some(current) = best
        .iter()
        .enumerate()
        .filter(|(i, b)| !settled[*i] && b.is_some())
        .min_by(|a, b| a.1.unwrap().0.total_cmp(&b.1.unwrap().0))
        .map(|(i, _)| i)
    {
        settled[current] = true;
        let (cost, hops) = best[current].expect("selected node has a cost");

        for &edge_idx in &nodes[current].incident {
            let edge = &edges[edge_idx as usize];
            if !edge.is_routable(allow_gated) {
                continue;
            }
            let Some(next) = edge.other(current as u16) else {
                continue;
            };
            if settled[next as usize] {
                continue;
            }
            // Walking *from* `next` *to* `current` is the direction a player
            // escaping through `current` would actually travel.
            let Some(step) = edge.cost_from(next) else {
                continue;
            };
            let candidate = (cost + step, hops.saturating_add(1));
            if best[next as usize].is_none_or(|(c, _)| candidate.0 < c) {
                best[next as usize] = Some(candidate);
            }
        }
    }

    for (node, label) in nodes.iter_mut().zip(best) {
        if let Some((cost, hops)) = label {
            node.cost_to_exit = Some(cost);
            node.hops_to_exit = Some(hops);
        }
    }
}

/// Every interior that authored escape guidance, resolved from the same
/// per-`Index` cache world-gen already populates.
///
/// Mirrors [`undercompact_gate_antechamber_world_geometry`]'s contract: by
/// the time any runtime consumer has a reason to call this, the relevant
/// chunk has been generated once through the ordinary pipeline, so the
/// cache is warm and this is a cheap read. Intended for low-cadence server
/// code only -- never a per-column path.
pub fn interior_nav_graphs<'a>(
    index: IndexRef<'a>,
    sim: &WorldSim,
) -> impl Iterator<Item = &'a InteriorNavGraph> + 'a {
    let map_size = sim.map_size_lg();
    // `index.index`, not `index.cromatolis_interiors` via `Deref`: the
    // deref borrows the local `IndexRef` value, so the returned iterator
    // would not live for `'a`. The sibling accessor below can use the
    // deref only because it returns an owned value rather than borrowing.
    index
        .index
        .cromatolis_interiors
        .get_or_init(|| build_all_layouts_for_map_size(map_size))
        .iter()
        .filter_map(|layout| layout.nav.as_ref())
}

/// Resolve every enabled authored interior into ready-to-carve geometry.
///
/// Takes only the map size, never a `CanvasInfo`, because a chunk is not what
/// this needs and two callers do not have one: the runtime gate accessor below
/// (which has a `WorldSim`) and the authored-void index, which is built once at
/// world generation rather than from inside a chunk.
pub(crate) fn build_all_layouts_for_map_size(map_size: MapSizeLg) -> Vec<InteriorLayout> {
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

    let world_size =
        TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);

    graphs
        .interiors
        .iter()
        .filter(|graph| ENABLED_INTERIOR_IDS.contains(&graph.id.as_str()))
        .filter_map(|graph| match build_layout(graph, map_size, world_size) {
            Ok(mut layout) => {
                layout.id = graph.id.clone();
                Some(layout)
            },
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

    // No pixel authored for this interior's entry: fall back to its
    // `parent_surface_site_id` settlement's position (e.g. a district
    // access built into an existing city rather than its own map pin).
    let sites = SitesAsset::load_owned(SITES_ASSET)
        .map_err(|err| format!("failed to load {SITES_ASSET}: {err}"))?;
    let anchor_site = sites
        .settlements
        .iter()
        .find(|site| site.id == graph.parent_surface_site_id)
        .ok_or_else(|| {
            format!(
                "parent_surface_site_id {} not found in the authored site list",
                graph.parent_surface_site_id
            )
        })?;
    let chunk_pos = anchor_site.center.to_chunk_pos(map_size);
    let wpos = chunk_pos.cpos_to_wpos_center();
    // Offset the district doorway away from the settlement's own nominal
    // center. This is an approximate, un-verified offset -- it does not
    // check against the parent site's actual generated footprint (city
    // plots can be considerably larger than this offset), so it could in
    // principle land inside another COW-6 building. Confirming that
    // requires the real generated `Site`'s footprint, which isn't
    // available at the point this interior's layout gets built; flagged
    // as a follow-up rather than solved here.
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
            id: level.id.clone(),
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
            id: conn.id.clone(),
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

    // Only interiors that opted in get a navigation view, and only if the
    // geometry actually produced a bounding circle to scope it to.
    //
    // A broken `escape_guidance` block must never cost this interior its
    // geometry. Guidance is an optional, additive overlay; the caller's
    // error path (`build_all_layouts_for_map_size`) drops the whole
    // interior on `Err`, which would leave solid rock where the rooms and
    // tunnels should be. So the overlay degrades to "absent" and says so,
    // and the authoring mistake is caught loudly where that is safe to do:
    // by the tests that build every opted-in interior from the real asset.
    let nav = match (&graph.escape_guidance, bounds) {
        (Some(cfg), Some(bounds)) => {
            match build_nav_graph(graph, cfg, &levels, &connections, bounds) {
                Ok(nav) => Some(nav),
                Err(err) => {
                    warn!(
                        interior_id = %graph.id, %err,
                        "Invalid escape_guidance; guidance disabled, geometry unaffected"
                    );
                    None
                },
            }
        },
        (Some(_), None) => {
            warn!(
                interior_id = %graph.id,
                "escape_guidance is authored but the interior carved no geometry; guidance \
                 disabled"
            );
            None
        },
        (None, _) => None,
    };

    Ok(InteriorLayout {
        // Set by the caller (`build_all_layouts_for_map_size`), which knows
        // the graph's own `id` -- this function only builds the geometry.
        id: String::new(),
        levels,
        connections,
        water,
        bounds,
        nav,
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

/// The subset of one [`InteriorLayout`]'s shapes that can plausibly touch
/// the chunk currently being generated, computed once before
/// `foreach_col` (not per-column) so the column closure only ever iterates
/// shapes that were pre-filtered against this specific chunk.
struct RelevantInterior<'a> {
    levels: Vec<&'a LevelGeom>,
    connections: Vec<&'a ConnectionSeg>,
    water: Vec<&'a WaterSeg>,
}

pub fn apply_cromatolis_interiors_to(canvas: &mut Canvas) {
    if !canvas.info().chunk().authored_cromatolis_v0 {
        return;
    }
    let info = canvas.info();
    let index_ref = info.index();
    let interiors = index_ref
        .cromatolis_interiors
        .get_or_init(|| build_all_layouts_for_map_size(info.chunks().map_size_lg()));
    if interiors.is_empty() {
        return;
    }

    let chunk_wpos = info.wpos();
    let chunk_size_i = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    let chunk_size = chunk_size_i.map(|e| e as f32);
    let chunk_center = chunk_wpos.map(|e| e as f32) + chunk_size / 2.0;
    let chunk_diag = (chunk_size.map(|e| e * e).sum()).sqrt() / 2.0;
    // The rectangle of columns this chunk's `foreach_col` will visit -- the
    // exact domain every shape below is pruned against.
    let chunk_rect = chunk_query_rect(chunk_wpos);

    // Two-stage pruning: first reject whole interiors whose overall
    // bounding circle can't reach this chunk at all, then -- within each
    // surviving interior -- reject individual rooms/connections/water
    // features that can't reach that rectangle. Without this second stage,
    // a chunk anywhere inside a large interior's overall bounding circle
    // (e.g. `kharvun_reach`'s, spanning many chained BFS-offset
    // connections) would pay the cost of every shape in that interior, even
    // ones nowhere near it.
    let relevant: Vec<RelevantInterior> = interiors
        .iter()
        .filter(|interior| {
            interior.bounds.is_some_and(|(center, radius)| {
                chunk_center.distance(center.map(|e| e as f32)) <= radius + chunk_diag + 32.0
            })
        })
        .map(|interior| RelevantInterior {
            levels: interior
                .levels
                .iter()
                .filter(|level| level_touches_chunk(level, chunk_rect))
                .collect(),
            connections: interior
                .connections
                .iter()
                .filter(|conn| connection_touches_chunk(conn, chunk_rect))
                .collect(),
            water: interior
                .water
                .iter()
                .filter(|water| water_touches_chunk(water, chunk_rect))
                .collect(),
        })
        .filter(|relevant| {
            !relevant.levels.is_empty()
                || !relevant.connections.is_empty()
                || !relevant.water.is_empty()
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
            // Placed last so the lever sprites always win the column over
            // whatever the room/connection/plug carving above wrote there.
            for level in &interior.levels {
                if level.id == GATE_ANTECHAMBER_LEVEL_ID {
                    carve_gate_levers(canvas, wpos2d, level);
                }
            }
        }
    });
}

/// Conservative (never under-counts) reach of a level room: base radius,
/// plus the max the procedural-dressing edge jitter could ever add, plus the
/// edge-softness fade.
///
/// Its own function so the pruning below and the measurement test that
/// scores the pruning cannot drift apart on what a room's reach is.
fn level_reach(level: &LevelGeom) -> f32 {
    level.radius
        + EDGE_SOFTNESS
        + if level.generation.allows_dressing() {
            MAX_ROOM_JITTER
        } else {
            0.0
        }
}

/// Does a level room reach the chunk's column rectangle?
///
/// Measured from the anchor clamped into the rectangle, not from the
/// chunk's four corners -- a room lying entirely inside one chunk is near
/// no corner of it, and the corner form used to prune exactly that case
/// away. For today's two interiors that cost no whole room, but it *was*
/// clipping 8 of the 20 connections out of individual chunks along their
/// span; the generic caves were losing 60 % of a `Small` chamber to the same
/// defect. COW-23 §7.1.
fn level_touches_chunk(level: &LevelGeom, chunk_rect: Aabr<f64>) -> bool {
    let anchor = level.anchor2d.map(|e| e as f64);
    dist_to_rect(chunk_rect, anchor) <= level_reach(level) as f64
}

/// Conservative reach of a connection tunnel (covers its carved passage,
/// bridge deck, and -- for a sealed connection -- the slightly wider gate
/// plug) against the chunk's column rectangle, measured along the
/// connection's real bowed spline rather than its chord.
fn connection_touches_chunk(conn: &ConnectionSeg, chunk_rect: Aabr<f64>) -> bool {
    let a2 = conn.a.xy().map(|e| e as f64 + 0.5);
    let b2 = conn.b.xy().map(|e| e as f64 + 0.5);
    let max_dist = conn.style.radius as f64 + EDGE_SOFTNESS as f64 + 1.0;
    spline_reaches_rect(a2, b2, conn.curve, chunk_rect, max_dist)
}

/// Conservative reach of a water feature (its carved fill plus its
/// waterfall column) against the chunk's column rectangle.
fn water_touches_chunk(seg: &WaterSeg, chunk_rect: Aabr<f64>) -> bool {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let max_dist = seg.radius as f64 + EDGE_SOFTNESS as f64;
    if spline_reaches_rect(a2, b2, seg.curve, chunk_rect, max_dist) {
        return true;
    }
    // A waterfall's vertical column sits at endpoint `b` and isn't
    // necessarily close to the spline itself once `t` truncates at 1.0.
    seg.drop_m
        .is_some_and(|_| dist_to_rect(chunk_rect, b2) <= max_dist)
}

fn edge_weight(dist: f32, radius: f32) -> f32 { ((radius - dist) / EDGE_SOFTNESS).clamp(0.0, 1.0) }

fn carve_level_room(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, level: &LevelGeom) {
    let dist = wpos2d
        .map(|e| e as f32)
        .distance(level.anchor2d.map(|e| e as f32));
    let dressed = level.generation.allows_dressing();

    // Cheap reject before paying for a noise sample: even with the maximum
    // possible dressing jitter, this column couldn't be inside the room.
    let max_possible_radius =
        level.radius + EDGE_SOFTNESS + if dressed { MAX_ROOM_JITTER } else { 0.0 };
    if dist > max_possible_radius {
        return;
    }

    let mut radius = level.radius;
    if dressed {
        // Additive-only jitter: never shrinks the guaranteed authored
        // skeleton, only adds an irregular fringe on top of it.
        let jitter = FastNoise2d::new(9001)
            .get(wpos2d.map(|e| e as f64 / 48.0))
            .max(0.0);
        radius += jitter * MAX_ROOM_JITTER;
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

/// [`spline_sample`], reachable from the authored-void index's parity test.
///
/// That test is what keeps this module's copy, `cromatolis_cave_features`'s
/// copy and the index's own copy byte-equivalent. They have to be: the
/// protection index is defined as the volume these carves produce, dilated by
/// one margin, and a one-sided edit would silently mis-protect.
#[cfg(test)]
pub(crate) fn spline_sample_for_parity(
    a2: Vec2<f64>,
    b2: Vec2<f64>,
    curve: f32,
    point: Vec2<f64>,
) -> Option<(f64, f64)> {
    spline_sample(a2, b2, curve, point)
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

/// If column `wpos2d` falls inside the solid plug built for a sealed
/// connection, returns the inclusive `(floor_z, ceiling_z)` range the plug
/// occupies there. Shared by [`plug_sealed_gate`] (world-gen carving) and
/// [`UndercompactGateAntechamberGeometry::plug_contains_column`] (the COW-7b
/// runtime clear-on-solve write), so the two can never disagree about the
/// plug's exact shape.
fn plug_column_z_range(seg: &ConnectionSeg, wpos2d: Vec2<i32>) -> Option<(i32, i32)> {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let (t, dist) = spline_sample(a2, b2, seg.curve, wpos2d.map(|e| e as f64 + 0.5))?;
    if (t - 0.5).abs() > GATE_PLUG_HALF_T || dist > (seg.style.radius as f64) + 1.0 {
        return None;
    }

    let floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t) as i32 - 1;
    let ceiling_z = Lerp::lerp_unclamped(seg.a_ceiling as f64, seg.b_ceiling as f64, t) as i32 + 1;
    Some((floor_z, ceiling_z))
}

/// Fills a solid stone plug across the full cross-section of `seg` at its
/// midpoint, guaranteeing a sealed connection stays physically blocked
/// regardless of any carving `carve_connection` already did there. No
/// puzzle/interaction logic.
fn plug_sealed_gate(canvas: &mut Canvas, wpos2d: Vec2<i32>, seg: &ConnectionSeg) {
    let Some((floor_z, ceiling_z)) = plug_column_z_range(seg, wpos2d) else {
        return;
    };
    let plug = Block::new(BlockKind::Rock, Rgb::new(60, 55, 60));
    for z in floor_z..=ceiling_z {
        canvas.set(wpos2d.with_z(z), plug);
    }
}

/// Places the two lever sprites inside the antechamber room (COW-7b). Both
/// start in their default, unpulled `Ori`. World-gen never needs to know
/// which levers (if any) were already pulled in a previous session -- a
/// solved gate is recovered on chunk load by clearing the *plug*
/// (`server/src/undercompact_gate.rs`'s restart-recovery pass), not by
/// re-carving the levers themselves.
fn carve_gate_levers(canvas: &mut Canvas, wpos2d: Vec2<i32>, level: &LevelGeom) {
    for lever_pos in antechamber_lever_positions(level.anchor2d, level.radius, level.floor_z) {
        if wpos2d == lever_pos.xy() {
            canvas.set(lever_pos, Block::air(SpriteKind::VaultLever));
        }
    }
}

/// How far below its authored surface a water feature's fill reaches, from
/// that feature's own radius. Shared by [`carve_water`] and
/// [`InteriorLayout::void_capsules`] so the carved volume and the protected
/// volume can never disagree about how deep the water is.
fn water_depth(radius: f32) -> f32 { (radius * 0.6).clamp(3.0, 10.0) }

/// The radius of the vertical fall column at a water feature's downstream end,
/// from that feature's own radius. Shared by [`carve_water`] and
/// [`InteriorLayout::void_waterfall_discs`] so the carved volume and the
/// protected volume can never disagree about how wide the fall is.
fn waterfall_radius(radius: f32) -> f32 { (radius * 0.5).max(2.0) }

fn carve_water(canvas: &mut Canvas, wpos2d: Vec2<i32>, col_alt: f32, seg: &WaterSeg) {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let point = wpos2d.map(|e| e as f64 + 0.5);

    if let Some((t, dist)) = spline_sample(a2, b2, seg.curve, point)
        && edge_weight(dist as f32, seg.radius) > 0.0
    {
        let surface_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t);
        let depth = water_depth(seg.radius) as f64;
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
        let fall_radius = waterfall_radius(seg.radius);
        if dist_to_b <= fall_radius {
            let fill = Block::new(BlockKind::Water, Rgb::zero());
            let top = (seg.b.z as f32 + drop_m).min(col_alt - SURFACE_MARGIN) as i32;
            for z in seg.b.z..=top {
                canvas.set(wpos2d.with_z(z), fill);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Public runtime accessors (COW-7b). Mirrors the "Public anchor accessors"
// convention `world::civ::cromatolis_fortification_gate_world_aabb` already
// established: resolve the authored geometry fresh from `sim` (not cached,
// not requiring a live `Canvas`), so a server-side runtime consumer never
// has to duplicate or hardcode this module's authoring math. Only ever
// called by low-cadence server code (a lever interaction, a restart-recovery
// check on chunk load) -- never a per-column hot path.
// ---------------------------------------------------------------------

/// World-space geometry for the Undercompact gate antechamber's two-lever
/// puzzle: the antechamber room itself (for lever placement -- though
/// world-gen is the one that actually carves the levers; this lets runtime
/// code recognize an interaction against the same positions) and the sealed
/// connection's solid plug (for the one-shot clear-on-solve write).
pub struct UndercompactGateAntechamberGeometry {
    /// The antechamber room's own 2D center and vertical band.
    pub room_center2d: Vec2<i32>,
    pub room_radius: f32,
    pub room_floor_z: i32,
    pub room_ceiling_z: i32,
    /// The two lever world positions, in the exact same order/positions
    /// `carve_gate_levers` placed their sprites at.
    pub lever_positions: [Vec3<i32>; 2],
    /// A conservative world-space AABB covering the sealed gate's solid
    /// plug. Only a scan bound -- [`Self::plug_contains_column`] is the
    /// precise per-column test.
    pub plug_aabb: Aabb<i32>,
    plug_seg: ConnectionSeg,
}

impl UndercompactGateAntechamberGeometry {
    /// If column `wpos2d` is inside the sealed gate's solid plug, returns
    /// the inclusive `(floor_z, ceiling_z)` range to clear there -- the
    /// exact same test [`plug_sealed_gate`] used to fill it in the first
    /// place (via the shared [`plug_column_z_range`] helper), so clearing
    /// never carves outside, or leaves a sliver inside, the authored plug
    /// shape.
    pub fn plug_contains_column(&self, wpos2d: Vec2<i32>) -> Option<(i32, i32)> {
        plug_column_z_range(&self.plug_seg, wpos2d)
    }
}

/// Resolves [`UndercompactGateAntechamberGeometry`] from the same cached,
/// per-`Index` `Vec<InteriorLayout>` [`apply_cromatolis_interiors_to`]
/// already populates at world-gen time (`Index::cromatolis_interiors`) --
/// **not** a fresh RON-parse-plus-BFS-rebuild on every call. By the time any
/// server-side runtime consumer (COW-7b's lever-activation event, its
/// restart-recovery chunk-load check) has a reason to call this, the
/// relevant chunk has already been generated once through the ordinary
/// world-gen pipeline, which means that cache is already warm and this call
/// is just a cheap `OnceLock` read. The `get_or_init` closure below only
/// ever actually runs in the (practically unreachable, but still handled
/// correctly) case where this is called before any authored Cromatolis
/// chunk has ever been generated for this `Index`.
///
/// Returns `None` if the authored asset is missing, malformed, or (should
/// never happen for the real data) does not contain both the antechamber
/// level and the sealed connection.
pub fn undercompact_gate_antechamber_world_geometry(
    index: IndexRef,
    sim: &WorldSim,
) -> Option<UndercompactGateAntechamberGeometry> {
    let map_size = sim.map_size_lg();
    let layouts = index
        .cromatolis_interiors
        .get_or_init(|| build_all_layouts_for_map_size(map_size));
    let layout = layouts
        .iter()
        .find(|layout| layout.id == UNDERCOMPACT_INTERIOR_ID)?;
    geometry_from_layout(layout)
}

/// The `map_size`-only core of
/// [`undercompact_gate_antechamber_world_geometry`], split out so it is
/// testable without a full, expensive `WorldSim::generate`/`Index` -- every
/// other test in this module already builds layouts from a bare `MapSizeLg`
/// the same way. Always builds fresh (no cache), which is fine: it exists
/// only for tests, never called on any per-tick path -- `#[cfg(test)]` since
/// it genuinely has no non-test caller (unlike the pub accessor above,
/// which always goes through the `Index` cache instead).
#[cfg(test)]
fn undercompact_gate_antechamber_geometry_for_map_size(
    map_size: MapSizeLg,
) -> Option<UndercompactGateAntechamberGeometry> {
    let layouts = build_all_layouts_for_map_size(map_size);
    let layout = layouts
        .iter()
        .find(|layout| layout.id == UNDERCOMPACT_INTERIOR_ID)?;
    geometry_from_layout(layout)
}

/// Shared by both accessors above: picks the antechamber room and the
/// sealed connection's segment out of an already-resolved `layout`.
fn geometry_from_layout(layout: &InteriorLayout) -> Option<UndercompactGateAntechamberGeometry> {
    let room = layout
        .levels
        .iter()
        .find(|level| level.id == GATE_ANTECHAMBER_LEVEL_ID)?;
    let plug_seg = layout
        .connections
        .iter()
        .find(|conn| conn.id == GATE_SEALED_CONNECTION_ID)?
        .clone();

    let lever_positions = antechamber_lever_positions(room.anchor2d, room.radius, room.floor_z);
    let plug_aabb = plug_bounding_aabb(&plug_seg);

    Some(UndercompactGateAntechamberGeometry {
        room_center2d: room.anchor2d,
        room_radius: room.radius,
        room_floor_z: room.floor_z,
        room_ceiling_z: room.ceiling_z,
        lever_positions,
        plug_aabb,
        plug_seg,
    })
}

/// A conservative world-space AABB covering `seg`'s solid plug: samples the
/// plug's own `t` range (see [`GATE_PLUG_HALF_T`]) and unions each sample's
/// footprint. The plug spans only a short arc of the connection's spline, so
/// a handful of samples is enough to bound it tightly without evaluating
/// every column.
fn plug_bounding_aabb(seg: &ConnectionSeg) -> Aabb<i32> {
    let a2 = seg.a.xy().map(|e| e as f64 + 0.5);
    let b2 = seg.b.xy().map(|e| e as f64 + 0.5);
    let ctrl_offset = ((b2 - a2) * 0.5
        + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * seg.curve as f64)
        .map(|e| e as f32);
    let spline = river_spline_coeffs(a2, ctrl_offset, b2);
    let radius = seg.style.radius as f64 + 1.0;

    const SAMPLES: i32 = 8;
    let mut min = Vec3::new(i32::MAX, i32::MAX, i32::MAX);
    let mut max = Vec3::new(i32::MIN, i32::MIN, i32::MIN);
    for i in 0..=SAMPLES {
        let t =
            0.5 - GATE_PLUG_HALF_T + (GATE_PLUG_HALF_T * 2.0) * (f64::from(i) / f64::from(SAMPLES));
        let p = spline.x * (t * t) + spline.y * t + spline.z;
        let floor_z = Lerp::lerp_unclamped(seg.a.z as f64, seg.b.z as f64, t) as i32 - 1;
        let ceiling_z =
            Lerp::lerp_unclamped(seg.a_ceiling as f64, seg.b_ceiling as f64, t) as i32 + 1;

        min.x = min.x.min((p.x - radius).floor() as i32);
        min.y = min.y.min((p.y - radius).floor() as i32);
        min.z = min.z.min(floor_z);
        max.x = max.x.max((p.x + radius).ceil() as i32);
        max.y = max.y.max((p.y + radius).ceil() as i32);
        max.z = max.z.max(ceiling_z);
    }
    Aabb { min, max }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layer::authored_voids::spline_coeffs, util::SQUARE_4};
    use common::vol::ReadVol;

    fn sample_graph() -> InteriorGraph {
        InteriorGraph {
            id: "interior.test".to_string(),
            parent_surface_site_id: "site.cutstone_city".to_string(),
            entry_level_id: "level.a".to_string(),
            surface_accesses: vec![SurfaceAccess {
                source_pixel: None,
                entry_level_id: "level.a".to_string(),
                required_capability: None,
            }],
            adventure_start_level_id: Some("level.gated".to_string()),
            escape_guidance: None,
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
        // Evaluate the actual curve (not the straight a-b line) at t=0.5.
        let midpoint = curve_midpoint(sealed.a, sealed.b, sealed.curve);

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
    fn level_touches_chunk_accepts_a_near_chunk_and_rejects_a_far_one() {
        let level = LevelGeom {
            id: "level.test".to_string(),
            anchor2d: Vec2::new(1000, 1000),
            floor_z: 0,
            ceiling_z: 40,
            radius: 20.0,
            medium: Medium::Air,
            generation: Generation::AuthoredGeometry,
        };
        // A chunk overlapping the room: should touch.
        let near = rect(Vec2::new(990.0, 1000.0), Vec2::new(1010.0, 1010.0));
        assert!(level_touches_chunk(&level, near));

        // A chunk far outside the room's radius: should not touch.
        let far = rect(Vec2::new(5000.0, 5000.0), Vec2::new(5032.0, 5032.0));
        assert!(!level_touches_chunk(&level, far));
    }

    /// The case the corner-only form got wrong: a room small enough to sit
    /// entirely inside one chunk, near none of its corners. This is
    /// `Small`'s exact situation in `cromatolis_cave_features` -- an anchor
    /// at the chunk centre, 22.63 blocks from every corner, with a reach
    /// well under that.
    #[test]
    fn level_touches_chunk_sees_a_room_wholly_inside_the_chunk() {
        let chunk_wpos = Vec2::new(1024, 1024);
        let centre = chunk_wpos + TerrainChunkSize::RECT_SIZE.map(|e| e as i32) / 2;
        let level = LevelGeom {
            id: "level.test".to_string(),
            anchor2d: centre,
            floor_z: 0,
            ceiling_z: 40,
            // Reach (11 + EDGE_SOFTNESS = 14) is comfortably under the
            // 22.63-block centre-to-corner distance, so every one of the
            // four corners is outside this room.
            radius: 11.0,
            medium: Medium::Air,
            generation: Generation::AuthoredGeometry,
        };
        let chunk_rect = chunk_query_rect(chunk_wpos);
        for corner in SQUARE_4 {
            let corner_wpos = (chunk_wpos + corner * TerrainChunkSize::RECT_SIZE.map(|e| e as i32))
                .map(|e| e as f32);
            assert!(
                corner_wpos.distance(centre.map(|e| e as f32)) > level.radius + EDGE_SOFTNESS,
                "the test's premise: no corner is inside the room"
            );
        }
        assert!(level_touches_chunk(&level, chunk_rect));
    }

    #[test]
    fn level_touches_chunk_accounts_for_the_max_dressing_jitter() {
        // Just past the base radius + edge softness, but still within the
        // max jitter a procedurally-dressed room could add: a
        // `authored_geometry` room (no jitter) should reject this, while
        // an `authored_core_procedural_dressing` room should not.
        let reach = (20.0 + EDGE_SOFTNESS + 1.0) as f64;
        let probe = Vec2::new(1000.0 + reach, 1000.0);
        let chunk_rect = rect(probe, probe);
        let base = LevelGeom {
            id: "level.test".to_string(),
            anchor2d: Vec2::new(1000, 1000),
            floor_z: 0,
            ceiling_z: 40,
            radius: 20.0,
            medium: Medium::Air,
            generation: Generation::AuthoredGeometry,
        };
        assert!(!level_touches_chunk(&base, chunk_rect));

        let dressed = LevelGeom {
            generation: Generation::AuthoredCoreProceduralDressing,
            ..base
        };
        assert!(level_touches_chunk(&dressed, chunk_rect));
    }

    #[test]
    fn connection_and_water_touch_chunk_accept_near_and_reject_far() {
        let graph = sample_graph();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(&graph, map_size, world_size).unwrap();
        let conn = &layout.connections[0];
        let water = &layout.water[0];

        // The curve bends away from the straight `a`-`b` line (see
        // `spline_sample`'s `ctrl_offset`), so a point actually on the
        // curve -- not just the straight-line midpoint -- is needed here.
        let mid = curve_midpoint(conn.a, conn.b, conn.curve);
        assert!(connection_touches_chunk(conn, rect(mid, mid)));

        let far = rect(Vec2::new(1.0e6, 1.0e6), Vec2::new(1.0e6, 1.0e6));
        assert!(!connection_touches_chunk(conn, far));

        let water_mid = curve_midpoint(water.a, water.b, water.curve);
        assert!(water_touches_chunk(water, rect(water_mid, water_mid)));
        assert!(!water_touches_chunk(water, far));
    }

    /// The capsule counterpart of
    /// `level_touches_chunk_sees_a_room_wholly_inside_the_chunk`: a
    /// connection whose whole bowed span sits inside one chunk, so the
    /// corner-only form saw nothing. Built directly rather than from the
    /// sample graph, whose connections all span several chunks.
    #[test]
    fn connection_touches_chunk_sees_a_span_wholly_inside_the_chunk() {
        let chunk_wpos = Vec2::new(2048, 2048);
        let centre = chunk_wpos + TerrainChunkSize::RECT_SIZE.map(|e| e as i32) / 2;
        let conn = ConnectionSeg {
            id: "connection.test".to_string(),
            a: (centre - Vec2::new(5, 5)).with_z(0),
            b: (centre + Vec2::new(5, 5)).with_z(0),
            a_ceiling: 8,
            b_ceiling: 8,
            curve: 0.0,
            style: TraversalStyle {
                radius: 3.0,
                headroom: 6.0,
                terraced: false,
                bridge_deck: false,
                slope: 4.0,
            },
            sealed: false,
        };
        assert!(connection_touches_chunk(
            &conn,
            chunk_query_rect(chunk_wpos)
        ));
    }

    /// A rectangle from two opposite points, for the `*_touches_chunk`
    /// tests -- `min`/`max` rather than a real chunk when the test is about
    /// the shape's reach, not about chunk geometry.
    fn rect(min: Vec2<f64>, max: Vec2<f64>) -> Aabr<f64> { Aabr { min, max } }

    /// A point actually on the quadratic curve `spline_sample` uses
    /// (`a`-to-`b`, bent by `curve`), evaluated at `t = 0.5`.
    fn curve_midpoint(a: Vec3<i32>, b: Vec3<i32>, curve: f32) -> Vec2<f64> {
        let a2 = a.xy().map(|e| e as f64 + 0.5);
        let b2 = b.xy().map(|e| e as f64 + 0.5);
        let ctrl_offset = ((b2 - a2) * 0.5
            + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * curve as f64)
            .map(|e| e as f32);
        let spline = river_spline_coeffs(a2, ctrl_offset, b2);
        spline.x * 0.25 + spline.y * 0.5 + spline.z
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
    fn anchor_for_uses_the_graph_s_own_parent_surface_site_id_not_a_hardcoded_one() {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);

        // A real, known site: resolves.
        let mut graph = sample_graph();
        graph.parent_surface_site_id = "site.cutstone_city".to_string();
        assert!(anchor_for(&graph, map_size, world_size).is_ok());

        // A site id that doesn't exist in the authored site list: this
        // must fail loudly, not silently fall back to some other site
        // (which is what a hardcoded id would have done regardless of
        // what this graph actually declares).
        graph.parent_surface_site_id = "site.does_not_exist".to_string();
        let err = anchor_for(&graph, map_size, world_size).unwrap_err();
        assert!(
            err.contains("site.does_not_exist"),
            "error should name the actual missing site id, got: {err}"
        );
    }

    #[test]
    fn enabled_interior_ids_contains_both_authored_interiors() {
        assert_eq!(ENABLED_INTERIOR_IDS, &[
            "interior.the_undercompact",
            "interior.kharvun_reach"
        ]);
    }

    /// COW-7b T8: the public runtime accessor must resolve against the real
    /// authored asset, find the antechamber and the (moved) sealed
    /// connection, and hand back two distinct lever positions actually
    /// inside the antechamber room.
    #[test]
    fn undercompact_gate_antechamber_geometry_resolves_against_the_real_asset() {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let geometry = undercompact_gate_antechamber_geometry_for_map_size(map_size)
            .expect("the real asset must carry the antechamber level and the sealed connection");

        let [lever_a, lever_b] = geometry.lever_positions;
        assert_ne!(lever_a, lever_b, "the two levers must not coincide");
        for lever in [lever_a, lever_b] {
            assert_eq!(lever.z, geometry.room_floor_z);
            let dist = lever
                .xy()
                .map(|e| e as f32)
                .distance(geometry.room_center2d.map(|e| e as f32));
            assert!(
                dist <= geometry.room_radius,
                "lever at {lever:?} must sit inside the antechamber room (radius {})",
                geometry.room_radius
            );
        }

        // The plug AABB must sit between the antechamber and the threshold,
        // not overlap the room itself.
        assert!(geometry.plug_aabb.min.x <= geometry.plug_aabb.max.x);
        assert!(geometry.plug_aabb.min.y <= geometry.plug_aabb.max.y);
        assert!(geometry.plug_aabb.min.z <= geometry.plug_aabb.max.z);
    }

    /// The one-shot clear-on-solve write (server-side) has to clear exactly
    /// the columns `plug_sealed_gate` would have filled -- this pins that
    /// [`UndercompactGateAntechamberGeometry::plug_contains_column`] agrees
    /// with the real, resolved plug segment at its own midpoint.
    #[test]
    fn plug_contains_column_matches_the_real_plug_at_its_midpoint() {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let geometry = undercompact_gate_antechamber_geometry_for_map_size(map_size).unwrap();

        let a2 = geometry.plug_seg.a.xy().map(|e| e as f64 + 0.5);
        let b2 = geometry.plug_seg.b.xy().map(|e| e as f64 + 0.5);
        let midpoint = curve_midpoint(
            geometry.plug_seg.a,
            geometry.plug_seg.b,
            geometry.plug_seg.curve,
        );
        let (t, _) = spline_sample(a2, b2, geometry.plug_seg.curve, midpoint).unwrap();
        assert!((t - 0.5).abs() < 0.15);

        let column = midpoint.map(|e| e.floor() as i32);
        assert!(
            geometry.plug_contains_column(column).is_some(),
            "a column at the plug's own midpoint must be reported as inside the plug"
        );

        // Far away from the connection entirely: never inside the plug.
        assert!(
            geometry
                .plug_contains_column(Vec2::new(-999_999, -999_999))
                .is_none()
        );
    }

    /// The plug bounding AABB is only a scan bound -- it must at least fully
    /// contain the exact midpoint column [`plug_contains_column`] reports as
    /// inside the plug, or a caller that only scans within the AABB (T7/T9's
    /// clear-on-solve write) would miss real plug blocks.
    #[test]
    fn plug_aabb_contains_the_exact_plug_midpoint() {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let geometry = undercompact_gate_antechamber_geometry_for_map_size(map_size).unwrap();
        let midpoint = curve_midpoint(
            geometry.plug_seg.a,
            geometry.plug_seg.b,
            geometry.plug_seg.curve,
        )
        .map(|e| e.floor() as i32);

        assert!(geometry.plug_aabb.min.x <= midpoint.x && midpoint.x <= geometry.plug_aabb.max.x);
        assert!(geometry.plug_aabb.min.y <= midpoint.y && midpoint.y <= geometry.plug_aabb.max.y);
        let (floor_z, ceiling_z) = geometry.plug_contains_column(midpoint).unwrap();
        assert!(geometry.plug_aabb.min.z <= floor_z && ceiling_z <= geometry.plug_aabb.max.z);
    }

    /// **T19 of the COW-23 task board, interior half.** Nothing in either
    /// authored interior may be pruned out of a chunk it carves.
    ///
    /// "Lost" here is per *shape, per chunk*: a chunk where the carve would
    /// write at least one column of the shape, but the predicate prunes the
    /// shape out of that chunk. That is deliberately stricter than the
    /// spec's own "0 of 22 rooms and 0 of 20 connections are lost today",
    /// which counted shapes that vanish *entirely* -- and the difference
    /// shows: 22/22 rooms were indeed intact under the corner-only
    /// predicate, but **8 of the 20 connections were being clipped out of
    /// at least one chunk they run through**, surviving elsewhere along
    /// their span and so never showing up as a lost connection. The
    /// interior defect was latent as a whole-shape loss, not as a
    /// per-chunk one.
    ///
    /// The surface cap is ignored on purpose: a column the cap closes is
    /// not written either way, so counting it only makes the claim
    /// stronger.
    #[test]
    fn neither_interior_loses_a_room_or_a_connection_to_pruning() {
        /// The corner-only predicates this change replaced, so the
        /// before/after is measured here rather than quoted.
        fn old_disc_touches(anchor: Vec2<i32>, max_radius: f32, corners: &[Vec2<f32>; 4]) -> bool {
            let anchor = anchor.map(|e| e as f32);
            corners
                .iter()
                .any(|corner| corner.distance(anchor) <= max_radius)
        }
        fn old_spline_touches(
            a: Vec3<i32>,
            b: Vec3<i32>,
            curve: f32,
            max_dist: f64,
            corners: &[Vec2<f64>; 4],
        ) -> bool {
            let a2 = a.xy().map(|e| e as f64 + 0.5);
            let b2 = b.xy().map(|e| e as f64 + 0.5);
            corners.iter().any(|&corner| {
                spline_sample(a2, b2, curve, corner).is_some_and(|(_, dist)| dist <= max_dist)
            })
        }

        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let chunk_size = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);

        let (mut rooms, mut connections) = (0_usize, 0_usize);
        let (mut rooms_lost_before, mut rooms_lost_after) = (0_usize, 0_usize);
        let (mut conns_lost_before, mut conns_lost_after) = (0_usize, 0_usize);

        for graph in &graphs.interiors {
            let layout = build_layout(graph, map_size, world_size).unwrap();

            for level in &layout.levels {
                rooms += 1;
                // The production formula, not a copy of it, so the two
                // cannot drift apart.
                let reach = level_reach(level);
                let (mut lost_before, mut lost_after) = (false, false);
                for chunk_pos in chunks_over(level.anchor2d, reach.ceil() as i32, chunk_size) {
                    let chunk_wpos = chunk_pos * chunk_size;
                    // Would the carve write any column of this chunk?
                    let writes = columns_of(chunk_wpos, chunk_size).any(|wpos2d| {
                        wpos2d
                            .map(|e| e as f32)
                            .distance(level.anchor2d.map(|e| e as f32))
                            < level.radius
                    });
                    if !writes {
                        continue;
                    }
                    let corners = corners_f32(chunk_wpos, chunk_size);
                    lost_before |= !old_disc_touches(level.anchor2d, reach, &corners);
                    lost_after |= !level_touches_chunk(level, chunk_query_rect(chunk_wpos));
                }
                rooms_lost_before += usize::from(lost_before);
                rooms_lost_after += usize::from(lost_after);
            }

            for conn in &layout.connections {
                connections += 1;
                let max_dist = conn.style.radius as f64 + EDGE_SOFTNESS as f64 + 1.0;
                let a2 = conn.a.xy().map(|e| e as f64 + 0.5);
                let b2 = conn.b.xy().map(|e| e as f64 + 0.5);
                // Which chunks to look at: the bounding box of the real
                // bowed centreline, walked rather than bounded by a
                // hand-picked constant, then dilated by the shape's reach.
                let (mut lo, mut hi) = (a2.map2(b2, f64::min), a2.map2(b2, f64::max));
                let spline = spline_coeffs(a2, b2, conn.curve);
                for i in 0..=32 {
                    let t = i as f64 / 32.0;
                    let point = spline.x * t * t + spline.y * t + spline.z;
                    lo = lo.map2(point, f64::min);
                    hi = hi.map2(point, f64::max);
                }
                let centre = ((lo + hi) * 0.5).map(|e| e.round() as i32);
                let reach = ((hi - lo) * 0.5).reduce_partial_max().ceil() as i32
                    + max_dist.ceil() as i32
                    + 1;
                let (mut lost_before, mut lost_after) = (false, false);
                for chunk_pos in chunks_over(centre, reach, chunk_size) {
                    let chunk_wpos = chunk_pos * chunk_size;
                    let writes = columns_of(chunk_wpos, chunk_size).any(|wpos2d| {
                        let point = wpos2d.map(|e| e as f64 + 0.5);
                        spline_sample(a2, b2, conn.curve, point)
                            .is_some_and(|(_, dist)| dist < conn.style.radius as f64)
                    });
                    if !writes {
                        continue;
                    }
                    let corners = corners_f64(chunk_wpos, chunk_size);
                    lost_before |=
                        !old_spline_touches(conn.a, conn.b, conn.curve, max_dist, &corners);
                    lost_after |= !connection_touches_chunk(conn, chunk_query_rect(chunk_wpos));
                }
                conns_lost_before += usize::from(lost_before);
                conns_lost_after += usize::from(lost_after);
            }
        }

        println!(
            "\ninteriors: {rooms} rooms, {connections} connections\n  rooms lost       before \
             {rooms_lost_before}  after {rooms_lost_after}\n  connections lost before \
             {conns_lost_before}  after {conns_lost_after}\n"
        );
        assert_eq!(
            rooms_lost_after, 0,
            "a room is pruned out of a chunk it carves"
        );
        assert_eq!(
            conns_lost_after, 0,
            "a connection is pruned out of a chunk it carves"
        );
        // The rectangle test admits everything the corner test admitted
        // (a chunk corner lies inside the chunk rectangle, so its distance
        // to a shape can never be the smaller of the two), so neither
        // figure may ever go up.
        assert!(rooms_lost_after <= rooms_lost_before);
        assert!(conns_lost_after <= conns_lost_before);
        // And the baseline these numbers are compared against is real: the
        // corner form did lose connections here. If this ever reads 0, the
        // "before" arm has stopped exercising the old predicate and the
        // before/after columns above mean nothing.
        assert!(
            conns_lost_before > 0,
            "the corner-only baseline should still lose connections; it lost 8 of 20 when this              was written"
        );
    }

    /// Chunk positions whose chunk can overlap the square of half-width
    /// `reach` around `centre`.
    fn chunks_over(
        centre: Vec2<i32>,
        reach: i32,
        chunk_size: Vec2<i32>,
    ) -> impl Iterator<Item = Vec2<i32>> {
        let min = (centre - reach).map2(chunk_size, |e, sz| e.div_euclid(sz));
        let max = (centre + reach).map2(chunk_size, |e, sz| e.div_euclid(sz));
        (min.y..=max.y).flat_map(move |y| (min.x..=max.x).map(move |x| Vec2::new(x, y)))
    }

    /// Every column `Canvas::foreach_col` visits for the chunk at `chunk_wpos`.
    fn columns_of(chunk_wpos: Vec2<i32>, chunk_size: Vec2<i32>) -> impl Iterator<Item = Vec2<i32>> {
        (0..chunk_size.y)
            .flat_map(move |y| (0..chunk_size.x).map(move |x| chunk_wpos + Vec2::new(x, y)))
    }

    fn corners_f32(chunk_wpos: Vec2<i32>, chunk_size: Vec2<i32>) -> [Vec2<f32>; 4] {
        SQUARE_4.map(|rpos| (chunk_wpos + rpos * chunk_size).map(|e| e as f32))
    }

    fn corners_f64(chunk_wpos: Vec2<i32>, chunk_size: Vec2<i32>) -> [Vec2<f64>; 4] {
        SQUARE_4.map(|rpos| (chunk_wpos + rpos * chunk_size).map(|e| e as f64 + 0.5))
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
            9,
            "the_undercompact has 9 authored levels (including the COW-7b lever antechamber)"
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
            authored_geometry_count, 3,
            "the lever antechamber, the sealed gate room, and the finale are the only \
             authored_geometry levels"
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

    /// A water feature that authored a `drop_m` carves a narrow vertical
    /// column at its downstream end, *outside* the capsule that follows its
    /// surface -- so it needs its own protection shape or it is the one
    /// authored volume a procedural tunnel could cross unnoticed.
    ///
    /// Pinned against the carve it mirrors rather than against a literal: the
    /// disc must stand at the segment's `b` endpoint, span `drop_m` upward
    /// from it, and use the carve's own fall radius.
    #[test]
    fn a_waterfall_gets_its_own_protection_disc() {
        let with_fall = WaterSeg {
            a: Vec3::new(0, 0, 100),
            b: Vec3::new(200, 0, 100),
            curve: 0.0,
            radius: 9.0,
            drop_m: Some(30.0),
        };
        let without_fall = WaterSeg {
            drop_m: None,
            ..WaterSeg {
                a: Vec3::new(0, 0, 100),
                b: Vec3::new(200, 0, 100),
                curve: 0.0,
                radius: 9.0,
                drop_m: None,
            }
        };
        let layout = InteriorLayout {
            water: vec![with_fall, without_fall],
            ..Default::default()
        };

        let falls: Vec<_> = layout.void_waterfall_discs().collect();
        assert_eq!(
            falls.len(),
            1,
            "only the segment that authored a drop_m carves a fall column"
        );
        let (disc, contact) = &falls[0];
        assert_eq!(*contact, INTERIOR_PROCEDURAL_CONTACT);
        assert_eq!(disc.centre, Vec2::new(200, 0), "the fall stands at `b`");
        assert_eq!(disc.floor_z, 100, "and rises from `b.z`");
        assert_eq!(disc.ceiling_z, 130, "by drop_m");
        assert!(
            (disc.radius - waterfall_radius(9.0)).abs() < f32::EPSILON,
            "the protection disc must use the carve's own fall radius"
        );
    }

    /// [`compute_bounds`] derives an interior's bounding circle from segment
    /// *endpoints* only, which looks as if it could under-cover a connection
    /// that bows well off its chord -- and the bounding circle is what the
    /// per-chunk prune trusts, so an under-cover would silently drop carving
    /// (and authored-void protection) for real geometry.
    ///
    /// It does not, over the real authored data, with room to spare. This
    /// pins that: every connection's real centreline, sampled the same way
    /// the rest of the module samples one, must stay inside the bounds circle
    /// once its own tunnel radius is added.
    ///
    /// `cargo test -p xindeler-world compute_bounds -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn compute_bounds_covers_every_bowed_connection_centreline() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, _index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let layouts = build_all_layouts_for_map_size(world.sim().map_size_lg());
        let mut connections = 0;
        let mut worst_deficit = f32::NEG_INFINITY;

        for layout in &layouts {
            let Some((centre, bounds_radius)) = layout.bounds else {
                continue;
            };
            let centre = centre.map(|e| e as f64);
            for seg in &layout.connections {
                connections += 1;
                let coeffs = connection_spline(seg);
                for i in 0..=SPLINE_SAMPLES {
                    let t = i as f64 / SPLINE_SAMPLES as f64;
                    let reach = spline_at(coeffs, t).distance(centre) as f32 + seg.style.radius;
                    worst_deficit = worst_deficit.max(reach - bounds_radius);
                }
            }
        }

        println!(
            "{connections} connections across {} interiors; worst spline-vs-bounds deficit \
             {worst_deficit:.3} blocks",
            layouts.len()
        );
        assert!(connections > 0, "the real data must contain connections");
        assert!(
            worst_deficit <= 0.0,
            "a connection's real centreline reaches {worst_deficit:.3} blocks OUTSIDE the \
             bounding circle the per-chunk prune trusts"
        );
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
                .generate_chunk(index_ref, chunk_pos, None, || false, None, None)
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

    // -----------------------------------------------------------------
    // Navigation view.
    // -----------------------------------------------------------------

    fn guidance(kind: &str, flags: &[&str]) -> EscapeGuidanceCfg {
        EscapeGuidanceCfg {
            activation_kind: kind.to_string(),
            activation_flags: flags.iter().map(|s| s.to_string()).collect(),
            exits: None,
            allow_gated_routes: false,
        }
    }

    /// The sample graph with guidance authored on it, plus a second,
    /// capability-gated surface access so the default exit policy is
    /// actually exercised.
    fn sample_guided_graph(cfg: EscapeGuidanceCfg) -> InteriorGraph {
        let mut graph = sample_graph();
        graph.surface_accesses.push(SurfaceAccess {
            source_pixel: None,
            entry_level_id: "level.gated".to_string(),
            required_capability: Some("underwater_breathing_or_short_dive".to_string()),
        });
        graph.escape_guidance = Some(cfg);
        graph
    }

    fn layout_of(graph: &InteriorGraph) -> InteriorLayout {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        build_layout(graph, map_size, world_size).expect("geometry should build")
    }

    /// `build_layout` deliberately swallows a guidance error so a bad
    /// `escape_guidance` block can never un-carve an interior, so tests that
    /// want the error call the builder directly, on the very same geometry
    /// `build_layout` just produced.
    fn nav_of(graph: &InteriorGraph) -> Result<InteriorNavGraph, String> {
        let layout = layout_of(graph);
        let cfg = graph
            .escape_guidance
            .as_ref()
            .expect("this helper is for graphs that authored escape_guidance");
        let bounds = layout.bounds.expect("geometry should have bounds");
        build_nav_graph(graph, cfg, &layout.levels, &layout.connections, bounds)
    }

    fn node<'a>(nav: &'a InteriorNavGraph, id: &str) -> &'a NavNode {
        nav.nodes
            .iter()
            .find(|n| n.level_id == id)
            .unwrap_or_else(|| panic!("no nav node for {id}"))
    }

    #[test]
    fn no_escape_guidance_authored_means_no_nav_view_is_built() {
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).unwrap();
        let world_size =
            TerrainChunkSize::RECT_SIZE.map(|e| e as f32) * map_size.chunks().map(|e| e as f32);
        let layout = build_layout(&sample_graph(), map_size, world_size).unwrap();
        assert!(
            layout.nav.is_none(),
            "an interior that did not opt in must pay nothing for the navigation view"
        );
    }

    #[test]
    fn every_activation_kind_round_trips_and_unknown_kinds_are_rejected() {
        assert_eq!(
            nav_of(&sample_guided_graph(guidance("manual", &[])))
                .unwrap()
                .activation,
            EscapeActivation::Manual
        );
        assert_eq!(
            nav_of(&sample_guided_graph(guidance("always", &[])))
                .unwrap()
                .activation,
            EscapeActivation::Always
        );
        assert_eq!(
            nav_of(&sample_guided_graph(guidance("narrative_flags", &[
                "quest.test.escaping"
            ])))
            .unwrap()
            .activation,
            EscapeActivation::NarrativeFlags(vec!["quest.test.escaping".to_string()])
        );

        let err = nav_of(&sample_guided_graph(guidance("sometimes", &[]))).unwrap_err();
        assert!(
            err.contains("sometimes"),
            "the error should name the offending kind, got {err:?}"
        );
    }

    #[test]
    fn narrative_flags_activation_requires_a_non_empty_well_formed_flag_list() {
        let err = nav_of(&sample_guided_graph(guidance("narrative_flags", &[]))).unwrap_err();
        assert!(
            err.contains("activation_flags"),
            "an empty flag list should be rejected by name, got {err:?}"
        );

        let err = nav_of(&sample_guided_graph(guidance("narrative_flags", &[
            " padded",
        ])))
        .unwrap_err();
        assert!(
            err.contains("malformed"),
            "a malformed flag id should be rejected, got {err:?}"
        );
    }

    #[test]
    fn a_capability_gated_surface_access_is_not_an_exit_unless_the_interior_opts_in() {
        // `level.gated`'s access carries a `required_capability`, so by
        // default it must not be treated as a way out.
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        assert!(node(&nav, "level.a").is_exit, "level.a is the free exit");
        assert!(
            !node(&nav, "level.gated").is_exit,
            "a capability-gated access must not become an exit by default"
        );

        let mut cfg = guidance("always", &[]);
        cfg.allow_gated_routes = true;
        let nav = nav_of(&sample_guided_graph(cfg)).unwrap();
        assert!(
            node(&nav, "level.gated").is_exit,
            "allow_gated_exits must actually be wired, not decorative"
        );
        assert_eq!(node(&nav, "level.gated").hops_to_exit, Some(0));
    }

    #[test]
    fn an_exits_override_naming_an_unknown_level_is_rejected_by_name() {
        let mut cfg = guidance("always", &[]);
        cfg.exits = Some(vec!["level.nowhere".to_string()]);
        let err = nav_of(&sample_guided_graph(cfg)).unwrap_err();
        assert!(
            err.contains("level.nowhere"),
            "the error should name the unknown level, got {err:?}"
        );
    }

    #[test]
    fn guidance_with_no_resolvable_exit_is_rejected_rather_than_pointing_nowhere() {
        let mut graph = sample_graph();
        // Only a gated access exists, and gated exits are not allowed.
        graph.surface_accesses = vec![SurfaceAccess {
            source_pixel: None,
            entry_level_id: "level.a".to_string(),
            required_capability: Some("flight".to_string()),
        }];
        graph.escape_guidance = Some(guidance("always", &[]));
        let err = nav_of(&graph).unwrap_err();
        assert!(
            err.contains("no exit level resolved"),
            "an unsatisfiable exit policy must fail loudly, got {err:?}"
        );
    }

    #[test]
    fn portals_sit_on_the_carved_tunnel_not_on_the_straight_chord() {
        // The regression this test exists for: deriving a portal from the
        // bearing between two room centres puts it on the wrong arc of the
        // wall, because `river_spline_coeffs` takes the control offset as
        // the *derivative*, so the tunnel leaves its room along a vector the
        // authored `curve` bows up to ~60 degrees off the chord.
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        assert!(!nav.edges.is_empty());
        for edge in &nav.edges {
            let a2 = edge.end_a.xy().map(|e| e as f64 + 0.5);
            let b2 = edge.end_b.xy().map(|e| e as f64 + 0.5);
            for portal in [edge.portal_a, edge.portal_b] {
                let (_, dist) = spline_sample(a2, b2, edge.curve, portal.xy().map(|e| e as f64))
                    .expect("a portal must lie within its own tunnel's span");
                assert!(
                    dist <= edge.tunnel_radius as f64,
                    "portal for {} is {dist} blocks off a tunnel only {} wide",
                    edge.connection_id,
                    edge.tunnel_radius
                );
            }
        }
    }

    #[test]
    fn two_tunnels_out_of_one_room_leave_through_different_walls() {
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        let b = nav
            .nodes
            .iter()
            .position(|n| n.level_id == "level.b")
            .unwrap() as u16;
        let portals: Vec<_> = nav.nodes[b as usize]
            .incident
            .iter()
            .map(|&e| nav.edges[e as usize].portal_at(b).unwrap())
            .collect();
        assert_eq!(portals.len(), 2);
        assert!(
            portals[0].distance(portals[1]) > 1.0,
            "two tunnels out of the same room must leave through different walls"
        );
        for (i, portal) in portals.iter().enumerate() {
            let centre = nav.nodes[b as usize].centre.xy().map(|e| e as f32);
            assert!(
                portal.xy().distance(centre) > 1.0,
                "portal {i} collapsed onto the room centre -- the degeneracy this whole mechanism \
                 exists to avoid"
            );
        }
    }

    #[test]
    fn a_position_inside_a_tunnel_localises_to_that_tunnel() {
        // Rooms are tens of blocks across and tunnels run for hundreds, so a
        // consumer that could only resolve rooms would lose the player for
        // most of a traversal.
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        let edge = &nav.edges[0];
        let a2 = edge.end_a.xy().map(|e| e as f64 + 0.5);
        let b2 = edge.end_b.xy().map(|e| e as f64 + 0.5);
        let coeffs = connection_spline(&ConnectionSeg {
            id: edge.connection_id.clone(),
            a: edge.end_a,
            b: edge.end_b,
            a_ceiling: 0,
            b_ceiling: 0,
            curve: edge.curve,
            style: Traversal::WalkDescend.style(),
            sealed: false,
        });
        let mid = spline_at(coeffs, 0.5);
        let z = edge.end_a.z as f32 + (edge.end_b.z - edge.end_a.z) as f32 * 0.5;
        let probe = Vec3::new(mid.x as f32, mid.y as f32, z);

        let (found, t) = nav
            .edge_containing(probe, 1.0)
            .expect("a point on the tunnel centreline is inside the tunnel");
        assert_eq!(found, 0);
        assert!(
            (t - 0.5).abs() < 0.05,
            "t should be near the middle, was {t}"
        );
        let _ = (a2, b2);

        // Well away from every tunnel, nothing matches.
        assert!(
            nav.edge_containing(Vec3::new(1.0e6, 1.0e6, 0.0), 1.0)
                .is_none()
        );
    }

    #[test]
    fn climbing_an_edge_costs_more_than_descending_the_same_edge() {
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        let edge = nav
            .edges
            .iter()
            .find(|e| e.connection_id == "connection.a_b")
            .unwrap();
        // `level.a` (floor 100) is above `level.b` (floor 40), so a -> b
        // descends and b -> a climbs.
        assert!(
            edge.cost_ba > edge.cost_ab,
            "climbing should cost more than descending: ab={} ba={}",
            edge.cost_ab,
            edge.cost_ba
        );
    }

    #[test]
    fn a_sealed_connection_is_never_used_to_reach_an_exit() {
        // `connection.b_gated` is a sealed stone gate, and `level.gated`
        // hangs off it, so `level.gated` has no unsealed route out at all.
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        assert_eq!(node(&nav, "level.a").hops_to_exit, Some(0));
        assert_eq!(node(&nav, "level.b").hops_to_exit, Some(1));
        assert_eq!(
            node(&nav, "level.gated").cost_to_exit,
            None,
            "a node reachable only through a sealed gate must have no route out"
        );
    }

    #[test]
    fn node_containing_and_within_bounds_resolve_a_position() {
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        let a = node(&nav, "level.a");
        let inside = a.centre.map(|e| e as f32) + Vec3::new(1.0, 1.0, 1.0);
        assert!(nav.within_bounds(inside));
        assert_eq!(
            nav.node_containing(inside, 2.0)
                .map(|i| nav.nodes[i as usize].level_id.as_str()),
            Some("level.a")
        );

        // Far outside the bounding circle: rejected before any room test.
        let far = Vec3::new(1.0e6, 1.0e6, 0.0);
        assert!(!nav.within_bounds(far));
        assert!(nav.node_containing(far, 2.0).is_none());

        // Inside the circle horizontally but far above every ceiling.
        let high = a.centre.map(|e| e as f32).with_z(100_000.0);
        assert!(nav.node_containing(high, 2.0).is_none());
    }

    #[test]
    fn a_skip_edge_strictly_lowers_the_cost_to_exit_it_bypasses() {
        let baseline = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        let before = node(&baseline, "level.gated").hops_to_exit;
        assert_eq!(
            before, None,
            "baseline: level.gated is only reachable through the sealed gate"
        );

        // Author a diagonal that skips `level.b` entirely, joining the
        // deepest level straight to the exit level.
        let mut graph = sample_guided_graph(guidance("always", &[]));
        graph.connections.push(Connection {
            id: "connection.a_gated_skip".to_string(),
            from_level_id: "level.a".to_string(),
            to_level_id: "level.gated".to_string(),
            traversal: "walk_descend".to_string(),
            bidirectional: true,
            condition: None,
        });
        let nav = nav_of(&graph).unwrap();
        assert_eq!(
            node(&nav, "level.gated").hops_to_exit,
            Some(1),
            "a skip edge must give the bypassed node a real, shorter route out"
        );
    }

    #[test]
    fn real_kharvun_reach_nav_graph_labels_every_level_and_keeps_the_prison_deepest() {
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let mut graph = graphs
            .interiors
            .into_iter()
            .find(|g| g.id == "interior.kharvun_reach")
            .expect("interior.kharvun_reach should be present in the authored data");
        // The real asset has not opted in yet (that is authored content, and
        // lands separately); opt in here so the projection is exercised
        // against real geometry rather than a synthetic stand-in.
        graph.escape_guidance = Some(guidance("narrative_flags", &[
            "quest.abyssal_awakening.escaping"
        ]));

        let nav = nav_of(&graph).unwrap();
        assert_eq!(nav.nodes.len(), 13, "kharvun_reach has 13 authored levels");
        assert_eq!(
            nav.edges.len(),
            12,
            "kharvun_reach has 12 authored connections"
        );

        // Only the dry hidden vent is an exit; the submerged respiradero
        // carries a required_capability and is excluded by default.
        let exits: Vec<&str> = nav
            .nodes
            .iter()
            .filter(|n| n.is_exit)
            .map(|n| n.level_id.as_str())
            .collect();
        assert_eq!(exits, vec!["level.kharvun_secret_shelf"]);

        // Every level reaches the vent *except* the submerged respiradero,
        // whose only tunnel demands a capability. Excluding it is the point:
        // the safety policy has to hold along the route, not just at the
        // exit, or it merely moves the hazard one edge inward.
        let stranded: Vec<&str> = nav
            .nodes
            .iter()
            .filter(|n| n.cost_to_exit.is_none())
            .map(|n| n.level_id.as_str())
            .collect();
        assert_eq!(stranded, vec!["level.kharvun_polder_respiradero"]);

        let prison = node(&nav, "level.kharvun_prison_depths");
        let deepest = nav
            .nodes
            .iter()
            .filter(|n| n.cost_to_exit.is_some())
            .max_by(|a, b| a.cost_to_exit.unwrap().total_cmp(&b.cost_to_exit.unwrap()))
            .unwrap();
        assert_eq!(
            deepest.level_id, prison.level_id,
            "the prison the escape starts from should be the furthest point from the vent"
        );
        assert_eq!(
            node(&nav, "level.kharvun_secret_shelf").hops_to_exit,
            Some(0)
        );
    }

    #[test]
    fn the_breathing_throat_is_a_dead_end_branch_not_a_second_exit() {
        // Regression guard: `level.kharvun_breathing_throat` sits between the
        // exit shelf and the submerged respiradero. If the gated access were
        // ever treated as an exit by default, the throat would label as 0 and
        // the compass would route an escaping party down into a flooded
        // 1.5 m hole instead of out through the dry vent.
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let mut graph = graphs
            .interiors
            .into_iter()
            .find(|g| g.id == "interior.kharvun_reach")
            .unwrap();
        graph.escape_guidance = Some(guidance("manual", &[]));
        let nav = nav_of(&graph).unwrap();

        let throat = node(&nav, "level.kharvun_breathing_throat");
        assert!(!throat.is_exit);
        assert_eq!(
            throat.hops_to_exit,
            Some(1),
            "the throat is one hop from the shelf, not an exit in its own right"
        );
        assert!(
            !node(&nav, "level.kharvun_polder_respiradero").is_exit,
            "the submerged respiradero must stay out of the exit set by default"
        );
    }

    #[test]
    fn opting_the_real_reach_into_gated_exits_changes_the_labelling() {
        let graphs = InteriorGraphsAsset::load_owned(INTERIOR_GRAPHS_ASSET).unwrap();
        let mut graph = graphs
            .interiors
            .into_iter()
            .find(|g| g.id == "interior.kharvun_reach")
            .unwrap();
        let mut cfg = guidance("always", &[]);
        cfg.allow_gated_routes = true;
        graph.escape_guidance = Some(cfg);
        let nav = nav_of(&graph).unwrap();

        assert!(
            node(&nav, "level.kharvun_polder_respiradero").is_exit,
            "with gated exits allowed the respiradero becomes a way out"
        );
        assert_eq!(
            node(&nav, "level.kharvun_polder_respiradero").hops_to_exit,
            Some(0)
        );
    }

    #[test]
    fn a_broken_escape_guidance_block_never_uncarves_the_interior() {
        // Guidance is an optional overlay. The caller's error path drops a
        // whole interior on `Err`, so a typo in this block must not be able
        // to reach it -- otherwise one bad character leaves solid rock where
        // 13 rooms and 12 tunnels should be.
        let good = layout_of(&sample_guided_graph(guidance("always", &[])));
        assert!(good.nav.is_some());

        for bad in [
            guidance("alway", &[]),              // typo'd kind
            guidance("narrative_flags", &[]),    // empty flag list
            guidance("always", &["stray.flag"]), // flags on the wrong kind
        ] {
            let graph = sample_guided_graph(bad);
            let layout = layout_of(&graph);
            assert!(
                layout.nav.is_none(),
                "a bad guidance block should disable guidance"
            );
            assert_eq!(
                layout.levels.len(),
                good.levels.len(),
                "geometry must survive a bad guidance block"
            );
            assert_eq!(layout.connections.len(), good.connections.len());
            assert!(layout.bounds.is_some());
        }
    }

    #[test]
    fn a_one_way_connection_is_never_offered_as_a_route_back_up() {
        let mut graph = sample_guided_graph(guidance("always", &[]));
        // `level.a` is the exit; make the tunnel down to `level.b` one-way,
        // so `level.b` has no way back up to it.
        graph.connections[0].bidirectional = false;
        let nav = nav_of(&graph).unwrap();

        let edge = &nav.edges[0];
        assert!(edge.cost_from(edge.a).is_some());
        assert!(
            edge.cost_from(edge.b).is_none(),
            "a one-way drop must not be walkable in reverse"
        );
        assert_eq!(
            node(&nav, "level.b").cost_to_exit,
            None,
            "the compass must never route a player up a chute they cannot climb"
        );

        // Same graph, two-way: now it is a route.
        let nav = nav_of(&sample_guided_graph(guidance("always", &[]))).unwrap();
        assert_eq!(node(&nav, "level.b").hops_to_exit, Some(1));
    }

    #[test]
    fn a_conditional_tunnel_is_excluded_unless_gated_routes_are_allowed() {
        let mut graph = sample_guided_graph(guidance("always", &[]));
        graph.connections[0].condition = Some(ConnectionCondition {
            kind: "underwater_breathing_or_short_dive".to_string(),
        });
        let nav = nav_of(&graph).unwrap();
        assert!(nav.edges[0].conditional);
        assert_eq!(
            node(&nav, "level.b").cost_to_exit,
            None,
            "gating the exit but not the tunnel to it just moves the hazard one edge inward"
        );

        let mut cfg = guidance("always", &[]);
        cfg.allow_gated_routes = true;
        let mut graph = sample_guided_graph(cfg);
        graph.connections[0].condition = Some(ConnectionCondition {
            kind: "underwater_breathing_or_short_dive".to_string(),
        });
        let nav = nav_of(&graph).unwrap();
        assert_eq!(node(&nav, "level.b").hops_to_exit, Some(1));
    }

    #[test]
    fn escape_activation_parse_classifies_and_rejects() {
        let none: [String; 0] = [];
        assert_eq!(
            EscapeActivation::parse("manual", &none).unwrap(),
            EscapeActivation::Manual
        );
        assert_eq!(
            EscapeActivation::parse("always", &none).unwrap(),
            EscapeActivation::Always
        );
        let flags = ["quest.a".to_string()];
        assert_eq!(
            EscapeActivation::parse("narrative_flags", &flags).unwrap(),
            EscapeActivation::NarrativeFlags(vec!["quest.a".to_string()])
        );
        assert!(EscapeActivation::parse("nope", &none).is_err());
        assert!(EscapeActivation::parse("narrative_flags", &none).is_err());
        assert!(EscapeActivation::parse("always", &flags).is_err());
        assert!(EscapeActivation::parse("narrative_flags", &[" pad".to_string()]).is_err());
    }
}
