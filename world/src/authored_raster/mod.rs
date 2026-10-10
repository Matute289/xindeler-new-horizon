//! XINDELER: authored raster layers -- the authored water layer (Stage 1)
//! and the authored ground layer (Stage 2).
//!
//! Not to be confused with [`crate::layer::authored_regions`], which indexes
//! the authored *voids* (caves, interiors) of a map for the procedural cave
//! guard. This module is about the terrain surface: exact water, banks and
//! ground.
//!
//! # The ground layer (Stage 2)
//!
//! A region may also declare a [`LayerKind::Ground`] layer: per block column
//! an absolute ground altitude in integer cm and a blend weight
//! ([`AuthoredCell::Ground`]). An *exact* ground cell (weight 255) is the
//! column's ground: top solid block `floor(cm / 100)`, with none of the
//! engine's terrain terms (chunk spline, undulation, `small_nz` relief,
//! cliffs, warp, mesa, bank pull, the -16 m registration). Precedence: a
//! water cell wins and a ground cell under it must agree with its bed or
//! bank, checked at load. A *blend* cell (weight 1..=254) lerps from the
//! terrain the region would render without the ground layer toward its
//! authored block by `weight / 255`: the exporter writes these as a ring
//! around every exact patch, so patches meet the engine's terrain without a
//! step. Ground cells in partial chunks meet natural columns directly; where
//! natural water would stand as a wall beside one, world generation stops
//! ([`queries::check_ground_seams`], after civ generation). Water below 0 m
//! is a per-region parameter ([`SeaFill`]). The sim chunk table stays the
//! natural map (the exporter builds it from the master as it was before the
//! patch); the authored-aware ground queries for sites and rtsim are not
//! routed to the ground layer yet (they read the table, as before).
//!
//! # What it does
//!
//! Inside a declared, chunk-aligned *authored region*, the column sampler
//! stops deriving water from the 32 m sim chunks (River discs, Lake/Ocean
//! chunk sets, chunk `alt` as the water level, bank pull) and reads a 1 m
//! raster instead: per block column either "water with this surface over this
//! bed" ([`AuthoredCell::Wet`]), "dry ground at this height"
//! ([`AuthoredCell::Bank`]) or nothing ([`AuthoredCell::None`]). Outside every
//! region nothing here runs, so every column there is bit-identical to an
//! engine without this module.
//!
//! # Contract
//!
//! * **Manifest** `<map>_authored_rasters.ron` ([`Manifest`], schema
//!   [`MANIFEST_SCHEMA`], unknown fields/layers are errors). It is optional:
//!   *missing* means "no authored water" (the old path, bit-identical) unless
//!   the map's registry entry requires it; *present but wrong in any way* stops
//!   world generation with a message naming the manifest, region, tile or cell.
//! * **Tiles** `<map>_ar_<region>_<layer>_<tx>_<ty>.bin`, 256 x 256 cells of 1
//!   m in *column space* (cell = block column, rows from the south), zstd,
//!   sha256-checked against the manifest; layout in [`format`]. Being in column
//!   space, the raster never sees the -16 m registration of the chunk-spline
//!   heightmap.
//! * **Quantisation** is integer: centimetres in the block-z frame (sea level
//!   140 m = 14 000 cm); top water block = `floor(surface / 100)`, top ground
//!   block = `floor(bed / 100)`. Water meeting the ocean uses surface block
//!   [`SEA_TOP_BLOCK`].
//! * **Containment** (checked at load): water only next to water or a bank at
//!   least as high; nothing below the ocean's top block; chunk-aligned,
//!   non-overlapping boxes at least [`RIM_MARGIN_M`] from the rim; the whole
//!   manifest within [`RESIDENT_BUDGET_BYTES`] resident.
//! * **Column sampler** ([`AuthoredColumn::resolve`]): wet and bank columns are
//!   exact at any feather weight; unauthored columns blend from the engine's
//!   terrain to the engine's *dry* terrain over the feather; no water above sea
//!   level in a region except authored water. Regional terrain overrides
//!   (crater, flood) apply on top.
//! * **Sim chunk table** (kinds, `water_alt`, downhill graph) is *not* mutated:
//!   any change there would leak up to three chunks outside the region
//!   (`local_cells` radius) and into civ generation. Consumers that must agree
//!   with the rendered water (rtsim boats and spawns, civ placement, ports,
//!   wildlife) ask the authored-aware queries in [`queries`] instead. The
//!   exporter must leave the water/elevated masks *untouched* inside regions: a
//!   mask pixel without the elevated mark sinks the chunk to sea level and the
//!   unauthored columns around the exact water inherit that pit.
//!   [`AuthoredRasters::check_consistency`] enforces it with a per-region
//!   budget ([`ConsistencyBudget`]).
//! * **Box margin.** Decorations rooted inside a region (boulders, trees) can
//!   change blocks a few metres outside its box (<= 10 m measured), so every
//!   authored cell must lie at least [`REGION_MARGIN_M`] inside the box; a
//!   closer one is a load error that names the box to use instead.
//! * **Decorations stay natural by default.** Procedural rocks, trees and
//!   sprites beside and in authored water are the natural map's and are kept; a
//!   region may opt out ([`RegionManifest::suppress_procedural_in_water`],
//!   [`RegionManifest::exclude_procedural_margin_m`]).
//! * **Unauthored columns.** By default a region owns its box: unauthored
//!   columns keep the engine's terrain but not the sim's water (only authored
//!   water exists inside). Chunks listed in
//!   [`RegionManifest::allow_partial_chunks`] (or every chunk, with
//!   [`RegionManifest::allow_partial`]) keep the *natural* map in their
//!   unauthored columns instead -- terrain and water exactly as without the
//!   region -- so a handful of authored cells can edit an existing river.
//! * **Not supported:** hot reload (generated chunks would keep the old water
//!   while new ones get the new; restart the server), and changing a manifest
//!   under a live world without a reset (persisted chunk edits and rtsim NPCs
//!   inside the region: see `rtsim::rule::authored_water` and
//!   `TerrainPersistence::check_authored_rasters_digest`).

pub mod format;
pub mod queries;
#[cfg(any(test, feature = "tools"))]
pub mod writer;

use crate::{
    CONFIG,
    sim::{AquaticEcologyProfileId, WorldSim},
};
use common::{
    assets::{self, AssetExt, BoxedError, FileAsset, load_ron},
    terrain::TerrainChunkSize,
    vol::RectVolSize,
};
use format::{
    GROUND_EXACT, GroundTile, GroundTileCm, LayerKind, TILE_CELLS, TILE_SIZE, WaterTile, cell_index,
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::OnceLock};
use tracing::info;
use vek::*;

/// Schema version of the manifest this engine reads. New *layers* (the
/// Stage 2 ground layer) are new [`LayerKind`] variants under this same
/// schema: an older engine rejects the unknown variant when it parses the
/// manifest. The schema only moves if the manifest's structure changes.
pub const MANIFEST_SCHEMA: u32 = 1;
/// Distances to authored water are tracked up to this many metres: the
/// distance at which the column sampler's warp fade (`water_dist / 64`)
/// saturates, so the two can never disagree.
pub const DIST_CAP_M: i32 = crate::column::WATER_WARP_FADE_M as i32;
/// Engine limit on the decoded tiles plus distance fields of a whole
/// manifest. Over it, loading fails: split or shrink regions, or list fewer
/// tiles (a tile without authored cells need not be listed).
pub const RESIDENT_BUDGET_BYTES: usize = 256 << 20;
/// Default (and recommended) process-wide cap on the resident bytes of every
/// loaded manifest together: a server hosting several worlds (one `WorldSim`
/// each) holds one copy per world.
///
/// Sizing (server with ~15 GiB of RAM; the rest of the server stack is
/// expected to need ~3 GiB): 8 GiB leaves room for the stack plus the OS and
/// its page cache. The cap is raised only explicitly, with
/// `XINDELER_AUTHORED_RASTERS_MAX_MIB`, up to
/// [`GLOBAL_RESIDENT_HARD_CAP_BYTES`].
pub const GLOBAL_RESIDENT_CAP_BYTES: usize = 8 << 30;
/// Hard ceiling of the process-wide cap: `XINDELER_AUTHORED_RASTERS_MAX_MIB`
/// above it stops world generation. Between the default and this ceiling the
/// server starts with a warning that prints the budget: at 10 GiB only ~5
/// GiB of a 15 GiB box remain for the rest of the server, the OS and the
/// page cache, which is little headroom. 8 GiB is the safe value.
pub const GLOBAL_RESIDENT_HARD_CAP_BYTES: usize = 10 << 30;
/// The environment variable that overrides [`GLOBAL_RESIDENT_CAP_BYTES`]
/// (MiB).
pub const RESIDENT_CAP_ENV: &str = "XINDELER_AUTHORED_RASTERS_MAX_MIB";
/// Resident bytes of every live [`AuthoredRasters`] in this process.
static GLOBAL_RESIDENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The process-wide cap an override value asks for: `Ok((bytes, warning))`
/// where `warning` is set when the cap is above the recommended default;
/// `Err` when the value does not parse or is above
/// [`GLOBAL_RESIDENT_HARD_CAP_BYTES`]. `None` = the default.
pub fn resident_cap_from(
    override_mib: Option<&str>,
    total_ram: Option<u64>,
) -> Result<(usize, Option<String>), String> {
    let Some(v) = override_mib else {
        return Ok((GLOBAL_RESIDENT_CAP_BYTES, None));
    };
    let mib: usize = v
        .trim()
        .parse()
        .map_err(|_| format!("{RESIDENT_CAP_ENV}={v:?} is not a whole number of MiB"))?;
    let bytes = mib.saturating_mul(1 << 20);
    let ram = total_ram.map_or_else(|| "unknown".to_string(), |b| format!("{} MiB", b >> 20));
    if bytes > GLOBAL_RESIDENT_HARD_CAP_BYTES {
        return Err(format!(
            "{RESIDENT_CAP_ENV}={mib} MiB is above the hard ceiling of {} MiB for authored \
             rasters (total RAM {ram}; the rest of the server needs ~3 GiB plus the OS): use at \
             most {} MiB, recommended {} MiB",
            GLOBAL_RESIDENT_HARD_CAP_BYTES >> 20,
            GLOBAL_RESIDENT_HARD_CAP_BYTES >> 20,
            GLOBAL_RESIDENT_CAP_BYTES >> 20
        ));
    }
    let warning = (bytes > GLOBAL_RESIDENT_CAP_BYTES).then(|| {
        format!(
            "authored rasters may use up to {mib} MiB ({RESIDENT_CAP_ENV}), above the recommended \
             {} MiB; total RAM {ram}, the rest of the server needs ~3 GiB plus the OS and page \
             cache",
            GLOBAL_RESIDENT_CAP_BYTES >> 20
        )
    });
    Ok((bytes, warning))
}

