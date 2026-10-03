//! XINDELER: authored raster layers -- the authored water layer.
//!
//! Inside a declared, chunk-aligned *authored region*, the column sampler
//! stops deriving water from the 32 m sim chunks (River discs, Lake/Ocean
//! chunk sets, chunk `alt` as the water level, bank pull) and reads a 1 m
//! raster instead: per block column either "water with this surface over
//! this bed" ([`AuthoredCell::Wet`]), "dry ground at this height"
//! ([`AuthoredCell::Bank`]) or nothing ([`AuthoredCell::None`]). Outside every
//! region nothing here runs, so every other column is bit-identical to an
//! engine without this module.
//!
//! The full contract (formats, quantisation, containment rules, sim
//! integration, gates) is
//! `references/precision-tooling/stage1-authored-water-design.md` in the
//! private design repo. In short:
//!
//! * the manifest `<map>_authored_rasters.ron` is optional: *missing* means "no
//!   authored water" (the old path, bit-identical); *present but wrong in any
//!   way* stops world generation with a message naming the problem;
//! * altitudes are integer centimetres in the block-z frame and become blocks
//!   with integer arithmetic: the top water block is `floor(surface / 1 m)`,
//!   the top ground block `floor(bed / 1 m)`, never a float rounding;
//! * the raster is defined in column space (cell = block column), so the -16 m
//!   chunk-spline registration of the heightmap does not apply to it;
//! * the sim chunk table (kinds, `water_alt`, downhill graph) is *not* mutated:
//!   those keep coming from the masks the exporter keeps consistent, because
//!   any change there would leak up to three chunks outside the region
//!   (`local_cells` radius) and into civ generation.

pub mod format;
pub mod writer;

use crate::sim::{RiverKind, WorldSim};
use common::{
    assets::{self, AssetExt, BoxedError, FileAsset, load_ron},
    terrain::TerrainChunkSize,
    vol::RectVolSize,
};
use format::{LayerKind, RawTile, TILE_CELLS, TILE_SIZE};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::OnceLock};
use tracing::{info, warn};
use vek::*;

/// Schema version of the manifest this engine reads.
pub const MANIFEST_SCHEMA: u32 = 1;
/// Distances to authored water are tracked up to this many metres, which is
/// also where the engine's warp fade (`water_dist / 64`) saturates.
pub const DIST_CAP_M: i32 = 64;
/// The whole manifest must fit this many bytes when every tile is resident.
pub const RESIDENT_BUDGET_BYTES: usize = 256 << 20;
/// Regions must keep this far from the map rim, where the column sampler has
/// no spline knots.
pub const RIM_MARGIN_M: i32 = 64;
/// Largest feather the manifest may ask for.
pub const MAX_FEATHER_M: i32 = 128;
/// Top block of the engine's ocean (`CONFIG.sea_level - 1 + 0.01`, filled
/// `z < level`): authored water and banks may not go below it.
pub const SEA_TOP_BLOCK: i32 = 139;

const BYTES_PER_RAW_TILE: usize = TILE_CELLS * 4;
const BYTES_PER_DIST_TILE: usize = TILE_CELLS;
const NO_DIST: u8 = u8::MAX;

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
// Loaded rasters
// --------------------------------------------------------------------------

/// One authored column, as the column sampler sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuthoredCell {
    /// Water whose top block is `surface_block` over ground whose top block
    /// is `bed_block` (`bed_block < surface_block`).
    Wet { surface_block: i32, bed_block: i32 },
    /// Authored dry ground whose top block is `bed_block`.
    Bank { bed_block: i32 },
    /// Inside a region, but not authored: engine terrain without the
    /// region's suppressed sim water.
    None,
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
    /// centre, metres (chamfer 3-4), `None` beyond [`DIST_CAP_M`].
    pub water_dist: Option<f32>,
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