/// Total RAM of this machine, when known (Linux `/proc/meminfo`).
pub fn total_ram_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// The process-wide cap in force ([`GLOBAL_RESIDENT_CAP_BYTES`] unless
/// [`RESIDENT_CAP_ENV`] overrides it; see [`resident_cap_from`]). Logs a
/// warning when the override is above the recommended default.
pub fn global_resident_cap() -> Result<usize, String> {
    let env = std::env::var(RESIDENT_CAP_ENV).ok();
    let (cap, warning) = resident_cap_from(env.as_deref(), total_ram_bytes())?;
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    Ok(cap)
}

/// Resident bytes of every live [`AuthoredRasters`] in this process.
pub fn global_resident_bytes() -> usize {
    GLOBAL_RESIDENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Regions must keep this far from the map rim, where the column sampler has
/// no spline knots.
pub const RIM_MARGIN_M: i32 = 64;
/// Largest feather the manifest may ask for.
pub const MAX_FEATHER_M: i32 = 128;
/// Top block of the engine's ocean: the column sampler fills `z <
/// sea_level - 1 + 0.01`. Authored water and banks may not go below it.
pub const SEA_TOP_BLOCK: i32 = (CONFIG.sea_level - 1.0 + 0.01) as i32;
/// How far below the lowest authored ground block a chunk's stone floor
/// (`base_z`) must reach; the same margin `SimChunk::get_base_z` keeps below
/// a chunk's own altitude.
pub const FLOOR_MARGIN_BLOCKS: i32 = 16;
/// Decorations rooted inside a region can reach this far outside its box:
/// every authored cell must lie at least this far inside the box edge (a
/// load error otherwise).
pub const REGION_MARGIN_M: i32 = 16;

const BYTES_PER_DIST_TILE: usize = TILE_CELLS;
const NO_DIST: u8 = u8::MAX;
const CHUNK: i32 = TerrainChunkSize::RECT_SIZE.x as i32;

// --------------------------------------------------------------------------
// Manifest
// --------------------------------------------------------------------------

/// `<map>_authored_rasters.ron`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub regions: Vec<RegionManifest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegionManifest {
    pub id: String,
    /// Inclusive south-west corner, wpos (multiple of 32).
    pub min: (i32, i32),
    /// Exclusive north-east corner, wpos (multiple of 32).
    pub max: (i32, i32),
    pub feather_m: i32,
    pub tile_size_m: i32,
    pub cell_size_m: i32,
    pub layers: Vec<LayerKind>,
    pub tiles: Vec<TileManifest>,
    /// Opt-in: no procedural boulders, trees or sprites rooted in authored
    /// wet columns. Default **off**: the natural map's river rocks, bank
    /// trees and bushes stay (edits start from the natural map).
    #[serde(default)]
    pub suppress_procedural_in_water: bool,
    /// With [`Self::suppress_procedural_in_water`]: also suppress them on dry
    /// columns closer than this to authored water. Default 0.
    #[serde(default)]
    pub exclude_procedural_margin_m: i32,
    /// Every chunk of the region keeps the natural map in its unauthored
    /// columns (see [`Self::allow_partial_chunks`]).
    #[serde(default)]
    pub allow_partial: bool,
    /// Chunks (chunk coordinates, inside the box) whose unauthored columns
    /// keep the natural map -- engine terrain *and* the sim's water, exactly
    /// as without the region -- so a few authored cells can edit an existing
    /// river or lake. They are exempt from
    /// [`ConsistencyBudget::max_authored_dry_table_wet_chunks`]: their table
    /// water is the natural water those columns render.
    #[serde(default)]
    pub allow_partial_chunks: Vec<(i32, i32)>,
    /// Aquatic fauna/flora profile id (`cromatolis_v0_aquatic_ecology.ron`)
    /// for authored water whose chunk carries none.
    #[serde(default)]
    pub aquatic_ecology_profile: Option<String>,
    #[serde(default)]
    pub consistency: ConsistencyBudget,
    /// Water below sea level in this region (default [`SeaFill::Auto`]).
    ///
    /// Every field added after Stage 1 is skipped when serialised at its
    /// default: the manifest digest is the sha256 of its serialisation, so a
    /// new field written at its default would change the digest of every
    /// existing manifest (terrain persistence would refuse to start, the
    /// rtsim water pass would re-run).
    #[serde(default, skip_serializing_if = "SeaFill::is_auto")]
    pub sea_fill: SeaFill,
}

/// Where water below sea level comes from inside a region.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum SeaFill {
    /// Today's behaviour: the sea fills everything below sea level. Ground
    /// cells below sea level are allowed only in chunks the sim table calls
    /// ocean or underwater (a sea floor, a dredged harbour basin) and fill
    /// with sea water; wet surfaces and banks must stay at or above the
    /// ocean's top block.
    #[default]
    Auto,
    /// The authored layers are the only source of water in the region:
    /// ground and bank cells are dry at any altitude (a dry pit below 0 m)
    /// and wet cells may have a surface below sea level (a lake below 0 m).
    /// Unauthored cells keep the Stage-1 rule (engine terrain, no water
    /// above sea level).
    AuthoredOnly,
}

impl SeaFill {
    pub fn is_auto(&self) -> bool { *self == SeaFill::Auto }
}

/// How far a region's authored water may disagree with the sim's chunk
/// table (the water and elevated masks), counted in chunks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConsistencyBudget {
    /// Chunks the table calls wet (river, lake, ocean or underwater) that are
    /// not fully authored (some column has neither water, bank nor ground).
    /// Default
    /// **0**: the sim lowers such a chunk's terrain, and the unauthored
    /// columns in it render that pit next to the exact authored water (the
    /// masks were painted inside the region). A chunk counts as authored when
    /// every column is a wet or bank cell: write bank cells for the ground the
    /// author intends around a channel. Exempt: partial chunks
    /// ([`RegionManifest::allow_partial_chunks`],
    /// [`RegionManifest::allow_partial`]), reported, not counted.
    #[serde(default)]
    pub max_authored_dry_table_wet_chunks: u32,
    /// Chunks with authored wet columns that the table calls dry. Default
    /// unlimited: that is the normal state when the exporter leaves the masks
    /// untouched (the consumers ask the authored-aware queries).
    #[serde(default)]
    pub max_authored_wet_table_dry_chunks: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TileManifest {
    pub layer: LayerKind,
    pub tx: i32,
    pub ty: i32,
    pub sha256: String,
}

impl FileAsset for Manifest {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

/// Raw bytes of one tile file (`.bin`), checked against the manifest.
struct TileBytes(Vec<u8>);

impl FileAsset for TileBytes {
    const EXTENSION: &'static str = "bin";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { Ok(Self(bytes.into_owned())) }
}

/// Asset specifier of the manifest of `map_asset`.
pub fn manifest_specifier(map_asset: &str) -> String { format!("{map_asset}_authored_rasters") }

/// Asset specifier of one tile of `map_asset`.
pub fn tile_specifier(map_asset: &str, region: &str, layer: LayerKind, tx: i32, ty: i32) -> String {
    format!("{map_asset}_ar_{region}_{}_{tx}_{ty}", layer.asset_name())
}

// --------------------------------------------------------------------------
// Cells and columns
// --------------------------------------------------------------------------

/// One authored column, as the column sampler sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuthoredCell {
    /// Water whose top block is `surface_block` over ground whose top block
    /// is `bed_block` (`bed_block < surface_block`).
    Wet { surface_block: i32, bed_block: i32 },
    /// Authored dry ground whose top block is `bed_block`.
    Bank { bed_block: i32 },
    /// Authored ground whose top block is `block`, with its blend `weight`
    /// ([`GROUND_EXACT`] = exact: the column's ground, no engine terrain).
    /// Only where the water layer authors nothing.
    Ground { block: i32, weight: u8 },
    /// Inside a region, but not authored: engine terrain without the
    /// region's suppressed sim water, or the natural map unchanged in a
    /// partial chunk ([`AuthoredColumn::natural`]).
    None,
}

impl AuthoredCell {
    pub fn is_wet(&self) -> bool { matches!(self, AuthoredCell::Wet { .. }) }

    /// The top ground block this cell fixes, if it fixes one exactly (a wet
    /// bed, a bank, an exact ground cell).
    pub fn exact_ground_block(&self) -> Option<i32> {
        match *self {
            AuthoredCell::Wet { bed_block, .. } | AuthoredCell::Bank { bed_block } => {
                Some(bed_block)
            },
            AuthoredCell::Ground { block, weight } => (weight == GROUND_EXACT).then_some(block),
            AuthoredCell::None => None,
        }
    }
}

/// The column altitude whose top solid block is `block`. `block.rs` makes
/// `z` solid when `z as i32 <= alt as i32`, and `as i32` truncates toward
/// zero: `block + 0.5` has top block `block` only for `block >= 0`; below
/// block 0 (more than 140 m under sea level) the altitude is the block
/// itself, whose truncation and floor are both `block`.
#[inline]
pub fn top_block_alt(block: i32) -> f32 {
    if block >= 0 {
        block as f32 + 0.5
    } else {
        block as f32
    }
}

/// Authored water at one column (the wet case of [`AuthoredCell`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthoredWater {
    /// Top water block.
    pub surface_block: i32,
    /// Top ground block under the water.
    pub bed_block: i32,
}

impl AuthoredWater {
    /// Number of water blocks.
    pub fn depth_blocks(&self) -> i32 { self.surface_block - self.bed_block }

    /// Altitude of the top of the water (the top face of the top water
    /// block), the quantity `SimChunk::water_alt` means for the sim's own
    /// water (sea level 140 over top water block 139).
    pub fn surface_alt(&self) -> f32 { (self.surface_block + 1) as f32 }
}

/// Per-region settings every column of the region carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionSettings {
    pub suppress_procedural_in_water: bool,
    pub exclude_procedural_margin_m: f32,
    pub(crate) aquatic_profile: Option<AquaticEcologyProfileId>,
    pub sea_fill: SeaFill,
}

/// What the column sampler needs to know about one column inside a region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoredColumn {
    /// Feather weight: 1 in the region's core, smoothstepping to 0 at its
    /// bbox edge. Only unauthored ([`AuthoredCell::None`]) terrain is
    /// blended by it; authored cells are exact at any weight.
    pub weight: f32,
    pub cell: AuthoredCell,
    /// Distance from the column centre to the nearest authored wet column
    /// centre, metres (chamfer 3-4), `None` beyond [`DIST_CAP_M`]. Not
    /// computed (`Some(0.0)`) for wet columns.
    pub water_dist: Option<f32>,
    pub settings: RegionSettings,
    /// The column's chunk is partial
    /// ([`RegionManifest::allow_partial_chunks`]): an unauthored column there
    /// is the natural engine column, untouched.
    pub natural: bool,
}