impl AuthoredColumn {
    /// Replace the engine's results for this column:
    ///
    /// * wet: bed and surface exact, no warp/cliff carving;
    /// * bank: ground exact, no water above sea level;
    /// * none: no water above sea level; terrain blended by the feather weight
    ///   from the engine's (with the sim water's carving) to the engine's *dry*
    ///   terrain (same noise and warp, faded by the distance to the authored
    ///   water instead of the sim's).
    pub fn resolve(&self, engine: EngineColumn, dry: DryTerrain) -> EngineColumn {
        match self.cell {
            AuthoredCell::Wet {
                surface_block,
                bed_block,
            } => EngineColumn {
                alt: bed_block as f32 + 0.5,
                water_level: (surface_block as f32 + 0.5).max(dry.base_sea_level),
                water_dist: Some(-1.0),
                warp_factor: 0.0,
                cliff_offset: 0.0,
                riverless_alt: engine.riverless_alt,
            },
            AuthoredCell::Bank { bed_block } => EngineColumn {
                alt: bed_block as f32 + 0.5,
                water_level: dry.base_sea_level,
                water_dist: self.water_dist,
                warp_factor: 0.0,
                cliff_offset: 0.0,
                riverless_alt: bed_block as f32 + 0.5,
            },
            AuthoredCell::None => {
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
            },
        }
    }
}

/// Per-chunk summary of the authored water, computed once at load.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChunkWaterSummary {
    pub wet_columns: u32,
    pub authored_columns: u32,
    pub min_surface_block: i32,
    pub max_surface_block: i32,
    pub min_bed_block: i32,
}

struct ListedTile {
    bytes: Box<[u8]>,
    raw: OnceLock<RawTile>,
}

struct Region {
    id: String,
    min: Vec2<i32>,
    max: Vec2<i32>,
    feather: i32,
    tiles: Vec2<i32>,
    /// One slot per tile of the region grid; `None` = unlisted (no data).
    water: Vec<Option<ListedTile>>,
    /// Distance-to-water fields, one per tile of the grid, built lazily.
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

    fn raw(&self, idx: usize) -> Option<&RawTile> {
        let listed = self.water[idx].as_ref()?;
        let t = Vec2::new(idx as i32 % self.tiles.x, idx as i32 / self.tiles.x);
        Some(listed.raw.get_or_init(|| {
            // Validated byte-for-byte at load; decoding is deterministic.
            format::decode_water(&listed.bytes, self.origin(t))
                .expect("authored raster tile decoded at load and cannot fail now")
        }))
    }

    /// Cell state at `wpos` (must be inside the region).
    fn cell(&self, wpos: Vec2<i32>) -> AuthoredCell {
        let local = wpos - self.min;
        let t = local.map(|e| e.div_euclid(TILE_SIZE));
        let Some(raw) = self.tile_index(t).and_then(|idx| self.raw(idx)) else {
            return AuthoredCell::None;
        };
        let c = local - t * TILE_SIZE;
        cell_of(raw, RawTile::idx(c.x, c.y))
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
        let d = field[RawTile::idx(c.x, c.y)];
        (d != NO_DIST).then(|| d as f32 / 3.0)
    }
}

#[inline(always)]
fn cell_of(raw: &RawTile, k: usize) -> AuthoredCell {
    match (raw.surface_cm(k), raw.bed_cm(k)) {
        (Some(s), Some(b)) => AuthoredCell::Wet {
            surface_block: s.div_euclid(100),
            bed_block: b.div_euclid(100),
        },
        (None, Some(b)) => AuthoredCell::Bank {
            bed_block: b.div_euclid(100),
        },
        // `(Some, None)` is refused at load.
        _ => AuthoredCell::None,
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
                    if !(0..n).contains(&wx) {
                        continue;
                    }
                    let k = RawTile::idx(i, j);
                    if raw.surface[k] != format::NONE && raw.bed[k] != format::NONE {
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
                out[RawTile::idx(i, j)] = v as u8;
            }
        }
    }
    Some(out.into_boxed_slice())
}