/// The engine's own per-column results that an authored column replaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineColumn {
    pub alt: f32,
    pub water_level: f32,
    pub water_dist: Option<f32>,
    pub warp_factor: f32,
    pub cliff_offset: f32,
    pub riverless_alt: f32,
}

/// The engine inputs needed to rebuild a column's terrain without the
/// region's suppressed sim water (see [`AuthoredColumn::resolve`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DryTerrain {
    /// Spline altitude after site flattening, before any water carving.
    pub riverless_alt: f32,
    /// The engine's procedural delta (`riverless_alt_delta` + cliffs)
    /// *before* it is faded by the warp factor.
    pub riverless_alt_delta: f32,
    /// The engine's warp offset before it is faded by the warp factor.
    pub warp: f32,
    pub max_warp: f32,
    pub base_sea_level: f32,
}

/// The column sampler's hook: replace `engine` by the authored result when
/// the column is authored, and re-apply the regional flood (`flood`) on top
/// of the authored water level. `None`, and an unauthored column of a
/// partial chunk, return `engine` untouched.
#[inline]
pub fn apply(
    authored: Option<AuthoredColumn>,
    engine: EngineColumn,
    dry: DryTerrain,
    flood: impl FnOnce(f32) -> f32,
) -> EngineColumn {
    match authored {
        None => engine,
        // The natural map in a partial chunk: the engine column exactly.
        Some(AuthoredColumn {
            cell: AuthoredCell::None,
            natural: true,
            ..
        }) => engine,
        Some(authored) => {
            let mut r = authored.resolve(engine, dry);
            r.water_level = flood(r.water_level);
            r
        },
    }
}

impl AuthoredColumn {
    /// Replace the engine's results for this column:
    ///
    /// * wet: bed and surface exact, no warp/cliff carving; `riverless_alt` is
    ///   the bed (paths and trees reading it see the authored ground);
    /// * bank, exact ground: ground exact, no warp/cliff carving, no water
    ///   above sea level ([`SeaFill::Auto`]: below it the sea fills the column)
    ///   or no water at all ([`SeaFill::AuthoredOnly`]);
    /// * blend ground (weight `w` of 255): altitude, warp, cliff and
    ///   `riverless_alt` lerped from the column this region gives without the
    ///   ground layer (the unauthored result below, or the natural column in a
    ///   partial chunk) toward the authored block; water as on exact ground;
    /// * none: no water above sea level; terrain blended by the feather weight
    ///   from the engine's (with the sim water's carving) to the engine's *dry*
    ///   terrain (same noise and warp, faded by the distance to the authored
    ///   water instead of the sim's). The water level is *not* blended, at any
    ///   weight: inside a region only authored water exists (a fractional level
    ///   would only move the cut-off of the sim's water somewhere less
    ///   predictable), so sim water crossing the box edge ends at the edge
    ///   unless the raster continues it (the exporter's seam rule).
    pub fn resolve(&self, engine: EngineColumn, dry: DryTerrain) -> EngineColumn {
        match self.cell {
            AuthoredCell::Wet {
                surface_block,
                bed_block,
            } => {
                let authored_only = self.settings.sea_fill == SeaFill::AuthoredOnly;
                // Guaranteed by load-time validation.
                debug_assert!(
                    (authored_only || surface_block >= SEA_TOP_BLOCK) && bed_block < surface_block
                );
                let surface = surface_block as f32 + 0.5;
                EngineColumn {
                    alt: top_block_alt(bed_block),
                    // A lake below 0 m keeps its own surface; elsewhere the
                    // sea still fills up to sea level.
                    water_level: if authored_only {
                        surface
                    } else {
                        surface.max(dry.base_sea_level)
                    },
                    water_dist: Some(-1.0),
                    warp_factor: 0.0,
                    cliff_offset: 0.0,
                    riverless_alt: top_block_alt(bed_block),
                }
            },
            // Exact ground is a bank without water beside it: both fix the
            // column's ground and nothing else.
            AuthoredCell::Bank { bed_block: block }
            | AuthoredCell::Ground {
                block,
                weight: GROUND_EXACT,
            } => {
                let alt = top_block_alt(block);
                EngineColumn {
                    alt,
                    water_level: self.ground_water_level(alt, dry.base_sea_level),
                    water_dist: self.water_dist,
                    warp_factor: 0.0,
                    cliff_offset: 0.0,
                    riverless_alt: alt,
                }
            },
            // A blend cell of the ring around a patch: from the terrain the
            // region renders here without the ground layer (`e`) toward the
            // authored block, by `weight / 255`. Water as on exact ground
            // (the ring is authored land; the load-time containment rule keeps
            // it away from authored water).
            AuthoredCell::Ground { block, weight } => {
                let e = if self.natural {
                    engine
                } else {
                    self.unauthored(engine, dry)
                };
                let w = weight as f32 / GROUND_EXACT as f32;
                let g = top_block_alt(block);
                let alt = Lerp::lerp(e.alt, g, w);
                EngineColumn {
                    alt,
                    water_level: self.ground_water_level(alt, dry.base_sea_level),
                    water_dist: e.water_dist,
                    warp_factor: Lerp::lerp(e.warp_factor, 0.0, w),
                    cliff_offset: e.cliff_offset * (1.0 - w),
                    riverless_alt: Lerp::lerp(e.riverless_alt, g, w),
                }
            },
            AuthoredCell::None if self.natural => engine,
            AuthoredCell::None => self.unauthored(engine, dry),
        }
    }

    /// The water level of a dry authored column (bank, exact or blend
    /// ground) whose altitude is `alt`: the sea's in [`SeaFill::Auto`] (below
    /// sea level the sea fills it), the ground itself in
    /// [`SeaFill::AuthoredOnly`] -- a level at the ground, so the column fill
    /// writes no water (not a NaN/MIN sentinel: `get_z_limits` takes a max
    /// over it).
    #[inline]
    fn ground_water_level(&self, alt: f32, base_sea_level: f32) -> f32 {
        match self.settings.sea_fill {
            SeaFill::Auto => base_sea_level,
            SeaFill::AuthoredOnly => alt,
        }
    }

    /// An unauthored column of an owned chunk: no water above sea level;
    /// terrain blended by the feather weight from the engine's (with the sim
    /// water's carving) to the engine's *dry* terrain (same noise and warp,
    /// faded by the distance to the authored water instead of the sim's).
    fn unauthored(&self, engine: EngineColumn, dry: DryTerrain) -> EngineColumn {
        let w = self.weight;
        let dry_wf = self
            .water_dist
            .map_or(1.0, |d| (d / DIST_CAP_M as f32).clamped(0.0, 1.0))
            * dry.max_warp;
        let dry_alt = dry.riverless_alt.max(dry.base_sea_level + 0.5)
            + Lerp::lerp(0.0, dry.riverless_alt_delta, dry_wf)
            + dry.warp * dry_wf;
        let water_dist = if w >= 1.0 {
            self.water_dist
        } else {
            match (engine.water_dist, self.water_dist) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        };
        EngineColumn {
            alt: Lerp::lerp(engine.alt, dry_alt, w),
            water_level: dry.base_sea_level,
            water_dist,
            warp_factor: Lerp::lerp(engine.warp_factor, dry_wf, w),
            cliff_offset: engine.cliff_offset,
            riverless_alt: engine.riverless_alt,
        }
    }

    /// The snow height the column sampler adds on top of this column's
    /// ground: none on an exact ground cell (the top block turns to snow, it
    /// does not grow one), faded by the weight on a blend cell, unchanged
    /// everywhere else (banks keep the Stage-1 behaviour: one block of snow
    /// may lie on them).
    #[inline]
    pub fn ground_snow_height(&self, snow_height: f32) -> f32 {
        match self.cell {
            AuthoredCell::Ground {
                weight: GROUND_EXACT,
                ..
            } => 0.0,
            AuthoredCell::Ground { weight, .. } => {
                snow_height * (1.0 - weight as f32 / GROUND_EXACT as f32)
            },
            AuthoredCell::Wet { .. } | AuthoredCell::Bank { .. } | AuthoredCell::None => {
                snow_height
            },
        }
    }

    /// Whether procedural boulders, trees and sprites must not be rooted in
    /// this column ([`RegionManifest::suppress_procedural_in_water`],
    /// [`RegionManifest::exclude_procedural_margin_m`]).
    pub fn procedural_suppressed(&self) -> bool {
        self.settings.suppress_procedural_in_water
            && match self.cell {
                AuthoredCell::Wet { .. } => true,
                AuthoredCell::Bank { .. } | AuthoredCell::Ground { .. } | AuthoredCell::None => {
                    self.water_dist
                        .is_some_and(|d| d < self.settings.exclude_procedural_margin_m)
                },
            }
    }

    /// The region's aquatic ecology profile, for a wet column.
    pub(crate) fn wet_aquatic_profile(&self) -> Option<AquaticEcologyProfileId> {
        self.cell
            .is_wet()
            .then_some(self.settings.aquatic_profile)
            .flatten()
    }
}

/// Per-chunk summary of the authored cells (water, banks and ground),
/// computed once at load.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkWaterSummary {
    pub wet_columns: u32,
    /// Wet, bank and ground cells.
    pub authored_columns: u32,
    /// Ground-layer cells (where the water layer authors nothing).
    pub ground_columns: u32,
    /// Ground-layer cells below the ocean's top block in a [`SeaFill::Auto`]
    /// region: a sea floor, under sea water.
    pub sea_ground_columns: u32,
    /// `i32::MAX` / `i32::MIN` when the chunk has no wet column.
    pub min_surface_block: i32,
    pub max_surface_block: i32,
    /// Lowest authored bed or bank block (`i32::MAX` when none).
    pub min_bed_block: i32,
    /// Lowest ground-layer block (`i32::MAX` when the chunk has no ground
    /// cell): the below-sea-level rule of [`SeaFill::Auto`] reads it.
    pub min_ground_cell_block: i32,
}

impl Default for ChunkWaterSummary {
    fn default() -> Self {
        Self {
            wet_columns: 0,
            authored_columns: 0,
            ground_columns: 0,
            sea_ground_columns: 0,
            min_surface_block: i32::MAX,
            max_surface_block: i32::MIN,
            min_bed_block: i32::MAX,
            min_ground_cell_block: i32::MAX,
        }
    }
}

impl ChunkWaterSummary {
    fn merge(&mut self, o: &Self) {
        self.wet_columns += o.wet_columns;
        self.authored_columns += o.authored_columns;
        self.ground_columns += o.ground_columns;
        self.sea_ground_columns += o.sea_ground_columns;
        self.min_surface_block = self.min_surface_block.min(o.min_surface_block);
        self.max_surface_block = self.max_surface_block.max(o.max_surface_block);
        self.min_bed_block = self.min_bed_block.min(o.min_bed_block);
        self.min_ground_cell_block = self.min_ground_cell_block.min(o.min_ground_cell_block);
    }

    /// Lowest authored ground block of any kind: wet bed, bank or ground
    /// cell (`i32::MAX` when none).
    pub fn min_ground_block(&self) -> i32 { self.min_bed_block.min(self.min_ground_cell_block) }

    /// Columns under water: authored wet cells plus sea-floor ground cells.
    pub fn water_columns(&self) -> u32 { self.wet_columns + self.sea_ground_columns }

    /// At least half of the chunk's columns are under authored water (or the
    /// sea over an authored sea floor).
    pub fn wet_majority(&self) -> bool { self.water_columns() * 2 >= (CHUNK * CHUNK) as u32 }
}

// --------------------------------------------------------------------------
// Regions
// --------------------------------------------------------------------------

struct Region {
    id: String,
    min: Vec2<i32>,
    max: Vec2<i32>,
    feather: i32,
    tiles: Vec2<i32>,
    settings: RegionSettings,
    budget: ConsistencyBudget,
    /// One flag per chunk of the box (row-major from the south-west):
    /// unauthored columns keep the natural map.
    partial: Vec<bool>,
    /// One slot per tile of the region grid and layer, decoded once at
    /// load; `None` = unlisted (no authored cell of that layer). A
    /// `[Water, Ground]` region holds both tiles at the same index.
    water: Vec<Option<WaterTile>>,
    ground: Vec<Option<GroundTile>>,
    /// Distance-to-water fields, one per tile of the grid.
    dist: Vec<OnceLock<Option<Box<[u8]>>>>,
}

impl Region {
    #[inline(always)]
    fn contains(&self, wpos: Vec2<i32>) -> bool {
        wpos.x >= self.min.x && wpos.y >= self.min.y && wpos.x < self.max.x && wpos.y < self.max.y
    }

    #[inline(always)]
    fn tile_index(&self, t: Vec2<i32>) -> Option<usize> {
        (t.x >= 0 && t.y >= 0 && t.x < self.tiles.x && t.y < self.tiles.y)
            .then(|| (t.y * self.tiles.x + t.x) as usize)
    }

    fn origin(&self, t: Vec2<i32>) -> Vec2<i32> { self.min + t * TILE_SIZE }

    /// Whether the chunk holding `wpos` (inside the region) is partial.
    #[inline(always)]
    fn partial_at(&self, wpos: Vec2<i32>) -> bool {
        let c = (wpos - self.min).map(|e| e.div_euclid(CHUNK));
        let w = (self.max.x - self.min.x) / CHUNK;
        self.partial[(c.y * w + c.x) as usize]
    }

    #[inline(always)]
    fn raw(&self, idx: usize) -> Option<&WaterTile> { self.water[idx].as_ref() }

    /// Cell state at `wpos` (must be inside the region).
    #[inline]
    fn cell(&self, wpos: Vec2<i32>) -> AuthoredCell {
        let local = wpos - self.min;
        let t = local.map(|e| e.div_euclid(TILE_SIZE));
        let Some(idx) = self.tile_index(t) else {
            return AuthoredCell::None;
        };
        let c = local - t * TILE_SIZE;
        cell_of(
            self.water[idx].as_ref(),
            self.ground[idx].as_ref(),
            cell_index(c.x, c.y),
        )
    }

    fn weight(&self, wpos: Vec2<i32>) -> f32 {
        if self.feather <= 0 {
            return 1.0;
        }
        let d = (wpos.x - self.min.x)
            .min(self.max.x - 1 - wpos.x)
            .min(wpos.y - self.min.y)
            .min(self.max.y - 1 - wpos.y);
        let t = ((d as f32 + 0.5) / self.feather as f32).clamped(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// The 4-neighbours of this region's ground cells (exact or blend)
    /// whose water the post-civ seam check must compare with the cell
    /// ([`queries::check_ground_seams`]): unauthored columns of partial
    /// chunks (the natural map, rivers and sea included), and, around
    /// [`SeaFill::AuthoredOnly`] ground below the ocean's top block, every
    /// unauthored column (the feather can bring the sea into an owned one).
    /// Pairs `(ground cell, neighbour)`, in tile order.
    fn ground_seam_pairs(&self) -> Vec<(Vec2<i32>, Vec2<i32>)> {
        let authored_only = self.settings.sea_fill == SeaFill::AuthoredOnly;
        if !authored_only && !self.partial.iter().any(|&p| p) {
            return Vec::new();
        }
        use rayon::prelude::*;
        let per_tile = |idx: usize| {
            let mut out = Vec::new();
            let Some(g) = self.ground[idx].as_ref() else {
                return out;
            };
            let t = Vec2::new(idx as i32 % self.tiles.x, idx as i32 / self.tiles.x);
            let origin = self.origin(t);
            for j in 0..TILE_SIZE {
                for i in 0..TILE_SIZE {
                    let wpos = origin + Vec2::new(i, j);
                    let Some((block, _)) = g.get(cell_index(i, j)) else {
                        continue;
                    };
                    // A water cell wins over the ground under it.
                    if !self.contains(wpos)
                        || !matches!(self.cell(wpos), AuthoredCell::Ground { .. })
                    {
                        continue;
                    }
                    let low_dry = authored_only && block < SEA_TOP_BLOCK;
                    for d in [
                        Vec2::new(1, 0),
                        Vec2::new(-1, 0),
                        Vec2::new(0, 1),
                        Vec2::new(0, -1),
                    ] {
                        let n = wpos + d;
                        if self.contains(n)
                            && self.cell(n) == AuthoredCell::None
                            && (low_dry || self.partial_at(n))
                        {
                            out.push((wpos, n));
                        }
                    }
                }
            }
            out
        };
        (0..self.ground.len())
            .into_par_iter()
            .map(per_tile)
            .collect::<Vec<_>>()
            .concat()
    }

    fn dist_field(&self, idx: usize) -> Option<&[u8]> {
        self.dist[idx]
            .get_or_init(|| {
                let t = Vec2::new(idx as i32 % self.tiles.x, idx as i32 / self.tiles.x);
                build_dist_field(self, t)
            })
            .as_deref()
    }

    fn water_dist(&self, wpos: Vec2<i32>) -> Option<f32> {
        let local = wpos - self.min;
        let t = local.map(|e| e.div_euclid(TILE_SIZE));
        let field = self.dist_field(self.tile_index(t)?)?;
        let c = local - t * TILE_SIZE;
        let d = field[cell_index(c.x, c.y)];
        (d != NO_DIST).then(|| d as f32 / 3.0)
    }
}

/// The one place the layers' precedence is decided: a water cell (wet or
/// bank) wins; a ground cell under it is checked at load to agree with it.
#[inline(always)]
fn cell_of(water: Option<&WaterTile>, ground: Option<&GroundTile>, k: usize) -> AuthoredCell {
    if let Some(raw) = water {
        match (raw.surface_cm(k), raw.bed_cm(k)) {
            (Some(s), Some(b)) => {
                return AuthoredCell::Wet {
                    surface_block: s.div_euclid(100),
                    bed_block: b.div_euclid(100),
                };
            },
            (None, Some(b)) => {
                return AuthoredCell::Bank {
                    bed_block: b.div_euclid(100),
                };
            },
            // `(Some, None)` is refused at load.
            (Some(_), None) | (None, None) => {},
        }
    }
    match ground.and_then(|g| g.get(k)) {
        Some((block, weight)) => AuthoredCell::Ground { block, weight },
        None => AuthoredCell::None,
    }
}

/// Chamfer (3-4) distance from every cell of tile `t` to the nearest wet
/// cell, looking up to [`DIST_CAP_M`] into the eight neighbouring tiles.
/// Integer arithmetic only, so the field is the same on every platform.
fn build_dist_field(region: &Region, t: Vec2<i32>) -> Option<Box<[u8]>> {
    let apron = DIST_CAP_M;
    let n = TILE_SIZE + 2 * apron;
    let any_listed = (-1..=1).any(|dy| {
        (-1..=1).any(|dx| {
            region
                .tile_index(t + Vec2::new(dx, dy))
                .is_some_and(|i| region.water[i].is_some())
        })
    });
    if !any_listed {
        return None;
    }
    const INF: u16 = u16::MAX;
    let mut d = vec![INF; (n * n) as usize];
    let origin = region.origin(t) - apron;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let nt = t + Vec2::new(dx, dy);
            let Some(raw) = region.tile_index(nt).and_then(|i| region.raw(i)) else {
                continue;
            };
            let o = region.origin(nt);
            for j in 0..TILE_SIZE {
                let wy = o.y + j - origin.y;
                if !(0..n).contains(&wy) {
                    continue;
                }
                for i in 0..TILE_SIZE {
                    let wx = o.x + i - origin.x;
                    if (0..n).contains(&wx) && raw.is_wet(cell_index(i, j)) {
                        d[(wy * n + wx) as usize] = 0;
                    }
                }
            }
        }
    }
    let at = |x: i32, y: i32| (y * n + x) as usize;
    let relax = |d: &mut Vec<u16>, x: i32, y: i32, nx: i32, ny: i32, w: u16| {
        if nx >= 0 && ny >= 0 && nx < n && ny < n {
            let v = d[at(nx, ny)].saturating_add(w);
            if v < d[at(x, y)] {
                d[at(x, y)] = v;
            }
        }
    };
    for y in 0..n {
        for x in 0..n {
            relax(&mut d, x, y, x - 1, y, 3);
            relax(&mut d, x, y, x, y - 1, 3);
            relax(&mut d, x, y, x - 1, y - 1, 4);
            relax(&mut d, x, y, x + 1, y - 1, 4);
        }
    }
    for y in (0..n).rev() {
        for x in (0..n).rev() {
            relax(&mut d, x, y, x + 1, y, 3);
            relax(&mut d, x, y, x, y + 1, 3);
            relax(&mut d, x, y, x + 1, y + 1, 4);
            relax(&mut d, x, y, x - 1, y + 1, 4);
        }
    }
    let cap = (DIST_CAP_M * 3) as u16;
    let mut out = vec![NO_DIST; TILE_CELLS];
    for j in 0..TILE_SIZE {
        for i in 0..TILE_SIZE {
            let v = d[at(i + apron, j + apron)];
            if v <= cap {
                out[cell_index(i, j)] = v as u8;
            }
        }
    }
    Some(out.into_boxed_slice())
}

// --------------------------------------------------------------------------
// Loaded rasters
// --------------------------------------------------------------------------

/// Up to this many regions, `region_at` scans them linearly after the union
/// box check (a few ns); above it, a [`RegionIndex`] narrows the scan.
pub const LINEAR_SCAN_MAX_REGIONS: usize = 16;
const INDEX_CELL_LG: i32 = 8;

/// Regions intersecting each 256 m cell of the union box (flattened lists).
struct RegionIndex {
    origin: Vec2<i32>,
    cells: Vec2<i32>,
    /// `offsets[c]..offsets[c + 1]` indexes `ids` for cell `c`.
    offsets: Vec<u32>,
    ids: Vec<u16>,
}