/// All authored rasters of one world. `None` on `WorldSim` when the map has
/// no manifest.
pub struct AuthoredRasters {
    regions: Vec<Region>,
    /// Union of the region boxes (min inclusive, max exclusive), checked
    /// first so every column outside all regions costs four comparisons.
    bounds: Aabr<i32>,
    chunk_summary: HashMap<Vec2<i32>, ChunkWaterSummary>,
}

impl std::fmt::Debug for AuthoredRasters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthoredRasters")
            .field(
                "regions",
                &self.regions.iter().map(|r| &r.id).collect::<Vec<_>>(),
            )
            .field("bounds", &self.bounds)
            .finish()
    }
}

/// Why a manifest was refused. Always fatal for world generation.
#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

impl AuthoredRasters {
    /// Load `<map_asset>_authored_rasters` through the asset system.
    ///
    /// `Ok(None)`: the manifest does not exist (the map has no authored
    /// water; the old path). `Err`: it exists but is wrong in any way -- the
    /// caller must stop world generation.
    pub fn load_for_map(
        map_asset: &str,
        map_size_blocks: Vec2<i32>,
    ) -> Result<Option<Self>, LoadError> {
        let specifier = manifest_specifier(map_asset);
        let manifest = match Manifest::load_owned(&specifier) {
            Ok(manifest) => manifest,
            Err(err) if is_not_found(&err) => return Ok(None),
            Err(err) => {
                return Err(LoadError(format!(
                    "authored raster manifest '{specifier}' exists but cannot be read: {:?}",
                    err.reason()
                )));
            },
        };
        let fetch = |region: &str, layer: LayerKind, tx: i32, ty: i32| {
            let spec = tile_specifier(map_asset, region, layer, tx, ty);
            TileBytes::load_owned(&spec)
                .map(|t| t.0)
                .map_err(|e| format!("tile '{spec}' cannot be loaded: {:?}", e.reason()))
        };
        Self::from_manifest(manifest, map_size_blocks, &fetch).map(Some)
    }