impl RegionIndex {
    fn new(regions: &[Region], bounds: Aabr<i32>) -> Self {
        let origin = bounds.min.map(|e| e >> INDEX_CELL_LG);
        let cells = bounds.max.map(|e| (e - 1) >> INDEX_CELL_LG) - origin + 1;
        let mut lists: Vec<Vec<u16>> = vec![Vec::new(); (cells.x * cells.y) as usize];
        for (i, r) in regions.iter().enumerate() {
            let c0 = r.min.map(|e| e >> INDEX_CELL_LG) - origin;
            let c1 = r.max.map(|e| (e - 1) >> INDEX_CELL_LG) - origin;
            for cy in c0.y..=c1.y {
                for cx in c0.x..=c1.x {
                    lists[(cy * cells.x + cx) as usize].push(i as u16);
                }
            }
        }
        let mut offsets = Vec::with_capacity(lists.len() + 1);
        let mut ids = Vec::new();
        offsets.push(0);
        for l in lists {
            ids.extend(l);
            offsets.push(ids.len() as u32);
        }
        Self {
            origin,
            cells,
            offsets,
            ids,
        }
    }

    /// Regions that may contain `wpos` (inside the union box).
    #[inline(always)]
    fn candidates(&self, wpos: Vec2<i32>) -> &[u16] {
        let c = wpos.map(|e| e >> INDEX_CELL_LG) - self.origin;
        let k = (c.y * self.cells.x + c.x) as usize;
        &self.ids[self.offsets[k] as usize..self.offsets[k + 1] as usize]
    }
}

/// All authored rasters of one world. `None` on `WorldSim` when the map has
/// no manifest.
pub struct AuthoredRasters {
    regions: Vec<Region>,
    /// Union of the region boxes (min inclusive, max exclusive), checked
    /// first so every column outside all regions costs four comparisons.
    bounds: Aabr<i32>,
    /// With more than [`LINEAR_SCAN_MAX_REGIONS`] regions: which regions
    /// intersect each 256 m cell of the union box, so a lookup scans only
    /// those (two far-apart regions make the union box cover much of the map).
    index: Option<RegionIndex>,
    chunk_summary: HashMap<Vec2<i32>, ChunkWaterSummary>,
    /// sha256 of the canonical manifest (it pins every tile by sha256).
    digest: String,
    /// What this manifest adds to the process-wide counter (released on
    /// drop).
    resident_bytes: usize,
}

impl Drop for AuthoredRasters {
    fn drop(&mut self) {
        GLOBAL_RESIDENT.fetch_sub(self.resident_bytes, std::sync::atomic::Ordering::Relaxed);
    }
}

impl std::fmt::Debug for AuthoredRasters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthoredRasters")
            .field(
                "regions",
                &self.regions.iter().map(|r| &r.id).collect::<Vec<_>>(),
            )
            .field("bounds", &self.bounds)
            .field("digest", &self.digest)
            .finish()
    }
}

/// A reservation on the process-wide counter, released on drop unless kept.
struct ReservedBytes(usize);

impl ReservedBytes {
    /// Reserve `bytes` against `cap`, or explain why not (nothing is left
    /// reserved on refusal).
    fn reserve(bytes: usize, cap: usize) -> Result<Self, String> {
        let before = GLOBAL_RESIDENT.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        if before + bytes > cap {
            GLOBAL_RESIDENT.fetch_sub(bytes, std::sync::atomic::Ordering::Relaxed);
            return Err(format!(
                "loading it would bring the authored rasters of every world in this process to {} \
                 MiB, over the process-wide cap of {} MiB (set XINDELER_AUTHORED_RASTERS_MAX_MIB \
                 to raise it)",
                (before + bytes) >> 20,
                cap >> 20
            ));
        }
        Ok(Self(bytes))
    }

    fn keep(mut self) -> usize { std::mem::take(&mut self.0) }
}

impl Drop for ReservedBytes {
    fn drop(&mut self) { GLOBAL_RESIDENT.fetch_sub(self.0, std::sync::atomic::Ordering::Relaxed); }
}

/// Why a manifest was refused. Always fatal for world generation.
#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

/// Counts of [`AuthoredRasters::check_consistency`] for one region.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionConsistency {
    pub region: String,
    pub authored_wet_table_dry: Vec<Vec2<i32>>,
    pub authored_dry_table_wet: Vec<Vec2<i32>>,
    /// Table-wet, not fully authored chunks the region declares partial:
    /// their unauthored columns render the natural water. Exempt from the
    /// budget; reported.
    pub partial_table_wet: Vec<Vec2<i32>>,
}

impl AuthoredRasters {
    /// Load `<map_asset>_authored_rasters` through the asset system.
    ///
    /// `Ok(None)`: the manifest does not exist and `required` is false (the
    /// map has no authored water; the old path). `Err`: it exists but is
    /// wrong in any way, or is required and missing -- the caller must stop
    /// world generation.
    pub fn load_for_map(
        map_asset: &str,
        map_size_blocks: Vec2<i32>,
        required: bool,
    ) -> Result<Option<Self>, LoadError> {
        let specifier = manifest_specifier(map_asset);
        let manifest = match Manifest::load_owned(&specifier) {
            Ok(manifest) => manifest,
            Err(err) => match io_error_kind(&err) {
                Some(std::io::ErrorKind::NotFound) if !required => return Ok(None),
                Some(std::io::ErrorKind::NotFound) => {
                    return Err(LoadError(format!(
                        "authored rasters [{specifier}]: the map requires an authored raster \
                         manifest and none was found"
                    )));
                },
                Some(kind) => {
                    return Err(LoadError(format!(
                        "authored rasters [{specifier}]: I/O error reading the manifest \
                         ({kind:?}): {:?}",
                        err.reason()
                    )));
                },
                None => {
                    return Err(LoadError(format!(
                        "authored rasters [{specifier}]: the manifest does not parse (schema \
                         {MANIFEST_SCHEMA}; unknown fields and layers are errors): {:?}",
                        err.reason()
                    )));
                },
            },
        };
        let fetch = |region: &str, layer: LayerKind, tx: i32, ty: i32| {
            let spec = tile_specifier(map_asset, region, layer, tx, ty);
            TileBytes::load_owned(&spec)
                .map(|t| t.0)
                .map_err(|e| format!("tile '{spec}' cannot be loaded: {:?}", e.reason()))
        };
        Self::from_manifest(manifest, map_size_blocks, &specifier, &fetch).map(Some)
    }