    /// Validate `manifest` and every tile `fetch` returns, and build the
    /// sampler. Asset-free, so tests can feed tiles from memory.
    pub fn from_manifest(
        manifest: Manifest,
        map_size_blocks: Vec2<i32>,
        fetch: &dyn Fn(&str, LayerKind, i32, i32) -> Result<Vec<u8>, String>,
    ) -> Result<Self, LoadError> {
        let err = |msg: String| LoadError(format!("authored rasters: {msg}"));
        if manifest.schema != MANIFEST_SCHEMA {
            return Err(err(format!(
                "manifest schema {} (this engine reads {MANIFEST_SCHEMA})",
                manifest.schema
            )));
        }
        if manifest.regions.is_empty() {
            return Err(err("manifest lists no region".into()));
        }
        let chunk = TerrainChunkSize::RECT_SIZE.x as i32;
        let mut regions: Vec<Region> = Vec::new();
        let mut resident = 0usize;
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
                    "tile_size_m {} / cell_size_m {} (format v1 is {TILE_SIZE} / 1)",
                    rm.tile_size_m, rm.cell_size_m
                )));
            }
            if rm.layers != [LayerKind::Water] {
                return Err(rerr(format!(
                    "layers {:?}: this engine supports exactly [Water]",
                    rm.layers
                )));
            }
            let min: Vec2<i32> = Vec2::from(rm.min);
            let max: Vec2<i32> = Vec2::from(rm.max);
            if min.x % chunk != 0 || min.y % chunk != 0 || max.x % chunk != 0 || max.y % chunk != 0
            {
                return Err(rerr(format!(
                    "box {min:?}..{max:?} is not aligned to {chunk} m chunks"
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
            if let Some(other) = regions
                .iter()
                .find(|r| min.x < r.max.x && r.min.x < max.x && min.y < r.max.y && r.min.y < max.y)
            {
                return Err(rerr(format!("overlaps region '{}'", other.id)));
            }
            let size = max - min;
            let tiles = size.map(|e| (e + TILE_SIZE - 1) / TILE_SIZE);
            let n_tiles = (tiles.x * tiles.y) as usize;
            let mut water: Vec<Option<ListedTile>> = (0..n_tiles).map(|_| None).collect();
            let mut decoded: HashMap<Vec2<i32>, RawTile> = HashMap::new();
            for tm in &rm.tiles {
                let t = Vec2::new(tm.tx, tm.ty);
                if t.x < 0 || t.y < 0 || t.x >= tiles.x || t.y >= tiles.y {
                    return Err(rerr(format!(
                        "tile {t:?} lies outside the region grid {tiles:?}"
                    )));
                }
                let idx = (t.y * tiles.x + t.x) as usize;
                if water[idx].is_some() {
                    return Err(rerr(format!("tile {t:?} listed twice")));
                }
                let bytes = fetch(&id, tm.layer, t.x, t.y).map_err(&rerr)?;
                let sha = format::sha256_hex(&bytes);
                if !sha.eq_ignore_ascii_case(&tm.sha256) {
                    return Err(rerr(format!(
                        "tile {t:?} sha256 {sha} does not match the manifest ({})",
                        tm.sha256
                    )));
                }
                let raw = format::decode_water(&bytes, min + t * TILE_SIZE)
                    .map_err(|e| rerr(format!("tile {t:?}: {e}")))?;
                decoded.insert(t, raw);
                water[idx] = Some(ListedTile {
                    bytes: bytes.into_boxed_slice(),
                    raw: OnceLock::new(),
                });
            }
            resident += rm.tiles.len() * BYTES_PER_RAW_TILE + n_tiles * BYTES_PER_DIST_TILE;
            if resident > RESIDENT_BUDGET_BYTES {
                return Err(rerr(format!(
                    "the manifest needs {} MiB resident, over the {} MiB budget",
                    resident >> 20,
                    RESIDENT_BUDGET_BYTES >> 20
                )));
            }
            validate_region(&id, min, max, &decoded, &mut chunk_summary).map_err(&rerr)?;
            regions.push(Region {
                id: id.clone(),
                min,
                max,
                feather: rm.feather_m,
                tiles,
                water,
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
        Ok(Self {
            regions,
            bounds,
            chunk_summary,
        })
    }

    /// The authored column at `wpos`, or `None` outside every region (the
    /// engine path, untouched).
    #[inline]
    pub fn column(&self, wpos: Vec2<i32>) -> Option<AuthoredColumn> {
        if wpos.x < self.bounds.min.x
            || wpos.y < self.bounds.min.y
            || wpos.x >= self.bounds.max.x
            || wpos.y >= self.bounds.max.y
        {
            return None;
        }
        let region = self.regions.iter().find(|r| r.contains(wpos))?;
        Some(AuthoredColumn {
            weight: region.weight(wpos),
            cell: region.cell(wpos),
            water_dist: region.water_dist(wpos),
        })
    }

    /// The authored water at `wpos`, as `(surface_block, bed_block)`, if
    /// `wpos` is an authored wet column of any region.
    pub fn water_at(&self, wpos: Vec2<i32>) -> Option<(i32, i32)> {
        match self.column(wpos)?.cell {
            AuthoredCell::Wet {
                surface_block,
                bed_block,
            } => Some((surface_block, bed_block)),
            _ => None,
        }
    }

    /// Region ids and boxes, for logs and tools.
    pub fn regions(&self) -> impl Iterator<Item = (&str, Aabr<i32>)> {
        self.regions.iter().map(|r| {
            (r.id.as_str(), Aabr {
                min: r.min,
                max: r.max,
            })
        })
    }

    /// Lowest authored ground block (bed or bank) in `chunk_pos` and its eight
    /// neighbours, if any of them holds an authored cell. Chunk generation
    /// lowers its stone floor below it.
    pub fn min_authored_block_near(&self, chunk_pos: Vec2<i32>) -> Option<i32> {
        let mut min: Option<i32> = None;
        for dy in -1..=1 {
            for dx in -1..=1 {
                if let Some(s) = self.chunk_summary.get(&(chunk_pos + Vec2::new(dx, dy))) {
                    min = Some(min.map_or(s.min_bed_block, |m| m.min(s.min_bed_block)));
                }
            }
        }
        min
    }

    /// Per-chunk summaries of the authored water (chunks with any authored
    /// cell only).
    pub fn chunk_summary(&self, chunk_pos: Vec2<i32>) -> Option<&ChunkWaterSummary> {
        self.chunk_summary.get(&chunk_pos)
    }

    /// Compare the authored water with the sim's chunk kinds and log the
    /// result. A mismatch is an exporter defect, reported, not fatal: the
    /// columns follow the raster either way.
    pub(crate) fn report_consistency(&self, sim: &WorldSim) -> (usize, usize) {
        let chunk = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
        let mut dry_in_sim = Vec::new();
        let mut wet_in_sim = Vec::new();
        for region in &self.regions {
            let c0 = region.min / chunk;
            let c1 = region.max / chunk;
            for cy in c0.y..c1.y {
                for cx in c0.x..c1.x {
                    let cpos = Vec2::new(cx, cy);
                    let Some(sim_chunk) = sim.get(cpos) else {
                        continue;
                    };
                    let kind = sim_chunk.river.river_kind;
                    let wet = self.chunk_summary.get(&cpos).map_or(0, |s| s.wet_columns);
                    let area = (chunk.x * chunk.y) as u32;
                    if wet * 2 >= area && kind.is_none() {
                        dry_in_sim.push(cpos);
                    }
                    if wet == 0
                        && matches!(kind, Some(RiverKind::River { .. } | RiverKind::Lake { .. }))
                    {
                        wet_in_sim.push(cpos);
                    }
                }
            }
        }
        info!(
            regions = ?self.regions.iter().map(|r| &r.id).collect::<Vec<_>>(),
            chunks = self.chunk_summary.len(),
            "Loaded authored water rasters"
        );
        if !dry_in_sim.is_empty() || !wet_in_sim.is_empty() {
            warn!(
                authored_wet_but_sim_dry = dry_in_sim.len(),
                sim_water_but_authored_dry = wet_in_sim.len(),
                examples_authored_wet_sim_dry = ?&dry_in_sim[..dry_in_sim.len().min(5)],
                examples_sim_water_authored_dry = ?&wet_in_sim[..wet_in_sim.len().min(5)],
                "Authored water and the sim's water masks disagree (the exporter should keep the \
                 masks consistent with the raster; columns follow the raster regardless)"
            );
        }
        (dry_in_sim.len(), wet_in_sim.len())
    }
}

fn is_not_found(err: &assets::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err.reason());
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return io.kind() == std::io::ErrorKind::NotFound;
        }
        source = e.source();
    }
    false
}

/// The containment rules over one region's decoded tiles, plus the
/// per-chunk summaries:
///
/// * wet: bed block below surface block, surface not below the ocean's top;
/// * bank: ground not below the ocean's top (it would flood);
/// * every 4-neighbour of a wet cell inside the region is wet, or a bank at
///   least as high as the water (otherwise the water stands as a wall);
/// * no authored cell beyond the region box; no surface without a bed.
fn validate_region(
    id: &str,
    min: Vec2<i32>,
    max: Vec2<i32>,
    decoded: &HashMap<Vec2<i32>, RawTile>,
    summary: &mut HashMap<Vec2<i32>, ChunkWaterSummary>,
) -> Result<(), String> {
    let cell = |wpos: Vec2<i32>| -> AuthoredCell {
        let local = wpos - min;
        let t = local.map(|e| e.div_euclid(TILE_SIZE));
        match decoded.get(&t) {
            Some(raw) => {
                let c = local - t * TILE_SIZE;
                cell_of(raw, RawTile::idx(c.x, c.y))
            },
            None => AuthoredCell::None,
        }
    };
    let inside = |p: Vec2<i32>| p.x >= min.x && p.y >= min.y && p.x < max.x && p.y < max.y;
    let mut problems: Vec<String> = Vec::new();
    let mut count = 0usize;
    let mut report = |msg: String| {
        count += 1;
        if problems.len() < 10 {
            problems.push(msg);
        }
    };
    let chunk = TerrainChunkSize::RECT_SIZE.x as i32;
    // Deterministic order (the hash map's is not).
    let mut keys: Vec<_> = decoded.keys().copied().collect();
    keys.sort_by_key(|t| (t.y, t.x));
    for t in keys {
        let raw = &decoded[&t];
        let origin = min + t * TILE_SIZE;
        for j in 0..TILE_SIZE {
            for i in 0..TILE_SIZE {
                let k = RawTile::idx(i, j);
                let wpos = origin + Vec2::new(i, j);
                let (s, b) = (raw.surface_cm(k), raw.bed_cm(k));
                if s.is_none() && b.is_none() {
                    continue;
                }
                if !inside(wpos) {
                    report(format!("{wpos:?}: authored cell outside the region box"));
                    continue;
                }
                let c = cell_of(raw, k);
                let entry =
                    summary
                        .entry(wpos.map(|e| e.div_euclid(chunk)))
                        .or_insert(ChunkWaterSummary {
                            min_surface_block: i32::MAX,
                            max_surface_block: i32::MIN,
                            min_bed_block: i32::MAX,
                            ..Default::default()
                        });
                entry.authored_columns += 1;
                match c {
                    AuthoredCell::Wet {
                        surface_block,
                        bed_block,
                    } => {
                        entry.wet_columns += 1;
                        entry.min_surface_block = entry.min_surface_block.min(surface_block);
                        entry.max_surface_block = entry.max_surface_block.max(surface_block);
                        entry.min_bed_block = entry.min_bed_block.min(bed_block);
                        if bed_block >= surface_block {
                            report(format!(
                                "{wpos:?}: bed block {bed_block} is not below surface block \
                                 {surface_block}"
                            ));
                        }
                        if surface_block < SEA_TOP_BLOCK {
                            report(format!(
                                "{wpos:?}: surface block {surface_block} is below the ocean's top \
                                 block {SEA_TOP_BLOCK}"
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
                            match cell(n) {
                                AuthoredCell::Wet { .. } => {},
                                AuthoredCell::Bank { bed_block: nb } if nb >= surface_block => {},
                                AuthoredCell::Bank { bed_block: nb } => report(format!(
                                    "{wpos:?}: water (top block {surface_block}) next to bank \
                                     {n:?} whose ground top {nb} is lower: the water would stand \
                                     as a wall"
                                )),
                                AuthoredCell::None => report(format!(
                                    "{wpos:?}: water next to unauthored column {n:?} inside the \
                                     region; write a bank cell there"
                                )),
                            }
                        }
                    },
                    AuthoredCell::Bank { bed_block } => {
                        entry.min_bed_block = entry.min_bed_block.min(bed_block);
                        if bed_block < SEA_TOP_BLOCK {
                            report(format!(
                                "{wpos:?}: bank ground block {bed_block} is below the ocean's top \
                                 block {SEA_TOP_BLOCK} and would flood"
                            ));
                        }
                    },
                    AuthoredCell::None => {
                        report(format!("{wpos:?}: surface without a bed"));
                    },
                }
            }
        }
    }
    if count == 0 {
        Ok(())
    } else {
        Err(format!(
            "{count} invalid cell(s) in region '{id}', first: {}",
            problems.join("; ")
        ))
    }
}

#[cfg(test)] mod real_world_tests;
#[cfg(test)] mod tests;