    /// Validate `manifest` and every tile `fetch` returns, and build the
    /// sampler. Asset-free, so tests can feed tiles from memory. `source`
    /// prefixes every error (normally the manifest's asset specifier).
    pub fn from_manifest(
        manifest: Manifest,
        map_size_blocks: Vec2<i32>,
        source: &str,
        fetch: &(dyn Fn(&str, LayerKind, i32, i32) -> Result<Vec<u8>, String> + Sync),
    ) -> Result<Self, LoadError> {
        let err = |msg: String| LoadError(format!("authored rasters [{source}]: {msg}"));
        if manifest.schema != MANIFEST_SCHEMA {
            return Err(err(format!(
                "manifest schema {} (this engine reads {MANIFEST_SCHEMA})",
                manifest.schema
            )));
        }
        if manifest.regions.is_empty() {
            return Err(err("manifest lists no region".into()));
        }
        let digest = format::sha256_hex(
            ron::ser::to_string(&manifest)
                .map_err(|e| err(format!("manifest does not serialise: {e}")))?
                .as_bytes(),
        );
        // The memory budget comes from the manifest's counts alone, before a
        // single tile is read.
        // Per layer: a water tile is 256 KiB, a ground tile 192 KiB; the
        // distance fields (64 KiB per grid tile) exist only for regions that
        // declare water.
        let mut resident = 0usize;
        for rm in &manifest.regions {
            let size = Vec2::<i32>::from(rm.max) - Vec2::<i32>::from(rm.min);
            let grid = size.map(|e| (e.max(0) + TILE_SIZE - 1) / TILE_SIZE);
            resident += rm
                .tiles
                .iter()
                .map(|t| format::resident_tile_bytes(t.layer))
                .sum::<usize>();
            if rm.layers.contains(&LayerKind::Water) {
                resident += (grid.x.max(0) * grid.y.max(0)) as usize * BYTES_PER_DIST_TILE;
            }
        }
        if resident > RESIDENT_BUDGET_BYTES {
            return Err(err(format!(
                "the manifest needs {} MiB resident (decoded tiles + distance fields), over this \
                 engine's limit of {} MiB: split or shrink the regions, or list only tiles that \
                 hold authored cells",
                resident >> 20,
                RESIDENT_BUDGET_BYTES >> 20
            )));
        }
        // Reserve this manifest's share of the process-wide cap now (released
        // by `Drop`, or below on any error).
        let reserved =
            ReservedBytes::reserve(resident, global_resident_cap().map_err(err)?).map_err(err)?;
        let mut regions: Vec<Region> = Vec::new();
        let mut chunk_summary: HashMap<Vec2<i32>, ChunkWaterSummary> = HashMap::new();
        for rm in manifest.regions {
            let id = rm.id.clone();
            let rerr = |msg: String| err(format!("region '{id}': {msg}"));
            if id.is_empty()
                || id.len() > 48
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(rerr("id must be 1-48 characters of [a-z0-9_]".into()));
            }
            if regions.iter().any(|r| r.id == id) {
                return Err(rerr("duplicate region id".into()));
            }
            if rm.tile_size_m != TILE_SIZE || rm.cell_size_m != 1 {
                return Err(rerr(format!(
                    "tile_size_m {} / cell_size_m {} (tile format 1 is {TILE_SIZE} / 1)",
                    rm.tile_size_m, rm.cell_size_m
                )));
            }
            if !matches!(
                rm.layers.as_slice(),
                [LayerKind::Water] | [LayerKind::Ground] | [LayerKind::Water, LayerKind::Ground]
            ) {
                return Err(rerr(format!(
                    "layers {:?}: this engine supports [Water], [Ground] or [Water, Ground] (in \
                     that order)",
                    rm.layers
                )));
            }
            let min: Vec2<i32> = Vec2::from(rm.min);
            let max: Vec2<i32> = Vec2::from(rm.max);
            if min.x % CHUNK != 0 || min.y % CHUNK != 0 || max.x % CHUNK != 0 || max.y % CHUNK != 0
            {
                return Err(rerr(format!(
                    "box {min:?}..{max:?} is not aligned to {CHUNK} m chunks"
                )));
            }
            if max.x <= min.x || max.y <= min.y {
                return Err(rerr(format!("empty box {min:?}..{max:?}")));
            }
            if min.x < RIM_MARGIN_M
                || min.y < RIM_MARGIN_M
                || max.x > map_size_blocks.x - RIM_MARGIN_M
                || max.y > map_size_blocks.y - RIM_MARGIN_M
            {
                return Err(rerr(format!(
                    "box {min:?}..{max:?} is within {RIM_MARGIN_M} m of the map rim \
                     ({map_size_blocks:?})"
                )));
            }
            if !(0..=MAX_FEATHER_M).contains(&rm.feather_m)
                || 2 * rm.feather_m > (max.x - min.x).min(max.y - min.y)
            {
                return Err(rerr(format!(
                    "feather_m {} must be 0..={MAX_FEATHER_M} and at most half of each side",
                    rm.feather_m
                )));
            }
            if !(0..=DIST_CAP_M).contains(&rm.exclude_procedural_margin_m) {
                return Err(rerr(format!(
                    "exclude_procedural_margin_m {} must be 0..={DIST_CAP_M}",
                    rm.exclude_procedural_margin_m
                )));
            }
            let aquatic_profile = match &rm.aquatic_ecology_profile {
                None => None,
                Some(name) => Some(AquaticEcologyProfileId::from_id(name).ok_or_else(|| {
                    rerr(format!(
                        "aquatic_ecology_profile '{name}' is not a profile this engine knows"
                    ))
                })?),
            };
            if let Some(other) = regions
                .iter()
                .find(|r| min.x < r.max.x && r.min.x < max.x && min.y < r.max.y && r.min.y < max.y)
            {
                return Err(rerr(format!("overlaps region '{}'", other.id)));
            }
            let tiles = (max - min).map(|e| (e + TILE_SIZE - 1) / TILE_SIZE);
            let n_tiles = (tiles.x * tiles.y) as usize;
            // The listing first (cheap, ordered errors), then every tile
            // fetched, checked and decoded in parallel on the current pool.
            let mut seen = hashbrown::HashSet::new();
            let mut jobs: Vec<(LayerKind, Vec2<i32>, usize, &str)> = Vec::new();
            for tm in &rm.tiles {
                let t = Vec2::new(tm.tx, tm.ty);
                if t.x < 0 || t.y < 0 || t.x >= tiles.x || t.y >= tiles.y {
                    return Err(rerr(format!(
                        "tile {t:?} lies outside the region grid {tiles:?}"
                    )));
                }
                if !rm.layers.contains(&tm.layer) {
                    return Err(rerr(format!(
                        "tile {t:?} is a {:?} tile, a layer the region does not declare ({:?})",
                        tm.layer, rm.layers
                    )));
                }
                let idx = (t.y * tiles.x + t.x) as usize;
                if !seen.insert((tm.layer, idx)) {
                    return Err(rerr(match tm.layer {
                        LayerKind::Water => format!("tile {t:?} listed twice"),
                        LayerKind::Ground => format!("{:?} tile {t:?} listed twice", tm.layer),
                    }));
                }
                jobs.push((tm.layer, t, idx, tm.sha256.as_str()));
            }
            // Fetch + sha256 check of one listed tile.
            let fetch_checked = |layer: LayerKind, t: Vec2<i32>, want_sha: &str| {
                let bytes = fetch(&id, layer, t.x, t.y)?;
                let sha = format::sha256_hex(&bytes);
                if !sha.eq_ignore_ascii_case(want_sha) {
                    let hint = if bytes.starts_with(b"version https://git-lfs") {
                        " (the file is a Git LFS pointer: fetch the LFS objects of this asset root)"
                    } else {
                        ""
                    };
                    let what = match layer {
                        LayerKind::Water => format!("tile {t:?}"),
                        LayerKind::Ground => format!("{layer:?} tile {t:?}"),
                    };
                    return Err(format!(
                        "{what} sha256 {sha} does not match the manifest ({want_sha}){hint}"
                    ));
                }
                Ok(bytes)
            };
            use rayon::prelude::*;
            // Water tiles first (the ground rules compare against them), each
            // decoded once; the compressed bytes are dropped. Results are
            // consumed in listing order, so the first error is deterministic.
            let decoded: Vec<Result<(usize, WaterTile), String>> = jobs
                .par_iter()
                .filter(|j| j.0 == LayerKind::Water)
                .map(|&(layer, t, idx, want_sha)| {
                    let bytes = fetch_checked(layer, t, want_sha)?;
                    format::decode_water(&bytes, min + t * TILE_SIZE)
                        .map(|w| (idx, w))
                        .map_err(|e| format!("tile {t:?}: {e}"))
                })
                .collect();
            let mut water: Vec<Option<WaterTile>> = (0..n_tiles).map(|_| None).collect();
            for d in decoded {
                let (idx, w) = d.map_err(&rerr)?;
                water[idx] = Some(w);
            }
            // Ground tiles: decode, check the rules that need the centimetres
            // (range, weights, agreement with the water layer) and pack, one
            // tile per task, so only a tile per thread is ever held in cm.
            let decoded: Vec<Result<(usize, Result<GroundTile, Problems>), String>> = jobs
                .par_iter()
                .filter(|j| j.0 == LayerKind::Ground)
                .map(|&(layer, t, idx, want_sha)| {
                    let bytes = fetch_checked(layer, t, want_sha)?;
                    let origin = min + t * TILE_SIZE;
                    let cm = format::decode_ground(&bytes, origin)
                        .map_err(|e| format!("{layer:?} tile {t:?}: {e}"))?;
                    let problems = validate_ground_tile(&cm, water[idx].as_ref());
                    Ok((
                        idx,
                        if problems.count > 0 {
                            Err(problems)
                        } else {
                            GroundTile::from_cm(&cm)
                                .map_err(|e| format!("{layer:?} tile {t:?}: {e}"))
                                .map(Ok)?
                        },
                    ))
                })
                .collect();
            let mut ground: Vec<Option<GroundTile>> = (0..n_tiles).map(|_| None).collect();
            let mut problems = Problems::default();
            for d in decoded {
                match d.map_err(&rerr)? {
                    (idx, Ok(g)) => ground[idx] = Some(g),
                    (_, Err(p)) => problems.merge(p),
                }
            }
            problems.into_result().map_err(&rerr)?;
            let chunks = (max - min) / CHUNK;
            let mut partial = vec![rm.allow_partial; (chunks.x * chunks.y) as usize];
            for &(cx, cy) in &rm.allow_partial_chunks {
                let c = Vec2::new(cx, cy) - min / CHUNK;
                if c.x < 0 || c.y < 0 || c.x >= chunks.x || c.y >= chunks.y {
                    return Err(rerr(format!(
                        "allow_partial_chunks lists chunk ({cx}, {cy}), outside the box (chunks \
                         {:?}..{:?})",
                        min / CHUNK,
                        max / CHUNK
                    )));
                }
                partial[(c.y * chunks.x + c.x) as usize] = true;
            }
            let is_partial = |p: Vec2<i32>| {
                let c = (p - min).map(|e| e.div_euclid(CHUNK));
                partial[(c.y * chunks.x + c.x) as usize]
            };
            let (authored_bounds, natural_seams) = validate_region(
                min,
                max,
                tiles,
                &water,
                &ground,
                rm.sea_fill,
                &is_partial,
                &mut chunk_summary,
            )
            .map_err(&rerr)?;
            if natural_seams > 0 {
                info!(
                    source,
                    region = id,
                    natural_seams,
                    "Authored water meets natural (unauthored) columns in partial chunks"
                );
            }
            if let Some(b) = authored_bounds {
                let margin = (b.min.x - min.x)
                    .min(b.min.y - min.y)
                    .min(max.x - b.max.x)
                    .min(max.y - b.max.y);
                if margin < REGION_MARGIN_M {
                    let floor = |v: i32| v.div_euclid(CHUNK) * CHUNK;
                    let ceil = |v: i32| (v + CHUNK - 1).div_euclid(CHUNK) * CHUNK;
                    let need_min = Vec2::new(
                        min.x.min(floor(b.min.x - REGION_MARGIN_M)),
                        min.y.min(floor(b.min.y - REGION_MARGIN_M)),
                    );
                    let need_max = Vec2::new(
                        max.x.max(ceil(b.max.x + REGION_MARGIN_M)),
                        max.y.max(ceil(b.max.y + REGION_MARGIN_M)),
                    );
                    return Err(rerr(format!(
                        "authored cells (bounds {:?}..{:?}) come within {margin} m of the box \
                         {min:?}..{max:?}; they must stay at least {REGION_MARGIN_M} m inside it \
                         (decorations rooted on authored columns change blocks up to ~10 m \
                         outside the box). Enlarge the box to at least min ({}, {}) max ({}, {}) \
                         (chunk-aligned), or move the authored cells inward",
                        b.min, b.max, need_min.x, need_min.y, need_max.x, need_max.y
                    )));
                }
            }
            regions.push(Region {
                id: id.clone(),
                min,
                max,
                feather: rm.feather_m,
                tiles,
                settings: RegionSettings {
                    suppress_procedural_in_water: rm.suppress_procedural_in_water,
                    exclude_procedural_margin_m: rm.exclude_procedural_margin_m as f32,
                    aquatic_profile,
                    sea_fill: rm.sea_fill,
                },
                budget: rm.consistency,
                partial,
                water,
                ground,
                dist: (0..n_tiles).map(|_| OnceLock::new()).collect(),
            });
        }
        let bounds = regions
            .iter()
            .map(|r| Aabr {
                min: r.min,
                max: r.max,
            })
            .reduce(|a, b| a.union(b))
            .expect("at least one region");
        let index =
            (regions.len() > LINEAR_SCAN_MAX_REGIONS).then(|| RegionIndex::new(&regions, bounds));
        Ok(Self {
            regions,
            bounds,
            index,
            chunk_summary,
            digest,
            resident_bytes: reserved.keep(),
        })
    }

    #[inline(always)]
    fn region_at(&self, wpos: Vec2<i32>) -> Option<&Region> {
        if wpos.x < self.bounds.min.x
            || wpos.y < self.bounds.min.y
            || wpos.x >= self.bounds.max.x
            || wpos.y >= self.bounds.max.y
        {
            return None;
        }
        match &self.index {
            None => self.regions.iter().find(|r| r.contains(wpos)),
            Some(index) => index
                .candidates(wpos)
                .iter()
                .map(|&i| &self.regions[i as usize])
                .find(|r| r.contains(wpos)),
        }
    }

    /// The authored column at `wpos` for the column sampler, or `None`
    /// outside every region (the engine path, untouched).
    #[inline]
    pub fn column(&self, wpos: Vec2<i32>) -> Option<AuthoredColumn> {
        let region = self.region_at(wpos)?;
        let cell = region.cell(wpos);
        Some(AuthoredColumn {
            weight: region.weight(wpos),
            cell,
            // A wet column's distance to water is zero; only dry columns pay
            // for the distance field.
            water_dist: if cell.is_wet() {
                Some(0.0)
            } else {
                region.water_dist(wpos)
            },
            settings: region.settings,
            natural: region.partial_at(wpos),
        })
    }

    /// The authored cell at `wpos` (no distance field, no feather), or
    /// `None` outside every region.
    #[inline]
    pub fn cell_at(&self, wpos: Vec2<i32>) -> Option<AuthoredCell> {
        Some(self.region_at(wpos)?.cell(wpos))
    }

    /// The authored cell at `wpos` with its region's [`SeaFill`] and whether
    /// its chunk is partial (one region lookup), or `None` outside every
    /// region.
    #[inline]
    pub fn cell_detail_at(&self, wpos: Vec2<i32>) -> Option<(AuthoredCell, SeaFill, bool)> {
        let r = self.region_at(wpos)?;
        Some((r.cell(wpos), r.settings.sea_fill, r.partial_at(wpos)))
    }

    /// The authored water at `wpos`, if `wpos` is an authored wet column of
    /// any region.
    pub fn water_at(&self, wpos: Vec2<i32>) -> Option<AuthoredWater> {
        match self.cell_at(wpos)? {
            AuthoredCell::Wet {
                surface_block,
                bed_block,
            } => Some(AuthoredWater {
                surface_block,
                bed_block,
            }),
            _ => None,
        }
    }

    /// Whether `wpos` lies in a partial chunk of a region (its unauthored
    /// columns keep the natural map); false outside every region.
    pub fn natural_at(&self, wpos: Vec2<i32>) -> bool {
        self.region_at(wpos).is_some_and(|r| r.partial_at(wpos))
    }

    /// Whether chunk `chunk_pos` lies inside a region.
    pub fn contains_chunk(&self, chunk_pos: Vec2<i32>) -> bool {
        self.region_at(chunk_pos * CHUNK).is_some()
    }

    /// sha256 of the canonical manifest: changes whenever any region or tile
    /// changes.
    pub fn digest(&self) -> &str { &self.digest }

    /// Region ids and boxes, for logs and tools.
    pub fn regions(&self) -> impl Iterator<Item = (&str, Aabr<i32>)> {
        self.regions.iter().map(|r| {
            (r.id.as_str(), Aabr {
                min: r.min,
                max: r.max,
            })
        })
    }

    /// Per-chunk summary (chunks with any authored cell only).
    pub fn chunk_summary(&self, chunk_pos: Vec2<i32>) -> Option<&ChunkWaterSummary> {
        self.chunk_summary.get(&chunk_pos)
    }

    /// The stone floor a chunk needs under its own authored ground:
    /// [`FLOOR_MARGIN_BLOCKS`] below its lowest authored bed, bank or ground
    /// block (the caller takes the min with the sim's floor, so raised ground
    /// never lowers it).
    /// `None` for a chunk without authored cells (outside every region in
    /// particular), whose floor stays the sim's.
    pub fn floor_block(&self, chunk_pos: Vec2<i32>) -> Option<i32> {
        self.chunk_summary
            .get(&chunk_pos)
            .map(|s| s.min_ground_block() - FLOOR_MARGIN_BLOCKS)
    }

    /// Every ground cell's 4-neighbour whose water the post-civ seam check
    /// compares with it (see [`queries::check_ground_seams`]), as `(ground
    /// cell, neighbour)` pairs in region and tile order. Empty when no
    /// region has a partial chunk or a [`SeaFill::AuthoredOnly`] setting.
    pub fn ground_seam_pairs(&self) -> Vec<(Vec2<i32>, Vec2<i32>)> {
        self.regions
            .iter()
            .flat_map(Region::ground_seam_pairs)
            .collect()
    }

    /// Build every distance field now (in parallel on the current rayon
    /// pool) so no chunk-generation worker stalls on one later.
    pub fn prewarm(&self) {
        use rayon::prelude::*;
        self.regions.par_iter().for_each(|r| {
            (0..r.dist.len()).into_par_iter().for_each(|i| {
                r.dist_field(i);
            });
        });
    }

    /// Compare every region's chunks with the sim's chunk table
    /// (`table_wet(chunk)`: river, lake, ocean or underwater) in both
    /// directions. Over a region's [`ConsistencyBudget`] this is an error.
    pub fn check_consistency(
        &self,
        source: &str,
        table_wet: impl Fn(Vec2<i32>) -> Option<bool>,
    ) -> Result<Vec<RegionConsistency>, LoadError> {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for r in &self.regions {
            let mut c = RegionConsistency {
                region: r.id.clone(),
                ..Default::default()
            };
            // Region boxes are chunk-aligned (checked at load), so these
            // divisions are exact.
            let (c0, c1) = (r.min / CHUNK, r.max / CHUNK);
            for cy in c0.y..c1.y {
                for cx in c0.x..c1.x {
                    let cpos = Vec2::new(cx, cy);
                    let Some(table) = table_wet(cpos) else {
                        continue;
                    };
                    let summary = self.chunk_summary.get(&cpos);
                    let authored_wet = summary.is_some_and(|s| s.wet_columns > 0);
                    let fully_authored =
                        summary.is_some_and(|s| s.authored_columns == (CHUNK * CHUNK) as u32);
                    if authored_wet && !table {
                        c.authored_wet_table_dry.push(cpos);
                    }
                    // The sim lowers the terrain of a chunk it calls water;
                    // only a chunk whose every column is authored hides that.
                    if table && !fully_authored {
                        if r.partial_at(cpos * CHUNK) {
                            c.partial_table_wet.push(cpos);
                        } else {
                            c.authored_dry_table_wet.push(cpos);
                        }
                    }
                }
            }
            let dry_wet = c.authored_dry_table_wet.len();
            let wet_dry = c.authored_wet_table_dry.len();
            if dry_wet as u32 > r.budget.max_authored_dry_table_wet_chunks {
                errors.push(format!(
                    "region '{}': {dry_wet} chunk(s) the sim table calls water have unauthored \
                     columns (budget {}), e.g. {:?}: the sim sinks those chunks and the \
                     unauthored terrain there with them. Leave the water/elevated masks untouched \
                     inside regions. Where the table's water is the natural river, lake or sea \
                     and only some of its columns are edited, declare those chunks partial \
                     (allow_partial_chunks, or allow_partial for the whole region): their \
                     unauthored columns then keep the natural water. Otherwise author every \
                     column of those chunks (water, and bank cells for the ground the author \
                     intends)",
                    r.id,
                    r.budget.max_authored_dry_table_wet_chunks,
                    &c.authored_dry_table_wet[..dry_wet.min(5)]
                ));
            }
            if let Some(max) = r.budget.max_authored_wet_table_dry_chunks
                && wet_dry as u32 > max
            {
                errors.push(format!(
                    "region '{}': {wet_dry} chunk(s) with authored water are dry in the sim table \
                     (budget {max}), e.g. {:?}",
                    r.id,
                    &c.authored_wet_table_dry[..wet_dry.min(5)]
                ));
            }
            info!(
                source,
                region = r.id,
                authored_wet_table_dry = wet_dry,
                authored_dry_table_wet = dry_wet,
                partial_table_wet = c.partial_table_wet.len(),
                "Authored water vs the sim chunk table"
            );
            out.push(c);
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(LoadError(format!(
                "authored rasters [{source}]: {}",
                errors.join("; ")
            )))
        }
    }

    /// Ground below sea level in a [`SeaFill::Auto`] region fills with sea
    /// water, which is only intended on the sea: every chunk holding a
    /// ground cell below the ocean's top block must be one the sim table
    /// calls ocean or underwater (`table_sea(chunk)`), else this is an error
    /// that names `sea_fill: AuthoredOnly`. [`SeaFill::AuthoredOnly`] regions
    /// are dry below sea level and exempt.
    pub fn check_sea_fill(
        &self,
        source: &str,
        table_sea: impl Fn(Vec2<i32>) -> Option<bool>,
    ) -> Result<(), LoadError> {
        let mut errors = Vec::new();
        for r in self
            .regions
            .iter()
            .filter(|r| r.settings.sea_fill == SeaFill::Auto)
        {
            let (c0, c1) = (r.min / CHUNK, r.max / CHUNK);
            let mut inland = Vec::new();
            let mut lowest = i32::MAX;
            for cy in c0.y..c1.y {
                for cx in c0.x..c1.x {
                    let cpos = Vec2::new(cx, cy);
                    let Some(s) = self.chunk_summary.get(&cpos) else {
                        continue;
                    };
                    if s.min_ground_cell_block < SEA_TOP_BLOCK && table_sea(cpos) != Some(true) {
                        inland.push(cpos);
                        lowest = lowest.min(s.min_ground_cell_block);
                    }
                }
            }
            if !inland.is_empty() {
                errors.push(format!(
                    "region '{}': {} chunk(s) the sim table calls dry land hold ground cells \
                     below the ocean's top block {SEA_TOP_BLOCK} (lowest block {lowest}), e.g. \
                     {:?}: with sea_fill: Auto they would fill with sea water. Author the water \
                     there, raise the ground, or set sea_fill: AuthoredOnly to keep them dry",
                    r.id,
                    inland.len(),
                    &inland[..inland.len().min(5)]
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(LoadError(format!(
                "authored rasters [{source}]: {}",
                errors.join("; ")
            )))
        }
    }

    /// [`Self::check_sea_fill`] then [`Self::check_consistency`] against a
    /// generated sim.
    pub(crate) fn check_consistency_with_sim(
        &self,
        source: &str,
        sim: &WorldSim,
    ) -> Result<Vec<RegionConsistency>, LoadError> {
        self.check_sea_fill(source, |cpos| {
            sim.get(cpos)
                .map(|c| c.river.is_ocean() || c.is_underwater())
        })?;
        self.check_consistency(source, |cpos| {
            sim.get(cpos)
                .map(|c| c.river.river_kind.is_some() || c.is_underwater())
        })
    }
}

pub(crate) fn io_error_kind(err: &assets::Error) -> Option<std::io::ErrorKind> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err.reason());
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        source = e.source();
    }
    None
}

/// Collects a validation pass's problems: the first few messages and the
/// total count.
#[derive(Default)]
struct Problems {
    count: usize,
    first: Vec<String>,
}

impl Problems {
    const SHOWN: usize = 10;

    fn report(&mut self, msg: String) {
        self.count += 1;
        if self.first.len() < Self::SHOWN {
            self.first.push(msg);
        }
    }

    fn merge(&mut self, o: Problems) {
        self.count += o.count;
        for m in o.first {
            if self.first.len() < Self::SHOWN {
                self.first.push(m);
            }
        }
    }

    fn into_result(self) -> Result<(), String> {
        if self.count == 0 {
            Ok(())
        } else {
            Err(format!(
                "{} invalid cell(s), first: {}",
                self.count,
                self.first.join("; ")
            ))
        }
    }
}

/// The ground-layer rules that need the centimetres, per cell of one tile
/// (`water`: the region's water tile at the same index, if listed):
///
/// * the top block lies in [`format::GROUND_BLOCK_RANGE`];
/// * under an authored wet cell the ground equals the bed, under a bank it
///   equals the bank, and is exact there (one truth per column: a disagreement
///   is a stale pin).
///
/// Any weight 1..=255 is valid elsewhere: [`GROUND_EXACT`] is exact, lower
/// weights are the blend ring toward the engine terrain.
fn validate_ground_tile(g: &GroundTileCm, water: Option<&WaterTile>) -> Problems {
    let mut p = Problems::default();
    for j in 0..TILE_SIZE {
        for i in 0..TILE_SIZE {
            let k = cell_index(i, j);
            let Some(cm) = g.ground[k] else {
                continue;
            };
            let wpos = g.origin + Vec2::new(i, j);
            let weight = g.weight[k];
            let block = cm.div_euclid(100);
            if !format::GROUND_BLOCK_RANGE.contains(&block) {
                p.report(format!(
                    "{wpos:?}: ground {cm} cm (block {block}) is outside the blocks {:?} the \
                     engine can hold",
                    format::GROUND_BLOCK_RANGE
                ));
            }
            let Some(w) = water else {
                continue;
            };
            match (w.surface_cm(k), w.bed_cm(k)) {
                (Some(_), Some(bed)) if cm != bed || weight != GROUND_EXACT => p.report(format!(
                    "{wpos:?}: ground {cm} cm (weight {weight}) under authored water must be \
                     exact and equal the bed ({bed} cm): the layers disagree (stale pins?)"
                )),
                (None, Some(bank)) if cm != bank || weight != GROUND_EXACT => p.report(format!(
                    "{wpos:?}: ground {cm} cm (weight {weight}) under an authored bank must be \
                     exact and equal the bank ({bank} cm): the layers disagree (stale pins?)"
                )),
                _ => {},
            }
        }
    }
    p
}

/// The containment rules over one region's decoded tiles, plus the
/// per-chunk summaries; returns the bounding box of the authored cells:
///
/// * wet: bed block below surface block; surface not below the ocean's top
///   ([`SeaFill::Auto`]; a lake below 0 m is fine in
///   [`SeaFill::AuthoredOnly`]);
/// * bank: ground not below the ocean's top in [`SeaFill::Auto`] (it would
///   flood);
/// * every 4-neighbour of a wet cell inside the region is wet, or a bank or
///   exact ground cell at least as high as the water (otherwise the water
///   stands as a wall);
/// * no authored cell beyond the region box; no surface without a bed.
///
/// Ground cells below sea level in a [`SeaFill::Auto`] region need the sim
/// table (ocean chunks only): [`AuthoredRasters::check_consistency`] checks
/// them.
fn validate_region(
    min: Vec2<i32>,
    max: Vec2<i32>,
    tiles: Vec2<i32>,
    water: &[Option<WaterTile>],
    ground: &[Option<GroundTile>],
    sea_fill: SeaFill,
    partial: &(dyn Fn(Vec2<i32>) -> bool + Sync),
    summary: &mut HashMap<Vec2<i32>, ChunkWaterSummary>,
) -> Result<(Option<Aabr<i32>>, usize), String> {
    use rayon::prelude::*;
    const CHUNKS_PER_TILE: i32 = TILE_SIZE / CHUNK;
    let cross_tile_cell = |wpos: Vec2<i32>| -> AuthoredCell {
        let local = wpos - min;
        let t = local.map(|e| e.div_euclid(TILE_SIZE));
        if t.x < 0 || t.y < 0 || t.x >= tiles.x || t.y >= tiles.y {
            return AuthoredCell::None;
        }
        let idx = (t.y * tiles.x + t.x) as usize;
        let c = local - t * TILE_SIZE;
        cell_of(
            water[idx].as_ref(),
            ground[idx].as_ref(),
            cell_index(c.x, c.y),
        )
    };
    let inside = |p: Vec2<i32>| p.x >= min.x && p.y >= min.y && p.x < max.x && p.y < max.y;
    let authored_only = sea_fill == SeaFill::AuthoredOnly;
    struct TileResult {
        problems: Problems,
        bounds: Option<Aabr<i32>>,
        natural_seams: usize,
        local: Vec<ChunkWaterSummary>,
    }
    let per_tile: Vec<Option<TileResult>> = (0..water.len())
        .into_par_iter()
        .map(|idx| {
            let w = water[idx].as_ref();
            let g = ground[idx].as_ref();
            if w.is_none() && g.is_none() {
                return None;
            }
            let (tx, ty) = (idx as i32 % tiles.x, idx as i32 / tiles.x);
            let origin = min + Vec2::new(tx, ty) * TILE_SIZE;
            let mut r = TileResult {
                problems: Problems::default(),
                bounds: None,
                natural_seams: 0,
                local: vec![
                    ChunkWaterSummary::default();
                    (CHUNKS_PER_TILE * CHUNKS_PER_TILE) as usize
                ],
            };
            for j in 0..TILE_SIZE {
                for i in 0..TILE_SIZE {
                    let k = cell_index(i, j);
                    let authored = w.is_some_and(|w| w.is_authored(k))
                        || g.is_some_and(|g| g.get(k).is_some());
                    if !authored {
                        continue;
                    }
                    let wpos = origin + Vec2::new(i, j);
                    if !inside(wpos) {
                        r.problems
                            .report(format!("{wpos:?}: authored cell outside the region box"));
                        continue;
                    }
                    r.bounds = Some(match r.bounds {
                        None => Aabr {
                            min: wpos,
                            max: wpos + 1,
                        },
                        Some(b) => Aabr {
                            min: Vec2::partial_min(b.min, wpos),
                            max: Vec2::partial_max(b.max, wpos + 1),
                        },
                    });
                    let entry = &mut r.local[((j / CHUNK) * CHUNKS_PER_TILE + i / CHUNK) as usize];
                    entry.authored_columns += 1;
                    match cell_of(w, g, k) {
                        AuthoredCell::Wet {
                            surface_block,
                            bed_block,
                        } => {
                            entry.wet_columns += 1;
                            entry.min_surface_block = entry.min_surface_block.min(surface_block);
                            entry.max_surface_block = entry.max_surface_block.max(surface_block);
                            entry.min_bed_block = entry.min_bed_block.min(bed_block);
                            if bed_block >= surface_block {
                                r.problems.report(format!(
                                    "{wpos:?}: bed block {bed_block} is not below surface block \
                                     {surface_block}"
                                ));
                            }
                            if surface_block < SEA_TOP_BLOCK && !authored_only {
                                r.problems.report(format!(
                                    "{wpos:?}: surface block {surface_block} is below the ocean's \
                                     top block {SEA_TOP_BLOCK} (a lake below sea level needs \
                                     sea_fill: AuthoredOnly)"
                                ));
                            }
                            for d in [
                                Vec2::new(1, 0),
                                Vec2::new(-1, 0),
                                Vec2::new(0, 1),
                                Vec2::new(0, -1),
                            ] {
                                let n = wpos + d;
                                if !inside(n) {
                                    continue;
                                }
                                let (ni, nj) = (i + d.x, j + d.y);
                                // Same tile: index directly; else look it up.
                                let ncell = if (0..TILE_SIZE).contains(&ni)
                                    && (0..TILE_SIZE).contains(&nj)
                                {
                                    cell_of(w, g, cell_index(ni, nj))
                                } else {
                                    cross_tile_cell(n)
                                };
                                match ncell {
                                    AuthoredCell::Wet { .. } => {},
                                    AuthoredCell::Bank { bed_block: nb } if nb >= surface_block => {
                                    },
                                    AuthoredCell::Bank { bed_block: nb } => {
                                        r.problems.report(format!(
                                            "{wpos:?}: water (top block {surface_block}) next to \
                                             bank {n:?} whose ground top {nb} is lower: the water \
                                             would stand as a wall"
                                        ))
                                    },
                                    // A quay, a canyon wall: exact ground at
                                    // least as high holds the water.
                                    AuthoredCell::Ground {
                                        block: nb,
                                        weight: GROUND_EXACT,
                                    } if nb >= surface_block => {},
                                    AuthoredCell::Ground { block: nb, weight } => {
                                        r.problems.report(format!(
                                            "{wpos:?}: water (top block {surface_block}) next to \
                                             ground {n:?} (top block {nb}, weight {weight}) that \
                                             is lower or not exact: the water would stand as a \
                                             wall"
                                        ))
                                    },
                                    // A partial chunk's unauthored column is
                                    // the natural map: the author edits the
                                    // natural water there, seam included.
                                    AuthoredCell::None if partial(n) => r.natural_seams += 1,
                                    AuthoredCell::None => r.problems.report(format!(
                                        "{wpos:?}: water next to unauthored column {n:?} inside \
                                         the region; write a bank cell there, or declare its \
                                         chunk partial (allow_partial_chunks) to meet the natural \
                                         map"
                                    )),
                                }
                            }
                        },
                        AuthoredCell::Bank { bed_block } => {
                            entry.min_bed_block = entry.min_bed_block.min(bed_block);
                            if bed_block < SEA_TOP_BLOCK && !authored_only {
                                r.problems.report(format!(
                                    "{wpos:?}: bank ground block {bed_block} is below the ocean's \
                                     top block {SEA_TOP_BLOCK} and would flood (dry ground below \
                                     sea level needs sea_fill: AuthoredOnly)"
                                ));
                            }
                        },
                        AuthoredCell::Ground { block, .. } => {
                            entry.ground_columns += 1;
                            entry.sea_ground_columns +=
                                (!authored_only && block < SEA_TOP_BLOCK) as u32;
                            entry.min_ground_cell_block = entry.min_ground_cell_block.min(block);
                        },
                        AuthoredCell::None => {
                            r.problems
                                .report(format!("{wpos:?}: surface without a bed"));
                        },
                    }
                }
            }
            Some(r)
        })
        .collect();
    let mut problems = Problems::default();
    let mut bounds: Option<Aabr<i32>> = None;
    let mut natural_seams = 0usize;
    for (idx, r) in per_tile.into_iter().enumerate() {
        let Some(r) = r else {
            continue;
        };
        problems.merge(r.problems);
        natural_seams += r.natural_seams;
        bounds = match (bounds, r.bounds) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (a, b) => a.or(b),
        };
        let origin = min + Vec2::new(idx as i32 % tiles.x, idx as i32 / tiles.x) * TILE_SIZE;
        for (n, s) in r.local.iter().enumerate() {
            if s.authored_columns > 0 {
                let c = (origin / CHUNK)
                    + Vec2::new(n as i32 % CHUNKS_PER_TILE, n as i32 / CHUNKS_PER_TILE);
                summary.entry(c).or_default().merge(s);
            }
        }
    }
    problems.into_result()?;
    Ok((bounds, natural_seams))
}

#[cfg(test)] mod ground_real_world_tests;
#[cfg(test)] mod ground_seam_real_world_tests;
#[cfg(test)] mod real_world_tests;
#[cfg(test)] mod tests;
