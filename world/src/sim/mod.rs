mod diffusion;
mod erosion;
mod location;
mod map;
mod util;
mod way;

// Reexports
use self::erosion::Compute;
pub use self::{
    diffusion::diffusion,
    location::Location,
    map::{sample_pos, sample_wpos},
    util::get_horizon_map,
    way::{Path, Way},
};
pub(crate) use self::{
    erosion::{
        Alt, RiverData, RiverKind, do_erosion, fill_sinks, get_lakes, get_multi_drainage,
        get_multi_rec, get_rivers,
    },
    util::{
        InverseCdf, cdf_irwin_hall, downhill, get_oceans, local_cells, local_channel_radius_chunks,
        map_edge_factor, uniform_noise, uphill,
    },
};

use crate::{
    CONFIG, IndexRef,
    all::{Environment, ForestKind, TreeAttr},
    block::BlockGen,
    civ::{Place, PointOfInterest},
    column::ColumnGen,
    config,
    site::Site,
    util::{
        CARDINALS, DHashSet, FastNoise, FastNoise2d, LOCALITY, NEIGHBORS, RandomField, Sampler,
        StructureGen2d, seed_expan,
    },
};
use bincode::{
    config::legacy,
    serde::{decode_from_std_read, encode_into_std_write},
};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_bincode_legacy, load_ron},
    calendar::Calendar,
    grid::Grid,
    lottery::Lottery,
    resources::MapKind,
    spiral::Spiral2d,
    spot::Spot,
    store::{Id, Store},
    terrain::{
        BiomeKind, CoordinateConversions, MapSizeLg, TerrainChunk, TerrainChunkSize,
        map::MapConfig, neighbors, uniform_idx_as_vec2, vec2_as_uniform_idx,
    },
    vol::RectVolSize,
};
use common_base::prof_span;
use common_net::msg::WorldMapMsg;
use noise::{
    BasicMulti, Billow, Fbm, HybridMulti, MultiFractal, NoiseFn, Perlin, RidgedMulti, SuperSimplex,
    core::worley::distance_functions,
};
use num::{Float, Signed, traits::FloatConst};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaChaRng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::VecDeque,
    f32,
    fs::File,
    io::{BufReader, BufWriter},
    ops::{Add, Div, Mul, Neg, Sub},
    path::PathBuf,
    sync::Arc,
};
use strum::IntoEnumIterator;
use tracing::{debug, info, warn};
use vek::*;

/// Default base two logarithm of the world size, in chunks, per dimension.
///
/// Currently, our default map dimensions are 2^10 × 2^10 chunks,
/// mostly for historical reasons.  It is likely that we will increase this
/// default at some point.
const DEFAULT_WORLD_CHUNKS_LG: MapSizeLg =
    if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
        map_size_lg
    } else {
        panic!("Default world chunk size does not satisfy required invariants.");
    };

/// A structure that holds cached noise values and cumulative distribution
/// functions for the input that led to those values.  See the definition of
/// InverseCdf for a description of how to interpret the types of its fields.
struct GenCdf {
    pub(crate) authored_cromatolis_v0: bool,
    /// Stable id of the specific authored region that's loaded (if any),
    /// independent of `authored_cromatolis_v0`. Only threaded through for
    /// call sites that need to distinguish "this specific region" from "any
    /// authored region" -- see `SimChunk::get_biome`'s `Snowland` check.
    pub(crate) authored_region_id: Option<&'static str>,
    authored_route_layer: Option<Box<[f32]>>,
    authored_vegetation_layer: Option<Box<[f32]>>,
    /// Independent authored visual-ground-cover signal. Unlike
    /// `authored_vegetation_layer`, this is never consumed by tree placement.
    authored_ground_cover_layer: Option<Box<[f32]>>,
    /// Categorical climate-zone raster: which [`ClimateZone`] each chunk sits
    /// in. Selects the chunk's sea-level temperature anchor -- the reason two
    /// chunks at opposite ends of the map at the same elevation no longer get
    /// bit-identical temperature.
    authored_climate_zone_layer: Option<Box<[f32]>>,
    /// Categorical terrain exceptions (currently only the small sand area
    /// outside Northwall Stone). Kept separate from the continuous cover
    /// raster because "bare" never implicitly means "sand".
    authored_ground_substrate_zones: Option<ResolvedGroundSubstrateZones>,
    /// Authored baseline-temperature-curve tuning for the loaded region (if
    /// any), or the default curve if none is loaded / the asset failed to
    /// parse. Already resolved into chunk space, so per-chunk generation only
    /// reads it. See `cromatolis_baseline_temp`.
    pub(crate) cromatolis_climate: ResolvedCromatolisClimate,
    authored_alpine_policy: Option<(&'static str, AuthoredAlpinePolicy)>,
    /// Per-chunk "adjacent to authored water" signal, see
    /// `SimChunk::authored_near_water`.
    authored_near_water: Box<[bool]>,
    /// Per-chunk ecological water-body classification, see
    /// `SimChunk::water_body`. `None` for dry chunks and everywhere outside an
    /// authored region.
    authored_water_body: Box<[Option<WaterBodyKind>]>,
    /// Per-chunk water salinity, see `SimChunk::salinity`. `None` wherever
    /// `authored_water_body` is `None`.
    authored_salinity: Box<[Option<Salinity>]>,
    humid_base: InverseCdf,
    temp_base: InverseCdf,
    chaos: InverseCdf,
    alt: Box<[Alt]>,
    basement: Box<[Alt]>,
    water_alt: Box<[f32]>,
    dh: Box<[isize]>,
    /// NOTE: Until we hit 4096 × 4096, this should suffice since integers with
    /// an absolute value under 2^24 can be exactly represented in an f32.
    flux: Box<[Compute]>,
    pure_flux: InverseCdf<Compute>,
    alt_no_water: InverseCdf,
    rivers: Box<[RiverData]>,
}

pub(crate) struct GenCtx {
    pub turb_x_nz: SuperSimplex,
    pub turb_y_nz: SuperSimplex,
    pub chaos_nz: RidgedMulti<Perlin>,
    pub alt_nz: util::HybridMulti<Perlin>,
    pub hill_nz: SuperSimplex,
    pub temp_nz: Fbm<Perlin>,
    // Humidity noise
    pub humid_nz: Billow<Perlin>,
    // Small amounts of noise for simulating rough terrain.
    pub small_nz: BasicMulti<Perlin>,
    pub rock_nz: HybridMulti<Perlin>,
    pub tree_nz: BasicMulti<Perlin>,

    // TODO: unused, remove??? @zesterer
    pub _cave_0_nz: SuperSimplex,
    pub _cave_1_nz: SuperSimplex,

    pub structure_gen: StructureGen2d,
    pub _big_structure_gen: StructureGen2d,
    pub _region_gen: StructureGen2d,

    pub _fast_turb_x_nz: FastNoise,
    pub _fast_turb_y_nz: FastNoise,

    pub _town_gen: StructureGen2d,
    pub river_seed: RandomField,
    pub rock_strength_nz: Fbm<Perlin>,
    pub uplift_nz: util::Worley,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GenOpts {
    pub x_lg: u32,
    pub y_lg: u32,
    pub scale: f64,
    pub map_kind: MapKind,
    pub erosion_quality: f32,
}

impl Default for GenOpts {
    fn default() -> Self {
        Self {
            x_lg: 10,
            y_lg: 10,
            scale: 2.0,
            map_kind: MapKind::Square,
            erosion_quality: 1.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum FileOpts {
    /// If set, generate the world map and do not try to save to or load from
    /// file (default).
    Generate(GenOpts),
    /// If set, generate the world map and save the world file (path is created
    /// the same way screenshot paths are).
    Save(PathBuf, GenOpts),
    /// Combination of Save and Load.
    /// Load map if exists or generate the world map and save the
    /// world file.
    LoadOrGenerate {
        name: String,
        #[serde(default)]
        opts: GenOpts,
        #[serde(default)]
        overwrite: bool,
    },
    /// If set, load the world file from this path in legacy format (errors if
    /// path not found).  This option may be removed at some point, since it
    /// only applies to maps generated before map saving was merged into
    /// master.
    LoadLegacy(PathBuf),
    /// If set, load the world file from this path (errors if path not found).
    Load(PathBuf),
    /// If set, look for  the world file at this asset specifier (errors if
    /// asset is not found).
    ///
    /// NOTE: Could stand to merge this with `Load` and construct an enum that
    /// can handle either a PathBuf or an asset specifier, at some point.
    LoadAsset(String),
}

impl Default for FileOpts {
    fn default() -> Self { Self::Generate(GenOpts::default()) }
}

impl FileOpts {
    /// The authored region this `FileOpts` activates, if any is registered
    /// for the underlying map asset. See [`AUTHORED_REGIONS`].
    fn authored_region(&self) -> Option<&'static AuthoredRegion> {
        match self {
            Self::LoadAsset(specifier) => authored_region_for_map_asset(specifier),
            _ => None,
        }
    }

    fn load_content(&self) -> (Option<ModernMap>, MapSizeLg, GenOpts) {
        let parsed_world_file = self.try_load_map();

        let mut gen_opts = self.gen_opts().unwrap_or_default();

        let map_size_lg = if let Some(map) = &parsed_world_file {
            MapSizeLg::new(map.map_size_lg)
                .expect("World size of loaded map does not satisfy invariants.")
        } else {
            self.map_size()
        };

        // NOTE: Change 1.0 to 4.0 for a 4x
        // improvement in world detail.  We also use this to automatically adjust
        // grid_scale (multiplying by 4.0) and multiply mins_per_sec by
        // 1.0 / (4.0 * 4.0) in ./erosion.rs, in order to get a similar rate of river
        // formation.
        //
        // FIXME: This is a hack!  At some point we will have a more principled way of
        // dealing with this.
        if let Some(map) = &parsed_world_file {
            gen_opts.scale = map.continent_scale_hack;
        };

        (parsed_world_file, map_size_lg, gen_opts)
    }

    fn gen_opts(&self) -> Option<GenOpts> {
        match self {
            Self::Generate(opts) | Self::Save(_, opts) | Self::LoadOrGenerate { opts, .. } => {
                Some(opts.clone())
            },
            _ => None,
        }
    }

    // TODO: this should return Option so that caller can choose fallback
    fn map_size(&self) -> MapSizeLg {
        match self {
            Self::Generate(opts) | Self::Save(_, opts) | Self::LoadOrGenerate { opts, .. } => {
                MapSizeLg::new(Vec2 {
                    x: opts.x_lg,
                    y: opts.y_lg,
                })
                .unwrap_or_else(|e| {
                    warn!("World size does not satisfy invariants: {:?}", e);
                    DEFAULT_WORLD_CHUNKS_LG
                })
            },
            _ => DEFAULT_WORLD_CHUNKS_LG,
        }
    }

    // TODO: This should probably return a Result, so that caller can choose
    // whether to log error
    fn try_load_map(&self) -> Option<ModernMap> {
        let map = match self {
            Self::LoadLegacy(path) => {
                let file = match File::open(path) {
                    Ok(file) => file,
                    Err(e) => {
                        warn!(?e, ?path, "Couldn't read path for maps");
                        return None;
                    },
                };

                let mut reader = BufReader::new(file);
                let map: WorldFileLegacy = match decode_from_std_read(&mut reader, legacy()) {
                    Ok(map) => map,
                    Err(e) => {
                        warn!(
                            ?e,
                            "Couldn't parse legacy map.  Maybe you meant to try a regular load?"
                        );
                        return None;
                    },
                };

                map.into_modern()
            },
            Self::Load(path) => {
                let file = match File::open(path) {
                    Ok(file) => file,
                    Err(e) => {
                        warn!(?e, ?path, "Couldn't read path for maps");
                        return None;
                    },
                };

                let mut reader = BufReader::new(file);
                let map: WorldFile = match decode_from_std_read(&mut reader, legacy()) {
                    Ok(map) => map,
                    Err(e) => {
                        warn!(
                            ?e,
                            "Couldn't parse modern map.  Maybe you meant to try a legacy load?"
                        );
                        return None;
                    },
                };

                map.into_modern()
            },
            Self::LoadAsset(specifier) => match WorldFile::load_owned(specifier) {
                Ok(map) => map.into_modern(),
                Err(err) => {
                    match err.reason().downcast_ref::<std::io::Error>() {
                        Some(e) => {
                            warn!(?e, ?specifier, "Couldn't read asset specifier for maps");
                        },
                        None => {
                            warn!(
                                ?err,
                                "Couldn't parse modern map.  Maybe you meant to try a legacy load?"
                            );
                        },
                    }
                    return None;
                },
            },
            Self::LoadOrGenerate {
                opts, overwrite, ..
            } => {
                // `unwrap` is safe here, because LoadOrGenerate has its path
                // always defined
                let path = self.map_path().unwrap();

                let file = match File::open(&path) {
                    Ok(file) => file,
                    Err(e) => {
                        warn!(?e, ?path, "Couldn't find needed map. Generating...");
                        return None;
                    },
                };

                let mut reader = BufReader::new(file);
                let map: WorldFile = match decode_from_std_read(&mut reader, legacy()) {
                    Ok(map) => map,
                    Err(e) => {
                        warn!(
                            ?e,
                            "Couldn't parse modern map.  Maybe you meant to try a legacy load?"
                        );
                        return None;
                    },
                };

                // FIXME:
                // We check if we need to generate new map by comparing gen opts.
                // But we also have another generation paramater that currently
                // passed outside and used for both worldsim and worldgen.
                //
                // Ideally, we need to figure out how we want to use seed, i. e.
                // moving worldgen seed to gen opts and use different sim seed from
                // server config or grab sim seed from world file.
                //
                // NOTE: we intentionally use pattern-matching here to get
                // options, so that when gen opts get another field, compiler
                // will force you to update following logic
                let GenOpts {
                    x_lg, y_lg, scale, ..
                } = opts;
                let map = match map {
                    WorldFile::Veloren0_7_0(map) => map,
                    WorldFile::Veloren0_5_0(_) => {
                        panic!("World file v0.5.0 isn't supported with LoadOrGenerate.")
                    },
                };

                if map.continent_scale_hack != *scale || map.map_size_lg != Vec2::new(*x_lg, *y_lg)
                {
                    if *overwrite {
                        warn!(
                            "{}\n{}",
                            "Specified options don't correspond to these in loaded map.",
                            "Map will be regenerated and overwritten."
                        );
                    } else {
                        panic!(
                            "{}\n{}",
                            "Specified options don't correspond to these in loaded map.",
                            "Use 'ovewrite' option, if you wish to regenerate map."
                        );
                    }

                    return None;
                }

                map.into_modern()
            },
            Self::Generate { .. } | Self::Save { .. } => return None,
        };

        match map {
            Ok(map) => Some(map),
            Err(e) => {
                match e {
                    WorldFileError::WorldSizeInvalid => {
                        warn!("World size of map is invalid.");
                    },
                }
                None
            },
        }
    }

    fn map_path(&self) -> Option<PathBuf> {
        // TODO: Work out a nice bincode file extension.
        match self {
            Self::Save(path, _) => Some(PathBuf::from(&path)),
            Self::LoadOrGenerate { name, .. } => {
                const MAP_DIR: &str = "./maps";
                let file_name = format!("{}.bin", name);
                Some(std::path::Path::new(MAP_DIR).join(file_name))
            },
            _ => None,
        }
    }

    fn save(&self, map: &WorldFile) {
        let path = if let Some(path) = self.map_path() {
            path
        } else {
            return;
        };

        // Check if folder exists and create it if it does not
        let map_dir = path.parent().expect("failed to get map directory");
        if !map_dir.exists()
            && let Err(e) = std::fs::create_dir_all(map_dir)
        {
            warn!(?e, ?map_dir, "Couldn't create folder for map");
            return;
        }

        let file = match File::create(path.clone()) {
            Ok(file) => file,
            Err(e) => {
                warn!(?e, ?path, "Couldn't create file for maps");
                return;
            },
        };

        let mut writer = BufWriter::new(file);
        if let Err(e) = encode_into_std_write(map, &mut writer, legacy()) {
            warn!(?e, "Couldn't write map");
        }
        if let Ok(p) = std::fs::canonicalize(path) {
            info!("Map saved at {}", p.to_string_lossy());
        }
    }
}

pub struct WorldOpts {
    /// Set to false to disable seeding elements during worldgen.
    pub seed_elements: bool,
    pub world_file: FileOpts,
    pub calendar: Option<Calendar>,
}

impl Default for WorldOpts {
    fn default() -> Self {
        Self {
            seed_elements: true,
            world_file: Default::default(),
            calendar: None,
        }
    }
}

/// LEGACY: Remove when people stop caring.
#[derive(Serialize, Deserialize)]
#[repr(C)]
pub struct WorldFileLegacy {
    /// Saved altitude height map.
    pub alt: Box<[Alt]>,
    /// Saved basement height map.
    pub basement: Box<[Alt]>,
}

/// Version of the world map intended for use in Veloren 0.5.0.
#[derive(Serialize, Deserialize)]
#[repr(C)]
pub struct WorldMap_0_5_0 {
    /// Saved altitude height map.
    pub alt: Box<[Alt]>,
    /// Saved basement height map.
    pub basement: Box<[Alt]>,
}

/// Version of the world map intended for use in Veloren 0.7.0.
#[derive(Serialize, Deserialize)]
#[repr(C)]
pub struct WorldMap_0_7_0 {
    /// Saved map size.
    pub map_size_lg: Vec2<u32>,
    /// Saved continent_scale hack, to try to better approximate the correct
    /// seed according to varying map size.
    ///
    /// TODO: Remove when generating new maps becomes more principled.
    pub continent_scale_hack: f64,
    /// Saved altitude height map.
    pub alt: Box<[Alt]>,
    /// Saved basement height map.
    pub basement: Box<[Alt]>,
}

/// Errors when converting a map to the most recent type (currently,
/// shared by the various map types, but at some point we might switch to
/// version-specific errors if it feels worthwhile).
#[derive(Debug)]
pub enum WorldFileError {
    /// Map size was invalid, and it can't be converted to a valid one.
    WorldSizeInvalid,
}

/// WORLD MAP.
///
/// A way to store certain components between runs of map generation.  Only
/// intended for development purposes--no attempt is made to detect map
/// invalidation or make sure that the map is synchronized with updates to
/// noise-rs, changes to other parameters, etc.
///
/// The map is versioned to enable format detection between versions of Veloren,
/// so that when we update the map format we don't break existing maps (or at
/// least, we will try hard not to break maps between versions; if we can't
/// avoid it, we can at least give a reasonable error message).
///
/// NOTE: We rely somewhat heavily on the implementation specifics of bincode
/// to make sure this is backwards compatible.  When adding new variants here,
/// Be very careful to make sure tha the old variants are preserved in the
/// correct order and with the correct names and indices, and make sure to keep
/// the #[repr(u32)]!
///
/// All non-legacy versions of world files should (ideally) fit in this format.
/// Since the format contains a version and is designed to be extensible
/// backwards-compatibly, the only reason not to use this forever would be if we
/// decided to move away from BinCode, or store data across multiple files (or
/// something else weird I guess).
///
/// Update this when you add a new map version.
#[derive(Serialize, Deserialize)]
#[repr(u32)]
pub enum WorldFile {
    Veloren0_5_0(WorldMap_0_5_0) = 0,
    Veloren0_7_0(WorldMap_0_7_0) = 1,
}

impl FileAsset for WorldFile {
    const EXTENSION: &'static str = "bin";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_bincode_legacy(&bytes) }
}

struct AuthoredF32Layer {
    values: Box<[f32]>,
}

impl FileAsset for AuthoredF32Layer {
    const EXTENSION: &'static str = "f32le";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> {
        let (chunks, remainder) = bytes.as_chunks::<4>();
        if !remainder.is_empty() {
            return Err("authored f32 layer length was not a multiple of 4".into());
        }
        let values = chunks
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self { values })
    }
}

type TreeCandidateField = (Vec2<i32>, u32);

/// A coordinate in source-map space: X increases eastward and Y increases
/// southward, matching the normalized top-left polygons exported by Open
/// World. World positions use the opposite Y direction, so conversion is
/// deliberately centralized in
/// [`AuthoredTreeCandidateZone::contains_world_pos`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
struct NormalizedTopLeftPoint {
    x: f32,
    y: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct TreeCandidateGridSpec {
    frequency_blocks: u32,
    spread_blocks: u32,
    seed_salt: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct AuthoredTreeCandidateZone {
    id: String,
    source_region_shape: String,
    polygon_normalized_top_left: Vec<NormalizedTopLeftPoint>,
    additional_grid: TreeCandidateGridSpec,
}

impl AuthoredTreeCandidateZone {
    fn contains_world_pos(&self, wpos: Vec2<i32>, world_blocks: Vec2<i32>) -> bool {
        if world_blocks.x <= 0 || world_blocks.y <= 0 {
            return false;
        }
        let point = NormalizedTopLeftPoint {
            x: wpos.x as f32 / world_blocks.x as f32,
            y: 1.0 - wpos.y as f32 / world_blocks.y as f32,
        };
        point_in_normalized_top_left_polygon(point, &self.polygon_normalized_top_left)
    }
}

/// Even-odd ray cast, with a point lying *on* an edge counted as inside.
///
/// The one point-in-polygon primitive for authored zones. It is deliberately
/// space-agnostic — callers hand it whatever 2D space their polygon is already
/// expressed in (normalized for tree-candidate zones, chunk space for
/// microclimates) — because the containment test is identical in every space
/// and two copies of it drift: the first pair of these that existed disagreed
/// on the on-edge case, which is exactly the case a hand-authored polygon
/// snapped to a round coordinate lands on.
///
/// Takes an iterator rather than a slice so a caller whose vertices are stored
/// as some other point type can map into it without allocating. `Clone` is
/// required because the edge walk pairs the sequence with itself.
fn point_in_polygon<I>(point: Vec2<f32>, vertices: I) -> bool
where
    I: IntoIterator<Item = Vec2<f32>>,
    I::IntoIter: Clone + ExactSizeIterator,
{
    let vertices = vertices.into_iter();
    let len = vertices.len();
    if len < 3 {
        return false;
    }

    let mut inside = false;
    for (start, end) in vertices.clone().zip(vertices.cycle().skip(1)).take(len) {
        let cross =
            (end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x);
        let on_segment = cross.abs() <= f32::EPSILON
            && point.x >= start.x.min(end.x)
            && point.x <= start.x.max(end.x)
            && point.y >= start.y.min(end.y)
            && point.y <= start.y.max(end.y);
        if on_segment {
            return true;
        }

        let crosses_ray = (start.y > point.y) != (end.y > point.y)
            && point.x < (end.x - start.x) * (point.y - start.y) / (end.y - start.y) + start.x;
        if crosses_ray {
            inside = !inside;
        }
    }
    inside
}

/// [`point_in_polygon`] for a polygon still in its authored
/// `polygon_normalized_top_left` form. The name describes where the *polygon*
/// came from, not the test: callers have already put `point` in the same space
/// as `polygon`, whichever that is.
fn point_in_normalized_top_left_polygon(
    point: NormalizedTopLeftPoint,
    polygon: &[NormalizedTopLeftPoint],
) -> bool {
    point_in_polygon(
        Vec2::new(point.x, point.y),
        polygon.iter().map(|vertex| Vec2::new(vertex.x, vertex.y)),
    )
}

/// Data-owned, region-scoped supplement to the normal tree-root lattice.
/// This is intentionally separate from the vegetation mask: it controls
/// only which roots are offered to the existing placement gates.
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct AuthoredTreeCandidatePolicy {
    schema: u32,
    zones: Vec<AuthoredTreeCandidateZone>,
}

impl AuthoredTreeCandidatePolicy {
    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!(
                "expected tree candidate policy schema 1, got {}",
                self.schema
            ));
        }
        if self.zones.is_empty() {
            return Err("tree candidate policy requires at least one zone".to_owned());
        }

        let mut ids = DHashSet::default();
        for zone in &self.zones {
            if zone.id.is_empty() || !ids.insert(zone.id.as_str()) {
                return Err("tree candidate zone ids must be unique and non-empty".to_owned());
            }
            if zone.source_region_shape.is_empty() {
                return Err(format!(
                    "tree candidate zone '{}' has no source region shape",
                    zone.id
                ));
            }
            if zone.polygon_normalized_top_left.len() < 3
                || zone.polygon_normalized_top_left.iter().any(|point| {
                    !point.x.is_finite()
                        || !point.y.is_finite()
                        || !(0.0..=1.0).contains(&point.x)
                        || !(0.0..=1.0).contains(&point.y)
                })
            {
                return Err(format!(
                    "tree candidate zone '{}' requires a finite normalized polygon with at least \
                     three points",
                    zone.id
                ));
            }
            let grid = &zone.additional_grid;
            if grid.frequency_blocks == 0
                || grid.frequency_blocks > i32::MAX as u32
                || grid.spread_blocks.saturating_mul(2) > grid.frequency_blocks
            {
                return Err(format!(
                    "tree candidate zone '{}' has an invalid grid frequency/spread",
                    zone.id
                ));
            }
        }
        Ok(())
    }

    fn compile(&self, world_seed: u32) -> Result<RegionalTreeCandidatePolicy, String> {
        self.validate()?;
        Ok(RegionalTreeCandidatePolicy {
            zones: self
                .zones
                .iter()
                .cloned()
                .map(|zone| RegionalTreeCandidateZone {
                    generator: StructureGen2d::new(
                        world_seed ^ zone.additional_grid.seed_salt,
                        zone.additional_grid.frequency_blocks,
                        zone.additional_grid.spread_blocks,
                    ),
                    zone,
                })
                .collect(),
        })
    }
}

impl FileAsset for AuthoredTreeCandidatePolicy {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

struct RegionalTreeCandidateZone {
    zone: AuthoredTreeCandidateZone,
    generator: StructureGen2d,
}

struct RegionalTreeCandidatePolicy {
    zones: Vec<RegionalTreeCandidateZone>,
}

impl RegionalTreeCandidateZone {
    /// A `StructureGen2d::get` samples its cell plus the eight neighbours.
    /// This is the largest possible root distance from the queried column,
    /// including the grid's jitter. It is deliberately conservative: a false
    /// positive merely evaluates the normal regional filter, whereas a false
    /// negative would drop a tree at the edge of an authored forest.
    fn candidate_query_margin(&self) -> i32 {
        let grid = &self.zone.additional_grid;
        let frequency = grid.frequency_blocks as i32;
        frequency
            .saturating_add(frequency / 2)
            .saturating_add(grid.spread_blocks as i32)
    }

    fn world_bounds(&self, world_blocks: Vec2<i32>) -> (Vec2<i32>, Vec2<i32>) {
        let (min_x, max_x, min_y, max_y) = self.zone.polygon_normalized_top_left.iter().fold(
            (
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ),
            |(min_x, max_x, min_y, max_y), point| {
                (
                    min_x.min(point.x),
                    max_x.max(point.x),
                    min_y.min(point.y),
                    max_y.max(point.y),
                )
            },
        );
        (
            Vec2::new(
                (min_x * world_blocks.x as f32).floor() as i32,
                ((1.0 - max_y) * world_blocks.y as f32).floor() as i32,
            ),
            Vec2::new(
                (max_x * world_blocks.x as f32).ceil() as i32,
                ((1.0 - min_y) * world_blocks.y as f32).ceil() as i32,
            ),
        )
    }

    fn query_can_reach_bounds(
        &self,
        min: Vec2<i32>,
        max: Vec2<i32>,
        world_blocks: Vec2<i32>,
    ) -> bool {
        let (zone_min, zone_max) = self.world_bounds(world_blocks);
        let margin = self.candidate_query_margin();
        zone_min.x.saturating_sub(margin) <= max.x
            && zone_max.x.saturating_add(margin) >= min.x
            && zone_min.y.saturating_sub(margin) <= max.y
            && zone_max.y.saturating_add(margin) >= min.y
    }

    fn may_supply_candidates_near(&self, wpos: Vec2<i32>, world_blocks: Vec2<i32>) -> bool {
        self.query_can_reach_bounds(wpos, wpos, world_blocks)
    }

    fn may_supply_candidates_in_area(
        &self,
        min: Vec2<i32>,
        max: Vec2<i32>,
        world_blocks: Vec2<i32>,
    ) -> bool {
        self.query_can_reach_bounds(min, max, world_blocks)
    }
}

impl RegionalTreeCandidatePolicy {
    fn may_supply_candidates_near(&self, wpos: Vec2<i32>, world_blocks: Vec2<i32>) -> bool {
        self.zones
            .iter()
            .any(|zone| zone.may_supply_candidates_near(wpos, world_blocks))
    }

    fn may_supply_candidates_in_area(
        &self,
        min: Vec2<i32>,
        max: Vec2<i32>,
        world_blocks: Vec2<i32>,
    ) -> bool {
        self.zones
            .iter()
            .any(|zone| zone.may_supply_candidates_in_area(min, max, world_blocks))
    }

    fn additional_candidates_near(
        &self,
        wpos: Vec2<i32>,
        world_blocks: Vec2<i32>,
    ) -> Vec<TreeCandidateField> {
        self.zones
            .iter()
            .filter(|zone| zone.may_supply_candidates_near(wpos, world_blocks))
            .flat_map(|zone| {
                zone.generator
                    .get(wpos)
                    .into_iter()
                    .filter(move |(candidate, _)| {
                        zone.zone.contains_world_pos(*candidate, world_blocks)
                    })
            })
            .collect()
    }

    fn additional_candidates_in_area(
        &self,
        min: Vec2<i32>,
        max: Vec2<i32>,
        world_blocks: Vec2<i32>,
    ) -> Vec<TreeCandidateField> {
        self.zones
            .iter()
            .filter(|zone| zone.may_supply_candidates_in_area(min, max, world_blocks))
            .flat_map(|zone| {
                zone.generator.iter(min, max).filter(move |(candidate, _)| {
                    zone.zone.contains_world_pos(*candidate, world_blocks)
                })
            })
            .collect()
    }
}

fn merge_tree_candidate_fields(
    global: impl IntoIterator<Item = TreeCandidateField>,
    regional: impl IntoIterator<Item = TreeCandidateField>,
) -> Vec<TreeCandidateField> {
    let mut positions = DHashSet::default();
    global
        .into_iter()
        .chain(regional)
        .filter(|(position, _)| positions.insert(*position))
        .collect()
}

/// One of the authored raster layers a region may ship alongside its base
/// heightmap `.bin`. The asset specifier for a given region + kind is always
/// `"{region.map_asset}_{kind.asset_suffix()}"` (e.g.
/// `"world.map.cromatolis_v0_water"`), matching the convention
/// `xindeler-open-world`'s `export-new-horizon` command already produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthoredLayerKind {
    Routes,
    Vegetation,
    GroundCover,
    Water,
    ElevatedLakes,
    RiverChannels,
    ClimateZone,
    EcologyZone,
}

impl AuthoredLayerKind {
    /// All layer kinds a region can ship, in the order they're loaded.
    const ALL: [Self; 8] = [
        Self::Routes,
        Self::Vegetation,
        Self::GroundCover,
        Self::Water,
        Self::ElevatedLakes,
        Self::RiverChannels,
        Self::ClimateZone,
        Self::EcologyZone,
    ];

    fn asset_suffix(self) -> &'static str {
        match self {
            Self::Routes => "routes",
            Self::Vegetation => "vegetation",
            Self::GroundCover => "ground_cover",
            Self::Water => "water",
            Self::ElevatedLakes => "elevated_lakes",
            Self::RiverChannels => "river_channels",
            Self::ClimateZone => "climate_zone",
            Self::EcologyZone => "ecology_zone",
        }
    }
}

/// A hand-authored map region that ships extra raster layers alongside its
/// base heightmap `.bin`, keyed by the `FileOpts::LoadAsset` specifier that
/// activates it.
///
/// V1 has exactly one populated region (Cromatolis, see
/// [`AUTHORED_REGIONS`]); this is the minimal abstraction needed so that
/// opening a second region later is "add a registry entry," not "add
/// another hardcoded branch" (per COW-2's region-within-one-world
/// architecture). This does *not* yet support multiple regions being loaded
/// simultaneously -- `WorldSim` still loads one `FileOpts` per server
/// instance.
struct AuthoredRegion {
    /// The `FileOpts::LoadAsset` specifier that activates this region.
    map_asset: &'static str,
    /// Stable region id, independent of the map asset name -- kept distinct
    /// from `map_asset` so a region can be renamed/re-pointed without
    /// changing its identity. Load-bearing, not just logging: this flows
    /// into `SimChunk::authored_region_id` (see `CROMATOLIS_V0_REGION_ID`),
    /// which 25+ call sites gate on, including biome Snowland/Desert
    /// scoping and `generate_cliffs`'s vegetation-crush guard here in
    /// `sim/mod.rs`, ground-cover resolution in `column.rs`, wildlife
    /// density gating in `layer/wildlife.rs`, `layer/spot.rs`, and
    /// settlement/landmark/pathfinding gating in `civ/mod.rs`.
    id: &'static str,
    /// Which authored layers this region ships.
    layers: &'static [AuthoredLayerKind],
    ground_cover_profile: &'static str,
    map_ecology_profile: &'static str,
    ground_substrate_zones: &'static str,
    fortifications: &'static str,
    tree_candidate_policy: &'static str,
    alpine_policy: Option<&'static str>,
}

/// Threshold above which an authored water/elevated-lake/river-channel mask
/// value (each in `[0, 1]`, see `AuthoredF32Layer`) counts as "hit". The real
/// exported Cromatolis masks are strictly binary (checked directly against
/// the LFS assets: every sampled value is either `0.0` or `1.0`, no
/// blended/fringe values), so this only has to split that binary signal, not
/// pick a point on a gradient.
const AUTHORED_WATER_THRESHOLD: f32 = 0.5;

/// Stable id of the Cromatolis authored region (see [`AUTHORED_REGIONS`]).
/// Kept as a named const, rather than a literal repeated at every call site
/// that needs to check "is this specifically Cromatolis" (as opposed to "is
/// any authored region loaded" -- see `authored_cromatolis_v0`), so a rename
/// can't silently desync the registry entry from its consumers.
///
/// `pub(crate)` (rather than private to this module) so that `world/src/civ`
/// can gate its own region-scoped loaders (settlements/landmarks, see COW-6)
/// on `SimChunk::authored_region_id` the same way this module's own
/// Snowland/Swamp checks do, instead of re-deriving a region check from the
/// generic `authored_cromatolis_v0` flag (the COW-2 debt this repo is trying
/// not to grow).
pub(crate) const CROMATOLIS_V0_REGION_ID: &str = "cromatolis_v0";

/// The region registry. Add an entry here to make a new hand-authored map
/// region's extra layers loadable; nothing else in the loading path should
/// need to change.
const AUTHORED_REGIONS: &[AuthoredRegion] = &[AuthoredRegion {
    map_asset: "world.map.cromatolis_v0",
    id: CROMATOLIS_V0_REGION_ID,
    layers: &AuthoredLayerKind::ALL,
    ground_cover_profile: "world.map.cromatolis_v0_ground_cover",
    map_ecology_profile: "world.map.cromatolis_v0_map_ecology",
    ground_substrate_zones: "world.map.cromatolis_v0_ground_substrate_zones",
    fortifications: "world.map.cromatolis_v0_fortifications",
    tree_candidate_policy: "world.map.cromatolis_v0_tree_candidate_policy",
    alpine_policy: Some("world.map.cromatolis_v0_alpine"),
}];

/// One regional alpine policy, loaded once per authored world. All heights
/// are real relief metres above sea level, never `SimChunk::alt`.
#[derive(Clone, Copy, Debug, Deserialize)]
pub(crate) struct AuthoredAlpinePolicy {
    schema: u32,
    pub tree_line_altitude_m: f32,
    pub snow_start_altitude_m: f32,
    pub persistent_snow_altitude_m: f32,
    pub transition_rock_blend: f32,
    pub slope_rock_blend: f32,
}

impl AuthoredAlpinePolicy {
    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!(
                "unsupported authored alpine policy schema {}",
                self.schema
            ));
        }
        for value in [
            self.tree_line_altitude_m,
            self.snow_start_altitude_m,
            self.persistent_snow_altitude_m,
            self.transition_rock_blend,
            self.slope_rock_blend,
        ] {
            if !value.is_finite() {
                return Err("alpine policy values must be finite".into());
            }
        }
        if self.tree_line_altitude_m < 0.0
            || self.tree_line_altitude_m > self.snow_start_altitude_m
            || self.persistent_snow_altitude_m <= self.snow_start_altitude_m
        {
            return Err("alpine policy altitudes must be ordered".into());
        }
        if !(0.0..=1.0).contains(&self.transition_rock_blend)
            || !(0.0..=1.0).contains(&self.slope_rock_blend)
        {
            return Err("alpine policy rock blends must be within 0..=1".into());
        }
        if self.transition_rock_blend + self.slope_rock_blend > 1.0 {
            return Err("alpine policy rock blend weights may not exceed 1.0 together".into());
        }
        Ok(())
    }

    /// Returns the explicit authored surface weights for a real relief and
    /// normalized slope. `None` means the column is below this policy's
    /// alpine transition and must keep its inherited terrain appearance.
    pub(crate) fn surface_at(&self, relief_m: f32, slope: f32) -> Option<AlpineSurface> {
        (relief_m >= self.snow_start_altitude_m).then(|| {
            let progress = ((relief_m - self.snow_start_altitude_m)
                / (self.persistent_snow_altitude_m - self.snow_start_altitude_m))
                .clamped(0.0, 1.0);
            let slope = slope.clamped(0.0, 1.0);
            AlpineSurface {
                rock: ((1.0 - progress) * self.transition_rock_blend
                    + slope * self.slope_rock_blend)
                    .clamped(0.0, 1.0),
                snow: (progress * (1.0 - slope * self.slope_rock_blend)).clamped(0.0, 1.0),
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AlpineSurface {
    pub(crate) rock: f32,
    pub(crate) snow: f32,
}

impl FileAsset for AuthoredAlpinePolicy {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> {
        let policy: Self = load_ron(&bytes)?;
        policy.validate().map_err(Into::into).map(|_| policy)
    }
}

fn authored_region_for_map_asset(specifier: &str) -> Option<&'static AuthoredRegion> {
    AUTHORED_REGIONS
        .iter()
        .find(|region| region.map_asset == specifier)
}

/// One climate class the authored `climate_zone` raster can carry, in the
/// order the mask's gray ramp paints them (`0` black -> `5` white; the
/// exporter ships them as `class / 5`).
///
/// All six exist even though the current Cromatolis mask only paints three:
/// the two cold classes are unreachable at this region's latitude, but keeping
/// the scale complete means a region that does reach them needs no new class
/// ids, and a mask that accidentally carries one decodes to something named
/// rather than to whichever neighbour happened to be nearest.
///
/// # Where the raster comes from
///
/// **The producer lives in another repository.** The class ids below are one
/// end of a contract whose other end is the sibling `xindeler-open-world`
/// repo: a hand-painted TIFF under `~/MyXindeler/OpenWorld/Cromatolis/l16-v10/`
/// goes through its `tools/import_l16_v10_manual_masks.py` (majority vote,
/// 16×16 → 2048×1536) and `tools/export_v0_terrain_bundle.py` (nearest
/// neighbour, → 1024×1024), and `open_world_cli export-new-horizon` stages the
/// `.f32le` this crate then loads. Nothing in *this* repository produces it,
/// so a change here that assumes a different gray ramp, a different class
/// count or an interpolable value will not fail to build -- it will just
/// disagree with the exporter.
///
/// Both of those resample steps are chosen specifically because this layer is
/// categorical, and neither matches what the other authored masks use.
/// Averaging two class ids yields the id of a third, real class; LANCZOS
/// yields values that are no class at all. See
/// `assets/world/map/cromatolis_v0_climate.ron`'s header for the full chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClimateZone {
    Polar,
    Subpolar,
    Temperate,
    Subtropical,
    Tropical,
    Equatorial,
}

impl ClimateZone {
    const ALL: [Self; 6] = [
        Self::Polar,
        Self::Subpolar,
        Self::Temperate,
        Self::Subtropical,
        Self::Tropical,
        Self::Equatorial,
    ];

    /// Decodes a sampled `climate_zone` layer value (`class / 5`, see
    /// `AuthoredLayerKind::ClimateZone`) back into its class.
    ///
    /// Rounds rather than truncates: the raster is categorical but travels
    /// through an f32 layer, so `0.6` may arrive as `0.5999999`. Rounding also
    /// means a value that somehow *was* interpolated resolves to the nearer
    /// painted class instead of always falling to the colder one -- though
    /// that case is prevented upstream rather than repaired here (the mask
    /// importer votes by majority and the layer exporter resamples nearest-
    /// neighbour, both specifically so that no unpainted value can reach this
    /// function).
    fn from_layer_value(value: f32) -> Self {
        let index = (value * 5.0).round().clamp(0.0, 5.0) as usize;
        Self::ALL[index]
    }
}

/// Sea-level temperature, in real degrees Celsius, for each [`ClimateZone`].
///
/// Absolute temperatures rather than deltas from some baseline: an absolute
/// value is what a map author is actually choosing ("the northern sea is
/// 28 C"), whereas a delta makes the result depend on a number they are not
/// looking at.
#[derive(Debug, Clone, Copy, Deserialize)]
struct AuthoredClimateZoneAnchors {
    polar_c: f32,
    subpolar_c: f32,
    temperate_c: f32,
    subtropical_c: f32,
    tropical_c: f32,
    equatorial_c: f32,
}

impl AuthoredClimateZoneAnchors {
    fn sea_level_temp_c(&self, zone: ClimateZone) -> f32 {
        match zone {
            ClimateZone::Polar => self.polar_c,
            ClimateZone::Subpolar => self.subpolar_c,
            ClimateZone::Temperate => self.temperate_c,
            ClimateZone::Subtropical => self.subtropical_c,
            ClimateZone::Tropical => self.tropical_c,
            ClimateZone::Equatorial => self.equatorial_c,
        }
    }

    fn named(&self) -> [(&'static str, f32); 6] {
        [
            ("polar_c", self.polar_c),
            ("subpolar_c", self.subpolar_c),
            ("temperate_c", self.temperate_c),
            ("subtropical_c", self.subtropical_c),
            ("tropical_c", self.tropical_c),
            ("equatorial_c", self.equatorial_c),
        ]
    }
}

/// A named local override of the zone raster's answer: "this area is `X` C at
/// sea level regardless of which band it sits in", with a linear fade outward.
///
/// Areal with a gradient, rather than site-anchored like the nearest existing
/// precedent for locally-exceptional ground
/// (`cromatolis_v0_ground_substrate_zones.ron`, a half-plane relative to a
/// named fortification). That precedent's shape cannot express "cold for N
/// chunks around a cursed vault, fading outward"; this one can, in either
/// direction, so a future warm *or* cold anomaly is a data row rather than an
/// engine change.
///
/// Deliberately not merged into the substrate zones: that asset answers "what
/// is the ground made of", this one answers "how warm is the air", and an
/// anomaly may want either without the other.
#[derive(Debug, Clone, Deserialize)]
struct AuthoredMicroclimateZone {
    id: String,
    /// What sea-level temperature this zone imposes, in real degrees Celsius.
    forced_sea_level_temp_c: f32,
    /// How far outside the polygon the override fades to nothing, **in
    /// chunks**. Chunks, not authoring pixels: that mapping is anisotropic
    /// (2 px/chunk in x, 1.5 in y), so a radius expressed in authoring space
    /// would come out an ellipse on the ground.
    falloff_chunks: f32,
    /// Same convention (and the same authoring tool) as
    /// `cromatolis_v0_tree_candidate_policy.ron`: normalized `[0, 1]`, origin
    /// top-left, `y` increasing southward.
    polygon_normalized_top_left: Vec<NormalizedTopLeftPoint>,
}

impl AuthoredMicroclimateZone {
    /// Projects the authored polygon into chunk space once, at load time.
    /// Returns `None` for a polygon too degenerate to contain anything --
    /// `validate` already rejects those, so this only covers a hand-built
    /// value that never went through the asset loader.
    fn resolve(&self, map_chunks: Vec2<u16>) -> Option<ResolvedMicroclimateZone> {
        (self.polygon_normalized_top_left.len() >= 3).then(|| ResolvedMicroclimateZone {
            forced_sea_level_temp_c: self.forced_sea_level_temp_c,
            falloff_chunks: self.falloff_chunks,
            vertices: self
                .polygon_normalized_top_left
                .iter()
                .map(|point| {
                    Vec2::new(
                        point.x * map_chunks.x as f32,
                        // Normalized space is top-left origin, chunk space is
                        // bottom-left. Flip once, here, rather than at every
                        // geometry call site.
                        (1.0 - point.y) * map_chunks.y as f32,
                    )
                })
                .collect(),
        })
    }
}

/// One [`AuthoredMicroclimateZone`] with its polygon already in chunk space.
///
/// Split out for cost, not tidiness: `weight_at` runs once per zone per chunk
/// inside `SimChunk::generate`'s parallel loop -- ~1M times per world at the
/// current map size -- and the projection it used to do there is invariant
/// across every one of those calls. Same reason (and the same shape) as
/// [`ResolvedGroundSubstrateZones`], the sibling zone type that solved this
/// first. Today's single zone would not notice; the mechanism exists to hold
/// more.
#[derive(Debug)]
struct ResolvedMicroclimateZone {
    forced_sea_level_temp_c: f32,
    falloff_chunks: f32,
    /// Polygon in chunk space, `y` already flipped out of the authoring
    /// convention.
    vertices: Vec<Vec2<f32>>,
}

impl ResolvedMicroclimateZone {
    /// Blend weight at `chunk_pos`: `1.0` inside the polygon, falling
    /// linearly to `0.0` at `falloff_chunks` outside it.
    fn weight_at(&self, chunk_pos: Vec2<i32>) -> f32 {
        let point = chunk_pos.map(|e| e as f32) + 0.5;
        if point_in_polygon(point, self.vertices.iter().copied()) {
            return 1.0;
        }
        if self.falloff_chunks <= 0.0 {
            return 0.0;
        }
        let distance = distance_to_polygon_edge(point, &self.vertices);
        (1.0 - distance / self.falloff_chunks).clamp(0.0, 1.0)
    }
}

/// Shortest distance from `point` to the polygon's boundary (not its
/// interior): callers test containment separately, so this only has to be
/// correct outside.
fn distance_to_polygon_edge(point: Vec2<f32>, vertices: &[Vec2<f32>]) -> f32 {
    let mut best = f32::INFINITY;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let (a, b) = (vertices[i], vertices[j]);
        let edge = b - a;
        let length_squared = edge.magnitude_squared();
        let projected = if length_squared <= f32::EPSILON {
            a
        } else {
            a + edge * ((point - a).dot(edge) / length_squared).clamp(0.0, 1.0)
        };
        best = best.min(point.distance(projected));
        j = i;
    }
    best
}

/// Authored parameters for a region's baseline temperature curve (see
/// `cromatolis_baseline_temp`). Cromatolis-specific tuned content, not a
/// general engine constant, so it lives in a RON asset
/// (`{region.map_asset}_climate`, e.g. `assets/world/map/
/// cromatolis_v0_climate.ron`) rather than a Rust literal -- the same
/// convention this crate already uses for every other authored Cromatolis
/// parameter (settlements, landmarks, bridges, fortifications, ...).
#[derive(Debug, Clone, Deserialize)]
struct AuthoredCromatolisClimate {
    /// Sea-level temperature per climate class, selected by the authored
    /// `climate_zone` raster.
    zone_anchors: AuthoredClimateZoneAnchors,
    /// Used when the `climate_zone` layer is absent (an LFS-free CI checkout,
    /// a partial clone, a failed export). Degrading to one flat baseline keeps
    /// the map generable; decoding a missing layer as `0.0` instead would
    /// silently paint the entire region Polar.
    fallback_sea_level_temp_c: f32,
    /// Local overrides of the zone answer. See [`AuthoredMicroclimateZone`].
    #[serde(default)]
    microclimate_zones: Vec<AuthoredMicroclimateZone>,
    /// How fast the curve cools with altitude, in degrees Celsius per meter
    /// of relief above sea level.
    lapse_rate_c_per_m: f32,
    /// Below this abstract temperature, trees are physically excluded even
    /// when the authored mask is white. Kept in the region asset because it
    /// is a climate/content policy, not a generic engine invariant.
    #[serde(default = "default_cromatolis_tree_min_temp")]
    tree_min_temp: f32,
    /// Legacy climate fallback cap. The exact Cromatolis alpine policy is
    /// loaded separately, so a missing policy preserves the previous path.
    #[serde(default = "default_cromatolis_max_tree_altitude_m")]
    max_tree_altitude_m: f32,
}

/// `CONFIG.snow_temp` (8 °C) on the abstract scale. This is the *cold*
/// cut-off for trees, not a mid-range one: excluding trees by altitude is
/// `max_tree_altitude_m`'s job. See `cromatolis_v0_climate.ron` for why the
/// previous `0.0` (= 20 °C) was a latent map-wide deforestation bug that only
/// the old flat 36 °C sea-level baseline was hiding.
const fn default_cromatolis_tree_min_temp() -> f32 { -0.8 }

const fn default_cromatolis_max_tree_altitude_m() -> f32 { 970.0 }

impl AuthoredCromatolisClimate {
    fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("fallback_sea_level_temp_c", self.fallback_sea_level_temp_c),
            ("lapse_rate_c_per_m", self.lapse_rate_c_per_m),
            ("max_tree_altitude_m", self.max_tree_altitude_m),
        ]
        .into_iter()
        .chain(self.zone_anchors.named())
        {
            if !value.is_finite() {
                return Err(format!("Cromatolis climate {name} must be finite"));
            }
        }
        if self.max_tree_altitude_m < 0.0 {
            return Err("Cromatolis max tree altitude must be non-negative".into());
        }
        // Not a style rule: the zone ids are an ordered scale, and a mask that
        // paints the Tropical band warmer than the Equatorial one would still
        // generate, just wrongly and silently.
        let ordered = [
            self.zone_anchors.polar_c,
            self.zone_anchors.subpolar_c,
            self.zone_anchors.temperate_c,
            self.zone_anchors.subtropical_c,
            self.zone_anchors.tropical_c,
            self.zone_anchors.equatorial_c,
        ];
        if ordered.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(
                "Cromatolis climate zone anchors must increase strictly from polar to equatorial"
                    .into(),
            );
        }
        for zone in &self.microclimate_zones {
            if !zone.forced_sea_level_temp_c.is_finite() || !zone.falloff_chunks.is_finite() {
                return Err(format!(
                    "microclimate zone {} has a non-finite value",
                    zone.id
                ));
            }
            if zone.falloff_chunks < 0.0 {
                return Err(format!(
                    "microclimate zone {} has a negative falloff",
                    zone.id
                ));
            }
            if zone.polygon_normalized_top_left.len() < 3 {
                return Err(format!(
                    "microclimate zone {} needs at least 3 polygon points",
                    zone.id
                ));
            }
        }
        Ok(())
    }

    /// Projects every microclimate polygon into chunk space once, producing
    /// the form `SimChunk::generate` actually queries.
    fn resolve(&self, map_chunks: Vec2<u16>) -> ResolvedCromatolisClimate {
        ResolvedCromatolisClimate {
            zone_anchors: self.zone_anchors,
            fallback_sea_level_temp_c: self.fallback_sea_level_temp_c,
            microclimate_zones: self
                .microclimate_zones
                .iter()
                .filter_map(|zone| zone.resolve(map_chunks))
                .collect(),
            lapse_rate_c_per_m: self.lapse_rate_c_per_m,
            tree_min_temp: self.tree_min_temp,
            max_tree_altitude_m: self.max_tree_altitude_m,
        }
    }
}

/// [`AuthoredCromatolisClimate`] in the form worldgen reads it: identical
/// scalars, with each microclimate polygon already projected into chunk space
/// (see [`ResolvedMicroclimateZone`]). Built once per world, then borrowed by
/// every chunk.
#[derive(Debug)]
struct ResolvedCromatolisClimate {
    zone_anchors: AuthoredClimateZoneAnchors,
    fallback_sea_level_temp_c: f32,
    microclimate_zones: Vec<ResolvedMicroclimateZone>,
    lapse_rate_c_per_m: f32,
    tree_min_temp: f32,
    max_tree_altitude_m: f32,
}

impl ResolvedCromatolisClimate {
    /// Sea-level temperature for one chunk, in real degrees Celsius: the
    /// anchor for whatever class the authored raster painted there, then any
    /// microclimate override blended over it.
    ///
    /// `zone_value` is the sampled `climate_zone` layer value, or `None` when
    /// that layer failed to load.
    ///
    /// Overlapping microclimates resolve by **greatest weight**, not by
    /// greatest temperature: a cold anomaly must be able to sit next to a warm
    /// one and win where it is closer to its own polygon. Ties keep the
    /// earlier declaration, so the asset's order is the tiebreak and the
    /// result does not depend on iteration order.
    fn resolve_sea_level_temp_c(&self, chunk_pos: Vec2<i32>, zone_value: Option<f32>) -> f32 {
        let base = zone_value.map_or(self.fallback_sea_level_temp_c, |value| {
            self.zone_anchors
                .sea_level_temp_c(ClimateZone::from_layer_value(value))
        });
        if self.microclimate_zones.is_empty() {
            return base;
        }
        let mut best: Option<(f32, f32)> = None;
        for zone in &self.microclimate_zones {
            let weight = zone.weight_at(chunk_pos);
            if weight > 0.0 && best.is_none_or(|(best_weight, _)| weight > best_weight) {
                best = Some((weight, zone.forced_sea_level_temp_c));
            }
        }
        match best {
            Some((weight, forced)) => Lerp::lerp(base, forced, weight),
            None => base,
        }
    }
}

impl Default for ResolvedCromatolisClimate {
    fn default() -> Self { AuthoredCromatolisClimate::default().resolve(Vec2::broadcast(1)) }
}

impl FileAsset for AuthoredCromatolisClimate {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> {
        let climate: Self = load_ron(&bytes)?;
        climate.validate().map_err(Into::into).map(|_| climate)
    }
}

impl Default for AuthoredCromatolisClimate {
    /// Used only if the authored climate asset is missing or fails to
    /// parse (same graceful-degradation posture as this module's other
    /// authored-layer loaders: warn, then fall back instead of panicking).
    /// Mirrors the values `assets/world/map/cromatolis_v0_climate.ron`
    /// ships today, so a missing/corrupt asset reproduces today's curve
    /// rather than an arbitrary one.
    fn default() -> Self {
        Self {
            zone_anchors: AuthoredClimateZoneAnchors {
                polar_c: 2.0,
                subpolar_c: 8.0,
                temperate_c: 17.0,
                subtropical_c: 24.0,
                tropical_c: 28.0,
                equatorial_c: 30.0,
            },
            fallback_sea_level_temp_c: 17.0,
            microclimate_zones: Vec::new(),
            lapse_rate_c_per_m: 0.0075,
            tree_min_temp: default_cromatolis_tree_min_temp(),
            max_tree_altitude_m: default_cromatolis_max_tree_altitude_m(),
        }
    }
}

/// Named bands over the authored linear `ground_cover` signal.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) enum GroundCoverBand {
    BareDry,
    Grassland,
    SparseWoodland,
    Forest,
    Jungle,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct GroundCoverBandDefinition {
    pub band: GroundCoverBand,
    pub max_cover: f32,
    /// Content-authored surface tint for this density band. The column
    /// generator blends it over its normal thermal/noise-derived color.
    pub surface_tint: (f32, f32, f32),
    /// Strength of [`Self::surface_tint`] in `0.0..=1.0`.
    pub surface_blend: f32,
    /// Content-authored tint for this band in the start-area map preview.
    pub map_tint: (f32, f32, f32),
    /// Strength of [`Self::map_tint`] in `0.0..=1.0`.
    pub map_blend: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct AuthoredGroundCoverProfile {
    pub bands: Vec<GroundCoverBandDefinition>,
}

impl AuthoredGroundCoverProfile {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.bands.len() != 5 {
            return Err(format!(
                "expected five ground-cover bands, got {}",
                self.bands.len()
            ));
        }
        let expected = [
            GroundCoverBand::BareDry,
            GroundCoverBand::Grassland,
            GroundCoverBand::SparseWoodland,
            GroundCoverBand::Forest,
            GroundCoverBand::Jungle,
        ];
        let mut previous = 0.0;
        for (index, definition) in self.bands.iter().enumerate() {
            if definition.band != expected[index] {
                return Err(format!("ground-cover band {index} is out of order"));
            }
            if !(0.0..=1.0).contains(&definition.max_cover) {
                return Err(format!(
                    "ground-cover threshold {index} is outside 0..=1: {}",
                    definition.max_cover
                ));
            }
            if index > 0 && definition.max_cover <= previous {
                return Err(format!(
                    "ground-cover thresholds must be strictly increasing: {} then {}",
                    previous, definition.max_cover
                ));
            }
            if !definition.surface_tint.0.is_finite()
                || !definition.surface_tint.1.is_finite()
                || !definition.surface_tint.2.is_finite()
            {
                return Err(format!(
                    "ground-cover surface tint {index} must have finite RGB"
                ));
            }
            if !definition.surface_blend.is_finite()
                || !(0.0..=1.0).contains(&definition.surface_blend)
            {
                return Err(format!(
                    "ground-cover surface blend {index} is outside 0..=1: {}",
                    definition.surface_blend
                ));
            }
            if !definition.map_tint.0.is_finite()
                || !definition.map_tint.1.is_finite()
                || !definition.map_tint.2.is_finite()
            {
                return Err(format!(
                    "ground-cover map tint {index} must have finite RGB"
                ));
            }
            if !definition.map_blend.is_finite() || !(0.0..=1.0).contains(&definition.map_blend) {
                return Err(format!(
                    "ground-cover map blend {index} is outside 0..=1: {}",
                    definition.map_blend
                ));
            }
            previous = definition.max_cover;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn classify(&self, cover: f32) -> GroundCoverBand {
        self.bands
            .iter()
            .find(|definition| cover <= definition.max_cover)
            .map(|definition| definition.band)
            .unwrap_or_else(|| {
                self.bands
                    .last()
                    .expect("validated profile is non-empty")
                    .band
            })
    }
}

impl FileAsset for AuthoredGroundCoverProfile {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> {
        let profile: Self = load_ron(&bytes)?;
        profile.validate().map_err(Into::into).map(|_| profile)
    }
}

/// A categorical, authored cartographic ecology class.
///
/// The integer values are deliberately sparse 8-bit codes, not an ordinal
/// gradient: the producer must preserve them through mode/nearest resampling.
/// The continuous vegetation mask remains the only source of tree density.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
pub(crate) enum AuthoredEcologyZone {
    Unspecified,
    OpenLand,
    Shrubland,
    TemperateForest,
    Wetland,
    Jungle,
    AlpineBarren,
}

impl AuthoredEcologyZone {
    const ALL: [Self; 7] = [
        Self::Unspecified,
        Self::OpenLand,
        Self::Shrubland,
        Self::TemperateForest,
        Self::Wetland,
        Self::Jungle,
        Self::AlpineBarren,
    ];
    const CODES: [u8; 7] = [0, 32, 64, 96, 128, 160, 192];

    fn from_layer_value(value: f32) -> Option<Self> {
        // f32 export/import can move a canonical `code / 255` by a few ULPs,
        // but a half-step is a corrupt/interpolated class and must not round
        // silently into a neighbouring authored zone.
        const EPSILON: f32 = 1.0e-6;
        Self::CODES
            .iter()
            .position(|code| (value - f32::from(*code) / 255.0).abs() <= EPSILON)
            .map(|index| Self::ALL[index])
    }

    fn validate_layer(layer: &[f32]) -> Result<(), String> {
        layer
            .iter()
            .enumerate()
            .find_map(|(index, value)| {
                Self::from_layer_value(*value)
                    .is_none()
                    .then_some((index, value))
            })
            .map_or(Ok(()), |(index, value)| {
                Err(format!(
                    "ecology-zone layer contains invalid categorical value {value} at index \
                     {index}"
                ))
            })
    }
}

/// Data-owned visual language for an authored region's map and minimap. The
/// categorical ecology raster selects a zone; this profile supplies only its
/// presentation. It never affects terrain blocks or tree placement.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct MapEcologyZoneDefinition {
    pub zone: AuthoredEcologyZone,
    pub map_tint: (f32, f32, f32),
    pub base_blend: f32,
    pub tree_density_blend: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct AuthoredMapEcologyProfile {
    /// Alpha of the local voxel overlay above this region's authored map.
    /// `255` is opaque; lower values deliberately preserve the cartographic
    /// hillshade and zone information below nearby loaded chunks.
    pub voxel_minimap_overlay_alpha: u8,
    pub zones: Vec<MapEcologyZoneDefinition>,
}

impl AuthoredMapEcologyProfile {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.zones.is_empty() {
            return Err("map ecology profile must define at least one zone".into());
        }
        let mut seen = std::collections::HashSet::new();
        for zone in &self.zones {
            if !seen.insert(zone.zone) {
                return Err(format!("map ecology has duplicate zone {:?}", zone.zone));
            }
            let tint = zone.map_tint;
            if !tint.0.is_finite()
                || !tint.1.is_finite()
                || !tint.2.is_finite()
                || !(0.0..=1.0).contains(&tint.0)
                || !(0.0..=1.0).contains(&tint.1)
                || !(0.0..=1.0).contains(&tint.2)
            {
                return Err(format!("map ecology tint for {:?} is invalid", zone.zone));
            }
            for (name, value) in [
                ("base_blend", zone.base_blend),
                ("tree_density_blend", zone.tree_density_blend),
            ] {
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(format!(
                        "map ecology {name} for {:?} is outside 0..=1: {value}",
                        zone.zone
                    ));
                }
            }
            if zone.base_blend + zone.tree_density_blend > 1.0 {
                return Err(format!(
                    "map ecology blends for {:?} exceed 1.0: {} + {}",
                    zone.zone, zone.base_blend, zone.tree_density_blend
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn zone_for(
        &self,
        ecology_zone: AuthoredEcologyZone,
    ) -> Option<&MapEcologyZoneDefinition> {
        self.zones.iter().find(|zone| zone.zone == ecology_zone)
    }
}

impl FileAsset for AuthoredMapEcologyProfile {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> {
        let profile: Self = load_ron(&bytes)?;
        profile.validate().map_err(Into::into).map(|_| profile)
    }
}

/// A categorical visible-ground material. It is intentionally not inferred
/// from either vegetation or continuous ground cover: an authored bare area
/// can be rock, earth, water, or a specially-declared substrate.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) enum GroundSubstrate {
    Sand,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum GroundSubstrateExterior {
    North,
}

#[derive(Debug, Deserialize)]
struct AuthoredGroundSubstrateZones {
    schema: String,
    zones: Vec<AuthoredGroundSubstrateZone>,
}

#[derive(Debug, Deserialize)]
struct AuthoredGroundSubstrateZone {
    id: String,
    substrate: GroundSubstrate,
    fortification_id: String,
    exterior: GroundSubstrateExterior,
}

#[derive(Debug, Deserialize)]
struct AuthoredFortificationAnchors {
    schema: String,
    coordinate_space: String,
    source_map: AuthoredFortificationSourceMap,
    fortifications: Vec<AuthoredFortificationAnchor>,
}

#[derive(Debug, Deserialize)]
struct AuthoredFortificationSourceMap {
    width_px: u32,
    height_px: u32,
}

#[derive(Debug, Deserialize)]
struct AuthoredFortificationAnchor {
    id: String,
    start: AuthoredSourcePixel,
    end: AuthoredSourcePixel,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct AuthoredSourcePixel {
    x: f32,
    y: f32,
}

impl FileAsset for AuthoredGroundSubstrateZones {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl FileAsset for AuthoredFortificationAnchors {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Clone, Copy, Debug)]
struct ResolvedGroundSubstrateZone {
    substrate: GroundSubstrate,
    min_x: f32,
    max_x: f32,
    north_boundary_y: f32,
}

#[derive(Debug)]
struct ResolvedGroundSubstrateZones {
    zones: Vec<ResolvedGroundSubstrateZone>,
}

impl AuthoredGroundSubstrateZones {
    fn resolve(
        &self,
        fortifications: &AuthoredFortificationAnchors,
    ) -> Result<ResolvedGroundSubstrateZones, String> {
        const ZONES_SCHEMA: &str = "xindeler_open_world.ground_substrate_zones.v1";
        const FORTIFICATIONS_SCHEMA: &str = "xindeler_open_world.authored_fortifications.v1";
        const SOURCE_PIXELS: &str = "source_pixels_xy_top_left_origin";

        if self.schema != ZONES_SCHEMA {
            return Err(format!(
                "expected schema {ZONES_SCHEMA}, got {}",
                self.schema
            ));
        }
        if fortifications.schema != FORTIFICATIONS_SCHEMA {
            return Err(format!(
                "expected fortifications schema {FORTIFICATIONS_SCHEMA}, got {}",
                fortifications.schema
            ));
        }
        if fortifications.coordinate_space != SOURCE_PIXELS {
            return Err(format!(
                "expected fortification coordinate space {SOURCE_PIXELS}, got {}",
                fortifications.coordinate_space
            ));
        }
        if fortifications.source_map.width_px < 2 || fortifications.source_map.height_px < 2 {
            return Err("fortification source_map must be at least 2x2".to_string());
        }

        let mut ids = DHashSet::default();
        let mut resolved = Vec::with_capacity(self.zones.len());
        for zone in &self.zones {
            if zone.id.is_empty() || !ids.insert(zone.id.as_str()) {
                return Err(format!(
                    "ground-substrate zone has duplicate or empty id {}",
                    zone.id
                ));
            }
            let anchor = fortifications
                .fortifications
                .iter()
                .find(|fortification| fortification.id == zone.fortification_id)
                .ok_or_else(|| {
                    format!(
                        "ground-substrate zone {} references missing fortification {}",
                        zone.id, zone.fortification_id
                    )
                })?;
            if anchor.start.y != anchor.end.y {
                return Err(format!(
                    "ground-substrate zone {} needs a horizontal fortification, but {} is not \
                     horizontal",
                    zone.id, zone.fortification_id
                ));
            }
            let width = (fortifications.source_map.width_px - 1) as f32;
            let height = (fortifications.source_map.height_px - 1) as f32;
            let min_x = anchor.start.x.min(anchor.end.x) / width;
            let max_x = anchor.start.x.max(anchor.end.x) / width;
            let north_boundary_y = anchor.start.y / height;
            if !(0.0..=1.0).contains(&min_x)
                || !(0.0..=1.0).contains(&max_x)
                || !(0.0..=1.0).contains(&north_boundary_y)
            {
                return Err(format!(
                    "ground-substrate zone {} has an out-of-bounds fortification anchor {}",
                    zone.id, zone.fortification_id
                ));
            }
            match zone.exterior {
                GroundSubstrateExterior::North => resolved.push(ResolvedGroundSubstrateZone {
                    substrate: zone.substrate,
                    min_x,
                    max_x,
                    north_boundary_y,
                }),
            }
        }
        Ok(ResolvedGroundSubstrateZones { zones: resolved })
    }
}

impl ResolvedGroundSubstrateZones {
    fn substrate_at(
        &self,
        map_size_lg: MapSizeLg,
        chunk_pos: Vec2<i32>,
    ) -> Option<GroundSubstrate> {
        let chunks = map_size_lg.chunks().map(f32::from);
        self.zones
            .iter()
            .find(|zone| {
                // Mirror `AuthoredMapPoint::to_chunk_pos`: a categorical
                // region anchored to authored fortification geometry must
                // quantize on the exact same chunk row/columns. The strict
                // comparison then keeps the wall's own row out of its
                // exterior substrate.
                let min_x = (zone.min_x * (chunks.x - 1.0)).round() as i32;
                let max_x = (zone.max_x * (chunks.x - 1.0)).round() as i32;
                let boundary_y = ((1.0 - zone.north_boundary_y) * (chunks.y - 1.0)).round() as i32;
                chunk_pos.x >= min_x && chunk_pos.x <= max_x && chunk_pos.y > boundary_y
            })
            .map(|zone| zone.substrate)
    }
}

/// Which purely-procedural voxel layers still run inside an authored
/// region's chunks.
///
/// These four passes in `World::generate_chunk` (`apply_caverns_to`,
/// `apply_caves_to`, `apply_rocks_to`, `apply_spots_to`) read no authored
/// data at all and stamp hash-derived geometry straight into the block
/// volume, so inside a hand-authored region they compete with authored
/// geometry — procedural caves against the carved interiors/cave features
/// of `apply_cromatolis_cave_features_to` being the clearest case, since
/// the two are voxel-indistinguishable.
///
/// Whether a given region wants them is **content policy, not a
/// data-priority invariant**: unlike `tree_density`/`path` (hand-painted
/// fields a procedural pass can silently overwrite), nothing authored is
/// destroyed by a boulder or a hut. And the blast radius is real —
/// switching `caves` off removes that region's entire *procedural*
/// underground (cave biomes, cave fauna, ore and cave loot), which
/// `apply_cromatolis_cave_features_to` does not backfill: it carves
/// physical geometry only. So this lives in a RON asset
/// (`{region.map_asset}_features`, e.g.
/// `assets/world/map/cromatolis_v0_features.ron`) and is flippable without
/// a recompile — the same convention this crate already uses for every
/// other authored parameter (climate, settlements, landmarks, bridges,
/// fortifications, ...), rather than as Rust literals at the call sites.
///
/// `true` means "this procedural layer still runs inside the region".
/// These compose with the global `assets/world/features.ron` toggles: a
/// layer runs only when both allow it.
///
/// IMPORTANT: resolved **map-globally**, matching the fact that
/// `authored_region_id` is itself map-global today (see `GenCdf`). The day
/// authored regions cover only part of a map (the `COW-2` debt), these
/// need to be resolved per source feature — a spot structure spans a 3x3
/// chunk neighbourhood and a tunnel spans many, so a per-rendered-chunk
/// answer would slice geometry in half exactly on the region border.
///
/// Per-field `#[serde(default)]` on purpose: this asset exists to be
/// hand-edited, and without it deleting one line (or adding a fifth toggle
/// later) fails the *whole* file to parse, which the loader answers by
/// warning and reverting all four toggles at once. Per-field, a missing
/// entry only relaxes that one layer back to upstream behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) struct AuthoredProceduralLayers {
    #[serde(default = "layer_enabled_by_default")]
    pub caverns: bool,
    #[serde(default = "layer_enabled_by_default")]
    pub caves: bool,
    #[serde(default = "layer_enabled_by_default")]
    pub rocks: bool,
    #[serde(default = "layer_enabled_by_default")]
    pub spots: bool,
    /// Whether a boulder that lands inside a carved void must keep that void
    /// traversable.
    ///
    /// They do land in them: a boulder's bounding box is symmetric in `z`
    /// and reaches roughly twenty blocks below the surface, a carved ceiling
    /// is clamped only [`crate::layer::VOID_SURFACE_MARGIN`] blocks below
    /// it, and the rock pass runs *after* every carve — so the rock wins.
    /// With this on, one that would seal the way through instead gets a
    /// locally carved channel, or is not placed at all.
    ///
    /// Unlike the four toggles above, this is **opt-in** rather than
    /// opt-out, and its absence means "off". The four above suppress a layer
    /// a region does not want, so their permissive value is the upstream
    /// behaviour; this one *adds* geometry (it opens air a carve did not),
    /// so its upstream-behaviour value is the disabled one. A region that
    /// never mentions it -- including every plain procedural world -- keeps
    /// generating exactly the terrain it generated before.
    #[serde(default)]
    pub rock_traversal_repair: bool,
}

/// See [`AuthoredProceduralLayers::default`] for why an absent toggle
/// means "this layer still runs" rather than "suppressed".
fn layer_enabled_by_default() -> bool { true }

impl FileAsset for AuthoredProceduralLayers {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl Default for AuthoredProceduralLayers {
    /// Used only if a region's procedural-layer asset is missing or fails
    /// to parse (same graceful-degradation posture as this module's other
    /// authored loaders: warn, then fall back instead of panicking).
    ///
    /// Deliberately the *permissive* fallback — every layer on, i.e.
    /// upstream behaviour — rather than a copy of whatever one region
    /// happens to ship. Two reasons. This type is region-generic, so a
    /// Cromatolis-shaped default would silently hand some future region a
    /// policy it never asked for. And it fails in the safe direction: one
    /// typo in a RON would otherwise delete a whole map's procedural
    /// caves, ore, rocks and spots behind nothing louder than a `warn!`,
    /// whereas falling back to "nothing is suppressed" can only ever add
    /// content back.
    ///
    /// Each region's real policy lives in its own asset. The shipped
    /// Cromatolis values are pinned by
    /// `cromatolis_procedural_layer_asset_pins_the_shipped_policy`, so a
    /// silent fall back to this default cannot go unnoticed either.
    fn default() -> Self {
        Self {
            caverns: true,
            caves: true,
            rocks: true,
            spots: true,
            // Not part of the permissive fallback: see the field's doc
            // comment. "Nothing is suppressed" is the safe direction for the
            // four above; for this one the safe direction is "nothing extra
            // is carved".
            rock_traversal_repair: false,
        }
    }
}

/// Data for the most recent map type.  Update this when you add a new map
/// version.
pub type ModernMap = WorldMap_0_7_0;

/// The default world map.
///
/// TODO: Consider using some naming convention to automatically change this
/// with changing versions, or at least keep it in a constant somewhere that's
/// easy to change.
// Generation parameters:
//
// gen_opts: (
//     erosion_quality: 1.0,
//     map_kind: Circle,
//     scale: 2.157574498096227,
//     x_lg: 10,
//     y_lg: 10,
// )
// seed: 3582734543
//
// The biome seed can found below
pub const DEFAULT_WORLD_MAP: &str = "world.map.veloren_0_18_0_0";
/// This is *not* the seed used to generate the default map, this seed was used
/// to generate a better set of biomes on it as the original ones were
/// unsuitable.
///
/// See DEFAULT_WORLD_MAP to get the original worldgen parameters.
pub const DEFAULT_WORLD_SEED: u32 = 130626853;

impl WorldFileLegacy {
    #[inline]
    /// Idea: each map type except the latest knows how to transform
    /// into the the subsequent map version, and each map type including the
    /// latest exposes an "into_modern()" method that converts this map type
    /// to the modern map type.  Thus, to migrate a map from an old format to a
    /// new format, we just need to transform the old format to the
    /// subsequent map version, and then call .into_modern() on that--this
    /// should construct a call chain that ultimately ends up with a modern
    /// version.
    pub fn into_modern(self) -> Result<ModernMap, WorldFileError> {
        // NOTE: At this point, we assume that any remaining legacy maps were 1024 ×
        // 1024.
        if self.alt.len() != self.basement.len() || self.alt.len() != 1024 * 1024 {
            return Err(WorldFileError::WorldSizeInvalid);
        }

        let map = WorldMap_0_5_0 {
            alt: self.alt,
            basement: self.basement,
        };

        map.into_modern()
    }
}

impl WorldMap_0_5_0 {
    #[inline]
    pub fn into_modern(self) -> Result<ModernMap, WorldFileError> {
        let pow_size = (self.alt.len().trailing_zeros()) / 2;
        let two_coord_size = 1 << (2 * pow_size);
        if self.alt.len() != self.basement.len() || self.alt.len() != two_coord_size {
            return Err(WorldFileError::WorldSizeInvalid);
        }

        // The recommended continent scale for maps from version 0.5.0 is (in all
        // existing cases) just 1.0 << (f64::from(pow_size) - 10.0).
        let continent_scale_hack = (f64::from(pow_size) - 10.0).exp2();

        let map = WorldMap_0_7_0 {
            map_size_lg: Vec2::new(pow_size, pow_size),
            continent_scale_hack,
            alt: self.alt,
            basement: self.basement,
        };

        map.into_modern()
    }
}

impl WorldMap_0_7_0 {
    #[inline]
    pub fn into_modern(self) -> Result<ModernMap, WorldFileError> {
        if self.alt.len() != self.basement.len()
            || self.alt.len() != (1 << (self.map_size_lg.x + self.map_size_lg.y))
            || self.continent_scale_hack <= 0.0
        {
            return Err(WorldFileError::WorldSizeInvalid);
        }

        Ok(self)
    }
}

impl WorldFile {
    /// Turns map data from the latest version into a versioned WorldFile ready
    /// for serialization. Whenever a new map is updated, just change the
    /// variant we construct here to make sure we're using the latest map
    /// version.
    pub fn new(map: ModernMap) -> Self { WorldFile::Veloren0_7_0(map) }

    #[inline]
    /// Turns a WorldFile into the latest version.  Whenever a new map version
    /// is added, just add it to this match statement.
    pub fn into_modern(self) -> Result<ModernMap, WorldFileError> {
        match self {
            WorldFile::Veloren0_5_0(map) => map.into_modern(),
            WorldFile::Veloren0_7_0(map) => map.into_modern(),
        }
    }
}

#[derive(Debug)]
pub enum WorldSimStage {
    // TODO: Add more stages
    Erosion {
        progress: f64,
        estimate: Option<std::time::Duration>,
    },
}

pub struct WorldSim {
    pub seed: u32,
    /// Base 2 logarithm of the map size.
    map_size_lg: MapSizeLg,
    /// Maximum height above sea level of any chunk in the map (not including
    /// post-erosion warping, cliffs, and other things like that).
    pub max_height: f32,
    pub(crate) chunks: Vec<SimChunk>,
    //TODO: remove or use this property
    pub(crate) _locations: Vec<Location>,

    pub(crate) gen_ctx: GenCtx,
    pub rng: ChaChaRng,

    pub(crate) calendar: Option<Calendar>,

    /// Which purely-procedural voxel layers still run inside this world's
    /// authored region. `None` for a normal procedural world, where every
    /// layer always runs. See [`AuthoredProceduralLayers`].
    pub(crate) authored_procedural_layers: Option<AuthoredProceduralLayers>,
    /// Ground-cover classification profile for the loaded authored region.
    /// `None` for procedural worlds or when the configured profile is invalid.
    #[allow(dead_code)]
    pub(crate) authored_ground_cover_profile: Option<AuthoredGroundCoverProfile>,
    /// Cartographic ecology profile for the loaded authored region. This is
    /// intentionally separate from ground cover and tree-density contracts.
    pub(crate) authored_map_ecology_profile: Option<AuthoredMapEcologyProfile>,
    /// Categorical cartographic ecology kept beside the map, not copied into
    /// every `SimChunk`: it is consumed only by map/minimap rendering and a
    /// per-chunk enum would add avoidable permanent world memory.
    authored_ecology_zone_layer: Option<Box<[f32]>>,
    /// Additional tree-root lattices supplied by a loaded authored region.
    /// Procedural worlds intentionally leave this empty and retain the exact
    /// upstream `StructureGen2d` candidate sequence.
    authored_tree_candidate_policy: Option<RegionalTreeCandidatePolicy>,
    pub(crate) authored_alpine_policy: Option<(&'static str, AuthoredAlpinePolicy)>,
}

/// The forest-species lottery for a position, given the [`Environment`]
/// (temperature/humidity/nearness-to-water) that governs it at that
/// position. Extracted out of [`WorldSim::make_forest_lottery`] (which
/// fetches `env` from the raw, un-overridden `SimChunk` at `wpos`) so a
/// caller holding an already climate-override-patched `Environment` — see
/// `world/src/layer/tree.rs`'s real tree-placement call site — can drive the
/// same lottery without that patch being silently bypassed by a second,
/// separate raw sim lookup.
pub fn make_forest_lottery_for_env(
    wpos: Vec2<i32>,
    env: Environment,
) -> Lottery<Option<ForestKind>> {
    Lottery::from(
        ForestKind::iter()
            .enumerate()
            .map(|(i, fk)| {
                const CLUSTER_SIZE: f64 = 48.0;
                let nz = (FastNoise2d::new(i as u32 * 37)
                    .get(wpos.map(|e| e as f64) / CLUSTER_SIZE)
                    + 1.0)
                    / 2.0;
                (fk.proclivity(&env) * nz, Some(fk))
            })
            .chain(std::iter::once((0.001, None)))
            .collect::<Vec<_>>(),
    )
}

impl WorldSim {
    pub fn empty() -> Self {
        let gen_ctx = GenCtx {
            turb_x_nz: SuperSimplex::new(0),
            turb_y_nz: SuperSimplex::new(0),
            chaos_nz: RidgedMulti::new(0),
            hill_nz: SuperSimplex::new(0),
            alt_nz: util::HybridMulti::new(0),
            temp_nz: Fbm::new(0),

            small_nz: BasicMulti::new(0),
            rock_nz: HybridMulti::new(0),
            tree_nz: BasicMulti::new(0),
            _cave_0_nz: SuperSimplex::new(0),
            _cave_1_nz: SuperSimplex::new(0),

            structure_gen: StructureGen2d::new(0, 24, 10),
            _big_structure_gen: StructureGen2d::new(0, 768, 512),
            _region_gen: StructureGen2d::new(0, 400, 96),
            humid_nz: Billow::new(0),

            _fast_turb_x_nz: FastNoise::new(0),
            _fast_turb_y_nz: FastNoise::new(0),

            _town_gen: StructureGen2d::new(0, 2048, 1024),
            river_seed: RandomField::new(0),
            rock_strength_nz: Fbm::new(0),
            uplift_nz: util::Worley::new(0),
        };
        Self {
            seed: 0,
            map_size_lg: MapSizeLg::new(Vec2::one()).unwrap(),
            max_height: 0.0,
            chunks: vec![SimChunk {
                authored_cromatolis_v0: false,
                authored_region_id: None,
                authored_near_water: false,
                water_body: None,
                salinity: None,
                authored_alpine_snowland: false,
                chaos: 0.0,
                alt: 0.0,
                basement: 0.0,
                water_alt: 0.0,
                downhill: None,
                flux: 0.0,
                temp: 0.0,
                humidity: 0.0,
                rockiness: 0.0,
                tree_density: 0.0,
                ground_cover: 0.0,
                ground_substrate: None,
                forest_kind: ForestKind::Dead,
                spawn_rate: 0.0,
                river: RiverData::default(),
                surface_veg: 0.0,
                sites: vec![],
                place: None,
                poi: None,
                path: Default::default(),
                cliff_height: 0.0,
                spot: None,
                contains_waypoint: false,
            }],
            _locations: Vec::new(),
            gen_ctx,
            rng: rand_chacha::ChaCha20Rng::from_seed([0; 32]),
            calendar: None,
            authored_procedural_layers: None,
            authored_ground_cover_profile: None,
            authored_map_ecology_profile: None,
            authored_ecology_zone_layer: None,
            authored_tree_candidate_policy: None,
            authored_alpine_policy: None,
        }
    }

    pub fn generate(
        seed: u32,
        opts: WorldOpts,
        threadpool: &rayon::ThreadPool,
        stage_report: &dyn Fn(WorldSimStage),
    ) -> Self {
        prof_span!("WorldSim::generate");
        let calendar = opts.calendar; // separate lifetime of elements
        let world_file = opts.world_file;

        // Parse out the contents of various map formats into the values we need.
        let authored_region = world_file.authored_region();
        // NOTE: `authored_cromatolis_v0` stays the name of this flag (rather than a
        // generic `has_authored_region`) because a good deal of Cromatolis-specific
        // tuning downstream (temp/biome special-casing, `world/src/civ`,
        // `world/src/layer`) already keys off this exact field name and isn't part
        // of this generalization -- V1 has exactly one registered region anyway.
        let authored_cromatolis_v0 = authored_region.is_some();
        let authored_region_id = authored_region.map(|region| region.id);
        let (parsed_world_file, map_size_lg, gen_opts) = world_file.load_content();
        let load_authored_layer = |region: &AuthoredRegion, kind: AuthoredLayerKind| {
            if !region.layers.contains(&kind) {
                return None;
            }
            let specifier = format!("{}_{}", region.map_asset, kind.asset_suffix());
            match AuthoredF32Layer::load_owned(&specifier) {
                Ok(layer) if layer.values.len() == map_size_lg.chunks_len() => Some(layer.values),
                Ok(layer) => {
                    warn!(
                        actual = layer.values.len(),
                        expected = map_size_lg.chunks_len(),
                        region = region.id,
                        layer = kind.asset_suffix(),
                        "Ignoring authored layer with invalid length"
                    );
                    None
                },
                Err(err) => {
                    warn!(
                        ?err,
                        region = region.id,
                        layer = kind.asset_suffix(),
                        "Could not load authored layer"
                    );
                    None
                },
            }
        };
        let (
            authored_route_layer,
            authored_vegetation_layer,
            authored_ground_cover_layer,
            authored_water_layer,
            authored_elevated_lakes_layer,
            authored_river_channels_layer,
            authored_climate_zone_layer,
            authored_ecology_zone_layer,
        ) = if let Some(region) = authored_region {
            (
                load_authored_layer(region, AuthoredLayerKind::Routes),
                load_authored_layer(region, AuthoredLayerKind::Vegetation),
                load_authored_layer(region, AuthoredLayerKind::GroundCover),
                load_authored_layer(region, AuthoredLayerKind::Water),
                load_authored_layer(region, AuthoredLayerKind::ElevatedLakes),
                load_authored_layer(region, AuthoredLayerKind::RiverChannels),
                load_authored_layer(region, AuthoredLayerKind::ClimateZone),
                load_authored_layer(region, AuthoredLayerKind::EcologyZone),
            )
        } else {
            (None, None, None, None, None, None, None, None)
        };
        let authored_ecology_zone_layer =
            authored_ecology_zone_layer.and_then(
                |layer| match AuthoredEcologyZone::validate_layer(&layer) {
                    Ok(()) => Some(layer),
                    Err(error) => {
                        warn!(%error, "Ignoring invalid authored ecology-zone layer");
                        None
                    },
                },
            );
        // Never substitute vegetation when the independent cover layer is
        // missing: that would silently restore the coupling this layer was
        // introduced to remove. LFS-free CI and partial local checkouts are
        // nevertheless supported by leaving the visual overlay inactive;
        // `load_authored_layer` has already emitted the diagnostic warning.
        let ground_cover_available = authored_ground_cover_layer.is_some();
        // Not a raster layer (`AuthoredLayerKind`), so loaded separately: a
        // handful of scalar tuning values plus the microclimate polygons, not
        // a per-chunk array. Resolved into chunk space here, once, rather than
        // per chunk -- see `ResolvedMicroclimateZone`.
        let cromatolis_climate = authored_region
            .filter(|region| region.id == CROMATOLIS_V0_REGION_ID)
            .and_then(|region| {
                let specifier = format!("{}_climate", region.map_asset);
                match AuthoredCromatolisClimate::load_owned(&specifier) {
                    Ok(climate) => Some(climate.resolve(map_size_lg.chunks())),
                    Err(err) => {
                        warn!(
                            ?err,
                            region = region.id,
                            "Could not load authored Cromatolis climate; falling back to the \
                             default baseline curve"
                        );
                        None
                    },
                }
            })
            .unwrap_or_else(|| AuthoredCromatolisClimate::default().resolve(map_size_lg.chunks()));
        // Never apply an authored terrain policy to the procedural fallback
        // produced when its binary map cannot load.
        let authored_alpine_policy = authored_region
            .filter(|_| parsed_world_file.is_some())
            .and_then(|region| region.alpine_policy.map(|specifier| (region.id, specifier)))
            .and_then(
                |(region_id, specifier)| match AuthoredAlpinePolicy::load_owned(specifier) {
                    Ok(policy) => Some((region_id, policy)),
                    Err(err) => {
                        warn!(
                            ?err,
                            specifier,
                            "Could not load authored alpine policy; preserving legacy terrain path"
                        );
                        None
                    },
                },
            );
        // Also not a raster layer: which purely-procedural voxel layers are
        // still allowed to run inside this region (see
        // `AuthoredProceduralLayers`). `None` for a procedural world.
        let authored_procedural_layers = authored_region.map(|region| {
            let specifier = format!("{}_procedural_layers", region.map_asset);
            match AuthoredProceduralLayers::load_owned(&specifier) {
                Ok(layers) => layers,
                Err(err) => {
                    warn!(
                        ?err,
                        region = region.id,
                        "Could not load authored procedural-layer toggles; falling back to the \
                         shipped defaults"
                    );
                    AuthoredProceduralLayers::default()
                },
            }
        });
        let authored_ground_cover_profile =
            authored_region
                .filter(|_| ground_cover_available)
                .map(|region| {
                    AuthoredGroundCoverProfile::load_owned(region.ground_cover_profile)
                        .unwrap_or_else(|err| {
                            panic!(
                                "authored region '{}' requires valid ground-cover profile '{}': \
                                 {err:?}",
                                region.id, region.ground_cover_profile
                            )
                        })
                });
        let authored_map_ecology_profile = authored_region.map(|region| {
            AuthoredMapEcologyProfile::load_owned(region.map_ecology_profile).unwrap_or_else(
                |err| {
                    panic!(
                        "authored region '{}' requires valid map ecology profile '{}': {err:?}",
                        region.id, region.map_ecology_profile
                    )
                },
            )
        });
        let authored_ground_substrate_zones = authored_region
            .filter(|_| ground_cover_available)
            .map(|region| {
                let zones = AuthoredGroundSubstrateZones::load_owned(region.ground_substrate_zones)
                    .unwrap_or_else(|err| {
                        panic!(
                            "authored region '{}' requires valid ground-substrate zones '{}': \
                             {err:?}",
                            region.id, region.ground_substrate_zones
                        )
                    });
                let fortifications = AuthoredFortificationAnchors::load_owned(
                    region.fortifications,
                )
                .unwrap_or_else(|err| {
                    panic!(
                        "authored region '{}' requires valid fortification anchors '{}': {err:?}",
                        region.id, region.fortifications
                    )
                });
                zones.resolve(&fortifications).unwrap_or_else(|err| {
                    panic!(
                        "authored region '{}' has invalid ground-substrate zones '{}': {err}",
                        region.id, region.ground_substrate_zones
                    )
                })
            });
        // The tree candidate policy is load-bearing authored content: silently
        // falling back to a sparse global lattice would make a missing asset
        // look like a valid but different forest design.
        let authored_tree_candidate_policy = authored_region.map(|region| {
            let policy = AuthoredTreeCandidatePolicy::load_owned(region.tree_candidate_policy)
                .unwrap_or_else(|err| {
                    panic!(
                        "authored region '{}' requires valid tree candidate policy '{}': {err:?}",
                        region.id, region.tree_candidate_policy
                    )
                });
            policy.compile(seed).unwrap_or_else(|err| {
                panic!(
                    "authored region '{}' has invalid tree candidate policy '{}': {err}",
                    region.id, region.tree_candidate_policy
                )
            })
        });
        // Currently only used with LoadOrGenerate to know if we need to
        // overwrite world file
        let fresh = parsed_world_file.is_none();

        let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(seed));
        let continent_scale = gen_opts.scale
            * 5_000.0f64
                .div(32.0)
                .mul(TerrainChunkSize::RECT_SIZE.x as f64);
        let rock_lacunarity = 2.0;
        let uplift_scale = 128.0;
        let uplift_turb_scale = uplift_scale / 4.0;

        info!("Starting world generation");

        // NOTE: Changing order will significantly change WorldGen, so try not to!
        let gen_ctx = GenCtx {
            turb_x_nz: SuperSimplex::new(rng.random()),
            turb_y_nz: SuperSimplex::new(rng.random()),
            chaos_nz: RidgedMulti::new(rng.random()).set_octaves(7).set_frequency(
                RidgedMulti::<Perlin>::DEFAULT_FREQUENCY * (5_000.0 / continent_scale),
            ),
            hill_nz: SuperSimplex::new(rng.random()),
            alt_nz: util::HybridMulti::new(rng.random())
                .set_octaves(8)
                .set_frequency(10_000.0 / continent_scale)
                // persistence = lacunarity^(-(1.0 - fractal increment))
                .set_lacunarity(util::HybridMulti::<Perlin>::DEFAULT_LACUNARITY)
                .set_persistence(util::HybridMulti::<Perlin>::DEFAULT_LACUNARITY.powi(-1))
                .set_offset(0.0),
            temp_nz: Fbm::new(rng.random())
                .set_octaves(6)
                .set_persistence(0.5)
                .set_frequency(1.0 / (((1 << 6) * 64) as f64))
                .set_lacunarity(2.0),

            small_nz: BasicMulti::new(rng.random()).set_octaves(2),
            rock_nz: HybridMulti::new(rng.random()).set_persistence(0.3),
            tree_nz: BasicMulti::new(rng.random())
                .set_octaves(12)
                .set_persistence(0.75),
            _cave_0_nz: SuperSimplex::new(rng.random()),
            _cave_1_nz: SuperSimplex::new(rng.random()),

            structure_gen: StructureGen2d::new(rng.random(), 24, 10),
            _big_structure_gen: StructureGen2d::new(rng.random(), 768, 512),
            _region_gen: StructureGen2d::new(rng.random(), 400, 96),
            humid_nz: Billow::new(rng.random())
                .set_octaves(9)
                .set_persistence(0.4)
                .set_frequency(0.2),

            _fast_turb_x_nz: FastNoise::new(rng.random()),
            _fast_turb_y_nz: FastNoise::new(rng.random()),

            _town_gen: StructureGen2d::new(rng.random(), 2048, 1024),
            river_seed: RandomField::new(rng.random()),
            rock_strength_nz: Fbm::new(rng.random())
                .set_octaves(10)
                .set_lacunarity(rock_lacunarity)
                // persistence = lacunarity^(-(1.0 - fractal increment))
                // NOTE: In paper, fractal increment is roughly 0.25.
                .set_persistence(rock_lacunarity.powf(-0.75))
                .set_frequency(
                    1.0 * (5_000.0 / continent_scale)
                        / (2.0 * TerrainChunkSize::RECT_SIZE.x as f64 * 2.0.powi(10 - 1)),
                ),
            uplift_nz: util::Worley::new(rng.random())
                .set_frequency(1.0 / (TerrainChunkSize::RECT_SIZE.x as f64 * uplift_scale))
                .set_distance_function(distance_functions::euclidean),
        };

        let river_seed = &gen_ctx.river_seed;
        let rock_strength_nz = &gen_ctx.rock_strength_nz;

        // Suppose the old world has grid spacing Δx' = Δy', new Δx = Δy.
        // We define grid_scale such that Δx = height_scale * Δx' ⇒
        //  grid_scale = Δx / Δx'.
        let grid_scale = 1.0f64 / (4.0 / gen_opts.scale)/*1.0*/;

        // Now, suppose we want to generate a world with "similar" topography, defined
        // in this case as having roughly equal slopes at steady state, with the
        // simulation taking roughly as many steps to get to the point the
        // previous world was at when it finished being simulated.
        //
        // Some computations with our coupled SPL/debris flow give us (for slope S
        // constant) the following suggested scaling parameters to make this
        // work:   k_fs_scale ≡ (K𝑓 / K𝑓') = grid_scale^(-2m) =
        // grid_scale^(-2θn)
        let k_fs_scale = |theta, n| grid_scale.powf(-2.0 * (theta * n) as f64);

        //   k_da_scale ≡ (K_da / K_da') = grid_scale^(-2q)
        let k_da_scale = |q| grid_scale.powf(-2.0 * q);
        //
        // Some other estimated parameters are harder to come by and *much* more
        // dubious, not being accurate for the coupled equation. But for the SPL
        // only one we roughly find, for h the height at steady state and time τ
        // = time to steady state, with Hack's Law estimated b = 2.0 and various other
        // simplifying assumptions, the estimate:
        //   height_scale ≡ (h / h') = grid_scale^(n)
        let height_scale = |n: f32| grid_scale.powf(n as f64) as Alt;
        //   time_scale ≡ (τ / τ') = grid_scale^(n)
        let time_scale = |n: f32| grid_scale.powf(n as f64);
        //
        // Based on this estimate, we have:
        //   delta_t_scale ≡ (Δt / Δt') = time_scale
        let delta_t_scale = time_scale;
        //   alpha_scale ≡ (α / α') = height_scale^(-1)
        let alpha_scale = |n: f32| height_scale(n).recip() as f32;
        //
        // Slightly more dubiously (need to work out the math better) we find:
        //   k_d_scale ≡ (K_d / K_d') = grid_scale^2 / (/*height_scale * */ time_scale)
        let k_d_scale = |n: f32| grid_scale.powi(2) / (/* height_scale(n) * */time_scale(n));
        //   epsilon_0_scale ≡ (ε₀ / ε₀') = height_scale(n) / time_scale(n)
        let epsilon_0_scale = |n| (height_scale(n) / time_scale(n) as Alt) as f32;

        // Approximate n for purposes of computation of parameters above over the whole
        // grid (when a chunk isn't available).
        let n_approx = 1.0;
        let max_erosion_per_delta_t = 64.0 * delta_t_scale(n_approx);
        let n_steps = (100.0 * gen_opts.erosion_quality) as usize;
        let n_small_steps = 0;
        let n_post_load_steps = 0;

        // Logistic regression.  Make sure x ∈ (0, 1).
        let logit = |x: f64| x.ln() - (-x).ln_1p();
        // 0.5 + 0.5 * tanh(ln(1 / (1 - 0.1) - 1) / (2 * (sqrt(3)/pi)))
        let logistic_2_base = 3.0f64.sqrt() * std::f64::consts::FRAC_2_PI;
        // Assumes μ = 0, σ = 1
        let logistic_cdf = |x: f64| (x / logistic_2_base).tanh() * 0.5 + 0.5;

        let map_size_chunks_len_f64 = map_size_lg.chunks().map(f64::from).product();
        let min_epsilon = 1.0 / map_size_chunks_len_f64.max(f64::EPSILON * 0.5);
        let max_epsilon = (1.0 - 1.0 / map_size_chunks_len_f64).min(1.0 - f64::EPSILON * 0.5);

        // No NaNs in these uniform vectors, since the original noise value always
        // returns Some.
        let ((alt_base, _), (chaos, _)) = threadpool.join(
            || {
                uniform_noise(map_size_lg, |_, wposf| {
                    match gen_opts.map_kind {
                        MapKind::Square => {
                            // "Base" of the chunk, to be multiplied by CONFIG.mountain_scale
                            // (multiplied value is from -0.35 *
                            // (CONFIG.mountain_scale * 1.05) to
                            // 0.35 * (CONFIG.mountain_scale * 0.95), but value here is from -0.3675
                            // to 0.3325).
                            Some(
                                (gen_ctx
                                    .alt_nz
                                    .get((wposf.div(10_000.0)).into_array())
                                    .clamp(-1.0, 1.0))
                                .sub(0.05)
                                .mul(0.35),
                            )
                        },
                        MapKind::Circle => {
                            let world_sizef = map_size_lg.chunks().map(|e| e as f64)
                                * TerrainChunkSize::RECT_SIZE.map(|e| e as f64);
                            Some(
                                (gen_ctx
                                    .alt_nz
                                    .get((wposf.div(5_000.0 * gen_opts.scale)).into_array())
                                    .clamp(-1.0, 1.0))
                                .add(
                                    0.2 - ((wposf / world_sizef) * 2.0 - 1.0)
                                        .magnitude_squared()
                                        .powf(0.75)
                                        .clamped(0.0, 1.0)
                                        .powf(1.0)
                                        * 0.6,
                                )
                                .mul(0.5),
                            )
                        },
                    }
                })
            },
            || {
                uniform_noise(map_size_lg, |_, wposf| {
                    // From 0 to 1.6, but the distribution before the max is from -1 and 1.6, so
                    // there is a 50% chance that hill will end up at 0.3 or
                    // lower, and probably a very high change it will be exactly
                    // 0.
                    let hill = (0.0f64
                        + gen_ctx
                            .hill_nz
                            .get(
                                (wposf
                                    .mul(32.0)
                                    .div(TerrainChunkSize::RECT_SIZE.map(|e| e as f64))
                                    .div(1_500.0))
                                .into_array(),
                            )
                            .clamp(-1.0, 1.0)
                            .mul(1.0)
                        + gen_ctx
                            .hill_nz
                            .get(
                                (wposf
                                    .mul(32.0)
                                    .div(TerrainChunkSize::RECT_SIZE.map(|e| e as f64))
                                    .div(400.0))
                                .into_array(),
                            )
                            .clamp(-1.0, 1.0)
                            .mul(0.3))
                    .add(0.3)
                    .max(0.0);

                    // chaos produces a value in [0.12, 1.32].  It is a meta-level factor intended
                    // to reflect how "chaotic" the region is--how much weird
                    // stuff is going on on this terrain.
                    Some(
                        ((gen_ctx
                            .chaos_nz
                            .get((wposf.div(3_000.0)).into_array())
                            .clamp(-1.0, 1.0))
                        .add(1.0)
                        .mul(0.5)
                        // [0, 1] * [0.4, 1] = [0, 1] (but probably towards the lower end)
                        .mul(
                            (gen_ctx
                                .chaos_nz
                                .get((wposf.div(6_000.0)).into_array())
                                .clamp(-1.0, 1.0))
                            .abs()
                                .clamp(0.4, 1.0),
                        )
                        // Chaos is always increased by a little when we're on a hill (but remember
                        // that hill is 0.3 or less about 50% of the time).
                        // [0, 1] + 0.2 * [0, 1.6] = [0, 1.32]
                        .add(0.2 * hill)
                        // We can't have *no* chaos!
                        .max(0.12)) as f32,
                    )
                })
            },
        );

        // We ignore sea level because we actually want to be relative to sea level here
        // and want things in CONFIG.mountain_scale units, but otherwise this is
        // a correct altitude calculation.  Note that this is using the
        // "unadjusted" temperature.
        //
        // No NaNs in these uniform vectors, since the original noise value always
        // returns Some.
        let (alt_old, _) = uniform_noise(map_size_lg, |posi, wposf| {
            // This is the extension upwards from the base added to some extra noise from -1
            // to 1.
            //
            // The extra noise is multiplied by alt_main (the mountain part of the
            // extension) powered to 0.8 and clamped to [0.15, 1], to get a
            // value between [-1, 1] again.
            //
            // The sides then receive the sequence (y * 0.3 + 1.0) * 0.4, so we have
            // [-1*1*(1*0.3+1)*0.4, 1*(1*0.3+1)*0.4] = [-0.52, 0.52].
            //
            // Adding this to alt_main thus yields a value between -0.4 (if alt_main = 0 and
            // gen_ctx = -1, 0+-1*(0*.3+1)*0.4) and 1.52 (if alt_main = 1 and gen_ctx = 1).
            // Most of the points are above 0.
            //
            // Next, we add again by a sin of alt_main (between [-1, 1])^pow, getting
            // us (after adjusting for sign) another value between [-1, 1], and then this is
            // multiplied by 0.045 to get [-0.045, 0.045], which is added to [-0.4, 0.52] to
            // get [-0.445, 0.565].
            let alt_main = {
                // Extension upwards from the base.  A positive number from 0 to 1 curved to be
                // maximal at 0.  Also to be multiplied by CONFIG.mountain_scale.
                let alt_main = (gen_ctx
                    .alt_nz
                    .get((wposf.div(2_000.0)).into_array())
                    .clamp(-1.0, 1.0))
                .abs()
                .powf(1.35);

                fn spring(x: f64, pow: f64) -> f64 { x.abs().powf(pow) * x.signum() }

                0.0 + alt_main
                    + (gen_ctx
                        .small_nz
                        .get(
                            (wposf
                                .mul(32.0)
                                .div(TerrainChunkSize::RECT_SIZE.map(|e| e as f64))
                                .div(300.0))
                            .into_array(),
                        )
                        .clamp(-1.0, 1.0))
                    .mul(alt_main.powf(0.8).max(/* 0.25 */ 0.15))
                    .mul(0.3)
                    .add(1.0)
                    .mul(0.4)
                    + spring(alt_main.abs().sqrt().min(0.75).mul(60.0).sin(), 4.0).mul(0.045)
            };

            // Now we can compute the final altitude using chaos.
            // We multiply by chaos clamped to [0.1, 1.32] to get a value between [0.03,
            // 2.232] for alt_pre, then multiply by CONFIG.mountain_scale and
            // add to the base and sea level to get an adjusted value, then
            // multiply the whole thing by map_edge_factor (TODO: compute final
            // bounds).
            //
            // [-.3675, .3325] + [-0.445, 0.565] * [0.12, 1.32]^1.2
            // ~ [-.3675, .3325] + [-0.445, 0.565] * [0.07, 1.40]
            // = [-.3675, .3325] + ([-0.5785, 0.7345])
            // = [-0.946, 1.067]
            Some(
                ((alt_base[posi].1 + alt_main.mul((chaos[posi].1 as f64).powf(1.2)))
                    .mul(map_edge_factor(map_size_lg, posi) as f64)
                    .add(
                        (CONFIG.sea_level as f64)
                            .div(CONFIG.mountain_scale as f64)
                            .mul(map_edge_factor(map_size_lg, posi) as f64),
                    )
                    .sub((CONFIG.sea_level as f64).div(CONFIG.mountain_scale as f64)))
                    as f32,
            )
        });

        // Calculate oceans.
        let is_ocean = get_oceans(map_size_lg, |posi: usize| alt_old[posi].1);
        // NOTE: Uncomment if you want oceans to exclusively be on the border of the
        // map.
        /* let is_ocean = (0..map_size_lg.chunks())
        .into_par_iter()
        .map(|i| map_edge_factor(map_size_lg, i) == 0.0)
        .collect::<Vec<_>>(); */
        let is_ocean_fn = |posi: usize| is_ocean[posi];

        let turb_wposf_div = 8.0;
        let n_func = |posi| {
            if is_ocean_fn(posi) {
                return 1.0;
            }
            1.0
        };
        let old_height = |posi: usize| {
            alt_old[posi].1 * CONFIG.mountain_scale * height_scale(n_func(posi)) as f32
        };

        // NOTE: Needed if you wish to use the distance to the point defining the Worley
        // cell, not just the value within that cell.
        // let uplift_nz_dist = gen_ctx.uplift_nz.clone().enable_range(true);

        // Recalculate altitudes without oceans.
        // NaNs in these uniform vectors wherever is_ocean_fn returns true.
        let (alt_old_no_ocean, _) = uniform_noise(map_size_lg, |posi, _| {
            if is_ocean_fn(posi) {
                None
            } else {
                Some(old_height(posi))
            }
        });
        let (uplift_uniform, _) = uniform_noise(map_size_lg, |posi, _wposf| {
            if is_ocean_fn(posi) {
                None
            } else {
                let oheight = alt_old_no_ocean[posi].0 as f64 - 0.5;
                let height = (oheight + 0.5).powi(2);
                Some(height)
            }
        });

        let alt_old_min_uniform = 0.0;
        let alt_old_max_uniform = 1.0;

        let inv_func = |x: f64| x;
        let alt_exp_min_uniform = inv_func(min_epsilon);
        let alt_exp_max_uniform = inv_func(max_epsilon);

        let erosion_factor = |x: f64| {
            (inv_func(x) - alt_exp_min_uniform) / (alt_exp_max_uniform - alt_exp_min_uniform)
        };
        let rock_strength_div_factor = (2.0 * TerrainChunkSize::RECT_SIZE.x as f64) / 8.0;
        let theta_func = |_posi| 0.4;
        let kf_func = {
            |posi| {
                let kf_scale_i = k_fs_scale(theta_func(posi), n_func(posi));
                if is_ocean_fn(posi) {
                    return 1.0e-4 * kf_scale_i;
                }

                let kf_i = // kf = 1.5e-4: high-high (plateau [fan sediment])
                // kf = 1e-4: high (plateau)
                // kf = 2e-5: normal (dike [unexposed])
                // kf = 1e-6: normal-low (dike [exposed])
                // kf = 2e-6: low (mountain)
                // --
                // kf = 2.5e-7 to 8e-7: very low (Cordonnier papers on plate tectonics)
                // ((1.0 - uheight) * (1.5e-4 - 2.0e-6) + 2.0e-6) as f32
                //
                // ACTUAL recorded values worldwide: much lower...
                1.0e-6
                ;
                kf_i * kf_scale_i
            }
        };
        let kd_func = {
            |posi| {
                let n = n_func(posi);
                let kd_scale_i = k_d_scale(n);
                if is_ocean_fn(posi) {
                    let kd_i = 1.0e-2 / 4.0;
                    return kd_i * kd_scale_i;
                }
                // kd = 1e-1: high (mountain, dike)
                // kd = 1.5e-2: normal-high (plateau [fan sediment])
                // kd = 1e-2: normal (plateau)
                let kd_i = 1.0e-2 / 4.0;
                kd_i * kd_scale_i
            }
        };
        let g_func = |posi| {
            if map_edge_factor(map_size_lg, posi) == 0.0 {
                return 0.0;
            }
            // G = d* v_s / p_0, where
            //  v_s is the settling velocity of sediment grains
            //  p_0 is the mean precipitation rate
            //  d* is the sediment concentration ratio (between concentration near riverbed
            //  interface, and average concentration over the water column).
            //  d* varies with Rouse number which defines relative contribution of bed,
            // suspended,  and washed loads.
            //
            // G is typically on the order of 1 or greater.  However, we are only guaranteed
            // to converge for G ≤ 1, so we keep it in the chaos range of [0.12,
            // 1.32].
            1.0
        };
        let epsilon_0_func = |posi| {
            // epsilon_0_scale is roughly [using Hack's Law with b = 2 and SPL without
            // debris flow or hillslopes] equal to the ratio of the old to new
            // area, to the power of -n_i.
            let epsilon_0_scale_i = epsilon_0_scale(n_func(posi));
            if is_ocean_fn(posi) {
                // marine: ε₀ = 2.078e-3
                let epsilon_0_i = 2.078e-3 / 4.0;
                return epsilon_0_i * epsilon_0_scale_i;
            }
            let wposf = (uniform_idx_as_vec2(map_size_lg, posi)
                * TerrainChunkSize::RECT_SIZE.map(|e| e as i32))
            .map(|e| e as f64);
            let turb_wposf = wposf
                .mul(5_000.0 / continent_scale)
                .div(TerrainChunkSize::RECT_SIZE.map(|e| e as f64))
                .div(turb_wposf_div);
            let turb = Vec2::new(
                gen_ctx.turb_x_nz.get(turb_wposf.into_array()),
                gen_ctx.turb_y_nz.get(turb_wposf.into_array()),
            ) * uplift_turb_scale
                * TerrainChunkSize::RECT_SIZE.map(|e| e as f64);
            let turb_wposf = wposf + turb;
            let uheight = gen_ctx
                .uplift_nz
                .get(turb_wposf.into_array())
                .clamp(-1.0, 1.0)
                .mul(0.5)
                .add(0.5);
            let wposf3 = Vec3::new(
                wposf.x,
                wposf.y,
                uheight * CONFIG.mountain_scale as f64 * rock_strength_div_factor,
            );
            let rock_strength = gen_ctx
                .rock_strength_nz
                .get(wposf3.into_array())
                .clamp(-1.0, 1.0)
                .mul(0.5)
                .add(0.5);
            let center = 0.4;
            let dmin = center - 0.05;
            let dmax = center + 0.05;
            let log_odds = |x: f64| logit(x) - logit(center);
            let ustrength = logistic_cdf(
                1.0 * logit(rock_strength.clamp(1e-7, 1.0f64 - 1e-7))
                    + 1.0 * log_odds(uheight.clamp(dmin, dmax)),
            );
            // marine: ε₀ = 2.078e-3
            // San Gabriel Mountains: ε₀ = 3.18e-4
            // Oregon Coast Range: ε₀ = 2.68e-4
            // Frogs Hollow (peak production = 0.25): ε₀ = 1.41e-4
            // Point Reyes: ε₀ = 8.1e-5
            // Nunnock River (fractured granite, least weathered?): ε₀ = 5.3e-5
            let epsilon_0_i = ((1.0 - ustrength) * (2.078e-3 - 5.3e-5) + 5.3e-5) as f32 / 4.0;
            epsilon_0_i * epsilon_0_scale_i
        };
        let alpha_func = |posi| {
            let alpha_scale_i = alpha_scale(n_func(posi));
            if is_ocean_fn(posi) {
                // marine: α = 3.7e-2
                return 3.7e-2 * alpha_scale_i;
            }
            let wposf = (uniform_idx_as_vec2(map_size_lg, posi)
                * TerrainChunkSize::RECT_SIZE.map(|e| e as i32))
            .map(|e| e as f64);
            let turb_wposf = wposf
                .mul(5_000.0 / continent_scale)
                .div(TerrainChunkSize::RECT_SIZE.map(|e| e as f64))
                .div(turb_wposf_div);
            let turb = Vec2::new(
                gen_ctx.turb_x_nz.get(turb_wposf.into_array()),
                gen_ctx.turb_y_nz.get(turb_wposf.into_array()),
            ) * uplift_turb_scale
                * TerrainChunkSize::RECT_SIZE.map(|e| e as f64);
            let turb_wposf = wposf + turb;
            let uheight = gen_ctx
                .uplift_nz
                .get(turb_wposf.into_array())
                .clamp(-1.0, 1.0)
                .mul(0.5)
                .add(0.5);
            let wposf3 = Vec3::new(
                wposf.x,
                wposf.y,
                uheight * CONFIG.mountain_scale as f64 * rock_strength_div_factor,
            );
            let rock_strength = gen_ctx
                .rock_strength_nz
                .get(wposf3.into_array())
                .clamp(-1.0, 1.0)
                .mul(0.5)
                .add(0.5);
            let center = 0.4;
            let dmin = center - 0.05;
            let dmax = center + 0.05;
            let log_odds = |x: f64| logit(x) - logit(center);
            let ustrength = logistic_cdf(
                1.0 * logit(rock_strength.clamp(1e-7, 1.0f64 - 1e-7))
                    + 1.0 * log_odds(uheight.clamp(dmin, dmax)),
            );
            // Frog Hollow (peak production = 0.25): α = 4.2e-2
            // San Gabriel Mountains: α = 3.8e-2
            // marine: α = 3.7e-2
            // Oregon Coast Range: α = 3e-2
            // Nunnock river (fractured granite, least weathered?): α = 2e-3
            // Point Reyes: α = 1.6e-2
            // The stronger  the rock, the faster the decline in soil production.
            let alpha_i = (ustrength * (4.2e-2 - 1.6e-2) + 1.6e-2) as f32;
            alpha_i * alpha_scale_i
        };
        let uplift_fn = |posi| {
            if is_ocean_fn(posi) {
                return 0.0;
            }
            let height = (uplift_uniform[posi].1 - alt_old_min_uniform)
                / (alt_old_max_uniform - alt_old_min_uniform);

            let height = height.mul(max_epsilon - min_epsilon).add(min_epsilon);
            let height = erosion_factor(height);
            assert!(height >= 0.0);
            assert!(height <= 1.0);

            // u = 1e-3: normal-high (dike, mountain)
            // u = 5e-4: normal (mid example in Yuan, average mountain uplift)
            // u = 2e-4: low (low example in Yuan; known that lagoons etc. may have u ~
            // 0.05). u = 0: low (plateau [fan, altitude = 0.0])

            height.mul(max_erosion_per_delta_t)
        };
        let alt_func = |posi| {
            if is_ocean_fn(posi) {
                old_height(posi)
            } else {
                (old_height(posi) as f64 / CONFIG.mountain_scale as f64) as f32 - 0.5
            }
        };

        // Perform some erosion.

        let mut last = None;
        let mut all_samples = std::time::Duration::default();
        let mut sample_count = 0;
        let report_erosion: &mut dyn FnMut(f64) = &mut move |progress: f64| {
            let now = std::time::Instant::now();
            let estimate = if let Some((last_instant, last_progress)) = last {
                if last_progress > progress {
                    None
                } else {
                    if last_progress < progress {
                        let sample = now
                            .duration_since(last_instant)
                            .div_f64(progress - last_progress);
                        all_samples += sample;
                        sample_count += 1;
                    }

                    Some((all_samples / sample_count).mul_f64(100.0 - progress))
                }
            } else {
                None
            };
            last = Some((now, progress));
            stage_report(WorldSimStage::Erosion { progress, estimate })
        };

        let (alt, basement) = if let Some(map) = parsed_world_file {
            (map.alt, map.basement)
        } else {
            let (alt, basement) = do_erosion(
                map_size_lg,
                max_erosion_per_delta_t as f32,
                n_steps,
                river_seed,
                // varying conditions
                &rock_strength_nz,
                // initial conditions
                alt_func,
                alt_func,
                is_ocean_fn,
                // empirical constants
                uplift_fn,
                n_func,
                theta_func,
                kf_func,
                kd_func,
                g_func,
                epsilon_0_func,
                alpha_func,
                // scaling factors
                height_scale,
                k_d_scale(n_approx),
                k_da_scale,
                threadpool,
                report_erosion,
            );

            // Quick "small scale" erosion cycle in order to lower extreme angles.
            do_erosion(
                map_size_lg,
                1.0f32,
                n_small_steps,
                river_seed,
                &rock_strength_nz,
                |posi| alt[posi] as f32,
                |posi| basement[posi] as f32,
                is_ocean_fn,
                |posi| uplift_fn(posi) * (1.0 / max_erosion_per_delta_t),
                n_func,
                theta_func,
                kf_func,
                kd_func,
                g_func,
                epsilon_0_func,
                alpha_func,
                height_scale,
                k_d_scale(n_approx),
                k_da_scale,
                threadpool,
                report_erosion,
            )
        };

        // Save map, if necessary.
        // NOTE: We wll always save a map with latest version.
        let map = WorldFile::new(ModernMap {
            continent_scale_hack: gen_opts.scale,
            map_size_lg: map_size_lg.vec(),
            alt,
            basement,
        });
        if fresh {
            world_file.save(&map);
        }

        // Skip validation--we just performed a no-op conversion for this map, so it had
        // better be valid!
        let ModernMap {
            continent_scale_hack: _,
            map_size_lg: _,
            alt,
            basement,
        } = map.into_modern().unwrap();

        // Additional small-scale erosion after map load, only used during testing.
        let (alt, basement) = if n_post_load_steps == 0 {
            (alt, basement)
        } else {
            do_erosion(
                map_size_lg,
                1.0f32,
                n_post_load_steps,
                river_seed,
                &rock_strength_nz,
                |posi| alt[posi] as f32,
                |posi| basement[posi] as f32,
                is_ocean_fn,
                |posi| uplift_fn(posi) * (1.0 / max_erosion_per_delta_t),
                n_func,
                theta_func,
                kf_func,
                kd_func,
                g_func,
                epsilon_0_func,
                alpha_func,
                height_scale,
                k_d_scale(n_approx),
                k_da_scale,
                threadpool,
                report_erosion,
            )
        };

        let is_ocean = get_oceans(map_size_lg, |posi| alt[posi]);
        let is_ocean_fn = |posi: usize| is_ocean[posi];
        let mut dh = downhill(map_size_lg, |posi| alt[posi], is_ocean_fn);
        let (boundary_len, indirection, water_alt_pos, maxh) =
            get_lakes(map_size_lg, |posi| alt[posi], &mut dh);
        debug!(?maxh, "Max height");
        let (mrec, mstack, mwrec) = {
            let mut wh = vec![0.0; map_size_lg.chunks_len()];
            get_multi_rec(
                map_size_lg,
                |posi| alt[posi],
                &dh,
                &water_alt_pos,
                &mut wh,
                usize::from(map_size_lg.chunks().x),
                usize::from(map_size_lg.chunks().y),
                TerrainChunkSize::RECT_SIZE.x as Compute,
                TerrainChunkSize::RECT_SIZE.y as Compute,
                maxh,
                threadpool,
            )
        };
        let flux_old = get_multi_drainage(map_size_lg, &mstack, &mrec, &mwrec, boundary_len);
        // let flux_rivers = get_drainage(map_size_lg, &water_alt_pos, &dh,
        // boundary_len); TODO: Make rivers work with multi-direction flux as
        // well.
        let flux_rivers = flux_old.clone();

        let water_height_initial = |chunk_idx| {
            let indirection_idx = indirection[chunk_idx];
            // Find the lake this point is flowing into.
            let lake_idx = if indirection_idx < 0 {
                chunk_idx
            } else {
                indirection_idx as usize
            };
            let chunk_water_alt = if dh[lake_idx] < 0 {
                // This is either a boundary node (dh[chunk_idx] == -2, i.e. water is at sea
                // level) or part of a lake that flows directly into the ocean.
                // In the former case, water is at sea level so we just return
                // 0.0.  In the latter case, the lake bottom must have been a
                // boundary node in the first place--meaning this node flows directly
                // into the ocean.  In that case, its lake bottom is ocean, meaning its water is
                // also at sea level.  Thus, we return 0.0 in both cases.
                0.0
            } else {
                // This chunk is draining into a body of water that isn't the ocean (i.e., a
                // lake). Then we just need to find the pass height of the
                // surrounding lake in order to figure out the initial water
                // height (which fill_sinks will then extend to make
                // sure it fills the entire basin).

                // Find the height of "our" side of the pass (the part of it that drains into
                // this chunk's lake).
                let pass_idx = -indirection[lake_idx] as usize;
                let pass_height_i = alt[pass_idx];
                // Find the pass this lake is flowing into (i.e. water at the lake bottom gets
                // pushed towards the point identified by pass_idx).
                let neighbor_pass_idx = dh[pass_idx/*lake_idx*/];
                // Find the height of the pass into which our lake is flowing.
                let pass_height_j = alt[neighbor_pass_idx as usize];
                // Find the maximum of these two heights.
                // Use the pass height as the initial water altitude.
                pass_height_i.max(pass_height_j) /*pass_height*/
            };
            // Use the maximum of the pass height and chunk height as the parameter to
            // fill_sinks.
            let chunk_alt = alt[chunk_idx];
            chunk_alt.max(chunk_water_alt)
        };

        // NOTE: If for for some reason you need to avoid the expensive `fill_sinks`
        // step here, and we haven't yet replaced it with a faster version, you
        // may comment out this line and replace it with the commented-out code
        // below; however, there are no guarantees that this
        // will work correctly.
        let water_alt = fill_sinks(map_size_lg, water_height_initial, is_ocean_fn);
        /* let water_alt = (0..map_size_lg.chunks_len())
        .into_par_iter()
        .map(|posi| water_height_initial(posi))
        .collect::<Vec<_>>(); */

        let mut rivers = get_rivers(
            map_size_lg,
            gen_opts.scale,
            &water_alt_pos,
            &water_alt,
            &dh,
            &indirection,
            &flux_rivers,
        );
        if authored_cromatolis_v0 {
            // Blend the authored water / elevated-lake / river-channel masks into real
            // hydrology, instead of the altitude-only heuristic this block used before
            // COW-3. Each mask keeps its own semantics rather than being collapsed into
            // one generic "is wet" check: `elevated_lakes` marks standing water above
            // sea level, `water` marks the broader surface-water footprint (sea/ocean/
            // at-or-below-sea-level bodies), and `river_channels` marks flowing
            // corridors. Priority follows specificity: an elevated lake wins over the
            // broader water mask, which wins over a river channel, which wins over
            // nothing.
            // COW-22 `C22-1b`: the width of the channel each corridor chunk sits in,
            // precomputed once for the whole map rather than searched per chunk
            // (which would be O(n^2)). Feeds both the "can this be carved as a real
            // river at all" test and the river's cross-section, which used to be a
            // flat 3.2 m x 0.25 m ditch for every river on the map.
            //
            // `local_channel_radius_chunks`, not a bare distance transform: a chunk's
            // own distance to the bank is small on *both* banks of a wide body, so
            // thresholding that directly carves the body's rim and leaves its
            // interior flat -- the water walls this is supposed to avoid. See that
            // function's doc comment for the measured numbers.
            let authored_channel_width = authored_river_channels_layer.as_ref().map(|values| {
                local_channel_radius_chunks(map_size_lg, |idx| {
                    authored_layer_value_for_cromatolis_v0(map_size_lg, idx, values)
                        >= AUTHORED_WATER_THRESHOLD
                })
                .iter()
                .map(|channel_radius| cromatolis_channel_width(*channel_radius))
                .collect::<Vec<_>>()
                .into_boxed_slice()
            });
            let mask_value = |layer: &Option<Box<[f32]>>, idx: usize| {
                layer
                    .as_ref()
                    .map(|values| authored_layer_value_for_cromatolis_v0(map_size_lg, idx, values))
            };
            // If every mask failed to load (e.g. degraded/missing assets), fall back to
            // the pre-COW-3 altitude-only heuristic so oceans/lakes still form instead
            // of vanishing outright.
            let masks_loaded = authored_water_layer.is_some()
                || authored_elevated_lakes_layer.is_some()
                || authored_river_channels_layer.is_some();
            for (idx, river) in rivers.iter_mut().enumerate() {
                let is_elevated_lake = mask_value(&authored_elevated_lakes_layer, idx)
                    .is_some_and(|v| v >= AUTHORED_WATER_THRESHOLD);
                let is_water_body = mask_value(&authored_water_layer, idx)
                    .is_some_and(|v| v >= AUTHORED_WATER_THRESHOLD);
                let is_river_channel = mask_value(&authored_river_channels_layer, idx)
                    .is_some_and(|v| v >= AUTHORED_WATER_THRESHOLD);
                let channel_width = authored_channel_width
                    .as_ref()
                    .map_or(0.0, |widths| widths[idx]);
                let authored_river_cross_section =
                    cromatolis_authored_river_cross_section(channel_width);

                let neighbor_pass_pos = uniform_idx_as_vec2(map_size_lg, idx);
                river.river_kind = if !masks_loaded && alt[idx] < 0.0 {
                    // Every mask failed to load; fall back to the pre-COW-3
                    // altitude-only heuristic so oceans/lakes still form.
                    if is_ocean[idx] {
                        Some(RiverKind::Ocean)
                    } else {
                        Some(RiverKind::Lake { neighbor_pass_pos })
                    }
                } else {
                    authored_river_kind_override(AuthoredRiverKindInputs {
                        region_id: authored_region_id,
                        is_elevated_lake,
                        is_water_body,
                        is_river_channel,
                        channel_fits_max_river_width: channel_width <= CROMATOLIS_MAX_RIVER_WIDTH,
                        // `SimChunk::generate` maps `dh == -2` (an ocean
                        // boundary node) to `downhill: None`, which `column.rs`
                        // refuses to see on a river.
                        has_downhill: dh[idx] >= 0,
                        is_ocean: is_ocean[idx],
                        alt_below_sea_level: alt[idx] < 0.0,
                        neighbor_pass_pos,
                        authored_river_cross_section,
                    })
                };

                // An authored river gets its cross-section from the corridor mask
                // (see `authored_river_kind_override`), so its velocity has to be
                // re-derived to match -- `get_rivers`' own velocity, where it
                // produced one at all, belongs to a cross-section we just replaced.
                if matches!(river.river_kind, Some(RiverKind::River { .. })) {
                    river.velocity = cromatolis_authored_river_velocity(
                        map_size_lg,
                        idx,
                        dh[idx],
                        &alt,
                        authored_river_cross_section.y,
                    );
                }
            }
        }

        // Per-chunk "is this land tile adjacent to authored standing/flowing
        // water" signal, built from the same masks the block above already
        // loaded -- consumed by `SimChunk::get_biome`'s `Swamp` branch
        // (COW-4). A chunk that *is* water itself gets classified
        // Ocean/Lake by an earlier `get_biome` branch before `Swamp` is ever
        // considered, so this only needs to answer "is a neighbor water" for
        // chunks that are themselves dry land. The masks are strictly binary
        // (see `AUTHORED_WATER_THRESHOLD`'s doc comment), so there's no
        // per-chunk gradient to threshold against directly -- proximity has
        // to come from a neighbor check instead, same 3x3-neighborhood
        // pattern `pure_water` below uses.
        //
        // The same sweep also derives `authored_water_body` (COW-22 `C22-1b`):
        // the ecological classification of the chunks that *are* water. It
        // needs a 3x3 neighborhood too -- what separates a lagoon from a
        // landlocked lake is whether it touches marine water -- so both come
        // out of one pass rather than two.
        let (authored_near_water, authored_water_body) = if authored_cromatolis_v0 {
            let mask_hit =
                |layer: &Option<Box<[f32]>>, idx: usize| authored_mask_hit(map_size_lg, layer, idx);
            // Marine water: inside the authored `water` mask *and* reached by
            // the `get_oceans` border flood fill. Deliberately not "alt below
            // sea level" -- see `authored_river_kind_override` (COW-22
            // `C22-1c`).
            let is_marine = |idx: usize| mask_hit(&authored_water_layer, idx) && is_ocean[idx];
            let (near_water, water_body): (Vec<bool>, Vec<Option<WaterBodyKind>>) = (0
                ..map_size_lg.chunks_len())
                .into_par_iter()
                .map(|posi| {
                    let pos = uniform_idx_as_vec2(map_size_lg, posi);
                    let mut near_water = false;
                    let mut adjacent_to_marine = false;
                    for x in pos.x - 1..=pos.x + 1 {
                        for y in pos.y - 1..=pos.y + 1 {
                            if x < 0
                                || y < 0
                                || x >= map_size_lg.chunks().x as i32
                                || y >= map_size_lg.chunks().y as i32
                            {
                                continue;
                            }
                            let nidx = vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y));
                            near_water |= mask_hit(&authored_water_layer, nidx)
                                || mask_hit(&authored_elevated_lakes_layer, nidx)
                                || mask_hit(&authored_river_channels_layer, nidx);
                            adjacent_to_marine |= nidx != posi && is_marine(nidx);
                        }
                    }
                    let water_body = authored_water_body_kind(AuthoredWaterBodyInputs {
                        is_elevated_lake: mask_hit(&authored_elevated_lakes_layer, posi),
                        is_water_body: mask_hit(&authored_water_layer, posi),
                        is_river_channel: mask_hit(&authored_river_channels_layer, posi),
                        is_marine: is_marine(posi),
                        is_adjacent_to_marine: adjacent_to_marine,
                        // Nothing computes the offshore band that splits
                        // `Sea` from `Ocean` yet, so every marine chunk is open
                        // ocean. COW-22 `C22-3` reshaped the seabed either side
                        // of the waterline but derived no engine-side shelf
                        // classification from it.
                        is_shelf_sea: false,
                    });
                    (near_water, water_body)
                })
                .unzip();
            let mut water_body = water_body.into_boxed_slice();
            promote_lagoon_basins(map_size_lg, &mut water_body, |idx| {
                mask_hit(&authored_elevated_lakes_layer, idx)
            });
            (near_water.into_boxed_slice(), water_body)
        } else {
            (
                vec![false; map_size_lg.chunks_len()].into_boxed_slice(),
                vec![None; map_size_lg.chunks_len()].into_boxed_slice(),
            )
        };

        // Salinity is the second, independent classification axis: a body can
        // be a river *and* salt. It is derived from the topology of the bodies
        // classified above and never authored as its own raster (COW-22
        // `C22-4`), so it needs nothing the block above did not already
        // produce -- only the `elevated_lakes` decree, which it treats exactly
        // as `promote_lagoon_basins` does.
        let authored_salinity = if authored_cromatolis_v0 {
            derive_salinity(map_size_lg, &authored_water_body, &alt, |idx| {
                authored_mask_hit(map_size_lg, &authored_elevated_lakes_layer, idx)
            })
        } else {
            vec![None; map_size_lg.chunks_len()].into_boxed_slice()
        };

        let water_alt = indirection
            .par_iter()
            .enumerate()
            .map(|(chunk_idx, &indirection_idx)| {
                // Find the lake this point is flowing into.
                let lake_idx = if indirection_idx < 0 {
                    chunk_idx
                } else {
                    indirection_idx as usize
                };
                let lake_is_authored_elevated = authored_cromatolis_v0
                    && authored_elevated_lakes_layer
                        .as_ref()
                        .is_some_and(|values| {
                            authored_layer_value_for_cromatolis_v0(map_size_lg, lake_idx, values)
                                >= AUTHORED_WATER_THRESHOLD
                        });
                if cromatolis_forces_sea_level(
                    authored_cromatolis_v0,
                    dh[lake_idx],
                    lake_is_authored_elevated,
                ) {
                    0.0
                } else {
                    // This is not flowing into the ocean, so we can use the existing water_alt.
                    water_alt[chunk_idx] as f32
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        let is_underwater = |chunk_idx: usize| match rivers[chunk_idx].river_kind {
            Some(RiverKind::Ocean) | Some(RiverKind::Lake { .. }) => true,
            Some(RiverKind::River { .. }) => false, // TODO: inspect width
            None => false,
        };

        // Check whether any tiles around this tile are not water (since Lerp will
        // ensure that they are included).
        let pure_water = |posi: usize| {
            let pos = uniform_idx_as_vec2(map_size_lg, posi);
            for x in pos.x - 1..(pos.x + 1) + 1 {
                for y in pos.y - 1..(pos.y + 1) + 1 {
                    if x >= 0
                        && y >= 0
                        && x < map_size_lg.chunks().x as i32
                        && y < map_size_lg.chunks().y as i32
                    {
                        let posi = vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y));
                        if !is_underwater(posi) {
                            return false;
                        }
                    }
                }
            }
            true
        };

        // NaNs in these uniform vectors wherever pure_water() returns true.
        let (((alt_no_water, _), (pure_flux, _)), ((temp_base, _), (humid_base, _))) = threadpool
            .join(
                || {
                    threadpool.join(
                        || {
                            uniform_noise(map_size_lg, |posi, _| {
                                if pure_water(posi) {
                                    None
                                } else {
                                    // A version of alt that is uniform over *non-water* (or
                                    // land-adjacent water) chunks.
                                    Some(alt[posi] as f32)
                                }
                            })
                        },
                        || {
                            uniform_noise(map_size_lg, |posi, _| {
                                if pure_water(posi) {
                                    None
                                } else {
                                    Some(flux_old[posi])
                                }
                            })
                        },
                    )
                },
                || {
                    threadpool.join(
                        || {
                            uniform_noise(map_size_lg, |posi, wposf| {
                                if pure_water(posi) {
                                    None
                                } else {
                                    // -1 to 1.
                                    Some(gen_ctx.temp_nz.get((wposf).into_array()) as f32)
                                }
                            })
                        },
                        || {
                            uniform_noise(map_size_lg, |posi, wposf| {
                                // Check whether any tiles around this tile are water.
                                if pure_water(posi) {
                                    None
                                } else {
                                    // 0 to 1, hopefully.
                                    Some(
                                        (gen_ctx.humid_nz.get(wposf.div(1024.0).into_array())
                                            as f32)
                                            .add(1.0)
                                            .mul(0.5),
                                    )
                                }
                            })
                        },
                    )
                },
            );

        let gen_cdf = GenCdf {
            authored_cromatolis_v0,
            authored_region_id,
            authored_route_layer,
            authored_vegetation_layer,
            authored_ground_cover_layer,
            authored_climate_zone_layer,
            authored_ground_substrate_zones,
            cromatolis_climate,
            authored_alpine_policy,
            authored_near_water,
            authored_water_body,
            authored_salinity,
            humid_base,
            temp_base,
            chaos,
            alt,
            basement,
            water_alt,
            dh,
            flux: flux_old,
            pure_flux,
            alt_no_water,
            rivers,
        };

        let chunks = (0..map_size_lg.chunks_len())
            .into_par_iter()
            .map(|i| SimChunk::generate(map_size_lg, i, &gen_ctx, &gen_cdf))
            .collect::<Vec<_>>();

        let mut this = Self {
            seed,
            map_size_lg,
            max_height: maxh as f32,
            chunks,
            _locations: Vec::new(),
            gen_ctx,
            rng,
            calendar,
            authored_procedural_layers,
            authored_ground_cover_profile,
            authored_map_ecology_profile,
            authored_ecology_zone_layer,
            authored_tree_candidate_policy,
            authored_alpine_policy,
        };

        this.generate_cliffs();

        if opts.seed_elements {
            this.seed_elements();
        }

        this
    }

    #[inline(always)]
    pub const fn map_size_lg(&self) -> MapSizeLg { self.map_size_lg }

    pub fn get_size(&self) -> Vec2<u32> { self.map_size_lg().chunks().map(u32::from) }

    /// Which purely-procedural voxel layers still run in this world.
    /// `None` means no authored region is loaded, i.e. every layer runs —
    /// the upstream-Veloren case. See [`AuthoredProceduralLayers`].
    pub(crate) fn authored_procedural_layers(&self) -> Option<AuthoredProceduralLayers> {
        self.authored_procedural_layers
    }

    /// Replace this world's procedural-layer policy, for a test that generates
    /// the same chunks under two policies and diffs them.
    ///
    /// The alternative — replicating in the test whatever the layer under
    /// examination would have done — tests the replica. This runs the real
    /// `World::generate_chunk` both times, so the diff is between two things
    /// the engine actually produces.
    ///
    /// Reached through
    /// [`crate::World::set_authored_procedural_layers_for_test`],
    /// which is where the reasoning for the shape of this lives.
    #[cfg(test)]
    pub(crate) fn set_authored_procedural_layers_for_test(
        &mut self,
        layers: AuthoredProceduralLayers,
    ) {
        self.authored_procedural_layers = Some(layers);
    }

    /// Whether this world's *region* allows the purely-procedural
    /// cave/tunnel layer. Only half the answer — the global
    /// `Features::caves` toggle is the other half, and callers must `&&`
    /// it in (see `World::generate_chunk`, which is where the two are
    /// composed for the carving pass itself).
    ///
    /// Callers outside `apply_caves_to` need this at all because `Tunnel`s
    /// are hash-derived on demand rather than stored: skipping the carving
    /// pass does not make the tunnel *data* go away. Anything that reacts
    /// to a tunnel's presence — the surface cave markers on the world map,
    /// the tree/shrub suppression above a near-surface tunnel — has to
    /// ask the same question, or it reacts to caves that were never
    /// carved.
    pub(crate) fn authored_procedural_caves_enabled(&self) -> bool {
        self.authored_procedural_layers
            .is_none_or(|layers| layers.caves)
    }

    /// Returns the unmodified global candidate lattice plus any data-owned,
    /// authored regional candidates whose polygons contain their roots.
    /// Position de-duplication preserves the global candidate's seed and
    /// order, so a procedural world is bit-identical and overlapping grids
    /// never create a duplicate tree.
    pub(crate) fn tree_candidate_fields_near(&self, wpos: Vec2<i32>) -> Vec<TreeCandidateField> {
        let world_blocks = self.world_blocks();
        match self.authored_tree_candidate_policy.as_ref() {
            Some(policy) if policy.may_supply_candidates_near(wpos, world_blocks) => {
                merge_tree_candidate_fields(
                    self.gen_ctx.structure_gen.get(wpos),
                    policy.additional_candidates_near(wpos, world_blocks),
                )
            },
            _ => self.gen_ctx.structure_gen.get(wpos).to_vec(),
        }
    }

    pub(crate) fn has_additional_tree_candidate_fields_near(&self, wpos: Vec2<i32>) -> bool {
        self.authored_tree_candidate_policy
            .as_ref()
            .is_some_and(|policy| policy.may_supply_candidates_near(wpos, self.world_blocks()))
    }

    pub(crate) fn tree_candidate_fields_in_area(
        &self,
        min: Vec2<i32>,
        max: Vec2<i32>,
    ) -> Vec<TreeCandidateField> {
        let world_blocks = self.world_blocks();
        match self.authored_tree_candidate_policy.as_ref() {
            Some(policy) if policy.may_supply_candidates_in_area(min, max, world_blocks) => {
                merge_tree_candidate_fields(
                    self.gen_ctx.structure_gen.iter(min, max),
                    policy.additional_candidates_in_area(min, max, world_blocks),
                )
            },
            _ => self.gen_ctx.structure_gen.iter(min, max).collect(),
        }
    }

    fn world_blocks(&self) -> Vec2<i32> {
        self.map_size_lg().chunks().map(|chunks| chunks as i32)
            * TerrainChunkSize::RECT_SIZE.as_::<i32>()
    }

    pub fn get_aabr(&self) -> Aabr<i32> {
        let size = self.get_size();
        Aabr {
            min: Vec2 { x: 0, y: 0 },
            max: Vec2 {
                x: size.x as i32,
                y: size.y as i32,
            },
        }
    }

    pub fn generate_oob_chunk(&self) -> TerrainChunk {
        TerrainChunk::water(CONFIG.sea_level as i32)
    }

    pub fn approx_chunk_terrain_normal(&self, chunk_pos: Vec2<i32>) -> Option<Vec3<f32>> {
        let curr_chunk = self.get(chunk_pos)?;
        let downhill_chunk_pos = curr_chunk.downhill?.wpos_to_cpos();
        let downhill_chunk = self.get(downhill_chunk_pos)?;
        // special case if chunks are flat
        if (curr_chunk.alt - downhill_chunk.alt) == 0. {
            return Some(Vec3::unit_z());
        }
        let curr = chunk_pos.cpos_to_wpos_center().as_().with_z(curr_chunk.alt);
        let down = downhill_chunk_pos
            .cpos_to_wpos_center()
            .as_()
            .with_z(downhill_chunk.alt);
        let downwards = curr - down;
        let flat = downwards.with_z(down.z);
        let mut res = downwards.cross(flat).cross(downwards);
        res.normalize();
        Some(res)
    }

    /// Draw a map of the world based on chunk information.  Returns a buffer of
    /// u32s.
    pub fn get_map(&self, index: IndexRef, calendar: Option<&Calendar>) -> WorldMapMsg {
        prof_span!("WorldSim::get_map");
        let mut map_config = MapConfig::orthographic(
            self.map_size_lg(),
            core::ops::RangeInclusive::new(CONFIG.sea_level, CONFIG.sea_level + self.max_height),
        );
        // Build a horizon map.
        let scale_angle = |angle: Alt| {
            (/* 0.0.max( */angle /* ) */
                .atan()
                * <Alt as FloatConst>::FRAC_2_PI()
                * 255.0)
                .floor() as u8
        };
        let scale_height = |height: Alt| {
            (/* 0.0.max( */height/*)*/ as Alt * 255.0 / self.max_height as Alt).floor() as u8
        };

        let samples_data = {
            prof_span!("samples data");
            let column_sample = ColumnGen::new(self);
            (0..self.map_size_lg().chunks_len())
                .into_par_iter()
                .map_init(
                    || Box::new(BlockGen::new(ColumnGen::new(self))),
                    |_block_gen, posi| {
                        let sample = column_sample.get(
                            (
                                uniform_idx_as_vec2(self.map_size_lg(), posi) * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                                index,
                                calendar,
                            )
                        )?;
                        // sample.water_level = CONFIG.sea_level.max(sample.water_level);

                        Some(sample)
                    },
                )
                /* .map(|posi| {
                    let mut sample = column_sample.get(
                        uniform_idx_as_vec2(self.map_size_lg(), posi) * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                    );
                }) */
                .collect::<Vec<_>>()
                .into_boxed_slice()
        };

        let horizons = get_horizon_map(
            self.map_size_lg(),
            Aabr {
                min: Vec2::zero(),
                max: self.map_size_lg().chunks().map(|e| e as i32),
            },
            CONFIG.sea_level,
            CONFIG.sea_level + self.max_height,
            |posi| {
                /* let chunk = &self.chunks[posi];
                chunk.alt.max(chunk.water_alt) as Alt */
                let sample = samples_data[posi].as_ref();
                sample
                    .map(|s| s.alt.max(s.water_level))
                    .unwrap_or(CONFIG.sea_level)
            },
            |a| scale_angle(a.into()),
            |h| scale_height(h.into()),
        )
        .unwrap();

        let mut v = vec![0u32; self.map_size_lg().chunks_len()];
        let mut alts = vec![0u32; self.map_size_lg().chunks_len()];
        // TODO: Parallelize again.
        map_config.is_shaded = false;

        map_config.generate(
            |pos| sample_pos(&map_config, self, index, Some(&samples_data), pos),
            |pos| sample_wpos(&map_config, self, pos),
            |pos, (r, g, b, _a)| {
                // We currently ignore alpha and replace it with the height at pos, scaled to
                // u8.
                let alt = sample_wpos(
                    &map_config,
                    self,
                    pos.map(|e| e as i32) * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                );
                let a = 0; //(alt.min(1.0).max(0.0) * 255.0) as u8;

                // NOTE: Safe by invariants on map_size_lg.
                let posi = (pos.y << self.map_size_lg().vec().x) | pos.x;
                v[posi] = u32::from_le_bytes([r, g, b, a]);
                alts[posi] = (((alt.clamp(0.0, 1.0) * 8191.0) as u32) & 0x1FFF) << 3;
            },
        );
        WorldMapMsg {
            dimensions_lg: self.map_size_lg().vec(),
            max_height: self.max_height,
            minimap_voxel_overlay_alpha: self
                .authored_map_ecology_profile
                .as_ref()
                .map_or(u8::MAX, |profile| profile.voxel_minimap_overlay_alpha),
            rgba: Grid::from_raw(self.get_size().map(|e| e as i32), v),
            alt: Grid::from_raw(self.get_size().map(|e| e as i32), alts),
            horizons,
            sites: Vec::new(),                   // Will be substituted later
            pois: Vec::new(),                    // Will be substituted later
            possible_starting_sites: Vec::new(), // Will be substituted later
            default_chunk: Arc::new(self.generate_oob_chunk()),
        }
    }

    pub fn generate_cliffs(&mut self) {
        let mut rng = self.rng.clone();

        for _ in 0..self.get_size().product() / 10 {
            let mut pos = self.get_size().map(|e| rng.random_range(0..e) as i32);

            let mut cliffs = DHashSet::default();
            let mut cliff_path = Vec::new();

            for _ in 0..64 {
                if self.get_gradient_approx(pos).is_some_and(|g| g > 1.5) {
                    if !cliffs.insert(pos) {
                        break;
                    }
                    cliff_path.push((pos, 0.0));

                    pos += CARDINALS
                        .iter()
                        .copied()
                        .max_by_key(|rpos| {
                            self.get_gradient_approx(pos + rpos)
                                .map_or(0, |g| (g * 1000.0) as i32)
                        })
                        .unwrap(); // Can't fail
                } else {
                    break;
                }
            }

            for cliff in cliffs {
                Spiral2d::new()
                    .take((4usize * 2 + 1).pow(2))
                    .for_each(|rpos| {
                        let dist = rpos.map(|e| e as f32).magnitude();
                        if let Some(c) = self.get_mut(cliff + rpos) {
                            let warp = 1.0 / (1.0 + dist);
                            if !c.river.near_water() {
                                // Cliff steepness is real physical terrain and
                                // stays authoritative for every world (it
                                // still drives `near_cliffs()`/`cliff_height`
                                // below), but crushing `tree_density` toward
                                // zero near a detected cliff is a purely
                                // procedural heuristic. For ANY authored
                                // region it would silently override a
                                // hand-painted vegetation density with
                                // engine noise -- the same principle
                                // `cromatolis_forces_sea_level` already
                                // applies to `water_alt` (that one stays
                                // scoped to `authored_cromatolis_v0`
                                // deliberately, since forcing sea level for
                                // closed basins is a Cromatolis-specific
                                // hydrology rule, not a general "authored
                                // data wins" rule). This check uses the
                                // broader `authored_region_id` instead of
                                // `authored_cromatolis_v0` because THIS rule
                                // -- don't let procedural noise override
                                // hand-painted vegetation -- isn't
                                // Cromatolis-specific the way the
                                // Snowland/Desert exemptions near
                                // `CROMATOLIS_V0_REGION_ID` are: any future
                                // authored region should get the same
                                // priority for its own painted vegetation.
                                if c.authored_region_id.is_none() {
                                    c.tree_density *= 1.0 - warp;
                                }
                                c.cliff_height = Lerp::lerp(44.0, 0.0, -1.0 + dist / 3.5);
                            }
                        }
                    });
            }
        }
    }

    /// Prepare the world for simulation
    pub fn seed_elements(&mut self) {
        let mut rng = self.rng.clone();

        let cell_size = 16;
        let grid_size = self.map_size_lg().chunks().map(usize::from) / cell_size;
        let loc_count = 100;

        let mut loc_grid = vec![None; grid_size.product()];
        let mut locations = Vec::new();

        // Seed the world with some locations
        (0..loc_count).for_each(|_| {
            let cell_pos = Vec2::new(
                (self.rng.random::<u64>() as usize) % grid_size.x,
                (self.rng.random::<u64>() as usize) % grid_size.y,
            );
            let wpos = (cell_pos * cell_size + cell_size / 2)
                .map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
                    e as i32 * sz as i32 + sz as i32 / 2
                });

            locations.push(Location::generate(wpos, &mut rng));

            loc_grid[cell_pos.y * grid_size.x + cell_pos.x] = Some(locations.len() - 1);
        });

        // Find neighbours
        let mut loc_clone = locations
            .iter()
            .map(|l| l.center)
            .enumerate()
            .collect::<Vec<_>>();
        // NOTE: We assume that usize is 8 or fewer bytes.
        (0..locations.len()).for_each(|i| {
            let pos = locations[i].center.map(|e| e as i64);

            loc_clone.sort_by_key(|(_, l)| l.map(|e| e as i64).distance_squared(pos));

            loc_clone.iter().skip(1).take(2).for_each(|(j, _)| {
                locations[i].neighbours.insert(*j as u64);
                locations[*j].neighbours.insert(i as u64);
            });
        });

        // Simulate invasion!
        let invasion_cycles = 25;
        (0..invasion_cycles).for_each(|_| {
            (0..grid_size.y).for_each(|j| {
                (0..grid_size.x).for_each(|i| {
                    if loc_grid[j * grid_size.x + i].is_none() {
                        const R_COORDS: [i32; 5] = [-1, 0, 1, 0, -1];
                        let idx = (self.rng.random::<u64>() % 4) as usize;
                        let new_i = i as i32 + R_COORDS[idx];
                        let new_j = j as i32 + R_COORDS[idx + 1];
                        if new_i >= 0 && new_j >= 0 {
                            let loc = Vec2::new(new_i as usize, new_j as usize);
                            loc_grid[j * grid_size.x + i] =
                                loc_grid.get(loc.y * grid_size.x + loc.x).cloned().flatten();
                        }
                    }
                });
            });
        });

        // Place the locations onto the world
        /*
        let gen = StructureGen2d::new(self.seed, cell_size as u32, cell_size as u32 / 2);

        self.chunks
            .par_iter_mut()
            .enumerate()
            .for_each(|(ij, chunk)| {
                let chunk_pos = uniform_idx_as_vec2(self.map_size_lg(), ij);
                let i = chunk_pos.x as usize;
                let j = chunk_pos.y as usize;
                let block_pos = Vec2::new(
                    chunk_pos.x * TerrainChunkSize::RECT_SIZE.x as i32,
                    chunk_pos.y * TerrainChunkSize::RECT_SIZE.y as i32,
                );
                let _cell_pos = Vec2::new(i / cell_size, j / cell_size);

                // Find the distance to each region
                let near = gen.get(chunk_pos);
                let mut near = near
                    .iter()
                    .map(|(pos, seed)| RegionInfo {
                        chunk_pos: *pos,
                        block_pos: pos
                            .map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| e * sz as i32),
                        dist: (pos - chunk_pos).map(|e| e as f32).magnitude(),
                        seed: *seed,
                    })
                    .collect::<Vec<_>>();

                // Sort regions based on distance
                near.sort_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap());

                let nearest_cell_pos = near[0].chunk_pos;
                if nearest_cell_pos.x >= 0 && nearest_cell_pos.y >= 0 {
                    let nearest_cell_pos = nearest_cell_pos.map(|e| e as usize) / cell_size;
                    chunk.location = loc_grid
                        .get(nearest_cell_pos.y * grid_size.x + nearest_cell_pos.x)
                        .cloned()
                        .unwrap_or(None)
                        .map(|loc_idx| LocationInfo { loc_idx, near });
                }
            });
        */

        // Create waypoints
        const WAYPOINT_EVERY: usize = 16;
        let this = &self;
        let waypoints = (0..this.map_size_lg().chunks().x)
            .step_by(WAYPOINT_EVERY)
            .flat_map(|i| {
                (0..this.map_size_lg().chunks().y)
                    .step_by(WAYPOINT_EVERY)
                    .map(move |j| (i, j))
            })
            .collect::<Vec<_>>()
            .into_par_iter()
            .filter_map(|(i, j)| {
                let mut pos = Vec2::new(i as i32, j as i32);
                let mut chunk = this.get(pos)?;

                if chunk.is_underwater() {
                    return None;
                }
                // Slide the waypoints down hills
                const MAX_ITERS: usize = 64;
                for _ in 0..MAX_ITERS {
                    let downhill_pos = match chunk.downhill {
                        Some(downhill) => {
                            downhill.map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| e / (sz as i32))
                        },
                        None => return Some(pos),
                    };

                    let new_chunk = this.get(downhill_pos)?;
                    const SLIDE_THRESHOLD: f32 = 5.0;
                    if new_chunk.river.near_water() || new_chunk.alt + SLIDE_THRESHOLD < chunk.alt {
                        break;
                    } else {
                        chunk = new_chunk;
                        pos = downhill_pos;
                    }
                }
                Some(pos)
            })
            .collect::<Vec<_>>();

        for waypoint in waypoints {
            self.get_mut(waypoint).map(|sc| sc.contains_waypoint = true);
        }

        self.rng = rng;
        self._locations = locations;
    }

    pub fn get(&self, chunk_pos: Vec2<i32>) -> Option<&SimChunk> {
        if chunk_pos
            .map2(self.map_size_lg().chunks(), |e, sz| e >= 0 && e < sz as i32)
            .reduce_and()
        {
            Some(&self.chunks[vec2_as_uniform_idx(self.map_size_lg(), chunk_pos)])
        } else {
            None
        }
    }

    /// Returns the exact authored cartographic ecology class at `chunk_pos`.
    /// This is intentionally map-only; terrain and tree placement continue to
    /// consume their independent physical and vegetation signals.
    pub(crate) fn authored_ecology_zone_at(
        &self,
        chunk_pos: Vec2<i32>,
    ) -> Option<AuthoredEcologyZone> {
        self.authored_ecology_zone_layer.as_ref().and_then(|layer| {
            if chunk_pos
                .map2(self.map_size_lg().chunks(), |coord, size| {
                    coord >= 0 && coord < size as i32
                })
                .reduce_and()
            {
                let chunk_index = vec2_as_uniform_idx(self.map_size_lg(), chunk_pos);
                let layer_index =
                    authored_layer_idx_for_cromatolis_v0(self.map_size_lg(), chunk_index);
                AuthoredEcologyZone::from_layer_value(layer[layer_index])
            } else {
                None
            }
        })
    }

    pub fn get_gradient_approx(&self, chunk_pos: Vec2<i32>) -> Option<f32> {
        let a = self.get(chunk_pos)?;
        if let Some(downhill) = a.downhill {
            let b = self.get(downhill.wpos_to_cpos())?;
            Some((a.alt - b.alt).abs() / TerrainChunkSize::RECT_SIZE.x as f32)
        } else {
            Some(0.0)
        }
    }

    /// Get the altitude of the surface, could be water or ground.
    pub fn get_surface_alt_approx(&self, wpos: Vec2<i32>) -> f32 {
        self.get_interpolated(wpos, |chunk| chunk.alt)
            .zip(self.get_interpolated(wpos, |chunk| chunk.water_alt))
            .map(|(alt, water_alt)| alt.max(water_alt))
            .unwrap_or(CONFIG.sea_level)
    }

    pub fn get_alt_approx(&self, wpos: Vec2<i32>) -> Option<f32> {
        self.get_interpolated(wpos, |chunk| chunk.alt)
    }

    pub fn get_wpos(&self, wpos: Vec2<i32>) -> Option<&SimChunk> {
        self.get(wpos.map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
            e.div_euclid(sz as i32)
        }))
    }

    pub fn get_mut(&mut self, chunk_pos: Vec2<i32>) -> Option<&mut SimChunk> {
        let map_size_lg = self.map_size_lg();
        if chunk_pos
            .map2(map_size_lg.chunks(), |e, sz| e >= 0 && e < sz as i32)
            .reduce_and()
        {
            Some(&mut self.chunks[vec2_as_uniform_idx(map_size_lg, chunk_pos)])
        } else {
            None
        }
    }

    pub fn get_base_z(&self, chunk_pos: Vec2<i32>) -> Option<f32> {
        let in_bounds = chunk_pos
            .map2(self.map_size_lg().chunks(), |e, sz| {
                e > 0 && e < sz as i32 - 2
            })
            .reduce_and();
        if !in_bounds {
            return None;
        }

        let chunk_idx = vec2_as_uniform_idx(self.map_size_lg(), chunk_pos);
        local_cells(self.map_size_lg(), chunk_idx)
            .flat_map(|neighbor_idx| {
                let neighbor_pos = uniform_idx_as_vec2(self.map_size_lg(), neighbor_idx);
                let neighbor_chunk = self.get(neighbor_pos);
                let river_kind = neighbor_chunk.and_then(|c| c.river.river_kind);
                let has_water = river_kind.is_some() && river_kind != Some(RiverKind::Ocean);
                if (neighbor_pos - chunk_pos).reduce_partial_max() <= 1 || has_water {
                    neighbor_chunk.map(|c| c.get_base_z())
                } else {
                    None
                }
            })
            .fold(None, |a: Option<f32>, x| a.map(|a| a.min(x)).or(Some(x)))
    }

    pub fn get_interpolated<T, F>(&self, pos: Vec2<i32>, mut f: F) -> Option<T>
    where
        T: Copy + Default + Add<Output = T> + Mul<f32, Output = T>,
        F: FnMut(&SimChunk) -> T,
    {
        let pos = pos.as_::<f64>().wpos_to_cpos();

        let cubic = |a: T, b: T, c: T, d: T, x: f32| -> T {
            let x2 = x * x;

            // Catmull-Rom splines
            let co0 = a * -0.5 + b * 1.5 + c * -1.5 + d * 0.5;
            let co1 = a + b * -2.5 + c * 2.0 + d * -0.5;
            let co2 = a * -0.5 + c * 0.5;
            let co3 = b;

            co0 * x2 * x + co1 * x2 + co2 * x + co3
        };

        let mut x = [T::default(); 4];

        for (x_idx, j) in (-1..3).enumerate() {
            let y0 = f(self.get(pos.map2(Vec2::new(j, -1), |e, q| e.max(0.0) as i32 + q))?);
            let y1 = f(self.get(pos.map2(Vec2::new(j, 0), |e, q| e.max(0.0) as i32 + q))?);
            let y2 = f(self.get(pos.map2(Vec2::new(j, 1), |e, q| e.max(0.0) as i32 + q))?);
            let y3 = f(self.get(pos.map2(Vec2::new(j, 2), |e, q| e.max(0.0) as i32 + q))?);

            x[x_idx] = cubic(y0, y1, y2, y3, pos.y.fract() as f32);
        }

        Some(cubic(x[0], x[1], x[2], x[3], pos.x.fract() as f32))
    }

    /// M. Steffen splines.
    ///
    /// A more expensive cubic interpolation function that can preserve
    /// monotonicity between points.  This is useful if you rely on relative
    /// differences between endpoints being preserved at all interior
    /// points.  For example, we use this with riverbeds (and water
    /// height on along rivers) to maintain the invariant that the rivers always
    /// flow downhill at interior points (not just endpoints), without
    /// needing to flatten out the river.
    pub fn get_interpolated_monotone<T, F>(&self, pos: Vec2<i32>, mut f: F) -> Option<T>
    where
        T: Copy + Default + Signed + Float + Add<Output = T> + Mul<f32, Output = T>,
        F: FnMut(&SimChunk) -> T,
    {
        // See http://articles.adsabs.harvard.edu/cgi-bin/nph-iarticle_query?1990A%26A...239..443S&defaultprint=YES&page_ind=0&filetype=.pdf
        //
        // Note that these are only guaranteed monotone in one dimension; fortunately,
        // that is sufficient for our purposes.
        let pos = pos.as_::<f64>().wpos_to_cpos();

        let secant = |b: T, c: T| c - b;

        let parabola = |a: T, c: T| -a * 0.5 + c * 0.5;

        let slope = |_a: T, _b: T, _c: T, s_a: T, s_b: T, p_b: T| {
            // ((b - a).signum() + (c - b).signum()) * s
            (s_a.signum() + s_b.signum()) * (s_a.abs().min(s_b.abs()).min(p_b.abs() * 0.5))
        };

        let cubic = |a: T, b: T, c: T, d: T, x: f32| -> T {
            // Compute secants.
            let s_a = secant(a, b);
            let s_b = secant(b, c);
            let s_c = secant(c, d);
            // Computing slopes from parabolas.
            let p_b = parabola(a, c);
            let p_c = parabola(b, d);
            // Get slopes (setting distance between neighbors to 1.0).
            let slope_b = slope(a, b, c, s_a, s_b, p_b);
            let slope_c = slope(b, c, d, s_b, s_c, p_c);
            let x2 = x * x;

            // Interpolating splines.
            let co0 = slope_b + slope_c - s_b * 2.0;
            // = a * -0.5 + c * 0.5 + b * -0.5 + d * 0.5 - 2 * (c - b)
            // = a * -0.5 + b * 1.5 - c * 1.5 + d * 0.5;
            let co1 = s_b * 3.0 - slope_b * 2.0 - slope_c;
            // = (3.0 * (c - b) - 2.0 * (a * -0.5 + c * 0.5) - (b * -0.5 + d * 0.5))
            // = a + b * -2.5 + c * 2.0 + d * -0.5;
            let co2 = slope_b;
            // = a * -0.5 + c * 0.5;
            let co3 = b;

            co0 * x2 * x + co1 * x2 + co2 * x + co3
        };

        let mut x = [T::default(); 4];

        for (x_idx, j) in (-1..3).enumerate() {
            let y0 = f(self.get(pos.map2(Vec2::new(j, -1), |e, q| e.max(0.0) as i32 + q))?);
            let y1 = f(self.get(pos.map2(Vec2::new(j, 0), |e, q| e.max(0.0) as i32 + q))?);
            let y2 = f(self.get(pos.map2(Vec2::new(j, 1), |e, q| e.max(0.0) as i32 + q))?);
            let y3 = f(self.get(pos.map2(Vec2::new(j, 2), |e, q| e.max(0.0) as i32 + q))?);

            x[x_idx] = cubic(y0, y1, y2, y3, pos.y.fract() as f32);
        }

        Some(cubic(x[0], x[1], x[2], x[3], pos.x.fract() as f32))
    }

    /// Bilinear interpolation.
    ///
    /// Linear interpolation in both directions (i.e. quadratic interpolation).
    pub fn get_interpolated_bilinear<T, F>(&self, pos: Vec2<i32>, mut f: F) -> Option<T>
    where
        T: Copy + Default + Signed + Float + Add<Output = T> + Mul<f32, Output = T>,
        F: FnMut(&SimChunk) -> T,
    {
        // (i) Find downhill for all four points.
        // (ii) Compute distance from each downhill point and do linear interpolation on
        // their heights. (iii) Compute distance between each neighboring point
        // and do linear interpolation on       their distance-interpolated
        // heights.

        // See http://articles.adsabs.harvard.edu/cgi-bin/nph-iarticle_query?1990A%26A...239..443S&defaultprint=YES&page_ind=0&filetype=.pdf
        //
        // Note that these are only guaranteed monotone in one dimension; fortunately,
        // that is sufficient for our purposes.
        let pos = pos.as_::<f64>().wpos_to_cpos();

        // Orient the chunk in the direction of the most downhill point of the four.  If
        // there is no "most downhill" point, then we don't care.
        let x0 = pos.map2(Vec2::new(0, 0), |e, q| e.max(0.0) as i32 + q);
        let p0 = self.get(x0)?;
        let y0 = f(p0);

        let x1 = pos.map2(Vec2::new(1, 0), |e, q| e.max(0.0) as i32 + q);
        let p1 = self.get(x1)?;
        let y1 = f(p1);

        let x2 = pos.map2(Vec2::new(0, 1), |e, q| e.max(0.0) as i32 + q);
        let p2 = self.get(x2)?;
        let y2 = f(p2);

        let x3 = pos.map2(Vec2::new(1, 1), |e, q| e.max(0.0) as i32 + q);
        let p3 = self.get(x3)?;
        let y3 = f(p3);

        let z0 = y0
            .mul(1.0 - pos.x.fract() as f32)
            .mul(1.0 - pos.y.fract() as f32);
        let z1 = y1.mul(pos.x.fract() as f32).mul(1.0 - pos.y.fract() as f32);
        let z2 = y2.mul(1.0 - pos.x.fract() as f32).mul(pos.y.fract() as f32);
        let z3 = y3.mul(pos.x.fract() as f32).mul(pos.y.fract() as f32);

        Some(z0 + z1 + z2 + z3)
    }

    pub fn get_nearest_ways<'a, M: Clone + Lerp<Output = M>>(
        &'a self,
        wpos: Vec2<i32>,
        get_way: &'a impl Fn(&SimChunk) -> Option<(Way, M)>,
    ) -> impl Iterator<Item = NearestWaysData<M, impl FnOnce() -> Vec2<f32>>> + 'a {
        let chunk_pos = wpos.map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
            e.div_euclid(sz as i32)
        });
        let get_chunk_centre = |chunk_pos: Vec2<i32>| {
            chunk_pos.map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
                e * sz as i32 + sz as i32 / 2
            })
        };

        LOCALITY
            .iter()
            .filter_map(move |ctrl| {
                let (way, meta) = get_way(self.get(chunk_pos + *ctrl)?)?;
                let ctrl_pos = get_chunk_centre(chunk_pos + *ctrl).map(|e| e as f32)
                    + way.offset.map(|e| e as f32);

                let chunk_connections = way.neighbors.count_ones();
                if chunk_connections == 0 {
                    return None;
                }

                let (start_pos, start_idx, start_meta) = if chunk_connections != 2 {
                    (ctrl_pos, None, meta.clone())
                } else {
                    let (start_idx, start_rpos) = NEIGHBORS
                        .iter()
                        .copied()
                        .enumerate()
                        .find(|(i, _)| way.neighbors & (1 << *i as u8) != 0)
                        .unwrap();
                    let start_pos_chunk = chunk_pos + *ctrl + start_rpos;
                    let (start_way, start_meta) = get_way(self.get(start_pos_chunk)?)?;
                    (
                        get_chunk_centre(start_pos_chunk).map(|e| e as f32)
                            + start_way.offset.map(|e| e as f32),
                        Some(start_idx),
                        start_meta,
                    )
                };

                Some(
                    NEIGHBORS
                        .iter()
                        .enumerate()
                        .filter(move |(i, _)| {
                            way.neighbors & (1 << *i as u8) != 0 && Some(*i) != start_idx
                        })
                        .filter_map(move |(i, end_rpos)| {
                            let end_pos_chunk = chunk_pos + *ctrl + end_rpos;
                            let (end_way, end_meta) = get_way(self.get(end_pos_chunk)?)?;
                            let end_pos = get_chunk_centre(end_pos_chunk).map(|e| e as f32)
                                + end_way.offset.map(|e| e as f32);

                            let bez = QuadraticBezier2 {
                                start: (start_pos + ctrl_pos) / 2.0,
                                ctrl: ctrl_pos,
                                end: (end_pos + ctrl_pos) / 2.0,
                            };
                            let nearest_interval = bez
                                .binary_search_point_by_steps(wpos.map(|e| e as f32), 16, 0.001)
                                .0
                                .clamped(0.0, 1.0);
                            let pos = bez.evaluate(nearest_interval);
                            let dist_sqrd = pos.distance_squared(wpos.map(|e| e as f32));
                            let meta = if nearest_interval < 0.5 {
                                Lerp::lerp(start_meta.clone(), meta.clone(), 0.5 + nearest_interval)
                            } else {
                                Lerp::lerp(meta.clone(), end_meta, nearest_interval - 0.5)
                            };
                            Some(NearestWaysData {
                                i,
                                dist_sqrd,
                                pos,
                                meta,
                                bezier: bez,
                                calc_tangent: move || {
                                    bez.evaluate_derivative(nearest_interval).normalized()
                                },
                            })
                        }),
                )
            })
            .flatten()
    }

    /// Return the distance to the nearest way in blocks, along with the
    /// closest point on the way, the way metadata, and the tangent vector
    /// of that way.
    pub fn get_nearest_way<M: Clone + Lerp<Output = M>>(
        &self,
        wpos: Vec2<i32>,
        get_way: impl Fn(&SimChunk) -> Option<(Way, M)>,
    ) -> Option<(f32, Vec2<f32>, M, Vec2<f32>)> {
        let get_way = &get_way;
        self.get_nearest_ways(wpos, get_way)
            .min_by_key(|NearestWaysData { dist_sqrd, .. }| (dist_sqrd * 1024.0) as i32)
            .map(
                |NearestWaysData {
                     dist_sqrd,
                     pos,
                     meta,
                     calc_tangent,
                     ..
                 }| (dist_sqrd.sqrt(), pos, meta, calc_tangent()),
            )
    }

    pub fn get_nearest_path(&self, wpos: Vec2<i32>) -> Option<(f32, Vec2<f32>, Path, Vec2<f32>)> {
        self.get_nearest_way(wpos, |chunk| Some(chunk.path))
    }

    /// Spiral outward from `chunk_pos` to find the nearest chunk whose
    /// [`Spot`] satisfies `predicate`, returning that chunk's position.
    pub fn get_nearest_spot(
        &self,
        chunk_pos: Vec2<i32>,
        predicate: impl Fn(&Spot) -> bool,
    ) -> Option<Vec2<i32>> {
        // The spiral below is bounded only by the map area, so it terminates
        // early *only* because it finds something. In a world whose region
        // disables the spot layer there is no `SimChunk::spot` anywhere, so
        // every call would walk all ~1M chunk positions -- one random index
        // into the chunk `Vec`, i.e. a cache miss, per probe -- and this runs
        // on the rtsim NPC-AI tick (courier-quest rolls), not at startup.
        // Answer immediately instead.
        //
        // `None` for a procedural world, so upstream behaviour is unchanged.
        if self
            .authored_procedural_layers
            .is_some_and(|layers| !layers.spots)
        {
            return None;
        }

        Spiral2d::new()
            .map(|o| chunk_pos + o)
            .take(self.map_size_lg().chunks_len())
            .find(|cpos| {
                self.get(*cpos)
                    .and_then(|c| c.spot.as_ref())
                    .is_some_and(&predicate)
            })
    }

    /// Create a [`Lottery<Option<ForestKind>>`] that generates [`ForestKind`]s
    /// according to the conditions at the given position. If no or fewer
    /// trees are appropriate for the conditions, `None` may be generated.
    pub fn make_forest_lottery(&self, wpos: Vec2<i32>) -> Lottery<Option<ForestKind>> {
        let chunk = if let Some(chunk) = self.get_wpos(wpos) {
            chunk
        } else {
            return Lottery::from(vec![(1.0, None)]);
        };
        make_forest_lottery_for_env(wpos, chunk.get_environment())
    }

    /// WARNING: Not currently used by the tree layer. Needs to be reworked.
    /// Return an iterator over candidate tree positions (note that only some of
    /// these will become trees since environmental parameters may forbid
    /// them spawning).
    pub fn get_near_trees(&self, wpos: Vec2<i32>) -> impl Iterator<Item = TreeAttr> + '_ {
        // Deterministic based on wpos
        self.gen_ctx
            .structure_gen
            .get(wpos)
            .into_iter()
            .filter_map(move |(wpos, seed)| {
                let lottery = self.make_forest_lottery(wpos);
                Some(TreeAttr {
                    pos: wpos,
                    seed,
                    scale: 1.0,
                    forest_kind: *lottery.choose_seeded(seed).as_ref()?,
                    inhabited: false,
                })
            })
    }

    pub fn get_area_trees(
        &self,
        wpos_min: Vec2<i32>,
        wpos_max: Vec2<i32>,
    ) -> impl Iterator<Item = TreeAttr> + '_ {
        self.tree_candidate_fields_in_area(wpos_min, wpos_max)
            .into_iter()
            .filter_map(move |(wpos, seed)| {
                let lottery = self.make_forest_lottery(wpos);
                Some(TreeAttr {
                    pos: wpos,
                    seed,
                    scale: 1.0,
                    forest_kind: *lottery.choose_seeded(seed).as_ref()?,
                    inhabited: false,
                })
            })
    }
}

#[derive(Clone, Debug)]
pub struct SimChunk {
    pub(crate) authored_cromatolis_v0: bool,
    /// Stable id of the specific authored region this chunk belongs to, if
    /// any. See `GenCdf::authored_region_id`.
    pub(crate) authored_region_id: Option<&'static str>,
    /// Whether this chunk is land adjacent to authored standing/flowing
    /// water (real Cromatolis water/elevated-lakes/river-channels masks from
    /// COW-3, not humidity/altitude heuristics). Always `false` outside an
    /// authored region. Consumed by `get_biome`'s `Swamp` branch.
    pub(crate) authored_near_water: bool,
    /// What kind of water body this chunk ecologically *is*, if it is water at
    /// all (COW-22 `C22-1b`). `None` for dry land and for every chunk outside
    /// an authored region.
    ///
    /// Deliberately separate from `river.river_kind`, which answers the
    /// *physical* question (how the chunk is carved, and where its water level
    /// comes from) and is upstream Veloren code shared with the procedural
    /// world. The two are allowed to disagree -- see [`WaterBodyKind`].
    pub(crate) water_body: Option<WaterBodyKind>,
    /// How salty this chunk's water is (COW-22 `C22-4`), if it is water at all.
    /// `None` exactly where `water_body` is `None`.
    ///
    /// Type and salinity are deliberately separate axes rather than one fused
    /// enum, because a body can be a river *and* salty; fusing them would mean
    /// mirroring a `SaltRiver`-style variant of [`WaterBodyKind`] for every
    /// kind that can be salty. Derived from the topology of the classified
    /// bodies -- see [`derive_salinity`] -- never from an authored raster.
    ///
    /// A field rather than a side table on `WorldSim` because the eventual
    /// consumers are the `SPAWN_RULES` closures in `layer::wildlife`, typed
    /// `|&SimChunk, &ColumnSample|` -- they have no handle on `WorldSim` to
    /// look anything up in.
    ///
    /// Free, today: `Option<Salinity>` is one byte and lands in what was
    /// already `SimChunk`'s end padding, so the struct stays 224 bytes and the
    /// 1,048,576-chunk map costs nothing extra. One padding byte is left after
    /// it; the field after *that* one takes `SimChunk` to 232 bytes, i.e. 8 MB.
    ///
    /// Carried but not yet consumed: the aquatic-ecology asset that selects a
    /// fauna/flora profile by water kind *and* salinity is COW-22's `C22-5`,
    /// which also replaces the two Cromatolis wildlife manifest entries that
    /// currently gate on `water_body` alone. `expect` rather than `allow` so
    /// the attribute cannot outlive the first real read, and `not(test)`
    /// because the regressions below do read the field, which would
    /// otherwise leave the expectation unfulfilled in the test build.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) salinity: Option<Salinity>,
    /// Compact post-generation fact. The policy remains owned by `WorldSim`.
    pub(crate) authored_alpine_snowland: bool,
    pub chaos: f32,
    pub alt: f32,
    pub basement: f32,
    pub water_alt: f32,
    pub downhill: Option<Vec2<i32>>,
    pub flux: f32,
    pub temp: f32,
    pub humidity: f32,
    pub rockiness: f32,
    pub tree_density: f32,
    /// Continuous authored visible-green-ground-cover signal. This is
    /// independent of `tree_density`; physical terrain decides whether it
    /// can be rendered at a particular column.
    pub ground_cover: f32,
    /// Explicit categorical surface exception, if the authored zone resolver
    /// selects one for this chunk. This deliberately does not affect trees.
    pub(crate) ground_substrate: Option<GroundSubstrate>,
    pub forest_kind: ForestKind,
    pub spawn_rate: f32,
    pub river: RiverData,
    pub surface_veg: f32,

    pub sites: Vec<Id<Site>>,
    pub place: Option<Id<Place>>,
    pub poi: Option<Id<PointOfInterest>>,

    pub path: (Way, Path),
    pub cliff_height: f32,
    pub spot: Option<Spot>,

    pub contains_waypoint: bool,
}

#[derive(Copy, Clone)]
pub struct RegionInfo {
    pub chunk_pos: Vec2<i32>,
    pub block_pos: Vec2<i32>,
    pub dist: f32,
    pub seed: u32,
}

pub struct NearestWaysData<M, F: FnOnce() -> Vec2<f32>> {
    pub i: usize,
    pub dist_sqrd: f32,
    pub pos: Vec2<f32>,
    pub meta: M,
    pub bezier: QuadraticBezier2<f32>,
    pub calc_tangent: F,
}

fn authored_layer_idx_for_cromatolis_v0(map_size_lg: MapSizeLg, posi: usize) -> usize {
    let width = usize::from(map_size_lg.chunks().x);
    let height = usize::from(map_size_lg.chunks().y);
    let x = posi % width;
    let y = posi / width;
    (height - 1 - y) * width + x
}

fn authored_layer_value_for_cromatolis_v0(
    map_size_lg: MapSizeLg,
    posi: usize,
    layer: &[f32],
) -> f32 {
    let layer_idx = authored_layer_idx_for_cromatolis_v0(map_size_lg, posi);
    layer
        .get(layer_idx)
        .copied()
        .unwrap_or_default()
        .clamp(0.0, 1.0)
}

/// Whether one authored binary mask claims a chunk. `false` for a layer that
/// failed to load, which is how every authored-layer consumer degrades.
///
/// A named function rather than a closure per call site so the threshold test
/// is spelled once: the water-body sweep and the salinity derivation both ask
/// it of the same `elevated_lakes` layer, and two spellings of one predicate
/// can drift apart.
fn authored_mask_hit(map_size_lg: MapSizeLg, layer: &Option<Box<[f32]>>, idx: usize) -> bool {
    layer.as_ref().is_some_and(|values| {
        authored_layer_value_for_cromatolis_v0(map_size_lg, idx, values) >= AUTHORED_WATER_THRESHOLD
    })
}

/// Applies Cromatolis's authored biome-mask contract to a sampled mask value.
///
/// The grayscale is a direct vegetation-density scale: black is `0.0` (no
/// vegetation) and white is `1.0` (maximum vegetation). Intermediate grays
/// are linear authoring values, so a 50% gray reduction must remain 0.50 in
/// `tree_density`; do not add response curves or density floors here.
///
/// Water, a temperature below the regional threshold, and the extreme
/// high-altitude cap are physical exclusions rather than reinterpretations
/// of the painted scale.
fn cromatolis_authored_tree_density(
    painted_density: f32,
    is_underwater: bool,
    temp: f32,
    alt_pre: f32,
    climate: &ResolvedCromatolisClimate,
    alpine_policy: Option<AuthoredAlpinePolicy>,
) -> f32 {
    let tree_line = alpine_policy.map_or(climate.max_tree_altitude_m, |p| p.tree_line_altitude_m);
    if is_underwater || temp < climate.tree_min_temp || alt_pre >= tree_line {
        0.0
    } else {
        painted_density
    }
}

/// Decides whether a chunk's `water_alt` should be forced to sea level
/// (`true`) rather than the erosion sim's own `fill_sinks` height (`false`),
/// for the basin identified by `dh_lake_idx`/`lake_is_authored_elevated`.
///
/// For a non-Cromatolis (procedural) world this must reduce to exactly
/// `dh_lake_idx < 0` (the original, upstream Veloren behavior: sea level
/// only for a boundary node or a lake draining straight to the ocean). The
/// `authored_cromatolis_v0` check on the right-hand side of the `||` is
/// load-bearing, not redundant with the one already gating
/// `lake_is_authored_elevated`'s own definition: on a non-Cromatolis world
/// `lake_is_authored_elevated` is trivially `false`, so without this second
/// check `!lake_is_authored_elevated` alone would force every procedural
/// lake to sea level too. Do not simplify this away.
///
/// On an authored Cromatolis map, every basin still defaults to sea level
/// (the hand-authored heightmap has many small unmarked dips that would
/// otherwise all become spurious procedural lakes) - the one exception is a
/// basin the `elevated_lakes` mask (COW-3) marks as an authored elevated
/// lake AND the erosion sim also found genuinely closed
/// (`dh_lake_idx >= 0`); a marked-but-open-draining basin has no real
/// elevated pass height to use either, so it still falls back to sea level.
fn cromatolis_forces_sea_level(
    authored_cromatolis_v0: bool,
    dh_lake_idx: isize,
    lake_is_authored_elevated: bool,
) -> bool {
    dh_lake_idx < 0 || (authored_cromatolis_v0 && !lake_is_authored_elevated)
}

#[cfg(test)]
mod cromatolis_forces_sea_level_tests {
    use super::cromatolis_forces_sea_level;

    #[test]
    fn procedural_world_matches_upstream_veloren_behavior() {
        // authored_cromatolis_v0=false must reduce to exactly `dh_lake_idx < 0`,
        // regardless of `lake_is_authored_elevated` (which can't be true here
        // in practice, but the function must still be safe if it were).
        assert!(cromatolis_forces_sea_level(false, -1, false));
        assert!(cromatolis_forces_sea_level(false, -1, true));
        assert!(!cromatolis_forces_sea_level(false, 0, false));
        assert!(!cromatolis_forces_sea_level(false, 0, true));
    }

    #[test]
    fn cromatolis_unmarked_basin_still_falls_back_to_sea_level() {
        // A closed basin (dh >= 0) that the elevated_lakes mask never
        // flagged - the "many small unmarked dips" case the sea-level
        // default exists to suppress.
        assert!(cromatolis_forces_sea_level(true, 5, false));
    }

    #[test]
    fn cromatolis_marked_but_open_draining_basin_still_falls_back_to_sea_level() {
        // A basin the mask marks as an elevated lake, but the erosion sim
        // still found open-draining to a boundary/the ocean - no real
        // elevated pass height exists, matches the documented real-world
        // canyon case (pixel (30816,640) / chunk (1926,40)) before it's
        // re-terraformed into a genuinely closed basin.
        assert!(cromatolis_forces_sea_level(true, -1, true));
    }

    #[test]
    fn cromatolis_marked_and_closed_basin_uses_the_real_fill_sinks_height() {
        // The actual fix: a basin that's both mask-marked AND genuinely
        // closed per the erosion sim is the one case that should NOT force
        // sea level, letting the real fill_sinks lake-bottom-fill value
        // through instead.
        assert!(!cromatolis_forces_sea_level(true, 5, true));
    }
}

fn authored_route_way(map_size_lg: MapSizeLg, posi: usize, routes: &[f32]) -> Option<Way> {
    const ROUTE_THRESHOLD: f32 = 0.62;
    const NEIGHBOR_ROUTE_THRESHOLD: f32 = 0.50;

    if authored_layer_value_for_cromatolis_v0(map_size_lg, posi, routes) < ROUTE_THRESHOLD {
        return None;
    }

    let pos = uniform_idx_as_vec2(map_size_lg, posi);
    let world_size = map_size_lg.chunks();
    let mut way = Way::default();

    for (idx, neighbor) in NEIGHBORS.iter().enumerate() {
        let neighbor_pos = pos + *neighbor;
        if neighbor_pos.x < 0
            || neighbor_pos.y < 0
            || neighbor_pos.x >= i32::from(world_size.x)
            || neighbor_pos.y >= i32::from(world_size.y)
        {
            continue;
        }

        let neighbor_idx = vec2_as_uniform_idx(map_size_lg, neighbor_pos);
        if authored_layer_value_for_cromatolis_v0(map_size_lg, neighbor_idx, routes)
            >= NEIGHBOR_ROUTE_THRESHOLD
        {
            way.neighbors |= 1 << idx as u8;
        }
    }

    way.is_way().then_some(way)
}

/// Ecological classification of an authored water chunk.
///
/// This is deliberately *parallel* to `RiverKind` rather than an extension of
/// it. `RiverKind` is upstream Veloren code consumed across `column.rs`,
/// `map.rs`, `civ/mod.rs`, `site/mod.rs`, `lib.rs` and all of `voxygen/`, and
/// it answers a *physical* question (how is this chunk carved, and where does
/// its water level come from). Widening it to carry ecology would widen the
/// upstream-merge surface forever for no benefit here, so the two stay
/// separate: `RiverKind` keeps deciding carving and water level, and
/// `WaterBodyKind` says what kind of water body a chunk ecologically *is*.
///
/// The two can legitimately disagree. A Cromatolis river corridor wider than
/// `CONFIG.river_max_width` stays `RiverKind::Lake` physically (carving a
/// wider-than-max river would leave water walls, see `erosion.rs`'s
/// `max_width` handling) while still being `WaterBodyKind::River`
/// ecologically.
///
/// Scoped `pub(crate)` for now: every consumer this row and the rest of COW-22
/// add lives inside `world` (`sim`, `layer`). Widen it the day something in
/// `server`/`voxygen` needs it -- that is a one-line change, where narrowing it
/// again would not be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaterBodyKind {
    /// Open marine water.
    Ocean,
    /// Marine water over the near-shore shelf. Not produced yet: nothing
    /// computes the offshore-distance/bathymetry band that separates `Sea`
    /// from `Ocean`. COW-22's `C22-3` reshaped the seabed either side of the
    /// waterline -- a shelf now exists in the terrain -- but derived no
    /// engine-side classification from it, so whoever does has only to compute
    /// the band and flip `AuthoredWaterBodyInputs::is_shelf_sea`, not re-shape
    /// everything that reads this enum.
    ///
    /// If the shelf classification is ever descoped, this variant and
    /// `AuthoredWaterBodyInputs::is_shelf_sea` come out with it -- they exist
    /// only as its landing seam, and
    /// `cromatolis_water_body_histogram_regression_against_real_lfs_assets`
    /// asserts the count is still zero precisely so the decision has to be
    /// made rather than drifting.
    Sea,
    /// Flowing water: an authored river corridor.
    River,
    /// Inland standing water not adjacent to marine water.
    Lake,
    /// Inland standing water adjacent to marine water.
    Lagoon,
    // NOTE: COW-22's taxonomy also names a `Swamp` case -- standing water
    // inside a swamp -- which this enum deliberately does *not* carry yet,
    // because nothing can produce it. The rule it specifies is "standing water
    // whose `get_biome` resolves to `BiomeKind::Swamp`", and `get_biome`
    // answers `Ocean`/`Lake` for any chunk that *is* water (its second branch)
    // long before its `Swamp` branch is reached, so that promotion is
    // unreachable by construction rather than merely unused on today's data.
    // Deriving it instead from the inputs that branch uses
    // (`authored_near_water` and `SWAMP_HUMIDITY_THRESHOLD`) *would* fire, but
    // it would reclassify real lakes and break the exporter reconciliation
    // `C22-1b` is measured against -- so it belongs to a row that can
    // re-measure the histogram, which is also the row that should add the
    // variant.
}

/// Inputs to [`authored_water_body_kind`], grouped into a struct rather than
/// several positional `bool` parameters (same shape as
/// [`AuthoredRiverKindInputs`]).
struct AuthoredWaterBodyInputs {
    is_elevated_lake: bool,
    is_water_body: bool,
    is_river_channel: bool,
    /// `water` mask ∧ the `get_oceans` border flood fill -- i.e. this chunk's
    /// water is connected to the sea.
    is_marine: bool,
    /// 8-adjacent to a marine chunk (`is_marine` above), which is what
    /// separates a lagoon from a landlocked lake.
    is_adjacent_to_marine: bool,
    /// Whether this marine chunk sits on the near-shore shelf rather than in
    /// open ocean. Always `false` today -- see [`WaterBodyKind::Sea`].
    is_shelf_sea: bool,
}

/// Classifies one authored water chunk into a [`WaterBodyKind`] from its
/// (already mask-value-thresholded) authored flags.
///
/// Priority, highest first:
/// 1. `elevated_lakes` -- an authored decree that bypasses the shape test
///    entirely (it is how the map says "standing water above sea level here",
///    including the two cells that sit outside the `water` mask).
/// 2. `river_channels` -- the authored decision about what is a river,
///    unconditional. Every corridor cell is also inside the broader `water`
///    mask, so this has to outrank it or no chunk is ever a river.
/// 3. marine water -- `water` ∧ the `get_oceans` flood fill.
/// 4. remaining standing water -- `Lagoon` when it touches marine water, `Lake`
///    otherwise.
///
/// COW-22's taxonomy also names a `Swamp` case, which this does not produce --
/// see the note at the end of [`WaterBodyKind`] for why, and for what a later
/// row would have to do to change that.
fn authored_water_body_kind(inputs: AuthoredWaterBodyInputs) -> Option<WaterBodyKind> {
    let AuthoredWaterBodyInputs {
        is_elevated_lake,
        is_water_body,
        is_river_channel,
        is_marine,
        is_adjacent_to_marine,
        is_shelf_sea,
    } = inputs;

    if is_elevated_lake {
        Some(WaterBodyKind::Lake)
    } else if is_river_channel {
        Some(WaterBodyKind::River)
    } else if is_water_body && is_marine {
        Some(if is_shelf_sea {
            WaterBodyKind::Sea
        } else {
            WaterBodyKind::Ocean
        })
    } else if is_water_body {
        Some(if is_adjacent_to_marine {
            WaterBodyKind::Lagoon
        } else {
            WaterBodyKind::Lake
        })
    } else {
        None
    }
}

/// Resolves [`WaterBodyKind::Lagoon`] from a per-chunk rim marking into a
/// per-*basin* one.
///
/// Whether standing water is a lagoon or a landlocked lake is a property of the
/// body, not of the individual chunk: the middle of a lagoon is no less lagoon
/// for sitting a few chunks away from the sea, and the rim of a landlocked lake
/// does not become one by touching a river mouth. [`authored_water_body_kind`]
/// only sees one chunk's own 3x3 neighbourhood, so it can only mark the rim;
/// this pass floods each basin (8-connectivity, the same neighbourhood) and
/// makes the whole basin agree with its rim.
///
/// Chunks the authored `elevated_lakes` mask claims are excluded from the flood
/// fill entirely, not merely exempted from the promotion at the end: that mask
/// is a decree about standing water *above sea level*, so such a chunk is
/// neither a lagoon itself nor a valid bridge between a coastal rim and an
/// inland basin behind it.
fn promote_lagoon_basins(
    map_size_lg: MapSizeLg,
    water_body: &mut [Option<WaterBodyKind>],
    is_elevated_lake: impl Fn(usize) -> bool,
) {
    let is_standing = |idx: usize, kind: Option<WaterBodyKind>| {
        matches!(kind, Some(WaterBodyKind::Lake | WaterBodyKind::Lagoon)) && !is_elevated_lake(idx)
    };
    let chunks = map_size_lg.chunks().map(i32::from);
    let mut visited = vec![false; water_body.len()];
    let mut basin = Vec::new();
    let mut stack = Vec::new();

    for start in 0..water_body.len() {
        if visited[start] || !is_standing(start, water_body[start]) {
            continue;
        }
        basin.clear();
        stack.clear();
        stack.push(start);
        visited[start] = true;
        let mut basin_touches_marine = false;

        while let Some(idx) = stack.pop() {
            basin_touches_marine |= water_body[idx] == Some(WaterBodyKind::Lagoon);
            basin.push(idx);
            let pos = uniform_idx_as_vec2(map_size_lg, idx);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let neighbor = pos + Vec2::new(dx, dy);
                    if neighbor.x < 0
                        || neighbor.y < 0
                        || neighbor.x >= chunks.x
                        || neighbor.y >= chunks.y
                    {
                        continue;
                    }
                    let nidx = vec2_as_uniform_idx(map_size_lg, neighbor);
                    if !visited[nidx] && is_standing(nidx, water_body[nidx]) {
                        visited[nidx] = true;
                        stack.push(nidx);
                    }
                }
            }
        }

        for &idx in &basin {
            water_body[idx] = Some(if basin_touches_marine {
                WaterBodyKind::Lagoon
            } else {
                WaterBodyKind::Lake
            });
        }
    }
}

/// How salty a water chunk's water is.
///
/// A second classification axis, orthogonal to [`WaterBodyKind`]: every
/// combination of the two is legal, including the salt *river* that both rises
/// in and returns to the sea. Fusing the two into one enum would mean mirroring
/// a `SaltRiver`-style variant for every kind that can be salty, so they stay
/// apart -- one answers "what body is this", the other "what is in the water".
///
/// Scoped `pub(crate)` for the same reason [`WaterBodyKind`] is: every consumer
/// lives inside `world` today, and widening it later is a one-line change where
/// narrowing it again would not be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Salinity {
    /// Inland water: a river above its estuary, and standing water with a way
    /// out.
    Fresh,
    /// Mixing water: the estuary reach of a river mouth, and a lagoon fed by
    /// both the sea and a freshwater channel.
    Brackish,
    /// The sea, and the water that behaves like it: the seaward end of an
    /// estuary, a closed basin at or below sea level, and a river whose own
    /// source is marine.
    Saline,
}

/// Chunks from a river's marine mouth over which its water is still fully
/// [`Salinity::Saline`], and the further reach over which it is
/// [`Salinity::Brackish`] before turning fresh.
///
/// A judgement call rather than a measured constant, so the reasoning matters
/// more than the exact figures. A real estuary's salt wedge runs for
/// kilometres, which at this map's 32 m chunks would be a hundred chunks or
/// more -- a large fraction of an average Cromatolis river -- and the map would
/// then read as "most river water is salty" rather than "rivers meet the sea at
/// an estuary". These two keep the gradient where it is legible and where it
/// changes what lives there: roughly 96 m of fully marine water at the mouth,
/// then roughly 256 m of mixing, then fresh. Roughly, because the distance is
/// counted in 8-connected steps along the channel, so a diagonal reach is the
/// same number of chunks over √2 times the ground.
///
/// What they buy on the shipped raster, so the number is not a surprise later:
/// 15,134 fresh / 5,471 brackish / 12,961 salt corridor chunks. That "most
/// river water is not fresh after all" is not the gradient overreaching -- it
/// is Cromatolis's rivers being wide and their deltas meeting the sea along a
/// long waterline, so the mouth seeds are many rather than deep. Worth
/// revisiting with the aquatic-ecology profiles that will select on the result
/// (COW-22 `C22-5`), and `cromatolis_salinity_histogram_regression_against_
/// real_lfs_assets` is the test to re-measure when either constant moves.
///
/// Authored tuning for a region belongs in that region's RON, the way
/// [`AuthoredCromatolisClimate`] does, and these two are authored tuning: the
/// person who wants to move "where does brackish start" is the same person
/// editing the aquatic-ecology profiles that select on the result. They stay
/// here only because there is no hydrology asset to put them in yet and
/// inventing one for two integers -- with its own loader, schema and
/// whole-asset failure mode -- costs more than it buys. The aquatic-ecology
/// asset (COW-22 `C22-5`) is the right home: express them there in *metres*
/// (96 m and 256 m) and divide by `TerrainChunkSize::RECT_SIZE` here, so the
/// authored number survives a chunk-size change, and have `derive_salinity`
/// take them as parameters.
const RIVER_SALINE_MOUTH_CHUNKS: u32 = 3;
/// See [`RIVER_SALINE_MOUTH_CHUNKS`].
const RIVER_BRACKISH_MOUTH_CHUNKS: u32 = 8;

/// Sentinel for "this chunk belongs to no river component" in the label map
/// [`label_river_components`] returns. A raw `u32` rather than `Option<u32>`
/// because the map has one entry per chunk and `Option<u32>` is eight bytes
/// wide, not four.
const NO_RIVER_COMPONENT: u32 = u32::MAX;

/// One 8-connected component of the authored river network, reduced to the
/// facts salinity needs from it.
#[derive(Clone, Debug, PartialEq)]
struct RiverComponent {
    /// Corridor chunks in the component.
    len: usize,
    /// The component's highest chunk: its source.
    source: usize,
    /// `alt` at [`RiverComponent::source`], in metres.
    source_alt: f32,
    /// Whether the component never rises out of the sea: its highest chunk is
    /// at or below sea level *and* it reaches marine water. Such a river has no
    /// freshwater head to dilute anything, so it is salt along its whole length
    /// however far inland it then runs.
    ///
    /// Deliberately not "the source chunk touches the sea": on this map most
    /// corridor components are a handful of chunks long and sit on the coast,
    /// so their highest chunk is marine-adjacent by accident of being short.
    /// That reading marked 362 of 434 components salt-sourced -- the opposite
    /// of the rare exception this is meant to capture.
    source_is_marine: bool,
    /// Chunks of the component adjacent to marine water: its mouths, and the
    /// seeds the estuary gradient is measured from. Empty for a system that
    /// never reaches the sea.
    mouths: Vec<usize>,
}

/// Labels the 8-connected components of the authored river network, and
/// summarises each one into a [`RiverComponent`].
///
/// One pass over the map, not a search per chunk: the per-chunk questions
/// salinity asks ("how far is my mouth", "did my river start in the sea") are
/// properties of the whole component, and answering them per chunk would be
/// quadratic.
///
/// Returns the per-chunk component label ([`NO_RIVER_COMPONENT`] for every
/// chunk that is not a river) alongside the components themselves, indexed by
/// that label.
fn label_river_components(
    map_size_lg: MapSizeLg,
    is_river: impl Fn(usize) -> bool,
    is_marine: impl Fn(usize) -> bool,
    alt: &[Alt],
) -> (Box<[u32]>, Vec<RiverComponent>) {
    let len = map_size_lg.chunks_len();
    let mut label = vec![NO_RIVER_COMPONENT; len].into_boxed_slice();
    let mut components: Vec<RiverComponent> = Vec::new();
    let mut stack = Vec::new();

    for start in 0..len {
        if label[start] != NO_RIVER_COMPONENT || !is_river(start) {
            continue;
        }
        let id = components.len() as u32;
        label[start] = id;
        stack.clear();
        stack.push(start);
        let mut component = RiverComponent {
            len: 0,
            source: start,
            source_alt: f32::NEG_INFINITY,
            source_is_marine: false,
            mouths: Vec::new(),
        };

        while let Some(idx) = stack.pop() {
            component.len += 1;
            let mut adjacent_to_marine = false;
            for nidx in neighbors(map_size_lg, idx) {
                adjacent_to_marine |= is_marine(nidx);
                if label[nidx] == NO_RIVER_COMPONENT && is_river(nidx) {
                    label[nidx] = id;
                    stack.push(nidx);
                }
            }
            if adjacent_to_marine {
                component.mouths.push(idx);
            }
            let chunk_alt = alt[idx] as f32;
            if chunk_alt > component.source_alt {
                component.source = idx;
                component.source_alt = chunk_alt;
            }
        }

        component.source_is_marine = component.source_alt <= 0.0 && !component.mouths.is_empty();
        components.push(component);
    }

    (label, components)
}

/// Salinity of one river chunk from its distance, in chunks along the channel,
/// to the nearest marine mouth of its own component.
///
/// [`u32::MAX`] means "no mouth in this component at all", which grades to
/// fresh like any other faraway chunk.
fn river_salinity_from_mouth_distance(chunks_from_mouth: u32) -> Salinity {
    if chunks_from_mouth <= RIVER_SALINE_MOUTH_CHUNKS {
        Salinity::Saline
    } else if chunks_from_mouth <= RIVER_BRACKISH_MOUTH_CHUNKS {
        Salinity::Brackish
    } else {
        Salinity::Fresh
    }
}

/// Derives a [`Salinity`] for every chunk an already-classified
/// `water_body` map calls water.
///
/// Salinity is never authored. A per-chunk salinity raster would need a new
/// asset, would have to be repainted every time the river or water mask moved,
/// and would encode a fact the topology already determines, so this derives it
/// instead, from the same masks that produced `water_body`:
///
/// - Marine water (`Ocean`/`Sea`) is salt by definition.
/// - A river is fresh at its head and grades through [`Salinity::Brackish`] to
///   [`Salinity::Saline`] within [`RIVER_SALINE_MOUTH_CHUNKS`] /
///   [`RIVER_BRACKISH_MOUTH_CHUNKS`] of a marine mouth -- unless its own source
///   is marine, in which case the whole channel is salt.
/// - A lagoon is brackish, or salt when it touches the sea and every channel it
///   connects to is itself salt (nothing fresh reaches it). The marine clause
///   is redundant on any map [`promote_lagoon_basins`] produced -- a lagoon is
///   marine-adjacent by definition there -- and is kept so this function still
///   answers sensibly for a `water_body` map built some other way.
/// - A lake is fresh, or salt when it is endorheic: no water leaves it for the
///   sea and it sits at or below sea level, so what evaporation leaves behind
///   stays.
///
/// The passes are ordered, not independent: the lagoon rule reads the salinity
/// the river pass assigned to the channels touching it.
///
/// `alt` is the generation-time altitude, in the sea-level-is-zero space
/// `WorldSim::generate` works in -- not `SimChunk::alt`, which is that value
/// plus `CONFIG.sea_level` and, for a lake-carved chunk, already lowered to its
/// bed. Running before that lowering is what keeps the endorheic rule's floor
/// test meaningful.
///
/// The connectivity this works from is undirected: any contiguous run of water
/// counts as a path to the sea, so a lake whose only marine link is an
/// *inflowing* river reads as draining. The authored raster carries no flow
/// direction, and every basin that is not an elevated lake is pinned to sea
/// level anyway, so there is little for a direction to disagree with.
///
/// `is_elevated_lake` identifies chunks the authored elevated-lake decree
/// claims; they are excluded from the basin flood fill for the same reason
/// [`promote_lagoon_basins`] excludes them (the decree is about standing water
/// *above* sea level, so such a chunk is neither salt itself nor a valid bridge
/// between a coastal basin and an inland one), and come out fresh.
///
/// Guarantees `salinity[i].is_some() == water_body[i].is_some()` for every
/// chunk.
fn derive_salinity(
    map_size_lg: MapSizeLg,
    water_body: &[Option<WaterBodyKind>],
    alt: &[Alt],
    is_elevated_lake: impl Fn(usize) -> bool,
) -> Box<[Option<Salinity>]> {
    let len = water_body.len();
    let is_marine = |idx: usize| {
        matches!(
            water_body[idx],
            Some(WaterBodyKind::Ocean | WaterBodyKind::Sea)
        )
    };
    let is_river = |idx: usize| water_body[idx] == Some(WaterBodyKind::River);
    let mut salinity = vec![None; len].into_boxed_slice();

    // 1. The sea.
    for (idx, salinity) in salinity.iter_mut().enumerate() {
        if is_marine(idx) {
            *salinity = Some(Salinity::Saline);
        }
    }

    // 2. Rivers. One multi-source breadth-first search seeded with every mouth on
    //    the map at once, rather than one search per component: the search only
    //    ever steps onto river chunks, so it cannot leak between components, and
    //    each chunk is settled once.
    let (label, components) = label_river_components(map_size_lg, is_river, is_marine, alt);
    let mut mouth_distance = vec![u32::MAX; len].into_boxed_slice();
    let mut frontier = VecDeque::new();
    for component in &components {
        if component.source_is_marine {
            // Salt from end to end; no gradient to measure.
            continue;
        }
        for &mouth in &component.mouths {
            mouth_distance[mouth] = 0;
            frontier.push_back(mouth);
        }
    }
    while let Some(idx) = frontier.pop_front() {
        let distance = mouth_distance[idx];
        if distance >= RIVER_BRACKISH_MOUTH_CHUNKS {
            // Everything beyond this is fresh regardless, so there is nothing
            // left to learn from walking further up the channel.
            continue;
        }
        for nidx in neighbors(map_size_lg, idx) {
            if is_river(nidx) && mouth_distance[nidx] == u32::MAX {
                mouth_distance[nidx] = distance + 1;
                frontier.push_back(nidx);
            }
        }
    }
    for idx in 0..len {
        if !is_river(idx) {
            continue;
        }
        salinity[idx] = Some(if components[label[idx] as usize].source_is_marine {
            Salinity::Saline
        } else {
            river_salinity_from_mouth_distance(mouth_distance[idx])
        });
    }

    // 3. Which water chunks are connected to the sea at all, across every kind of
    //    water body. "Has an outflow" is not a question about a basin's own rim: a
    //    lake that drains into a channel which itself dead-ends in another closed
    //    basin has no more of a way out than one with no channel at all, and on
    //    this map that is the common shape (the authored raster paints whole inland
    //    systems below sea level). One breadth-first search from every marine chunk
    //    at once answers it for the whole map.
    //
    //    Elevated-lake chunks are not crossed, the same exclusion pass 4 makes
    //    below and for the same reason: the decree marks standing water *above*
    //    sea level, so a basin on the far side of one is not connected to the sea
    //    through it -- reaching the sea that way would mean running uphill and
    //    back down.
    let mut reaches_sea = vec![false; len];
    frontier.clear();
    for (idx, reaches_sea) in reaches_sea.iter_mut().enumerate() {
        if is_marine(idx) {
            *reaches_sea = true;
            frontier.push_back(idx);
        }
    }
    while let Some(idx) = frontier.pop_front() {
        for nidx in neighbors(map_size_lg, idx) {
            if water_body[nidx].is_some() && !reaches_sea[nidx] && !is_elevated_lake(nidx) {
                reaches_sea[nidx] = true;
                frontier.push_back(nidx);
            }
        }
    }

    // 4. Standing water, one basin at a time -- whether a body is endorheic, or
    //    whether anything fresh reaches it, is a property of the body and not of
    //    the individual chunk, exactly as its lagoon/lake type is.
    let is_standing = |idx: usize| {
        matches!(
            water_body[idx],
            Some(WaterBodyKind::Lake | WaterBodyKind::Lagoon)
        ) && !is_elevated_lake(idx)
    };
    let mut visited = vec![false; len];
    let mut basin = Vec::new();
    let mut stack = Vec::new();

    for start in 0..len {
        if visited[start] || !is_standing(start) {
            continue;
        }
        basin.clear();
        stack.clear();
        stack.push(start);
        visited[start] = true;
        let mut is_lagoon = false;
        let mut touches_marine = false;
        let mut touches_fresh_channel = false;
        let mut has_outflow = false;
        // The basin *floor*: its deepest chunk. Deliberately not its water
        // surface, which would be a vacuous test -- `cromatolis_forces_sea_
        // level` pins every basin that is not an authored elevated lake to
        // exactly sea level, so every one of them would pass. The floor
        // answers the question the rule is actually asking: is this a real
        // depression, where water collects and only evaporation takes it away.
        let mut floor_alt = f32::INFINITY;

        while let Some(idx) = stack.pop() {
            basin.push(idx);
            is_lagoon |= water_body[idx] == Some(WaterBodyKind::Lagoon);
            has_outflow |= reaches_sea[idx];
            floor_alt = floor_alt.min(alt[idx] as f32);
            for nidx in neighbors(map_size_lg, idx) {
                touches_marine |= is_marine(nidx);
                touches_fresh_channel |= is_river(nidx) && salinity[nidx] != Some(Salinity::Saline);
                if !visited[nidx] && is_standing(nidx) {
                    visited[nidx] = true;
                    stack.push(nidx);
                }
            }
        }

        let basin_salinity = if is_lagoon {
            if touches_marine && !touches_fresh_channel {
                Salinity::Saline
            } else {
                Salinity::Brackish
            }
        } else if !has_outflow && floor_alt <= 0.0 {
            Salinity::Saline
        } else {
            Salinity::Fresh
        };
        for &idx in &basin {
            salinity[idx] = Some(basin_salinity);
        }
    }

    // 5. Whatever the passes above did not claim -- in practice the elevated-lake
    //    chunks the basin fill deliberately skipped, which the decree puts above
    //    sea level and therefore beyond the reach of any of the salt rules.
    //
    //    Spelled as a wildcard-free `match` rather than a blanket "anything left
    //    is fresh": a new `WaterBodyKind` (the taxonomy still names a `Swamp` case
    //    -- see that enum's closing note) must fail to compile here and be routed
    //    deliberately, not slip through as fresh water.
    for idx in 0..len {
        let Some(kind) = water_body[idx] else {
            continue;
        };
        if salinity[idx].is_some() {
            continue;
        }
        salinity[idx] = Some(match kind {
            // Settled by pass 1 and pass 2 respectively, so a chunk of either
            // kind reaching this point means the map contradicts itself. Answer
            // in character rather than panicking halfway through world
            // generation.
            WaterBodyKind::Ocean | WaterBodyKind::Sea => Salinity::Saline,
            WaterBodyKind::River => Salinity::Fresh,
            // The elevated-lake decree: standing water above sea level.
            WaterBodyKind::Lake | WaterBodyKind::Lagoon => Salinity::Fresh,
        });
    }

    debug_assert!(
        (0..len).all(|idx| water_body[idx].is_some() == salinity[idx].is_some()),
        "every classified water chunk carries a salinity, and nothing else does"
    );
    salinity
}

/// Widest channel, in metres, the engine will carve as a real
/// `RiverKind::River`. `CONFIG.river_max_width` is a multiplier on the chunk
/// size, exactly as `erosion.rs`'s `get_rivers` uses it, so this is the same
/// 64 m ceiling the procedural path already enforces. Wider authored corridors
/// stay `RiverKind::Lake` physically (carving past this leaves water walls)
/// while still being `WaterBodyKind::River`.
const CROMATOLIS_MAX_RIVER_WIDTH: f32 =
    TerrainChunkSize::RECT_SIZE.x as f32 * CONFIG.river_max_width;

/// Local channel width in metres for one authored river-corridor chunk, from
/// the radius (in chunks) of the widest channel it sits in -- i.e. from
/// `local_channel_radius_chunks`, *not* from a bare distance transform.
///
/// A channel of radius `r` chunks is `2 * r` chunks across; multiplying by the
/// chunk size turns that into metres. Exact for even-chunk-count channels and
/// one chunk generous for odd ones, which is the right way to round here --
/// the alternative under-reports every single-chunk corridor to nothing.
fn cromatolis_channel_width(channel_radius: f32) -> f32 {
    2.0 * channel_radius * TerrainChunkSize::RECT_SIZE.x as f32
}

/// Cross-section (width × depth, in metres) for an authored river chunk of the
/// given local channel width.
///
/// Replaces the flat `3.2 m × 0.25 m` ditch every authored river used to get:
/// the authored map skips erosion entirely, so `get_rivers`' physical
/// derivation never runs on it and the fallback constant was the *only* answer
/// any Cromatolis river ever had. Depth follows `CONFIG.river_width_to_depth`
/// (the same ratio the procedural path assumes) with `CONFIG.river_min_height`
/// as a floor, so the shape of an authored river matches a procedural one of
/// the same width.
fn cromatolis_authored_river_cross_section(channel_width: f32) -> Vec2<f32> {
    let width = channel_width.clamp(0.0, CROMATOLIS_MAX_RIVER_WIDTH);
    let depth = (width / CONFIG.river_width_to_depth).max(CONFIG.river_min_height);
    Vec2::new(width, depth)
}

/// Flow velocity for an authored river chunk, derived from the authored
/// heightmap's own downhill slope with the same Gauckler–Manning–Strickler
/// formula `erosion.rs`'s `get_rivers` uses for procedural rivers, so authored
/// and procedural rivers animate and spline identically.
///
/// Slope comes from `alt` (the authored bed) rather than the water surface
/// `get_rivers` uses. On an authored map the water surface is flattened to sea
/// level almost everywhere (see `cromatolis_forces_sea_level`), so it carries
/// no usable gradient; `alt` is also what `downhill` was computed from, which
/// guarantees the step is genuinely downhill.
///
/// Returns a zero vector for a chunk with no downhill neighbour (a boundary or
/// sink node) or a flat one, matching `get_rivers`' own "this is not a river"
/// handling of a zero slope.
fn cromatolis_authored_river_velocity(
    map_size_lg: MapSizeLg,
    posi: usize,
    downhill_idx: isize,
    alt: &[Alt],
    depth: f32,
) -> Vec3<f32> {
    if downhill_idx < 0 {
        return Vec3::zero();
    }
    let downhill_idx = downhill_idx as usize;
    let neighbor_dim = (uniform_idx_as_vec2(map_size_lg, downhill_idx)
        - uniform_idx_as_vec2(map_size_lg, posi))
    .map2(TerrainChunkSize::RECT_SIZE, |e, sz| e as f64 * sz as f64);
    let neighbor_distance = neighbor_dim.magnitude();
    let dz = alt[downhill_idx] - alt[posi];
    let slope = dz.abs() / neighbor_distance;
    if neighbor_distance == 0.0 || !slope.is_normal() {
        return Vec3::zero();
    }
    let velocity_magnitude =
        1.0 / CONFIG.river_roughness as f64 * (depth as f64).powf(2.0 / 3.0) * slope.sqrt();
    // NOTE: the z component is `|dz|`, not `dz`, matching `get_rivers`'
    // `dz.signum() * dz`.
    let mut velocity = Vec3::new(neighbor_dim.x, neighbor_dim.y, dz.abs());
    velocity.normalize();
    (velocity * velocity_magnitude).map(|e| e as f32)
}

/// Inputs to [`authored_river_kind_override`], grouped into a struct rather
/// than several positional `bool` parameters.
struct AuthoredRiverKindInputs {
    /// Stable id of the authored region this chunk belongs to. Only used to
    /// scope the `alt_below_sea_level` disjunct out for Cromatolis
    /// specifically -- see the comment on that branch.
    region_id: Option<&'static str>,
    is_elevated_lake: bool,
    is_water_body: bool,
    is_river_channel: bool,
    /// Whether this corridor chunk's precomputed local channel width is within
    /// [`CROMATOLIS_MAX_RIVER_WIDTH`], i.e. whether it can be carved as a real
    /// river at all. Meaningless unless `is_river_channel`.
    channel_fits_max_river_width: bool,
    /// Whether this chunk has a downhill neighbour at all. A chunk the
    /// `get_oceans` flood fill reached is a boundary node with none, and
    /// `column.rs`'s neighbour-river sampling *panics* ("How can a river have
    /// no downhill?") on a `RiverKind::River` chunk whose `SimChunk::downhill`
    /// is `None`, so such a chunk must never be typed as a river however the
    /// authored masks paint it.
    has_downhill: bool,
    is_ocean: bool,
    alt_below_sea_level: bool,
    neighbor_pass_pos: Vec2<i32>,
    authored_river_cross_section: Vec2<f32>,
}

/// Decides the authored `RiverKind` for one chunk from its (already
/// mask-value-thresholded) authored water flags. Each authored mask keeps
/// its own semantics instead of being collapsed into one generic "is wet"
/// check: an elevated lake wins over a river channel, which wins over the
/// broader water-body mask it is a subset of, which wins over leaving the tile
/// dry.
///
/// A corridor chunk always takes `authored_river_cross_section`, even where
/// the erosion sim happened to produce a `RiverKind::River` of its own (which
/// this function used to keep). The authored map skips erosion entirely, so a
/// sim-derived cross-section there is computed from a flux field that never
/// shaped the terrain: measured on the shipped rasters those come out as
/// little as 0.48 m wide and 4 mm deep -- not a river, and far below
/// `CONFIG.river_min_height`. The authored width is the better answer
/// everywhere on this map.
///
/// This mirrors [`authored_water_body_kind`]'s priority order on purpose: the
/// two functions answer different questions (physical carving vs. ecology) but
/// must agree about *which authored mask owns a chunk*, or the map ends up
/// with, say, a `WaterBodyKind::River` chunk carved as ocean.
fn authored_river_kind_override(inputs: AuthoredRiverKindInputs) -> Option<RiverKind> {
    let AuthoredRiverKindInputs {
        region_id,
        is_elevated_lake,
        is_water_body,
        is_river_channel,
        channel_fits_max_river_width,
        has_downhill,
        is_ocean,
        alt_below_sea_level,
        neighbor_pass_pos,
        authored_river_cross_section,
    } = inputs;

    if is_elevated_lake {
        Some(RiverKind::Lake { neighbor_pass_pos })
    } else if is_river_channel {
        // COW-22 `C22-1b`: checked *before* the water mask (it used to be
        // checked after). Every authored corridor cell also sits inside the
        // broader `water` mask, so while the water arm ran first no chunk on
        // the map could ever resolve to `RiverKind::River` -- the authored
        // river network existed in the data and nowhere in the simulation.
        if channel_fits_max_river_width && has_downhill {
            Some(RiverKind::River {
                cross_section: authored_river_cross_section,
            })
        } else {
            // Either wider than `CROMATOLIS_MAX_RIVER_WIDTH` -- carving it as a
            // river would overflow the chunks either side of the channel and
            // leave water walls (see `erosion.rs`'s `max_width` handling) -- or
            // a boundary node with nowhere to flow, which `column.rs` would
            // panic on (see `has_downhill`). Either way it stays a lake
            // *physically*: flat water at a consistent level.
            // `WaterBodyKind::River` still calls it a river ecologically; see
            // that enum's doc comment for why the two are allowed to disagree.
            Some(RiverKind::Lake { neighbor_pass_pos })
        }
    } else if is_water_body {
        // `is_ocean` (the `get_oceans` border flood fill over `alt <= 0`) is
        // already the complete answer to "is this chunk connected to the sea".
        // The extra `alt_below_sea_level` disjunct additionally claimed every
        // *inland* body whose authored bed happens to be painted below sea
        // level -- 14,665 chunks across 327 bodies as the exporter measured the
        // v21 master, including the deep middle of Sapphire Loch (bed at
        // -139.7 m) whose shallow rim stayed `Lake`, i.e. one lake split into
        // two `RiverKind`s along the sea-level contour (COW-22 `C22-1c`).
        // Scoped off for Cromatolis specifically rather than for every
        // authored region, following the same precedent (and the same COW-2
        // debt note) as `SimChunk::get_biome`'s `Snowland`/`Desert` checks: a
        // future authored region shouldn't silently inherit a Cromatolis
        // decision it was never measured against.
        let below_sea_level_counts_as_ocean =
            region_id != Some(CROMATOLIS_V0_REGION_ID) && alt_below_sea_level;
        if is_ocean || below_sea_level_counts_as_ocean {
            Some(RiverKind::Ocean)
        } else {
            // Flagged as water but neither connected to the sea nor tagged
            // `elevated_lakes` -- still real standing water.
            Some(RiverKind::Lake { neighbor_pass_pos })
        }
    } else {
        None
    }
}

/// Humidity floor for `SimChunk::get_biome`'s `Swamp` branch (also requires
/// `authored_near_water`).
///
/// **The rule is "just below the p25 of the near-water humidity
/// distribution", not the literal number.** Land next to authored water skews
/// humid already, so the threshold's job is only to drop the driest tail --
/// narrow river mouths on otherwise arid stretches rather than real wetland
/// -- while keeping the bulk, which is how common coastal and riverine
/// wetlands are meant to be on a Caribbean-coast map. Anything that moves the
/// humidity field moves that distribution and this constant with it; re-derive
/// it rather than adjusting the regression band around it.
///
/// Measured over the land chunks flagged `authored_near_water` in a real
/// generated world (LFS assets, not synthetic):
///
/// |                  |  count |   min |   p10 |   p25 | median |   p75 |   p90 |
/// |------------------|-------:|------:|------:|------:|-------:|------:|------:|
/// | before the zones | 17,921 | 0.250 | 0.501 | 0.637 |  0.736 | 0.846 | 0.930 |
/// | with the zones   | 17,957 | 0.250 | 0.578 | 0.705 |  0.812 | 0.905 | 0.959 |
///
/// The whole distribution shifted up ~0.07 because the evaporation dampener
/// (`humidity *= 1 - (temp - tropical_temp)/(1 - tropical_temp)`) used to
/// multiply coastal humidity by exactly 0 -- the old flat sea-level baseline
/// pinned `temp` at 1.0 there -- so procedural humidity was erased at the
/// coast and only the authored vegetation floor kept it nonzero. With real
/// per-zone temperatures the dampener is inert over most of the map and the
/// procedural field survives.
///
/// Holding 0.6 while p25 moved to 0.705 would have quietly widened the branch
/// from "the wettest three quarters of near-water land" to "the wettest seven
/// eighths". Re-derived to 0.70; resulting Swamp coverage is 1.32% of the full
/// chunk grid, still rare but genuinely present. The pre-existing
/// commented-out 0.8 (with no water-proximity gate at all) would have kept
/// `Swamp` effectively unreachable: it sat behind `Forest`/`Jungle` in the old
/// branch order, and both already claim most tiles humid enough to clear 0.8.
const SWAMP_HUMIDITY_THRESHOLD: f32 = 0.70;

/// Cromatolis's authored baseline temperature curve: colder with altitude,
/// computed in real degrees Celsius via a simple lapse-rate formula and
/// converted onto the engine's existing abstract world-gen scale so every
/// existing consumer of `SimChunk::temp` (biome assignment, scatter/
/// wildlife/rock placement, dungeon-site eligibility) keeps working
/// unmodified. Replaces a hard two-point clamp (`-1.0` above 970 m of
/// relief, `0.55` at or below it) that structurally prevented any site
/// predicate needing a *middle* temperature band from ever matching in
/// Cromatolis.
///
/// `alt_pre` is meters of relief above `CONFIG.sea_level` (may be
/// negative for underwater terrain, hence the `max(0.0)` -- underwater
/// chunks get the sea-level baseline temperature, same as before this
/// change).
///
/// `sea_level_temp_c` is this chunk's own sea-level baseline, already
/// resolved from the authored climate-zone raster and any microclimate
/// override (see `AuthoredCromatolisClimate::resolve_sea_level_temp_c`).
/// Passing the resolved value rather than the whole asset keeps this function
/// a pure curve, so the tests below can sweep it over altitude without
/// building a map.
///
/// `lapse_rate_c_per_m` sets how much of that per-zone baseline the region's
/// relief is allowed to spend. At `0.0075` C/m (7.5 C/km, close to Earth's
/// ~6.5 C/km moist average) Cromatolis's 1.25 km of relief spans about 9 C,
/// which is the intended amount: the map is a mesa whose median dry relief is
/// ~107 m, so a steeper rate makes an ordinary plateau read as highland. At
/// the previous `0.023` C/m, `BiomeKind::Taiga`'s `-0.7..-0.3` window opened
/// only 65 m above sea level once the temperate baseline dropped to 17 C,
/// which would have classified ~96% of the landmass as Taiga. The *zone*
/// raster, not the lapse rate, is now what makes one end of the map warmer
/// than the other; the lapse rate only has to make summits colder than
/// valleys.
fn cromatolis_baseline_temp(alt_pre: f32, sea_level_temp_c: f32, lapse_rate_c_per_m: f32) -> f32 {
    let temp_c = sea_level_temp_c - alt_pre.max(0.0) * lapse_rate_c_per_m;
    config::celsius_to_abstract_temp(temp_c).clamp(-1.0, 1.0)
}

impl SimChunk {
    fn generate(map_size_lg: MapSizeLg, posi: usize, gen_ctx: &GenCtx, gen_cdf: &GenCdf) -> Self {
        let pos = uniform_idx_as_vec2(map_size_lg, posi);
        let wposf = (pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32)).map(|e| e as f64);

        let (_, chaos) = gen_cdf.chaos[posi];
        let alt_pre = gen_cdf.alt[posi] as f32;
        let basement_pre = gen_cdf.basement[posi] as f32;
        let water_alt_pre = gen_cdf.water_alt[posi];
        let downhill_pre = gen_cdf.dh[posi];
        let flux = gen_cdf.flux[posi] as f32;
        let river = gen_cdf.rivers[posi].clone();

        // Can have NaNs in non-uniform part where pure_water returned true.  We just
        // test one of the four in order to find out whether this is the case.
        let (flux_uniform, /* flux_non_uniform */ _) = gen_cdf.pure_flux[posi];
        let (alt_uniform, _) = gen_cdf.alt_no_water[posi];
        let (temp_uniform, _) = gen_cdf.temp_base[posi];
        let (humid_uniform, _) = gen_cdf.humid_base[posi];

        /* // Vertical difference from the equator (NOTE: "uniform" with much lower granularity than
        // other uniform quantities, but hopefully this doesn't matter *too* much--if it does, we
        // can always add a small x component).
        //
        // Not clear that we want this yet, let's see.
        let latitude_uniform = (pos.y as f32 / f32::from(self.map_size_lg().chunks().y)).sub(0.5).mul(2.0);

        // Even less granular--if this matters we can make the sign affect the quantity slightly.
        let abs_lat_uniform = latitude_uniform.abs(); */

        // We also correlate temperature negatively with altitude and absolute latitude,
        // using different weighting than we use for humidity.
        const TEMP_WEIGHTS: [f32; 3] = [/* 1.5, */ 1.0, 2.0, 1.0];
        let mut temp = cdf_irwin_hall(
            &TEMP_WEIGHTS,
            [
                temp_uniform,
                1.0 - alt_uniform, /* 1.0 - abs_lat_uniform*/
                (gen_ctx.rock_nz.get((wposf.div(50000.0)).into_array()) as f32 * 2.5 + 1.0) * 0.5,
            ],
        )
        // Convert to [-1, 1]
        .sub(0.5)
        .mul(2.0);
        if gen_cdf.authored_region_id == Some(CROMATOLIS_V0_REGION_ID) {
            let zone_value = gen_cdf
                .authored_climate_zone_layer
                .as_ref()
                .map(|zones| authored_layer_value_for_cromatolis_v0(map_size_lg, posi, zones));
            let sea_level_temp_c = gen_cdf
                .cromatolis_climate
                .resolve_sea_level_temp_c(pos, zone_value);
            temp = cromatolis_baseline_temp(
                alt_pre,
                sea_level_temp_c,
                gen_cdf.cromatolis_climate.lapse_rate_c_per_m,
            );
        }

        // Take the weighted average of our randomly generated base humidity, and the
        // calculated water flux over this point in order to compute humidity.
        const HUMID_WEIGHTS: [f32; 3] = [1.0, 1.0, 0.75];
        let mut humidity = cdf_irwin_hall(&HUMID_WEIGHTS, [humid_uniform, flux_uniform, 1.0]);
        // Moisture evaporates more in hot places
        humidity *= (1.0
            - (temp - CONFIG.tropical_temp)
                .max(0.0)
                .div(1.0 - CONFIG.tropical_temp))
        .max(0.0);

        let mut alt = CONFIG.sea_level.add(alt_pre);
        let basement = CONFIG.sea_level.add(basement_pre);
        let water_alt = CONFIG.sea_level.add(water_alt_pre);
        let (downhill, _gradient) = if downhill_pre == -2 {
            (None, 0.0)
        } else if downhill_pre < 0 {
            panic!("Uh... shouldn't this never, ever happen?");
        } else {
            (
                Some(
                    uniform_idx_as_vec2(map_size_lg, downhill_pre as usize)
                        * TerrainChunkSize::RECT_SIZE.map(|e| e as i32)
                        + TerrainChunkSize::RECT_SIZE.map(|e| e as i32 / 2),
                ),
                (alt_pre - gen_cdf.alt[downhill_pre as usize] as f32).abs()
                    / TerrainChunkSize::RECT_SIZE.x as f32,
            )
        };

        // Logistic regression.  Make sure x ∈ (0, 1).
        let logit = |x: f64| x.ln() - x.neg().ln_1p();
        // 0.5 + 0.5 * tanh(ln(1 / (1 - 0.1) - 1) / (2 * (sqrt(3)/pi)))
        let logistic_2_base = 3.0f64.sqrt().mul(std::f64::consts::FRAC_2_PI);
        // Assumes μ = 0, σ = 1
        let logistic_cdf = |x: f64| x.div(logistic_2_base).tanh().mul(0.5).add(0.5);

        let is_underwater = match river.river_kind {
            Some(RiverKind::Ocean) | Some(RiverKind::Lake { .. }) => true,
            Some(RiverKind::River { .. }) => false, // TODO: inspect width
            None => false,
        };
        let authored_vegetation_density =
            gen_cdf
                .authored_vegetation_layer
                .as_ref()
                .map(|vegetation| {
                    let painted_density =
                        authored_layer_value_for_cromatolis_v0(map_size_lg, posi, vegetation);
                    cromatolis_authored_tree_density(
                        painted_density,
                        is_underwater,
                        temp,
                        alt_pre,
                        &gen_cdf.cromatolis_climate,
                        gen_cdf
                            .authored_alpine_policy
                            .filter(|(region_id, _)| gen_cdf.authored_region_id == Some(*region_id))
                            .map(|(_, policy)| policy),
                    )
                });
        let authored_ground_cover =
            gen_cdf
                .authored_ground_cover_layer
                .as_ref()
                .map(|ground_cover| {
                    authored_layer_value_for_cromatolis_v0(map_size_lg, posi, ground_cover)
                });
        let authored_ground_substrate = gen_cdf
            .authored_ground_substrate_zones
            .as_ref()
            .and_then(|zones| zones.substrate_at(map_size_lg, pos));
        // The authored humidity floor: ground that was painted as vegetated
        // cannot also be arid, whatever the procedural humidity field says.
        //
        // Gated on the region's own cold cut-off, not on a bare `temp >= 0.0`.
        // Abstract 0.0 is 20 C, which is not a statement about vegetation at
        // all -- it only ever looked like one because the old flat sea-level
        // baseline clamped nearly every chunk to abstract 1.0. Against a real
        // per-zone baseline that literal would silently switch the floor off
        // across the whole temperate zone (three quarters of the map), and the
        // painted forest there would read as dry scrub. `tree_min_temp` is the
        // threshold that actually means "too cold for this to be plant
        // cover", and it is the same one the density exclusion uses, so the
        // two cannot drift apart.
        if let Some(vegetation_density) = authored_vegetation_density
            && temp >= gen_cdf.cromatolis_climate.tree_min_temp
        {
            humidity = humidity.max((0.25 + vegetation_density * 0.72).min(1.0));
        }
        let river_xy = Vec2::new(river.velocity.x, river.velocity.y).magnitude();
        let river_slope = river.velocity.z / river_xy;
        match river.river_kind {
            Some(RiverKind::River { cross_section }) => {
                if cross_section.x >= 0.5 && cross_section.y >= CONFIG.river_min_height {
                    /* println!(
                        "Big area! Pos area: {:?}, River data: {:?}, slope: {:?}",
                        wposf, river, river_slope
                    ); */
                }
                if river_slope.abs() >= 0.25 && cross_section.x >= 1.0 {
                    let pos_area = wposf;
                    let river_data = &river;
                    debug!(?pos_area, ?river_data, ?river_slope, "Big waterfall!",);
                }
            },
            Some(RiverKind::Lake { .. }) => {
                // Forces lakes to be downhill from the land around them, and adds some noise to
                // the lake bed to make sure it's not too flat.
                let lake_bottom_nz = (gen_ctx.small_nz.get((wposf.div(20.0)).into_array()) as f32)
                    .clamp(-1.0, 1.0)
                    .mul(3.0);
                alt = alt.min(water_alt - 5.0) + lake_bottom_nz;
            },
            _ => {},
        }

        // No trees in the ocean, with zero humidity (currently), or directly on
        // bedrock.
        let tree_density = if is_underwater {
            0.0
        } else {
            let tree_density = Lerp::lerp(
                -1.5,
                2.5,
                gen_ctx.tree_nz.get((wposf.div(1024.0)).into_array()) * 0.5 + 0.5,
            )
            .clamp(0.0, 1.0);
            // Tree density should go (by a lot) with humidity.
            if humidity <= 0.0 || tree_density <= 0.0 {
                0.0
            } else if humidity >= 1.0 || tree_density >= 1.0 {
                1.0
            } else {
                // Weighted logit sum.
                logistic_cdf(logit(tree_density))
            }
            // rescale to (-0.95, 0.95)
            .sub(0.5)
            .add(0.5)
        } as f32;
        const MIN_TREE_HUM: f32 = 0.15;
        let mut tree_density = tree_density
            // Tree density increases exponentially with humidity...
            .mul((humidity - MIN_TREE_HUM).max(0.0).mul(1.0 + MIN_TREE_HUM) / temp.max(0.75))
            // Places that are *too* wet (like marshes) also get fewer trees because the ground isn't stable enough for
            // them.
            //.mul((1.0 - flux * 0.05/*(humidity - 0.9).max(0.0) / 0.1*/).max(0.0))
            .mul(0.25 + flux * 0.05)
            // ...but is ultimately limited by available sunlight (and our tree generation system)
            .min(1.0);
        if let Some(vegetation_density) = authored_vegetation_density {
            // Cromatolis's authored mask is the density contract itself.
            // `cromatolis_authored_tree_density` has already applied only
            // the water/cold/extreme-altitude exclusions above.
            tree_density = vegetation_density;
        }

        // Add geologically short timescale undulation to the world for various reasons
        let alt =
            // Don't add undulation to rivers, mainly because this could accidentally result in rivers flowing uphill
            if river.near_water() {
                alt
            } else {
                // Sand dunes (formed over a short period of time, so we don't care about erosion sim)
                let warp = Vec2::new(
                    gen_ctx.turb_x_nz.get(wposf.div(350.0).into_array()) as f32,
                    gen_ctx.turb_y_nz.get(wposf.div(350.0).into_array()) as f32,
                ) * 200.0;
                const DUNE_SCALE: f32 = 24.0;
                const DUNE_LEN: f32 = 96.0;
                const DUNE_DIR: Vec2<f32> = Vec2::new(1.0, 1.0);
                let dune_dist = (wposf.map(|e| e as f32) + warp)
                    .div(DUNE_LEN)
                    .mul(DUNE_DIR.normalized())
                    .sum();
                let dune_nz = 0.5 - dune_dist.sin().abs() + 0.5 * (dune_dist + 0.5).sin().abs();
                let dune = dune_nz * DUNE_SCALE * (temp - 0.75).clamped(0.0, 0.25) * 4.0;

                // Trees bind to soil and their roots result in small accumulating undulations over geologically short
                // periods of time. Forest floors are generally significantly bumpier than that of deforested areas.
                // This is particularly pronounced in high-humidity areas.
                let soil_nz = gen_ctx.hill_nz.get(wposf.div(96.0).into_array()) as f32;
                let soil_nz = (soil_nz + 1.0) * 0.5;
                const SOIL_SCALE: f32 = 16.0;
                let soil = soil_nz * SOIL_SCALE * tree_density.sqrt() * humidity.sqrt();

                let warp_factor = ((alt - CONFIG.sea_level) / 16.0).clamped(0.0, 1.0);

                let warp = (dune + soil) * warp_factor;

                // Prevent warping pushing the altitude underwater
                if alt + warp < water_alt {
                    alt
                } else {
                    alt + warp
                }
            };

        // `alt_pre` above governed the authored mask before local surface
        // undulation. Enforce the same data-owned tree line against the
        // final chunk altitude as well: soil/tree warping must not carry a
        // painted tree a few metres across the physical 700m boundary.
        let authored_alpine = gen_cdf
            .authored_alpine_policy
            .filter(|(region_id, _)| gen_cdf.authored_region_id == Some(*region_id))
            .map(|(_, policy)| policy);
        let tree_density = if authored_alpine
            .is_some_and(|policy| alt - CONFIG.sea_level >= policy.tree_line_altitude_m)
        {
            0.0
        } else {
            tree_density
        };

        let authored_path = gen_cdf
            .authored_route_layer
            .as_ref()
            .and_then(|routes| authored_route_way(map_size_lg, posi, routes))
            .map(|way| {
                (way, Path {
                    width: crate::layer::CROMATOLIS_AUTHORED_PATH_WIDTH,
                })
            })
            .unwrap_or_default();

        Self {
            authored_cromatolis_v0: gen_cdf.authored_cromatolis_v0,
            authored_region_id: gen_cdf.authored_region_id,
            authored_near_water: gen_cdf.authored_near_water[posi],
            water_body: gen_cdf.authored_water_body[posi],
            salinity: gen_cdf.authored_salinity[posi],
            authored_alpine_snowland: authored_alpine
                .is_some_and(|policy| alt - CONFIG.sea_level >= policy.snow_start_altitude_m),
            chaos,
            flux,
            alt,
            basement: basement.min(alt),
            water_alt,
            downhill,
            temp,
            humidity,
            rockiness: if true {
                (gen_ctx.rock_nz.get((wposf.div(1024.0)).into_array()) as f32)
                    //.add(if river.near_river() { 20.0 } else { 0.0 })
                    .sub(0.1)
                    .mul(1.3)
                    .max(0.0)
            } else {
                0.0
            },
            tree_density,
            ground_cover: authored_ground_cover.unwrap_or_default(),
            ground_substrate: authored_ground_substrate,
            forest_kind: {
                let env = Environment {
                    humid: humidity,
                    temp,
                    near_water: if river.is_lake() || river.near_river() {
                        1.0
                    } else {
                        0.0
                    },
                };

                ForestKind::iter()
                    .max_by_key(|fk| (fk.proclivity(&env) * 10000.0) as u32)
                    .unwrap() // Can't fail
            },
            spawn_rate: 1.0,
            river,
            surface_veg: 1.0,

            sites: Vec::new(),
            place: None,
            poi: None,
            path: authored_path,
            cliff_height: 0.0,
            spot: None,

            contains_waypoint: false,
        }
    }

    pub fn is_underwater(&self) -> bool {
        self.water_alt > self.alt || self.river.river_kind.is_some()
    }

    pub fn get_base_z(&self) -> f32 { self.alt - self.chaos * 50.0 - 16.0 }

    pub fn get_biome(&self) -> BiomeKind {
        let savannah_hum_temp = [0.05..0.55, 0.3..1.6];
        let taiga_hum_temp = [0.2..1.4, -0.7..-0.3];
        if self.river.is_ocean() {
            BiomeKind::Ocean
        } else if self.river.is_lake() {
            BiomeKind::Lake
        } else if self.authored_region_id == Some(CROMATOLIS_V0_REGION_ID) && self.river.is_river()
        {
            // COW-22 `C22-1b`: `RiverKind::River` only became reachable at all
            // with that row, and there is no `BiomeKind::River` -- without
            // this branch a river chunk would fall through the whole chain
            // below and report Jungle/Savannah/Forest/Grassland, i.e. a river
            // claiming to be a land biome.
            //
            // Answering `Lake` (what every one of these chunks already
            // reported while they were typed `RiverKind::Lake`) keeps every
            // existing consumer -- wildlife manifests, scatter configs, site
            // predicates, the map legend -- behaving exactly as before.
            // Adding a `BiomeKind::River` variant instead would widen a
            // wire-synced, upstream-shared enum for a distinction
            // `SimChunk::water_body` already carries losslessly.
            BiomeKind::Lake
        } else if self.authored_alpine_snowland
            || (self.authored_region_id != Some(CROMATOLIS_V0_REGION_ID)
                && self.temp < CONFIG.snow_temp)
        {
            // An authored region supplies its alpine threshold in real
            // relief metres. Procedural worlds retain the inherited
            // temperature-derived Snowland path exactly.
            BiomeKind::Snowland
        } else if self.alt > 500.0 && self.chaos > 0.3 && self.tree_density < 0.6 {
            BiomeKind::Mountain
        } else if self.authored_region_id != Some(CROMATOLIS_V0_REGION_ID)
            && self.temp > CONFIG.desert_temp
            && self.humidity < CONFIG.desert_hum
        {
            // Same rationale and scoping pattern as the `Snowland` check
            // above: Cromatolis is lore-authored as tropical/caribbean, not
            // arid, so this stays scoped to that specific region (not "any
            // authored region is loaded" -- same COW-2 debt note).
            //
            // What it protects against changed, but it is still load-bearing.
            // It used to be the whole coastline: one flat hot sea-level
            // baseline pinned coastal `temp` at 1.0, the evaporation dampener
            // a few lines above then crushed coastal humidity to ~0, and this
            // branch claimed ordinary tropical shore. Per-zone sea-level
            // temperatures ended that -- no *climatic* zone on this map
            // reaches `desert_temp` any more (the warmest anchor lands at
            // abstract 0.533). What still does is an authored microclimate
            // pocket, which exists precisely to be hot; without this check the
            // magically-warmed ground around a dungeon would render as sand.
            BiomeKind::Desert
        } else if self.authored_near_water && self.humidity > SWAMP_HUMIDITY_THRESHOLD {
            // Gated on real hydrology (`authored_near_water`, not humidity
            // alone) -- a swamp is wet ground *near standing/flowing water*,
            // not just any humid open area (that's Jungle/Savannah/
            // Grassland's territory). Uses the authored water/elevated-lake/
            // river-channel masks directly (`authored_near_water`) rather
            // than `RiverData::near_water`. The original COW-4 reason was
            // that no chunk could resolve to `RiverKind::River` at all, so
            // `near_water` never saw a land tile as "near" anything; COW-22
            // `C22-1b` made rivers reachable, so that specific reason is
            // gone. The choice stands on its own though: `near_water` is
            // `is_river || !neighbor_rivers.is_empty() || is_lake ||
            // is_ocean`, and `neighbor_rivers` comes from the erosion sim's
            // river network, which the authored map never runs -- so it
            // still would not see authored water next door.
            // `authored_near_water` reads the masks themselves. Checked
            // *before* Jungle/Forest
            // rather than after (unlike the original pre-disable ordering):
            // real swamps are frequently wooded (mangroves, cypress) and,
            // for Cromatolis specifically, the authored vegetation layer
            // drives `tree_density` and `humidity` together closely enough
            // that "near water and humid but *not* forest" would have
            // matched almost nothing -- checking this branch first lets a
            // waterlogged, vegetated tile become Swamp instead of always
            // losing to Jungle/Forest. See `SWAMP_HUMIDITY_THRESHOLD`'s doc
            // comment for how the threshold was picked.
            BiomeKind::Swamp
        } else if self.tree_density > 0.65 && self.humidity > 0.65 && self.temp > 0.45 {
            BiomeKind::Jungle
        } else if savannah_hum_temp[0].contains(&self.humidity)
            && savannah_hum_temp[1].contains(&self.temp)
        {
            BiomeKind::Savannah
        } else if taiga_hum_temp[0].contains(&self.humidity)
            && taiga_hum_temp[1].contains(&self.temp)
        {
            BiomeKind::Taiga
        } else if self.tree_density > 0.4 {
            BiomeKind::Forest
        } else {
            BiomeKind::Grassland
        }
    }

    pub fn near_cliffs(&self) -> bool { self.cliff_height > 0.0 }

    pub fn get_environment(&self) -> Environment {
        Environment {
            humid: self.humidity,
            temp: self.temp,
            near_water: if self.river.is_lake()
                || self.river.near_river()
                || self.alt < CONFIG.sea_level + 6.0
            // Close to sea in altitude
            {
                1.0
            } else {
                0.0
            },
        }
    }

    pub fn get_location_name(
        &self,
        index_sites: &Store<crate::site::Site>,
        civs_pois: &Store<PointOfInterest>,
        wpos2d: Vec2<i32>,
    ) -> Option<String> {
        self.sites
            .iter()
            .filter(|id| {
                index_sites[**id].origin.distance_squared(wpos2d) as f32
                    <= index_sites[**id].radius().powi(2)
            })
            .min_by_key(|id| index_sites[**id].origin.distance_squared(wpos2d))
            .and_then(|id| Some(index_sites[*id].name()?.to_string()))
            .or_else(|| self.poi.map(|poi| civs_pois[poi].name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn microclimate(id: &str, forced: f32, falloff: f32) -> AuthoredMicroclimateZone {
        // Chunks 10..=20 in both axes on a 100x100 grid. Normalized space has a
        // top-left origin, so the *southern* chunk edge is the larger `y`.
        AuthoredMicroclimateZone {
            id: id.to_owned(),
            forced_sea_level_temp_c: forced,
            falloff_chunks: falloff,
            polygon_normalized_top_left: vec![
                NormalizedTopLeftPoint { x: 0.10, y: 0.79 },
                NormalizedTopLeftPoint { x: 0.21, y: 0.79 },
                NormalizedTopLeftPoint { x: 0.21, y: 0.90 },
                NormalizedTopLeftPoint { x: 0.10, y: 0.90 },
            ],
        }
    }

    const TEST_MAP_CHUNKS: Vec2<u16> = Vec2::new(100, 100);

    #[test]
    fn climate_zone_decodes_every_painted_class_from_its_exported_value() {
        for (index, expected) in ClimateZone::ALL.into_iter().enumerate() {
            let exported = index as f32 / 5.0;
            assert_eq!(ClimateZone::from_layer_value(exported), expected);
        }
        // The layer is f32, so an exported class can arrive a few ulps off.
        assert_eq!(
            ClimateZone::from_layer_value(0.5999999),
            ClimateZone::Subtropical
        );
        assert_eq!(
            ClimateZone::from_layer_value(0.6000001),
            ClimateZone::Subtropical
        );
        // Out of range can only mean a corrupt layer; clamp rather than index
        // out of bounds.
        assert_eq!(ClimateZone::from_layer_value(-5.0), ClimateZone::Polar);
        assert_eq!(ClimateZone::from_layer_value(5.0), ClimateZone::Equatorial);
    }

    #[test]
    fn ecology_zone_accepts_only_the_authored_categorical_codes() {
        let exported = AuthoredEcologyZone::CODES.map(|code| f32::from(code) / 255.0);
        for (value, expected) in exported.into_iter().zip(AuthoredEcologyZone::ALL) {
            assert_eq!(AuthoredEcologyZone::from_layer_value(value), Some(expected));
        }
        assert!(AuthoredEcologyZone::validate_layer(&exported).is_ok());
        assert_eq!(
            AuthoredEcologyZone::from_layer_value(96.0 / 255.0 + 1.0e-7),
            Some(AuthoredEcologyZone::TemperateForest),
            "f32 representation noise must not invalidate an exported class"
        );
        assert!(AuthoredEcologyZone::validate_layer(&[0.0, 0.5]).is_err());
        assert_eq!(
            AuthoredEcologyZone::from_layer_value(1.0),
            None,
            "white is not an ecology-zone class"
        );
    }

    #[test]
    fn resolving_sea_level_temp_reads_the_zone_raster_and_degrades_to_the_fallback() {
        let climate = AuthoredCromatolisClimate::default().resolve(TEST_MAP_CHUNKS);
        let anywhere = Vec2::new(50, 50);

        assert_eq!(
            climate.resolve_sea_level_temp_c(anywhere, Some(0.4)),
            climate.zone_anchors.temperate_c
        );
        assert_eq!(
            climate.resolve_sea_level_temp_c(anywhere, Some(0.8)),
            climate.zone_anchors.tropical_c
        );
        // A missing layer must not read as class 0 (Polar), which is what
        // defaulting the sampled value to 0.0 would do.
        assert_eq!(
            climate.resolve_sea_level_temp_c(anywhere, None),
            climate.fallback_sea_level_temp_c
        );
    }

    #[test]
    fn a_microclimate_overrides_its_polygon_and_fades_out_over_its_falloff() {
        let climate = AuthoredCromatolisClimate {
            microclimate_zones: vec![microclimate("cold.vault", 1.0, 10.0)],
            ..AuthoredCromatolisClimate::default()
        }
        .resolve(TEST_MAP_CHUNKS);
        let temperate = climate.zone_anchors.temperate_c;
        let zone_value = Some(0.4);
        let at = |x, y| climate.resolve_sea_level_temp_c(Vec2::new(x, y), zone_value);

        // Fully inside: the override wins outright, cold against a warm region.
        assert_eq!(at(15, 15), 1.0);
        // Well outside the falloff: the zone raster is untouched.
        assert_eq!(at(60, 60), temperate);
        // Inside the falloff: strictly between, and monotonic with distance.
        let near = at(15, 6);
        let far = at(15, 2);
        assert!(
            1.0 < near && near < far && far < temperate,
            "expected a monotonic fade from the forced 1.0 up to {temperate}, got near={near} \
             far={far}"
        );
    }

    #[test]
    fn overlapping_microclimates_resolve_by_weight_so_a_cold_one_can_win() {
        // Same polygon, one cold and one hot. Resolving by greatest
        // *temperature* would make the cold zone unexpressible wherever a warm
        // one overlaps it; resolving by greatest weight keeps both usable.
        let cold_first = AuthoredCromatolisClimate {
            microclimate_zones: vec![
                microclimate("cold", 1.0, 10.0),
                microclimate("hot", 40.0, 10.0),
            ],
            ..AuthoredCromatolisClimate::default()
        }
        .resolve(TEST_MAP_CHUNKS);
        assert_eq!(
            cold_first.resolve_sea_level_temp_c(Vec2::new(15, 15), Some(0.4)),
            1.0,
            "equal weight must keep the earlier declaration, not the warmer one"
        );

        // A wider falloff wins outside the shared polygon, in either direction.
        let reach = AuthoredCromatolisClimate {
            microclimate_zones: vec![
                microclimate("narrow", 1.0, 2.0),
                microclimate("wide", 40.0, 20.0),
            ],
            ..AuthoredCromatolisClimate::default()
        }
        .resolve(TEST_MAP_CHUNKS);
        let outside = reach.resolve_sea_level_temp_c(Vec2::new(15, 4), Some(0.4));
        assert!(
            outside > reach.zone_anchors.temperate_c,
            "outside the narrow zone's reach the wider (hot) zone must be the one still blending, \
             got {outside}"
        );
    }

    /// Both authored zone kinds share one containment primitive, so the
    /// on-edge case has to agree between them. It used to not: the
    /// tree-candidate polygon counted a point lying exactly on an edge as
    /// inside, the microclimate polygon did not, and a hand-authored polygon
    /// snapped to round coordinates lands on that case constantly.
    #[test]
    fn both_authored_zone_kinds_agree_that_a_point_on_an_edge_is_inside() {
        let square = [
            Vec2::new(10.0, 10.0),
            Vec2::new(20.0, 10.0),
            Vec2::new(20.0, 20.0),
            Vec2::new(10.0, 20.0),
        ];
        assert!(point_in_polygon(Vec2::new(15.0, 15.0), square));
        assert!(point_in_polygon(Vec2::new(10.0, 15.0), square), "left edge");
        assert!(
            point_in_polygon(Vec2::new(20.0, 15.0), square),
            "right edge"
        );
        assert!(point_in_polygon(Vec2::new(10.0, 10.0), square), "corner");
        assert!(!point_in_polygon(Vec2::new(9.9, 15.0), square));
        // Fewer than three vertices cannot contain anything.
        assert!(!point_in_polygon(Vec2::new(15.0, 15.0), [
            Vec2::new(10.0, 10.0),
            Vec2::new(20.0, 20.0),
        ]));

        // The normalized wrapper must be the same test, not a second one.
        let normalized: Vec<NormalizedTopLeftPoint> = square
            .iter()
            .map(|v| NormalizedTopLeftPoint {
                x: v.x / 100.0,
                y: v.y / 100.0,
            })
            .collect();
        assert!(point_in_normalized_top_left_polygon(
            NormalizedTopLeftPoint { x: 0.10, y: 0.15 },
            &normalized
        ));
    }

    /// The resolved form must place the polygon exactly where the authored
    /// normalized coordinates say, including the top-left -> bottom-left `y`
    /// flip, and must do so without re-projecting per query.
    #[test]
    fn resolving_a_microclimate_projects_its_polygon_into_chunk_space_once() {
        let zone = microclimate("probe", 30.0, 0.0)
            .resolve(TEST_MAP_CHUNKS)
            .expect("a 4-point polygon resolves");

        // Authored x 0.10..0.21 over 100 chunks -> 10..21.
        // Authored y 0.79..0.90 top-left -> chunk y 10..21 after the flip.
        assert_eq!(zone.vertices.len(), 4);
        // Approximate: the flip is `(1.0 - y) * chunks`, so an authored 0.79
        // lands on 20.999998 rather than exactly 21.
        let near = |got: Vec2<f32>, want: Vec2<f32>| {
            assert!(
                got.distance(want) < 1e-4,
                "expected {want:?} in chunk space, got {got:?}"
            );
        };
        near(zone.vertices[0], Vec2::new(10.0, 21.0));
        near(zone.vertices[2], Vec2::new(21.0, 10.0));
        assert_eq!(zone.weight_at(Vec2::new(15, 15)), 1.0);
        // Zero falloff means the override stops dead at the boundary.
        assert_eq!(zone.weight_at(Vec2::new(30, 30)), 0.0);

        // A polygon too degenerate to contain anything never becomes a
        // resolved zone at all.
        assert!(
            AuthoredMicroclimateZone {
                polygon_normalized_top_left: vec![NormalizedTopLeftPoint { x: 0.1, y: 0.1 }],
                ..microclimate("degenerate", 30.0, 4.0)
            }
            .resolve(TEST_MAP_CHUNKS)
            .is_none()
        );
    }

    #[test]
    fn climate_validation_rejects_unordered_anchors_and_malformed_microclimates() {
        let base = AuthoredCromatolisClimate::default();
        assert!(base.validate().is_ok());

        let mut unordered = base.clone();
        unordered.zone_anchors.tropical_c = unordered.zone_anchors.equatorial_c + 1.0;
        assert!(
            unordered.validate().is_err(),
            "a tropical band warmer than the equatorial one would still generate, silently wrong"
        );

        let mut degenerate = base.clone();
        degenerate.microclimate_zones = vec![AuthoredMicroclimateZone {
            polygon_normalized_top_left: vec![NormalizedTopLeftPoint { x: 0.1, y: 0.1 }],
            ..microclimate("two.points", 30.0, 4.0)
        }];
        assert!(degenerate.validate().is_err());

        let mut negative = base.clone();
        negative.microclimate_zones = vec![microclimate("negative.falloff", 30.0, -1.0)];
        assert!(negative.validate().is_err());
    }

    #[test]
    fn the_shipped_climate_asset_keeps_every_zone_out_of_the_desert_band() {
        // `CONFIG.desert_temp` gates both `BiomeKind::Desert` and the desert
        // wildlife manifest entries, and both are only region-scoped away for
        // Cromatolis. No *climatic* zone may need that scoping to hold; only a
        // deliberately magical microclimate may.
        let climate = AuthoredCromatolisClimate::load_owned("world.map.cromatolis_v0_climate")
            .expect("the shipped Cromatolis climate asset must parse");
        for (name, celsius) in climate.zone_anchors.named() {
            let abstract_temp = config::celsius_to_abstract_temp(celsius).clamp(-1.0, 1.0);
            assert!(
                abstract_temp < CONFIG.desert_temp,
                "climate anchor {name} ({celsius} C = abstract {abstract_temp}) reaches \
                 CONFIG.desert_temp; the Cromatolis desert exemptions would become load-bearing \
                 for ordinary terrain again"
            );
        }
        // ...and no zone may be so cold that painted forest is excluded at sea
        // level, which is the deforestation failure `tree_min_temp` guards.
        let coldest_used = config::celsius_to_abstract_temp(climate.zone_anchors.temperate_c);
        assert!(
            coldest_used >= climate.tree_min_temp,
            "the temperate anchor ({coldest_used}) is below tree_min_temp ({}), which would zero \
             tree_density across the whole zone",
            climate.tree_min_temp
        );
    }

    #[test]
    fn tree_candidate_zone_uses_normalized_top_left_coordinates() {
        let zone = AuthoredTreeCandidateZone {
            id: "forest.test".to_owned(),
            source_region_shape: "region_shape.test".to_owned(),
            polygon_normalized_top_left: vec![
                NormalizedTopLeftPoint { x: 0.25, y: 0.25 },
                NormalizedTopLeftPoint { x: 0.75, y: 0.25 },
                NormalizedTopLeftPoint { x: 0.75, y: 0.75 },
                NormalizedTopLeftPoint { x: 0.25, y: 0.75 },
            ],
            additional_grid: TreeCandidateGridSpec {
                frequency_blocks: 12,
                spread_blocks: 5,
                seed_salt: 0x434F_5731,
            },
        };
        let world_blocks = Vec2::new(32_768, 32_768);

        assert!(zone.contains_world_pos(Vec2::new(16_384, 16_384), world_blocks));
        assert!(zone.contains_world_pos(Vec2::new(16_384, 8_192), world_blocks));
        assert!(!zone.contains_world_pos(Vec2::new(16_384, 4_096), world_blocks));
        assert!(!zone.contains_world_pos(Vec2::new(4_096, 16_384), world_blocks));
    }

    #[test]
    fn tree_candidate_union_keeps_global_order_and_deduplicates_root_positions() {
        let global = vec![(Vec2::new(8, 8), 11), (Vec2::new(20, 20), 22)];
        let regional = vec![(Vec2::new(20, 20), 99), (Vec2::new(12, 12), 33)];

        assert_eq!(merge_tree_candidate_fields(global, regional), vec![
            (Vec2::new(8, 8), 11),
            (Vec2::new(20, 20), 22),
            (Vec2::new(12, 12), 33),
        ]);
    }

    #[test]
    fn tree_candidate_policy_adds_roots_only_inside_its_authored_polygon() {
        let policy = AuthoredTreeCandidatePolicy {
            schema: 1,
            zones: vec![AuthoredTreeCandidateZone {
                id: "forest.test".to_owned(),
                source_region_shape: "region_shape.test".to_owned(),
                polygon_normalized_top_left: vec![
                    NormalizedTopLeftPoint { x: 0.25, y: 0.25 },
                    NormalizedTopLeftPoint { x: 0.75, y: 0.25 },
                    NormalizedTopLeftPoint { x: 0.75, y: 0.75 },
                    NormalizedTopLeftPoint { x: 0.25, y: 0.75 },
                ],
                additional_grid: TreeCandidateGridSpec {
                    frequency_blocks: 12,
                    spread_blocks: 5,
                    seed_salt: 0x434F_5731,
                },
            }],
        };
        let compiled = policy.compile(0).expect("test policy must be valid");
        let world_blocks = Vec2::new(32_768, 32_768);

        let inside = compiled.additional_candidates_near(Vec2::new(16_384, 16_384), world_blocks);
        assert!(!inside.is_empty());
        assert!(
            inside.iter().all(|(position, _)| {
                policy.zones[0].contains_world_pos(*position, world_blocks)
            })
        );
        assert!(
            compiled
                .additional_candidates_near(Vec2::new(2_048, 2_048), world_blocks)
                .is_empty()
        );
    }

    #[test]
    fn tree_candidate_policy_broad_phase_skips_columns_far_from_its_polygon() {
        let policy = AuthoredTreeCandidatePolicy {
            schema: 1,
            zones: vec![AuthoredTreeCandidateZone {
                id: "forest.test".to_owned(),
                source_region_shape: "region_shape.test".to_owned(),
                polygon_normalized_top_left: vec![
                    NormalizedTopLeftPoint { x: 0.25, y: 0.25 },
                    NormalizedTopLeftPoint { x: 0.75, y: 0.25 },
                    NormalizedTopLeftPoint { x: 0.75, y: 0.75 },
                    NormalizedTopLeftPoint { x: 0.25, y: 0.75 },
                ],
                additional_grid: TreeCandidateGridSpec {
                    frequency_blocks: 12,
                    spread_blocks: 5,
                    seed_salt: 0x434F_5731,
                },
            }],
        };
        let compiled = policy.compile(0).expect("test policy must be valid");
        let world_blocks = Vec2::new(32_768, 32_768);

        assert!(compiled.may_supply_candidates_near(Vec2::new(16_384, 16_384), world_blocks));
        assert!(!compiled.may_supply_candidates_near(Vec2::new(2_048, 2_048), world_blocks));
    }

    #[test]
    fn procedural_worlds_keep_the_unmodified_global_tree_candidate_lattice() {
        let sim = WorldSim::empty();
        let wpos = Vec2::new(1_024, -512);

        assert_eq!(
            sim.tree_candidate_fields_near(wpos),
            sim.gen_ctx.structure_gen.get(wpos).to_vec()
        );
    }

    #[test]
    fn procedural_worlds_keep_the_unmodified_global_tree_candidate_area_sequence() {
        let sim = WorldSim::empty();
        let min = Vec2::new(-64, -64);
        let max = Vec2::new(64, 64);

        assert_eq!(
            sim.tree_candidate_fields_in_area(min, max),
            sim.gen_ctx.structure_gen.iter(min, max).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cromatolis_tree_candidate_policy_pins_the_single_waning_moon_zone() {
        let policy = AuthoredTreeCandidatePolicy::load_owned(
            "world.map.cromatolis_v0_tree_candidate_policy",
        )
        .expect("Cromatolis requires a valid authored tree candidate policy");

        assert_eq!(policy.schema, 1);
        assert_eq!(policy.zones.len(), 1);
        let zone = &policy.zones[0];
        assert_eq!(zone.id, "forest.waning_moon");
        assert_eq!(zone.source_region_shape, "region_shape.waning_moon_forest");
        assert_eq!(zone.additional_grid.frequency_blocks, 12);
        assert_eq!(zone.additional_grid.spread_blocks, 5);
        assert_eq!(zone.additional_grid.seed_salt, 0x434F_5731);
        assert_eq!(zone.polygon_normalized_top_left, vec![
            NormalizedTopLeftPoint { x: 0.66, y: 0.35 },
            NormalizedTopLeftPoint { x: 0.74, y: 0.33 },
            NormalizedTopLeftPoint { x: 0.78, y: 0.41 },
            NormalizedTopLeftPoint { x: 0.74, y: 0.51 },
            NormalizedTopLeftPoint { x: 0.66, y: 0.49 },
            NormalizedTopLeftPoint { x: 0.62, y: 0.42 },
        ]);
        policy.validate().expect("shipped policy must validate");
    }

    // ---- AuthoredProceduralLayers: the per-region RON toggles ----

    /// A parse failure here degrades silently — the loader warns and falls
    /// back to [`AuthoredProceduralLayers::default()`] — and a warn-level
    /// log line during world-gen is not something anyone reads. So assert
    /// every registered region actually ships a loadable asset, and pin
    /// Cromatolis's shipped policy explicitly rather than against
    /// `Default` (which is deliberately the permissive upstream fallback,
    /// not a copy of this region's choices). Not `#[ignore]`d: RON assets
    /// are plain git files, not LFS binaries, so this runs everywhere.
    #[test]
    fn cromatolis_procedural_layer_asset_pins_the_shipped_policy() {
        for region in AUTHORED_REGIONS {
            let specifier = format!("{}_procedural_layers", region.map_asset);
            let layers = AuthoredProceduralLayers::load_owned(&specifier)
                .unwrap_or_else(|err| panic!("{specifier} failed to load/parse: {err:?}"));

            if region.id == CROMATOLIS_V0_REGION_ID {
                assert_eq!(
                    layers,
                    AuthoredProceduralLayers {
                        caverns: false,
                        caves: true,
                        rocks: true,
                        spots: true,
                        rock_traversal_repair: true,
                    },
                    "{specifier} has changed Cromatolis's procedural-layer policy. That is a real \
                     content decision (notably, `caves` gates the region's entire procedural \
                     underground: cave biomes, fauna, loot and ore) -- update this assertion \
                     deliberately, don't just make it pass"
                );
            }
        }
    }

    #[test]
    fn northwall_substrate_zone_resolves_from_the_fortification_not_rust_coordinates() {
        let region = authored_region_for_map_asset("world.map.cromatolis_v0")
            .expect("Cromatolis must stay registered");
        let zones = AuthoredGroundSubstrateZones::load_owned(region.ground_substrate_zones)
            .expect("Cromatolis ground-substrate zones must parse");
        let fortifications = AuthoredFortificationAnchors::load_owned(region.fortifications)
            .expect("Cromatolis fortification anchors must parse");
        let resolved = zones
            .resolve(&fortifications)
            .expect("Northwall substrate zone must resolve against its declared anchor");
        let map_size = MapSizeLg::new(Vec2::new(10, 10)).expect("synthetic map size must work");

        // The zone is constrained to the horizontal span of the named wall,
        // and only to its top-left-origin exterior. These positions are
        // deliberately expressed as map chunks, never a duplicate source-Y
        // literal in Rust.
        assert_eq!(
            resolved.substrate_at(map_size, Vec2::new(524, 1019)),
            Some(GroundSubstrate::Sand)
        );
        assert_eq!(
            resolved.substrate_at(map_size, Vec2::new(524, 1018)),
            None,
            "Northwall's own chunk row must remain non-sand"
        );
        assert_eq!(
            resolved.substrate_at(map_size, Vec2::new(100, 1023)),
            None,
            "sand may not escape the authored fortification span"
        );
    }

    #[test]
    fn ground_cover_profile_classifies_cover_boundaries_and_endpoints() {
        let profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");

        assert_eq!(profile.classify(0.0), GroundCoverBand::BareDry);
        assert_eq!(profile.classify(1.0), GroundCoverBand::Jungle);
        for (index, definition) in profile.bands.iter().enumerate() {
            assert_eq!(profile.classify(definition.max_cover), definition.band);
            if let Some(next) = profile.bands.get(index + 1) {
                assert_eq!(profile.classify(next.max_cover), next.band);
            }
        }
        let forest_max = profile.bands[3].max_cover;
        let jungle_max = profile.bands[4].max_cover;
        assert_eq!(
            profile.classify((forest_max + jungle_max) / 2.0),
            GroundCoverBand::Jungle
        );
    }

    #[test]
    fn ground_cover_profile_preserves_luminance_as_cover_and_blackness_is_inverse() {
        let profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        let luminance = profile.bands[1].max_cover;
        let ground_cover = luminance;
        let blackness_percent = (1.0 - ground_cover) * 100.0;
        assert!((ground_cover - luminance).abs() < f32::EPSILON);
        assert!((blackness_percent - (1.0 - luminance) * 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ground_cover_profile_validation_rejects_unordered_and_out_of_range_thresholds() {
        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[1].max_cover = profile.bands[0].max_cover;
        assert!(profile.validate().is_err());

        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[0].max_cover = -0.1;
        assert!(profile.validate().is_err());

        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[4].max_cover = 1.1;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn ground_cover_profile_validation_rejects_non_finite_tints_and_invalid_blends() {
        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[0].surface_tint = (f32::NAN, 0.2, 0.3);
        assert!(profile.validate().is_err());

        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[0].surface_blend = 1.1;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn ground_cover_profile_validation_rejects_invalid_map_preview_values() {
        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[0].map_tint = (f32::NAN, 0.2, 0.3);
        assert!(profile.validate().is_err());

        let mut profile =
            AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
                .expect("the configured Cromatolis ground-cover profile must load");
        profile.bands[0].map_blend = -0.1;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn every_configured_authored_region_has_a_valid_ground_cover_profile() {
        for region in AUTHORED_REGIONS {
            let profile = AuthoredGroundCoverProfile::load_owned(region.ground_cover_profile)
                .unwrap_or_else(|err| {
                    panic!(
                        "{} is configured for {} but failed to load: {err:?}",
                        region.ground_cover_profile, region.id
                    )
                });
            profile
                .validate()
                .unwrap_or_else(|err| panic!("{} is invalid: {err}", region.ground_cover_profile));
        }
        assert!(WorldSim::empty().authored_ground_cover_profile.is_none());
    }

    // ---- AuthoredF32Layer: raw f32le format ----

    #[test]
    fn authored_f32_layer_parses_little_endian_values() {
        let bytes = [1.0f32, -2.5, 0.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>();
        let layer = AuthoredF32Layer::from_bytes(Cow::Owned(bytes)).unwrap();
        assert_eq!(&*layer.values, [1.0, -2.5, 0.0]);
    }

    #[test]
    fn authored_f32_layer_rejects_length_not_a_multiple_of_4() {
        let bytes = vec![0u8; 6];
        assert!(AuthoredF32Layer::from_bytes(Cow::Owned(bytes)).is_err());
    }

    #[test]
    fn authored_f32_layer_accepts_empty_input() {
        let layer = AuthoredF32Layer::from_bytes(Cow::Owned(Vec::new())).unwrap();
        assert!(layer.values.is_empty());
    }

    // ---- Region registry ----

    #[test]
    fn registry_resolves_cromatolis_map_asset_with_all_six_layers() {
        let region = authored_region_for_map_asset("world.map.cromatolis_v0")
            .expect("cromatolis_v0 must be registered");
        assert_eq!(region.id, "cromatolis_v0");
        for kind in AuthoredLayerKind::ALL {
            assert!(
                region.layers.contains(&kind),
                "cromatolis_v0 is missing layer kind {kind:?}"
            );
        }
    }

    #[test]
    fn registry_does_not_resolve_unregistered_specifiers() {
        assert!(authored_region_for_map_asset("world.map.veloren_0_18_0_0").is_none());
        assert!(authored_region_for_map_asset("world.map.cromatolis_v1").is_none());
        assert!(authored_region_for_map_asset("").is_none());
    }

    #[test]
    fn file_opts_authored_region_only_matches_registered_load_asset() {
        assert!(
            FileOpts::LoadAsset("world.map.cromatolis_v0".to_string())
                .authored_region()
                .is_some()
        );
        assert!(
            FileOpts::LoadAsset("world.map.veloren_0_18_0_0".to_string())
                .authored_region()
                .is_none()
        );
        // Non-`LoadAsset` variants never resolve to an authored region, even
        // though nothing about their content rules it out.
        assert!(
            FileOpts::Generate(GenOpts::default())
                .authored_region()
                .is_none()
        );
        assert!(
            FileOpts::Load(PathBuf::from("/tmp/whatever.bin"))
                .authored_region()
                .is_none()
        );
    }

    fn asset_specifier_for(region: &AuthoredRegion, kind: AuthoredLayerKind) -> String {
        format!("{}_{}", region.map_asset, kind.asset_suffix())
    }

    #[test]
    fn layer_asset_specifiers_match_the_open_world_export_convention() {
        // Mirrors `xindeler-open-world`'s `export-new-horizon` output paths
        // (`cromatolis_v0_new_horizon_export.ron`'s `active_runtime_assets` /
        // `staged_runtime_layers`): "{map_asset}_{suffix}".
        let region = authored_region_for_map_asset("world.map.cromatolis_v0").unwrap();
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::Routes),
            "world.map.cromatolis_v0_routes"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::Vegetation),
            "world.map.cromatolis_v0_vegetation"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::GroundCover),
            "world.map.cromatolis_v0_ground_cover"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::EcologyZone),
            "world.map.cromatolis_v0_ecology_zone"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::Water),
            "world.map.cromatolis_v0_water"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::ElevatedLakes),
            "world.map.cromatolis_v0_elevated_lakes"
        );
        assert_eq!(
            asset_specifier_for(region, AuthoredLayerKind::RiverChannels),
            "world.map.cromatolis_v0_river_channels"
        );
    }

    // ---- Orientation: the single Y-flip when sampling authored layers ----

    #[test]
    fn authored_layer_index_flips_y_exactly_once() {
        // 4x4 grid (x_lg = y_lg = 2).
        let map_size_lg = MapSizeLg::new(Vec2 { x: 2, y: 2 }).unwrap();

        // Engine posi (0, 0) is top-left in *engine* space; after the single
        // flip it must read the *last* source row (source-top-left order),
        // not the first -- this is the inversion `routes`/`vegetation`
        // already rely on, extended here to cover the climate and ecology
        // categorical layers too.
        let posi_00 = vec2_as_uniform_idx(map_size_lg, Vec2::new(0, 0));
        assert_eq!(
            authored_layer_idx_for_cromatolis_v0(map_size_lg, posi_00),
            3 * 4 // row 3 (last), col 0
        );

        // Engine (0, 3) (top-left of the *last* engine row) must read source
        // row 0 (the *first* source row).
        let posi_03 = vec2_as_uniform_idx(map_size_lg, Vec2::new(0, 3));
        assert_eq!(
            authored_layer_idx_for_cromatolis_v0(map_size_lg, posi_03),
            0
        );

        // X is untouched by the flip.
        let posi_30 = vec2_as_uniform_idx(map_size_lg, Vec2::new(3, 0));
        assert_eq!(
            authored_layer_idx_for_cromatolis_v0(map_size_lg, posi_30),
            3 * 4 + 3
        );

        let ecology = [
            0.0,
            32.0 / 255.0,
            64.0 / 255.0,
            96.0 / 255.0,
            128.0 / 255.0,
            160.0 / 255.0,
            192.0 / 255.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            192.0 / 255.0,
            192.0 / 255.0,
            192.0 / 255.0,
            192.0 / 255.0,
        ];
        assert_eq!(
            AuthoredEcologyZone::from_layer_value(
                ecology[authored_layer_idx_for_cromatolis_v0(map_size_lg, posi_00)]
            ),
            Some(AuthoredEcologyZone::AlpineBarren),
            "ecology zones must use the same one-time source Y flip as every authored layer"
        );

        // Applying the flip twice must return to the original row (single
        // inversion is an involution) -- guards against a future edit
        // accidentally double-flipping.
        for y in 0..4 {
            for x in 0..4 {
                let posi = vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y));
                let once = authored_layer_idx_for_cromatolis_v0(map_size_lg, posi);
                let twice = authored_layer_idx_for_cromatolis_v0(
                    map_size_lg,
                    // `authored_layer_idx_for_cromatolis_v0` is its own
                    // inverse (row -> height-1-row), so feeding its output
                    // back in recovers `posi`.
                    once,
                );
                assert_eq!(twice, posi);
            }
        }
    }

    #[test]
    fn authored_layer_value_clamps_to_unit_range_and_defaults_out_of_bounds() {
        let map_size_lg = MapSizeLg::new(Vec2 { x: 1, y: 1 }).unwrap(); // 2x2
        let layer = [2.0f32, -1.0, 0.4]; // shorter than the 2x2 = 4 grid on purpose

        // In-bounds values are clamped to [0, 1].
        let posi_over = vec2_as_uniform_idx(map_size_lg, Vec2::new(0, 1)); // source idx 0
        assert_eq!(
            authored_layer_value_for_cromatolis_v0(map_size_lg, posi_over, &layer),
            1.0
        );
        let posi_under = vec2_as_uniform_idx(map_size_lg, Vec2::new(1, 1)); // source idx 1
        assert_eq!(
            authored_layer_value_for_cromatolis_v0(map_size_lg, posi_under, &layer),
            0.0
        );

        // Out-of-bounds (layer shorter than the grid) defaults to 0.0 rather
        // than panicking.
        let posi_oob = vec2_as_uniform_idx(map_size_lg, Vec2::new(1, 0)); // source idx 3, len is 3
        assert_eq!(
            authored_layer_value_for_cromatolis_v0(map_size_lg, posi_oob, &layer),
            0.0
        );
    }

    #[test]
    fn cromatolis_biome_mask_density_is_linear_except_for_physical_exclusions() {
        let climate = AuthoredCromatolisClimate::default().resolve(TEST_MAP_CHUNKS);
        // Matías's authored table is expressed as blackness: 100% black is
        // bare terrain and 0% black (white) is maximum vegetation. The
        // runtime input is the inverse grayscale intensity, which must pass
        // through unchanged at every named authoring step.
        for blackness_percent in [
            100.0, 96.0, 90.0, 85.0, 80.0, 75.0, 70.0, 65.0, 60.0, 55.0, 50.0, 45.0, 40.0, 35.0,
            30.0, 25.0, 20.0, 15.0, 10.0, 5.0, 0.0,
        ] {
            let painted_density = 1.0 - blackness_percent / 100.0;
            assert_eq!(
                cromatolis_authored_tree_density(
                    painted_density,
                    false,
                    0.5,
                    300.0,
                    &climate,
                    None
                ),
                painted_density,
                "{blackness_percent}% black must retain its authored vegetation density"
            );
        }

        // No gradual altitude attenuation or response curve is permitted:
        // a mid-gray forest value remains mid-gray up to the hard cap.
        assert_eq!(
            cromatolis_authored_tree_density(0.50, false, 0.5, 699.9, &climate, None),
            0.50
        );
        assert_eq!(
            cromatolis_authored_tree_density(1.0, false, 0.5, 970.0, &climate, None),
            0.0,
            "the legacy climate cap remains a physical exclusion"
        );
        assert_eq!(
            cromatolis_authored_tree_density(0.82, false, 0.5, 300.0, &climate, None),
            0.82
        );
        assert_eq!(
            cromatolis_authored_tree_density(0.83, false, 0.5, 300.0, &climate, None),
            0.83
        );

        assert_eq!(
            cromatolis_authored_tree_density(1.0, true, 0.5, 300.0, &climate, None),
            0.0
        );
        // The cold exclusion fires at the region's own `tree_min_temp`, not at
        // abstract 0.0. Straddle the real threshold rather than restating the
        // number, so a future retune of the asset moves both sides together.
        assert_eq!(
            cromatolis_authored_tree_density(
                1.0,
                false,
                climate.tree_min_temp - 0.01,
                300.0,
                &climate,
                None
            ),
            0.0
        );
        assert_eq!(
            cromatolis_authored_tree_density(
                1.0,
                false,
                climate.tree_min_temp,
                300.0,
                &climate,
                None
            ),
            1.0,
            "exactly at the threshold is still warm enough -- the exclusion is `temp < min`"
        );
        // The temperate zone's own sea-level temperature must not be excluded:
        // that combination (a 17 C baseline against a 20 C literal) is the
        // deforestation bug `tree_min_temp` was moved to avoid.
        assert_eq!(
            cromatolis_authored_tree_density(
                1.0,
                false,
                config::celsius_to_abstract_temp(climate.zone_anchors.temperate_c),
                300.0,
                &climate,
                None
            ),
            1.0,
            "the temperate zone's sea-level temperature must not exclude trees"
        );
        assert_eq!(
            cromatolis_authored_tree_density(
                1.0,
                false,
                0.5,
                700.0,
                &climate,
                Some(AuthoredAlpinePolicy {
                    schema: 1,
                    tree_line_altitude_m: 700.0,
                    snow_start_altitude_m: 700.0,
                    persistent_snow_altitude_m: 1010.0,
                    transition_rock_blend: 0.62,
                    slope_rock_blend: 0.22,
                }),
            ),
            0.0,
            "the separate authored alpine policy applies its 700m tree line"
        );

        // The regional asset owns the two non-water exclusions. A different
        // reviewed Cromatolis climate profile can move either limit without
        // adding another response curve to the mask interpretation.
        let permissive_climate = AuthoredCromatolisClimate {
            tree_min_temp: -0.5,
            max_tree_altitude_m: 1_200.0,
            ..AuthoredCromatolisClimate::default()
        }
        .resolve(TEST_MAP_CHUNKS);
        assert_eq!(
            cromatolis_authored_tree_density(
                0.50,
                false,
                -0.25,
                1_000.0,
                &permissive_climate,
                None
            ),
            0.50
        );
    }

    #[test]
    fn alpine_surface_policy_has_no_effect_below_start_and_composes_valid_weights() {
        let policy = AuthoredAlpinePolicy {
            schema: 1,
            tree_line_altitude_m: 700.0,
            snow_start_altitude_m: 700.0,
            persistent_snow_altitude_m: 1010.0,
            transition_rock_blend: 0.62,
            slope_rock_blend: 0.22,
        };
        assert_eq!(policy.surface_at(699.9, 0.0), None);
        assert_eq!(
            policy.surface_at(700.0, 0.0),
            Some(AlpineSurface {
                rock: 0.62,
                snow: 0.0
            })
        );
        for (relief, slope) in [(700.0, 0.0), (855.0, 0.5), (1010.0, 0.0), (1010.0, 1.0)] {
            let surface = policy.surface_at(relief, slope).unwrap();
            assert!(surface.rock >= 0.0 && surface.snow >= 0.0);
            assert!(surface.rock + surface.snow <= 1.0 + f32::EPSILON);
        }
        assert_eq!(policy.surface_at(1010.0, 0.0).unwrap().snow, 1.0);
        let mut incompatible_schema = policy;
        incompatible_schema.schema = 2;
        assert!(
            incompatible_schema.validate().is_err(),
            "an incompatible alpine policy schema must fail closed"
        );
        let mut invalid_altitudes = policy;
        invalid_altitudes.persistent_snow_altitude_m = invalid_altitudes.snow_start_altitude_m;
        assert!(
            invalid_altitudes.validate().is_err(),
            "an unordered alpine transition must fail closed"
        );
    }

    // ---- Authored river-kind priority (elevated lake > water body > river
    // channel > nothing) ----

    fn river_kind_inputs() -> AuthoredRiverKindInputs {
        AuthoredRiverKindInputs {
            region_id: Some(CROMATOLIS_V0_REGION_ID),
            is_elevated_lake: false,
            is_water_body: false,
            is_river_channel: false,
            channel_fits_max_river_width: true,
            has_downhill: true,
            is_ocean: false,
            alt_below_sea_level: false,
            neighbor_pass_pos: Vec2::new(3, 4),
            authored_river_cross_section: Vec2::new(1.0, 2.0),
        }
    }

    #[test]
    fn elevated_lake_mask_wins_over_water_and_river_masks() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_elevated_lake: true,
            is_water_body: true,
            is_river_channel: true,
            is_ocean: true,
            alt_below_sea_level: true,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::new(3, 4)
            })
        );
    }

    #[test]
    fn water_mask_connected_to_the_sea_becomes_ocean() {
        let ocean_connected = authored_river_kind_override(AuthoredRiverKindInputs {
            is_water_body: true,
            is_ocean: true,
            ..river_kind_inputs()
        });
        assert_eq!(ocean_connected, Some(RiverKind::Ocean));
    }

    /// COW-22 `C22-1c`: an inland body whose authored bed is painted below sea
    /// level but which the `get_oceans` flood fill never reaches is a lake,
    /// not ocean. Before this fix the deep middle of such a body came back
    /// `Ocean` while its shallower rim came back `Lake`.
    #[test]
    fn inland_water_below_sea_level_is_a_lake_not_ocean_in_cromatolis() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_water_body: true,
            is_ocean: false,
            alt_below_sea_level: true,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::new(3, 4)
            })
        );
    }

    /// ...but the fix is scoped to Cromatolis, so any other authored region
    /// keeps the previous altitude-based behaviour until it is measured on its
    /// own data (same COW-2 debt scoping as `get_biome`'s `Snowland`/`Desert`
    /// checks).
    #[test]
    fn inland_water_below_sea_level_still_becomes_ocean_for_other_regions() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            region_id: Some("some_other_region"),
            is_water_body: true,
            is_ocean: false,
            alt_below_sea_level: true,
            ..river_kind_inputs()
        });
        assert_eq!(kind, Some(RiverKind::Ocean));
    }

    #[test]
    fn water_mask_above_sea_level_and_not_ocean_connected_becomes_lake() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_water_body: true,
            is_ocean: false,
            alt_below_sea_level: false,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::new(3, 4)
            })
        );
    }

    /// COW-22 `C22-1b`: the corridor mask is checked before the broader `water`
    /// mask it is a subset of. While the order was the other way round, no
    /// chunk on an authored map could resolve to `RiverKind::River` at all.
    #[test]
    fn a_narrow_river_channel_outranks_the_water_mask_it_sits_inside() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            is_water_body: true,
            channel_fits_max_river_width: true,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::River {
                cross_section: Vec2::new(1.0, 2.0)
            })
        );
    }

    /// ...but only where the channel is narrow enough to carve. A wider
    /// corridor stays a lake physically (no water walls); `WaterBodyKind` is
    /// what still calls it a river.
    #[test]
    fn a_wide_river_channel_stays_a_lake_physically() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            is_water_body: true,
            channel_fits_max_river_width: false,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::new(3, 4)
            })
        );
    }

    /// A corridor chunk the `get_oceans` flood fill reaches is still a river
    /// rather than ocean -- otherwise `WaterBodyKind::River` and `RiverKind`
    /// would disagree about the river mouth in a way nothing downstream
    /// expects -- as long as it has somewhere to flow.
    #[test]
    fn a_river_channel_outranks_ocean_connectivity() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            is_water_body: true,
            is_ocean: true,
            ..river_kind_inputs()
        });
        assert!(matches!(kind, Some(RiverKind::River { .. })), "{kind:?}");
    }

    /// ...but a corridor chunk with *no* downhill neighbour must never be
    /// typed as a river: `column.rs` panics outright ("How can a river have no
    /// downhill?") when it samples one. No chunk on the shipped raster is in
    /// that state, so this guards against a future river-mouth authoring edit
    /// crashing terrain generation rather than failing a test.
    #[test]
    fn a_river_channel_with_no_downhill_is_not_carved_as_a_river() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            is_water_body: true,
            is_ocean: true,
            has_downhill: false,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::new(3, 4)
            })
        );
    }

    #[test]
    fn no_mask_flagged_leaves_the_chunk_dry() {
        assert_eq!(authored_river_kind_override(river_kind_inputs()), None);
    }

    // ---- Smoke test: instantiate the Cromatolis world from scratch ----

    fn generate_cromatolis_world() -> WorldSim {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        WorldSim::generate(
            0,
            WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        )
    }

    /// COW-18.3 phase A discovery probe. It deliberately records generated
    /// facts and the exact server RGBA, rather than interpreting a TIFF or a
    /// client screenshot. Once these canonical positions are reviewed, this
    /// becomes the fixed eight-zone regression table.
    #[test]
    #[ignore]
    fn cromatolis_cartographic_baseline_discovery_against_real_lfs_assets() {
        use crate::index::{Index, IndexOwned};

        let sim = generate_cromatolis_world();
        let size = sim.map_size_lg();
        let index = IndexOwned::new(Index::new(0));
        let map = sim.get_map(index.as_index_ref(), None);
        let mut map_config = MapConfig::orthographic(
            sim.map_size_lg(),
            CONFIG.sea_level..=CONFIG.sea_level + sim.max_height,
        );
        map_config.is_shaded = false;
        let profile = sim
            .authored_ground_cover_profile
            .as_ref()
            .expect("real Cromatolis carries its ground-cover profile");
        let is_dry = |chunk: &SimChunk| chunk.river.river_kind.is_none();
        let locate = |name: &str, predicate: &dyn Fn(Vec2<i32>, &SimChunk) -> bool| {
            let pos = (0..size.chunks_len())
                .map(|idx| uniform_idx_as_vec2(size, idx))
                .find(|&pos| predicate(pos, sim.get(pos).expect("in-bounds chunk")))
                .unwrap_or_else(|| panic!("no real Cromatolis chunk matched baseline zone {name}"));
            let chunk = sim.get(pos).unwrap();
            let rgba = map.rgba[pos].to_le_bytes();
            let flipped_y = Vec2::new(pos.x, size.chunks().y as i32 - 1 - pos.y);
            let rgba_flipped_y = map.rgba[flipped_y].to_le_bytes();
            let mut samples = Vec::with_capacity(size.chunks_len());
            samples.resize_with(size.chunks_len(), || None);
            let column = ColumnGen::new(&sim).get((
                pos * TerrainChunkSize::RECT_SIZE.map(|edge| edge as i32),
                index.as_index_ref(),
                None,
            ));
            let column_alt = column.as_ref().map(|sample| sample.alt);
            let column_water = column.as_ref().map(|sample| sample.water_level);
            let column_surface = column.as_ref().map(|sample| sample.surface_color);
            let column_surface_is_physical =
                column.as_ref().map(|sample| sample.surface_is_physical);
            if let Some(column) = column {
                samples[vec2_as_uniform_idx(size, pos)] = Some(column);
            }
            let direct =
                sample_pos(&map_config, &sim, index.as_index_ref(), Some(&samples), pos).rgb;
            println!(
                "{name}: pos=({},{}) relief={:.1} alt_internal={:.1} water_alt={:.1} cover={:.3} \
                 density={:.3} temp={:.3} authored={} column_alt={:?} column_water={:?} \
                 column_surface={:?} column_physical={:?} biome={:?} river={:?} water={:?} \
                 substrate={:?} snowland={} direct=#{:02x}{:02x}{:02x} \
                 rgba=#{:02x}{:02x}{:02x}{:02x} rgba_flipped_y=#{:02x}{:02x}{:02x}{:02x}",
                pos.x,
                pos.y,
                chunk.alt - CONFIG.sea_level,
                chunk.alt,
                chunk.water_alt,
                chunk.ground_cover,
                chunk.tree_density,
                chunk.temp,
                chunk.authored_cromatolis_v0,
                column_alt,
                column_water,
                column_surface,
                column_surface_is_physical,
                chunk.get_biome(),
                chunk.river.river_kind,
                chunk.water_body,
                chunk.ground_substrate,
                chunk.authored_alpine_snowland,
                direct.r,
                direct.g,
                direct.b,
                rgba[0],
                rgba[1],
                rgba[2],
                rgba[3],
                rgba_flipped_y[0],
                rgba_flipped_y[1],
                rgba_flipped_y[2],
                rgba_flipped_y[3],
            );
        };

        locate("waning_moon", &|pos, chunk| {
            pos == Vec2::new(684, 599) && is_dry(chunk)
        });
        locate("central_grassland", &|pos, chunk| {
            (300..700).contains(&pos.x)
                && (350..700).contains(&pos.y)
                && is_dry(chunk)
                && profile.classify(chunk.ground_cover) == GroundCoverBand::Grassland
        });
        locate("southern_jungle", &|pos, chunk| {
            pos.y < 450 && is_dry(chunk) && chunk.get_biome() == BiomeKind::Jungle
        });
        locate("wetland", &|_pos, chunk| {
            is_dry(chunk) && chunk.get_biome() == BiomeKind::Swamp
        });
        locate("north_interior", &|pos, chunk| {
            pos.y > 700
                && is_dry(chunk)
                && chunk.ground_substrate != Some(GroundSubstrate::Sand)
                && chunk.alt - CONFIG.sea_level < 500.0
                && chunk.get_biome() != BiomeKind::Swamp
                && chunk.ground_cover < 0.30
                && chunk.tree_density < 0.45
        });
        locate("northwall_exterior", &|_pos, chunk| {
            chunk.ground_substrate == Some(GroundSubstrate::Sand)
        });
        locate("alpine_transition", &|_pos, chunk| {
            is_dry(chunk) && (700.0..1010.0).contains(&(chunk.alt - CONFIG.sea_level))
        });
        locate("persistent_snow", &|pos, chunk| {
            (4..1020).contains(&pos.x)
                && (4..1020).contains(&pos.y)
                && is_dry(chunk)
                && chunk.alt - CONFIG.sea_level >= 1010.0
        });
    }

    /// Guards the authored map-only layer against an LFS pointer, a bad
    /// exporter interpolation, or an accidental second Y flip. These points
    /// are the COW-18.4 review anchors for the named organic envelopes; tree
    /// density and physical biome deliberately remain outside this map-only
    /// contract.
    #[test]
    #[ignore]
    fn cromatolis_ecology_zone_contract_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let layer = sim
            .authored_ecology_zone_layer
            .as_ref()
            .expect("real Cromatolis must load its categorical ecology layer");
        assert_eq!(layer.len(), sim.map_size_lg().chunks_len());
        assert!(AuthoredEcologyZone::validate_layer(layer).is_ok());
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(710, 690)),
            Some(AuthoredEcologyZone::TemperateForest),
            "the top of Waning Moon must remain an authored forest mass"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(730, 476)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Waning Moon's descending lower lobe must survive the export"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(725, 876)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Sourcil must surround Pleasant Loch on its northern side"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(860, 833)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Sourcil must surround Pleasant Loch on its eastern side"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(285, 616)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Silver Forest must extend west of Sapphire Loch"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(460, 523)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Silver Forest must extend east of Sapphire Loch"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(485, 556)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Moon Forest must extend west of Moon Lake"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(580, 476)),
            Some(AuthoredEcologyZone::TemperateForest),
            "Moon Forest must extend east of Moon Lake"
        );
        assert_ne!(
            sim.authored_ecology_zone_at(Vec2::new(450, 356)),
            Some(AuthoredEcologyZone::Jungle),
            "Greenlife must not leak north into the Red Peaks"
        );
        assert_eq!(
            sim.authored_ecology_zone_at(Vec2::new(600, 143)),
            Some(AuthoredEcologyZone::Jungle),
            "Greenlife must remain present south of its Mazon-Tathune boundary"
        );
    }

    /// Always runs (no `#[ignore]`, unlike the real-data regression below):
    /// must not panic and must produce a full chunk grid, whether or not the
    /// real (LFS-hosted) Cromatolis assets are actually available. CI never
    /// pulls LFS, so `FileOpts::try_load_map` gracefully degrades to
    /// procedural generation instead of failing (see `FileOpts::load_content`
    /// / `try_load_map`), and every authored-layer load degrades the same way
    /// via `load_authored_layer`'s `Err` arm.
    #[test]
    fn cromatolis_world_generates_without_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        assert_eq!(sim.chunks.len(), map_size_lg.chunks_len());
    }

    /// Regression for `cromatolis_baseline_temp` replacing the old hard
    /// two-value clamp (`-1.0` above 970 m of relief, `0.55` at or below
    /// it): sampling a spread of altitudes must produce a real, continuous
    /// gradient of distinct values, not just those two fixed points.
    #[test]
    fn cromatolis_baseline_temp_is_continuous_not_two_fixed_points() {
        let climate = AuthoredCromatolisClimate::default();
        let samples: Vec<f32> = (0..=2000)
            .step_by(20)
            .map(|alt_pre| {
                cromatolis_baseline_temp(
                    alt_pre as f32,
                    climate.fallback_sea_level_temp_c,
                    climate.lapse_rate_c_per_m,
                )
            })
            .collect();

        let distinct_values = samples
            .iter()
            .map(|t| (t * 1_000_000.0).round() as i64)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            distinct_values.len() > 10,
            "expected a continuous gradient of distinct temperatures across altitude, got only {} \
             distinct value(s): {samples:?}",
            distinct_values.len()
        );

        // Neither of the two old fixed points should be the *only* values
        // produced -- some samples must land strictly between them.
        let strictly_between_old_extremes = samples.iter().any(|&t| t > -1.0 && t < 0.55);
        assert!(
            strictly_between_old_extremes,
            "expected some altitude to produce a temperature strictly between the old clamp's two \
             fixed points (-1.0 and 0.55), got: {samples:?}"
        );
    }

    /// The curve must cool monotonically with altitude (colder higher up),
    /// matching the pre-existing intent the old hard clamp also expressed.
    #[test]
    fn cromatolis_baseline_temp_decreases_monotonically_with_altitude() {
        let climate = AuthoredCromatolisClimate::default();
        let mut prev = cromatolis_baseline_temp(
            -100.0,
            climate.fallback_sea_level_temp_c,
            climate.lapse_rate_c_per_m,
        );
        for alt_pre in (0..3000).step_by(10) {
            let temp = cromatolis_baseline_temp(
                alt_pre as f32,
                climate.fallback_sea_level_temp_c,
                climate.lapse_rate_c_per_m,
            );
            assert!(
                temp <= prev,
                "temperature must never increase with altitude: alt_pre={alt_pre} gave {temp}, \
                 previous (lower) altitude gave {prev}"
            );
            prev = temp;
        }
    }

    /// Must never panic and must always stay within the abstract scale's
    /// normal `[-1.0, 1.0]` range, even for extreme or non-finite input.
    #[test]
    fn cromatolis_baseline_temp_never_panics_and_stays_in_abstract_range() {
        let climate = AuthoredCromatolisClimate::default();
        for &alt_pre in &[
            f32::MIN,
            f32::MAX,
            -1_000_000.0,
            -1.0,
            0.0,
            1_000_000.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let temp = cromatolis_baseline_temp(
                alt_pre,
                climate.fallback_sea_level_temp_c,
                climate.lapse_rate_c_per_m,
            );
            assert!(
                temp.is_finite(),
                "alt_pre={alt_pre} produced non-finite temp {temp}"
            );
            assert!(
                (-1.0..=1.0).contains(&temp),
                "alt_pre={alt_pre} produced out-of-range temp {temp}"
            );
        }
    }

    /// Regression for the `Desert` branch of `get_biome` staying scoped away
    /// from Cromatolis (same pattern as the pre-existing `Snowland` scoping
    /// immediately above it): `cromatolis_baseline_temp`'s hot coastal end
    /// (needed so some chunks reach the `(0.9..1.0)` band several
    /// dungeon-site predicates require) drives real low-altitude coastal
    /// humidity toward 0 via the evaporation dampener, which would
    /// otherwise satisfy `Desert`'s `temp > CONFIG.desert_temp && humidity <
    /// CONFIG.desert_hum` check on real tropical/Caribbean coastline.
    /// Without the scoping fix this found zero (not "rare but present" like
    /// `Swamp`) `Desert` chunks in the real generated grid -- Cromatolis is
    /// lore-authored to have none at all.
    #[test]
    #[ignore]
    fn cromatolis_desert_biome_stays_scoped_out_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let desert_count = sim
            .chunks
            .iter()
            .filter(|c| c.get_biome() == BiomeKind::Desert)
            .count();
        assert_eq!(
            desert_count, 0,
            "expected zero Desert-biome chunks in real Cromatolis terrain (lore-authored as \
             tropical/caribbean, no real desert) -- got {desert_count}"
        );
    }

    /// Regression for `crate::layer::wildlife::not_cromatolis` gating the
    /// desert wildlife density formulas away from Cromatolis. Reproduces the
    /// `world.wildlife.spawn.desert.hot` window (`close(chunk.temp,
    /// CONFIG.desert_temp + 0.2, 0.3)`, no humidity check) and asserts it is
    /// zero across the real generated map.
    ///
    /// What the gate holds back changed with the per-zone climate, so the
    /// bound on the ungated population is asserted too. It was ~86% of the
    /// grid when one flat hot baseline covered the whole map; now no climatic
    /// zone reaches the window at all and the only chunks inside it are the
    /// authored microclimate pocket, which is hot on purpose. A *small*
    /// nonzero ungated count is therefore the correct answer -- zero would
    /// mean the pocket stopped being hot, and a large one would mean a zone
    /// anchor had been raised into desert territory.
    #[test]
    #[ignore]
    fn cromatolis_desert_wildlife_density_stays_gated_out_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let in_window = |c: &&SimChunk| (c.temp - (CONFIG.desert_temp + 0.2)).abs() < 0.3;
        let ungated_hits = sim.chunks.iter().filter(in_window).count();
        let gated_hits = sim
            .chunks
            .iter()
            .filter(|c| crate::layer::wildlife::not_cromatolis(c) > 0.0 && in_window(c))
            .count();
        let ungated_fraction = ungated_hits as f64 / sim.chunks.len() as f64;
        assert!(
            (0.0001..0.01).contains(&ungated_fraction),
            "the ungated desert window should cover only the authored microclimate pocket -- \
             expected a small nonzero fraction, got {ungated_fraction:.6} ({ungated_hits}/{})",
            sim.chunks.len()
        );
        assert_eq!(
            gated_hits, 0,
            "expected the Cromatolis-exclusion gate to zero out every hit of the desert wildlife \
             density formula in real Cromatolis terrain, got {gated_hits}"
        );
    }

    /// Requires the real Cromatolis LFS assets to be pulled locally (`git lfs
    /// pull` against the VPS store); not run automated, matching
    /// `site::economy::context::tests::test_economy0`/`test_economy1`'s
    /// precedent in this crate for tests whose meaningful assertion depends
    /// on real, environment-specific data rather than synthetic input.
    /// Recommended command: `cargo test
    /// cromatolis_world_orientation_regression_against_real_lfs_assets --
    /// --ignored`
    #[test]
    #[ignore]
    fn cromatolis_world_orientation_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();

        // Regression-test the Y-orientation end to end: chunks the loaded
        // `.bin` puts below sea level should overwhelmingly agree with the
        // authored water mask sampled through the exact single-flip
        // convention `routes`/`vegetation` already use (see
        // `authored_layer_idx_for_cromatolis_v0`). A doubled or missing flip
        // would show up as these two independently-sourced signals being
        // essentially uncorrelated instead of in strong agreement.
        let water = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_water")
            .expect("real Cromatolis LFS assets must be pulled locally to run this test");
        assert_eq!(water.values.len(), map_size_lg.chunks_len());

        let mut agree = 0usize;
        for (idx, chunk) in sim.chunks.iter().enumerate() {
            let mask_idx = authored_layer_idx_for_cromatolis_v0(map_size_lg, idx);
            let is_water_mask = water.values[mask_idx] >= 0.5;
            let is_below_sea_level = chunk.alt < 0.0;
            if is_water_mask == is_below_sea_level {
                agree += 1;
            }
        }
        let agreement = agree as f64 / sim.chunks.len() as f64;
        assert!(
            agreement > 0.9,
            "authored water mask and loaded terrain altitude disagree on {:.1}% of chunks; \
             suspect a Y-orientation bug (north=up, west=left) in the water loader (expected > \
             90% agreement, got {:.1}%)",
            (1.0 - agreement) * 100.0,
            agreement * 100.0
        );

        // Coarse, independent sanity bound: Cromatolis should be neither "all
        // ocean/lake" nor "no water at all" -- catches a Y-flip broken badly
        // enough to invert the whole map (which the agreement check above
        // would also catch), cheaply and orthogonally.
        let water_fraction = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.river.is_ocean() || chunk.river.is_lake())
            .count() as f64
            / sim.chunks.len() as f64;
        assert!(
            (0.02..0.75).contains(&water_fraction),
            "unexpected authored water coverage fraction: {water_fraction:.3}"
        );
    }

    /// Requires the real Cromatolis LFS assets. The authored biome-mask
    /// scale is a design contract: for land below the hard altitude cap in a
    /// climate above the regional tree threshold, the raw mask value must
    /// arrive unchanged as the
    /// generated chunk's `tree_density`. This catches response curves,
    /// density floors, and hidden gradual altitude attenuation.
    #[test]
    #[ignore]
    fn cromatolis_biome_mask_density_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        let vegetation = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_vegetation")
            .expect("real Cromatolis LFS assets must be pulled locally to run this test");
        let climate = AuthoredCromatolisClimate::load_owned("world.map.cromatolis_v0_climate")
            .expect("Cromatolis climate asset must load for the biome-mask regression");
        let alpine = AuthoredAlpinePolicy::load_owned("world.map.cromatolis_v0_alpine")
            .expect("Cromatolis alpine policy asset must load for the biome-mask regression");
        assert_eq!(vegetation.values.len(), map_size_lg.chunks_len());

        let mut checked = 0usize;
        for (idx, chunk) in sim.chunks.iter().enumerate() {
            let underwater = matches!(
                chunk.river.river_kind,
                Some(RiverKind::Ocean) | Some(RiverKind::Lake { .. })
            );
            let alt_pre = chunk.alt - CONFIG.sea_level;
            if underwater
                || chunk.temp < climate.tree_min_temp
                || alt_pre >= alpine.tree_line_altitude_m
            {
                continue;
            }

            let expected = vegetation.values
                [authored_layer_idx_for_cromatolis_v0(map_size_lg, idx)]
            .clamp(0.0, 1.0);
            assert!(
                (chunk.tree_density - expected).abs() <= f32::EPSILON,
                "chunk {idx} changed its authored vegetation density: mask={expected:.6}, \
                 tree_density={:.6}, alt_pre={alt_pre:.1}, temp={:.3}",
                chunk.tree_density,
                chunk.temp,
            );
            checked += 1;
        }

        assert!(
            checked > 100_000,
            "expected a substantial sample of temperate, above-water Cromatolis chunks, got \
             {checked}"
        );
    }

    /// Requires the real Cromatolis LFS assets. The alpine contract is in
    /// authored real metres: trees stop exactly at 700m, Snowland begins at
    /// 700m, and the world contains a real persistent-snow band at 1010m.
    #[test]
    #[ignore]
    fn cromatolis_alpine_policy_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let alpine = AuthoredAlpinePolicy::load_owned("world.map.cromatolis_v0_alpine")
            .expect("Cromatolis alpine policy asset must load for alpine regression");
        assert_eq!(alpine.tree_line_altitude_m, 700.0);
        assert_eq!(alpine.snow_start_altitude_m, 700.0);
        assert_eq!(alpine.persistent_snow_altitude_m, 1010.0);

        let mut transition_chunks = 0usize;
        let mut persistent_chunks = 0usize;
        let mut below_transition_chunks = 0usize;
        for chunk in &sim.chunks {
            if chunk.is_underwater() {
                continue;
            }
            let alt_pre = chunk.alt - CONFIG.sea_level;
            if alt_pre >= alpine.tree_line_altitude_m {
                assert_eq!(
                    chunk.tree_density, 0.0,
                    "tree density survives at {alt_pre:.1}m despite Cromatolis's 700m tree line"
                );
            }
            if alt_pre >= alpine.snow_start_altitude_m {
                transition_chunks += 1;
                assert_eq!(
                    chunk.get_biome(),
                    BiomeKind::Snowland,
                    "Cromatolis must resolve Snowland from its authored alpine threshold at \
                     {alt_pre:.1}m"
                );
            }
            if alt_pre < alpine.snow_start_altitude_m {
                below_transition_chunks += 1;
                assert!(
                    !chunk.authored_alpine_snowland,
                    "Cromatolis lowland was incorrectly marked alpine at {alt_pre:.1}m"
                );
            }
            if alt_pre >= alpine.persistent_snow_altitude_m {
                persistent_chunks += 1;
            }
        }
        assert!(
            transition_chunks > 0,
            "real Cromatolis must contain a 700m alpine transition"
        );
        assert!(
            persistent_chunks > 0,
            "real Cromatolis must contain persistent-snow terrain"
        );
        assert!(
            below_transition_chunks > 0,
            "real Cromatolis must contain land below the alpine transition"
        );
    }

    /// Requires the real Cromatolis LFS assets. Waning Moon's authored mask
    /// was measured at 0.890196 tree density in both probes; its sparse
    /// appearance came from the global 24/10 root lattice offering just one
    /// and two candidates. The regional 12/5 lattice must supply at least
    /// four unique roots inside each measured 32×32 block chunk before the
    /// unchanged water/path/cave/density filters run.
    #[test]
    #[ignore]
    fn cromatolis_waning_moon_policy_offers_dense_root_candidates_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let chunk_size = TerrainChunkSize::RECT_SIZE.as_::<i32>();

        for chunk_pos in [Vec2::new(684, 599), Vec2::new(675, 587)] {
            let min = chunk_pos * chunk_size;
            let max = min + chunk_size;
            let root_count = sim
                .tree_candidate_fields_in_area(min, max)
                .into_iter()
                .filter(|(root, _)| {
                    root.x >= min.x && root.x < max.x && root.y >= min.y && root.y < max.y
                })
                .count();
            assert!(
                root_count >= 4,
                "{chunk_pos:?} must offer at least four unique tree roots before placement \
                 filters, got {root_count}"
            );
        }
    }

    /// Requires the real Cromatolis LFS assets. These hand-picked source-map
    /// positions are temperate, dry land below the preview's mountain cutoff
    /// and away from authored water. They pin the complete authored contract:
    /// independent raster luminance -> generated ground cover -> shared cover
    /// band -> preview, while vegetation remains the separate tree contract.
    #[test]
    #[ignore]
    fn cromatolis_ground_cover_profile_matches_real_lfs_raster_and_preview() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        let vegetation = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_vegetation")
            .expect("real Cromatolis LFS assets must be pulled locally to run this test");
        let ground_cover = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_ground_cover")
            .expect("real Cromatolis LFS assets must be pulled locally to run this test");
        let climate = AuthoredCromatolisClimate::load_owned("world.map.cromatolis_v0_climate")
            .expect("Cromatolis climate asset must load for the biome-mask regression");
        let alpine = AuthoredAlpinePolicy::load_owned("world.map.cromatolis_v0_alpine")
            .expect("Cromatolis alpine policy asset must load for the preview regression");
        let profile = sim.authored_ground_cover_profile.as_ref().expect(
            "generated Cromatolis WorldSim must retain its configured ground-cover profile",
        );
        let representatives = [
            (Vec2::new(476, 13), 22.0 / 255.0, GroundCoverBand::BareDry),
            (Vec2::new(213, 43), 58.0 / 255.0, GroundCoverBand::Grassland),
            (Vec2::new(199, 44), 46.0 / 255.0, GroundCoverBand::Grassland),
            (Vec2::new(201, 43), 40.0 / 255.0, GroundCoverBand::Grassland),
            (Vec2::new(204, 43), 57.0 / 255.0, GroundCoverBand::Grassland),
        ];
        let mut differs_from_vegetation = false;

        for (position, expected_cover, expected_band) in representatives {
            let chunk_idx = vec2_as_uniform_idx(map_size_lg, position);
            let chunk = &sim.chunks[chunk_idx];
            let alt_pre = chunk.alt - CONFIG.sea_level;
            assert_eq!(
                chunk.authored_region_id,
                Some(CROMATOLIS_V0_REGION_ID),
                "{position:?} must remain an authored Cromatolis chunk"
            );
            assert!(
                chunk.river.river_kind.is_none(),
                "{position:?} must stay dry land"
            );
            assert!(
                chunk.temp >= climate.tree_min_temp,
                "{position:?} must stay temperate"
            );
            assert!(
                alt_pre < 294.0,
                "{position:?} must stay below preview mountain treatment"
            );
            assert!(
                alt_pre < alpine.tree_line_altitude_m,
                "{position:?} must stay below the tree altitude cap"
            );

            let vegetation_luminance = vegetation.values
                [authored_layer_idx_for_cromatolis_v0(map_size_lg, chunk_idx)]
            .clamp(0.0, 1.0);
            assert_eq!(
                chunk.tree_density, vegetation_luminance,
                "{position:?} changed its authored density"
            );
            let cover_luminance = ground_cover.values
                [authored_layer_idx_for_cromatolis_v0(map_size_lg, chunk_idx)]
            .clamp(0.0, 1.0);
            assert!(
                (cover_luminance - expected_cover).abs() < f32::EPSILON,
                "{position:?} changed in the shipped ground-cover raster; this pins its source \
                 orientation and export values"
            );
            assert_eq!(
                chunk.ground_cover, cover_luminance,
                "{position:?} changed its independent authored ground cover"
            );
            differs_from_vegetation |=
                (cover_luminance - vegetation_luminance).abs() > f32::EPSILON;

            let (preview_band, _) = map::authored_ground_cover_preview_tint(
                Rgb::new(0x80, 0x80, 0x80),
                Some(profile),
                chunk.ground_cover,
                chunk.ground_substrate,
                false,
                false,
            );
            assert_eq!(
                preview_band,
                Some(expected_band),
                "{position:?} preview drifted from its independent cover layer"
            );
        }
        assert!(
            differs_from_vegetation,
            "the real ground-cover probes must prove this layer is not an alias for vegetation"
        );
    }

    /// COW-17 binds the terrain `.bin` and river-channel raster into one
    /// reviewed package. Belletoile is a dry authored lowland: the v22
    /// terrain master samples it at about 92.94 m above the external sea
    /// level. The real WorldSim interpolation yields about 96.82 m at the
    /// matching chunk, while the paired river raster retains all 33,566
    /// binary corridor cells.
    ///
    /// ⚠️ The cell count was **28,200** until COW-22 `[OQ3]`
    /// (`xindeler-open-world#30`) rebuilt the classification. The old raster
    /// was "painted water above sea level that is not a closed, non-boundary-
    /// touching basin", which could never type an outflowing lake as a lake
    /// (every Cromatolis river reaches the sea, so every such lake shares a
    /// connected component with the boundary-touching ocean) and split any
    /// body whose bed is painted below sea level into an "ocean" core and a
    /// "channel" rim. The layer is now a shape-derived corridor mask:
    /// 4,563 shipped cells moved out to standing water and 9,490 below-sea-
    /// level corridor cells moved in. `28_200` must never come back — it is
    /// the pre-COW-22 raster, exactly as `1_876` was the pre-COW-17 one.
    ///
    /// ⚠️ The count moved again, 33,127 → 33,566 (net +439), with COW-22
    /// `[C22-3]` (`xindeler-open-world#31`), and this is expected rather than
    /// a drift: `classify_authored_water()` reads *elevation*, so reshaping
    /// the seabed necessarily re-votes cells near the coast. `C22-3` raised
    /// the marine shelf, which moves some shallow river-mouth water across
    /// the ocean/inland boundary the classifier draws at `alt <= 0` (note:
    /// the exporter's boundary is `<=`, unlike the in-engine
    /// `alt_below_sea_level` check a few hundred lines up, which is strict
    /// `<` — two different functions in two different codebases, not a typo
    /// in either). Only the net delta was measured for this revision, not a
    /// full in/out cell-migration breakdown like the `[OQ3]` paragraph above
    /// gives — if a future revision needs to re-verify this number, re-run
    /// the classifier before/after and diff the raw cell sets rather than
    /// trusting the net alone. Belletoile is inland and its relief is
    /// bit-identical before and after (96.817 m), so this assertion still
    /// pins the terrain package as tightly as it did.
    ///
    /// Requires the real LFS assets, so CI without the VPS asset store skips
    /// it just like the other real-Cromatolis regressions in this module.
    #[test]
    #[ignore]
    fn cromatolis_v22_relief_and_river_package_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        let source_x = 499;
        let source_y = 476;
        let engine_y = i32::from(map_size_lg.chunks().y) - 1 - source_y;
        let probe_idx = vec2_as_uniform_idx(map_size_lg, Vec2::new(source_x, engine_y));
        let probe = &sim.chunks[probe_idx];
        let alt_pre = probe.alt - CONFIG.sea_level;

        // The package identity, pinned on the *unwarped* column. `basement`
        // comes straight from the bundle's elevation layer and, while it sits
        // below `alt`, nothing downstream of worldgen's noise touches it -- so
        // it moves if and only if the terrain package itself does. There is
        // ~13.8 m of headroom before it would start clamping to `alt` (the
        // soil term below can add at most 16 m), which is comfortable but not
        // unlimited: a future package that raises this probe would need this
        // assertion rechecked rather than just re-baselined.
        let basement_pre = probe.basement - CONFIG.sea_level;
        assert!(
            (84.5..=85.5).contains(&basement_pre),
            "Belletoile must come from the v22 terrain package; expected ~84.939 m of bundle \
             elevation, got {basement_pre:.3} m"
        );

        // ⚠️ The surface figure was **96.817 m** until the climate-zone rework.
        // The terrain package did not move -- `basement` above proves it. The
        // difference is `SimChunk::generate`'s soil-undulation term, which adds
        // `soil_nz * 16 * sqrt(tree_density) * sqrt(humidity)` to dry land.
        // Per-zone temperatures stopped the evaporation dampener from crushing
        // humidity (this probe now measures 1.000), so the same terrain carries
        // ~1.9 m more of it. Any future change that moves humidity or
        // `tree_density` moves this number too; re-measure it against
        // `basement` rather than widening the band.
        assert!(
            (98.0..=99.5).contains(&alt_pre),
            "Belletoile surface relief expected ~98.720 m above sea level after WorldSim \
             interpolation and soil undulation, got {alt_pre:.3} m"
        );
        assert!(
            probe.river.river_kind.is_none(),
            "Belletoile probe is authored dry land, not a river/lake/ocean chunk"
        );

        let river_channels = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_river_channels")
            .expect("real Cromatolis LFS assets must include the river-channel raster");
        assert_eq!(river_channels.values.len(), map_size_lg.chunks_len());
        assert!(
            river_channels
                .values
                .iter()
                .all(|value| *value == 0.0 || *value == 1.0),
            "river-channel raster must remain binary"
        );
        assert_eq!(
            river_channels
                .values
                .iter()
                .filter(|value| **value == 1.0)
                .count(),
            33_566,
            "v22 terrain must never be paired with the obsolete 1,876-cell, 28,200-cell or \
             33,127-cell river raster"
        );

        // The containment `authored_river_kind_override`'s and
        // `authored_water_body_kind`'s priority chains both depend on: every
        // corridor cell is also a water-mask cell. That is why COW-22 `C22-1b`
        // had to check the corridor mask *before* the water mask -- while the
        // water arm ran first it swallowed every corridor cell and the
        // `is_river_channel` arm was unreachable. If a future raster ever
        // paints a corridor outside the water mask, both chains need
        // re-checking rather than silently taking the corridor branch.
        let water = AuthoredF32Layer::load_owned("world.map.cromatolis_v0_water")
            .expect("real Cromatolis LFS assets must include the water raster");
        assert_eq!(water.values.len(), river_channels.values.len());
        assert!(
            river_channels
                .values
                .iter()
                .zip(water.values.iter())
                .all(|(channel, water)| *channel < AUTHORED_WATER_THRESHOLD
                    || *water >= AUTHORED_WATER_THRESHOLD),
            "every authored river-corridor cell must also be inside the authored water mask"
        );
    }

    /// Requires the real Cromatolis LFS assets, same precedent as
    /// `cromatolis_world_orientation_regression_against_real_lfs_assets`
    /// above. Sanity-bounds `Swamp` coverage (re-enabled by this row, COW-4)
    /// against the real hydrology/vegetation data `SWAMP_HUMIDITY_THRESHOLD`
    /// was tuned against, so a future change to that threshold or to the
    /// humidity formula that silently makes `Swamp` swallow the map (or
    /// vanish again) gets caught here instead of only by eyeballing a
    /// generated world.
    #[test]
    #[ignore]
    fn cromatolis_swamp_coverage_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let swamp_fraction = sim
            .chunks
            .iter()
            .filter(|c| c.get_biome() == BiomeKind::Swamp)
            .count() as f64
            / sim.chunks.len() as f64;
        assert!(
            (0.001..0.05).contains(&swamp_fraction),
            "unexpected Swamp coverage fraction: {swamp_fraction:.4} (expected a rare but present \
             biome, not ~0% or a large chunk of the map)"
        );
    }

    // ---- COW-22 `C22-1b`: authored water-body classification ----

    fn water_body_inputs() -> AuthoredWaterBodyInputs {
        AuthoredWaterBodyInputs {
            is_elevated_lake: false,
            is_water_body: false,
            is_river_channel: false,
            is_marine: false,
            is_adjacent_to_marine: false,
            is_shelf_sea: false,
        }
    }

    #[test]
    fn a_dry_chunk_has_no_water_body() {
        assert_eq!(authored_water_body_kind(water_body_inputs()), None);
    }

    #[test]
    fn the_elevated_lake_decree_outranks_every_shape_test() {
        let kind = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_elevated_lake: true,
            is_water_body: true,
            is_river_channel: true,
            is_marine: true,
            is_adjacent_to_marine: true,
            ..water_body_inputs()
        });
        assert_eq!(kind, Some(WaterBodyKind::Lake));
    }

    /// The corridor mask is a subset of the broader `water` mask, so it has to
    /// outrank it (and marine connectivity at a river mouth) or the 33,566
    /// authored corridor chunks would all classify as something else.
    #[test]
    fn a_river_channel_outranks_the_water_and_marine_masks() {
        let kind = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_river_channel: true,
            is_water_body: true,
            is_marine: true,
            is_adjacent_to_marine: true,
            ..water_body_inputs()
        });
        assert_eq!(kind, Some(WaterBodyKind::River));
    }

    #[test]
    fn marine_water_is_ocean_until_the_shelf_band_exists() {
        let open = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_water_body: true,
            is_marine: true,
            ..water_body_inputs()
        });
        assert_eq!(open, Some(WaterBodyKind::Ocean));

        // Nothing sets this yet; the taxonomy is wired for it so that
        // computing the offshore band is all it takes to produce a `Sea`.
        let shelf = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_water_body: true,
            is_marine: true,
            is_shelf_sea: true,
            ..water_body_inputs()
        });
        assert_eq!(shelf, Some(WaterBodyKind::Sea));
    }

    #[test]
    fn standing_water_is_a_lagoon_when_it_touches_marine_water() {
        let lagoon = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_water_body: true,
            is_adjacent_to_marine: true,
            ..water_body_inputs()
        });
        assert_eq!(lagoon, Some(WaterBodyKind::Lagoon));

        let lake = authored_water_body_kind(AuthoredWaterBodyInputs {
            is_water_body: true,
            ..water_body_inputs()
        });
        assert_eq!(lake, Some(WaterBodyKind::Lake));
    }

    // ---- Lagoon resolution is per-basin, not per-chunk ----

    /// Builds a 8 x 8 `water_body` grid from a picture: `.` dry, `L` standing
    /// water, `~` standing water the per-chunk sweep marked as touching marine
    /// water, `E` an elevated-lake chunk.
    fn water_body_grid(rows: [&str; 8]) -> (MapSizeLg, Vec<Option<WaterBodyKind>>, Vec<bool>) {
        let map_size_lg = MapSizeLg::new(Vec2 { x: 3, y: 3 }).expect("valid map size");
        let mut water_body = vec![None; map_size_lg.chunks_len()];
        let mut elevated = vec![false; map_size_lg.chunks_len()];
        for (y, row) in rows.iter().enumerate() {
            for (x, cell) in row.chars().enumerate() {
                let idx = vec2_as_uniform_idx(map_size_lg, Vec2::new(x as i32, y as i32));
                match cell {
                    '.' => {},
                    'L' => water_body[idx] = Some(WaterBodyKind::Lake),
                    '~' => water_body[idx] = Some(WaterBodyKind::Lagoon),
                    'E' => {
                        water_body[idx] = Some(WaterBodyKind::Lake);
                        elevated[idx] = true;
                    },
                    other => panic!("unexpected cell {other:?}"),
                }
            }
        }
        (map_size_lg, water_body, elevated)
    }

    fn resolved(rows: [&str; 8]) -> Vec<Option<WaterBodyKind>> {
        let (map_size_lg, mut water_body, elevated) = water_body_grid(rows);
        promote_lagoon_basins(map_size_lg, &mut water_body, |idx| elevated[idx]);
        water_body
    }

    fn cell(water_body: &[Option<WaterBodyKind>], x: i32, y: i32) -> Option<WaterBodyKind> {
        let map_size_lg = MapSizeLg::new(Vec2 { x: 3, y: 3 }).expect("valid map size");
        water_body[vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y))]
    }

    /// The middle of a lagoon is no less a lagoon for sitting a few chunks from
    /// the sea -- without this pass only the rim chunk the per-chunk sweep
    /// could see would be classified `Lagoon`.
    #[test]
    fn a_basin_touching_marine_water_is_a_lagoon_all_the_way_through() {
        let water_body = resolved([
            "........", "..~LLL..", "..LLLL..", "..LLLL..", "........", "........", "........",
            "........",
        ]);
        for (x, y) in [(2, 1), (5, 1), (2, 3), (5, 3)] {
            assert_eq!(
                cell(&water_body, x, y),
                Some(WaterBodyKind::Lagoon),
                "({x}, {y})"
            );
        }
    }

    #[test]
    fn a_landlocked_basin_stays_a_lake() {
        let water_body = resolved([
            "........", "..LLL...", "..LLL...", "........", "........", "........", "........",
            "........",
        ]);
        assert_eq!(cell(&water_body, 3, 2), Some(WaterBodyKind::Lake));
    }

    /// Two basins in the same map are classified independently -- a lagoon
    /// elsewhere on the coast must not drag a landlocked lake with it.
    #[test]
    fn separate_basins_are_classified_independently() {
        let water_body = resolved([
            "........", "~L....LL", ".L....LL", "........", "........", "........", "........",
            "........",
        ]);
        assert_eq!(cell(&water_body, 1, 2), Some(WaterBodyKind::Lagoon));
        assert_eq!(cell(&water_body, 6, 1), Some(WaterBodyKind::Lake));
    }

    /// Diagonal contact still makes one basin (8-connectivity, matching the
    /// neighbourhood the per-chunk adjacency test uses).
    #[test]
    fn basins_are_connected_diagonally() {
        let water_body = resolved([
            "........", "..~.....", "...L....", "........", "........", "........", "........",
            "........",
        ]);
        assert_eq!(cell(&water_body, 3, 2), Some(WaterBodyKind::Lagoon));
    }

    #[test]
    fn an_elevated_lake_chunk_is_never_promoted_to_a_lagoon() {
        let water_body = resolved([
            "........", "..~LE...", "........", "........", "........", "........", "........",
            "........",
        ]);
        assert_eq!(cell(&water_body, 3, 1), Some(WaterBodyKind::Lagoon));
        assert_eq!(cell(&water_body, 4, 1), Some(WaterBodyKind::Lake));
    }

    /// An elevated lake is water *above sea level*, so it is not a valid bridge
    /// either: the basin behind it stays landlocked rather than inheriting the
    /// coastal rim's lagoon-ness through it.
    #[test]
    fn an_elevated_lake_chunk_does_not_bridge_two_basins() {
        let water_body = resolved([
            "........", ".~LEL...", "........", "........", "........", "........", "........",
            "........",
        ]);
        assert_eq!(cell(&water_body, 2, 1), Some(WaterBodyKind::Lagoon));
        assert_eq!(cell(&water_body, 3, 1), Some(WaterBodyKind::Lake));
        assert_eq!(
            cell(&water_body, 4, 1),
            Some(WaterBodyKind::Lake),
            "the basin behind the elevated lake must stay landlocked"
        );
    }

    // ---- Authored river geometry ----

    #[test]
    fn channel_width_is_twice_the_distance_to_the_nearest_bank() {
        let chunk = TerrainChunkSize::RECT_SIZE.x as f32;
        assert_eq!(cromatolis_channel_width(1.0), 2.0 * chunk);
        assert_eq!(cromatolis_channel_width(3.0), 6.0 * chunk);
        assert_eq!(cromatolis_channel_width(0.0), 0.0);
    }

    #[test]
    fn river_cross_section_follows_the_configured_width_to_depth_ratio() {
        let cross_section = cromatolis_authored_river_cross_section(48.0);
        assert_eq!(cross_section.x, 48.0);
        assert_eq!(cross_section.y, 48.0 / CONFIG.river_width_to_depth);
    }

    #[test]
    fn river_cross_section_caps_at_the_max_river_width() {
        // Anything past the cap would overflow the chunks either side of the
        // channel and leave water walls, exactly as `get_rivers` guards
        // against for procedural rivers.
        let cross_section = cromatolis_authored_river_cross_section(4096.0);
        assert_eq!(cross_section.x, CROMATOLIS_MAX_RIVER_WIDTH);
        assert_eq!(
            cross_section.y,
            CROMATOLIS_MAX_RIVER_WIDTH / CONFIG.river_width_to_depth
        );
    }

    #[test]
    fn river_cross_section_never_goes_below_the_minimum_river_height() {
        let cross_section = cromatolis_authored_river_cross_section(0.0);
        assert_eq!(cross_section.y, CONFIG.river_min_height);
    }

    fn velocity_on_a_slope(drop: Alt, downhill: Option<(i32, i32)>) -> Vec3<f32> {
        let map_size_lg = MapSizeLg::new(Vec2 { x: 3, y: 3 }).expect("valid map size");
        let mut alt = vec![100.0; map_size_lg.chunks_len()];
        let posi = vec2_as_uniform_idx(map_size_lg, Vec2::new(4, 4));
        let downhill_idx = downhill.map_or(-1, |(x, y)| {
            let idx = vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y));
            alt[idx] = 100.0 - drop;
            idx as isize
        });
        cromatolis_authored_river_velocity(map_size_lg, posi, downhill_idx, &alt, 2.0)
    }

    #[test]
    fn an_authored_river_with_no_downhill_neighbour_does_not_flow() {
        assert_eq!(velocity_on_a_slope(10.0, None), Vec3::zero());
    }

    #[test]
    fn an_authored_river_on_flat_ground_does_not_flow() {
        assert_eq!(velocity_on_a_slope(0.0, Some((5, 4))), Vec3::zero());
    }

    #[test]
    fn an_authored_river_flows_downhill_at_the_manning_velocity() {
        let drop = 8.0;
        let velocity = velocity_on_a_slope(drop, Some((5, 4)));
        // Same formula as `erosion.rs`'s `get_rivers`: (1 / roughness) *
        // depth^(2/3) * sqrt(slope), over a one-chunk step.
        let slope = drop / TerrainChunkSize::RECT_SIZE.x as Alt;
        let expected =
            1.0 / CONFIG.river_roughness as Alt * (2.0 as Alt).powf(2.0 / 3.0) * slope.sqrt();
        assert!(
            (velocity.magnitude() as Alt - expected).abs() < 1e-3,
            "expected |v| ~ {expected}, got {}",
            velocity.magnitude()
        );
        assert!(velocity.x > 0.0, "should flow towards +x: {velocity:?}");
        assert_eq!(velocity.y, 0.0);
    }

    // ---- Real-LFS-asset regressions for COW-22 `C22-1b` / `C22-1c` ----
    //
    // The baseline the open-world exporter measured independently for COW-22
    // `[OQ3]`, stated once so the tests below derive from it instead of
    // restating it. A raster change means re-measuring *these*, not chasing
    // the same number through four assertions.
    //
    // Re-measured against the marine-shelf-and-beach terrain package: these are
    // the exporter's own `classify_authored_water` re-run over the current
    // masters it keeps (`heightmap_manual_l16.png`, `water_mask_manual.png`
    // and `elevated_lake_mask_manual.png`, none of which live in this repo),
    // not the engine's own numbers copied across -- which would have retired
    // the assertion rather than fixed it.
    //
    // How independent each one actually is, since it varies: the corridor mask
    // the exporter recomputes is byte-identical to the
    // `cromatolis_v0_river_channels.f32le` this crate loads, so
    // `EXPORTED_RIVER_CELLS` is one artifact counted twice -- it catches the
    // engine mis-reading the raster, not the two sides disagreeing about what
    // a river is. The marine/lagoon/lake split is the genuinely independent
    // part: two separate implementations of "which water is the sea, and which
    // standing water touches it", which is exactly the drift that would
    // silently mis-paint a coastline.

    /// Cells in the authored `water` mask.
    const AUTHORED_WATER_MASK_CELLS: usize = 366_935;
    /// Corridor cells, across 72 substantial river systems.
    const EXPORTED_RIVER_CELLS: usize = 33_566;
    /// Standing-water cells in the 24 basins that touch marine water.
    const EXPORTED_LAGOON_CELLS: usize = 2_815;
    /// Standing-water cells in the 14 landlocked basins.
    const EXPORTED_LAKE_CELLS: usize = 13_872;
    /// Marine cells: `water` mask ∧ the `get_oceans` flood fill, minus the
    /// corridor cells the river mask claims first.
    const EXPORTED_MARINE_CELLS: usize = 316_682;
    /// The `elevated_lakes` raster marks 267 cells but only 265 of them are
    /// inside the `water` mask. The elevated-lake decree claims the other two
    /// anyway, so the engine's classified total is two above the exporter's.
    /// A known authoring inconsistency, tracked separately -- *not*
    /// engine/exporter drift.
    const STRAY_ELEVATED_LAKE_CELLS: usize = 2;
    /// Corridor cells sitting in a channel narrow enough to carve as a real
    /// `RiverKind::River` (local channel width within
    /// `CROMATOLIS_MAX_RIVER_WIDTH`): 12.4% of them. Cromatolis genuinely has
    /// wide rivers, and this counts *channels*, not chunks near a bank -- see
    /// `cromatolis_wide_water_bodies_are_not_carved_as_rivers_against_real_lfs_assets`.
    ///
    /// Unlike the `EXPORTED_*` constants this is the engine's own quantity --
    /// the exporter does not compute a carve width -- so it is measured here.
    /// It moves with the corridor raster (the width transform runs over that
    /// mask) and with relief (carving also needs a downhill neighbour), so the
    /// marine-shelf terrain package moved it on both counts.
    const CARVEABLE_RIVER_CELLS: usize = 4_151;

    /// Counts every `WaterBodyKind` across the real Cromatolis map and
    /// reconciles it against the numbers the open-world exporter measured
    /// independently for COW-22 `[OQ3]`. A disagreement here means the engine
    /// and the exporter have drifted on what "ocean" (or "river", or
    /// "standing water") means, which is exactly the kind of drift that
    /// silently mis-paints a whole coastline.
    #[test]
    #[ignore]
    fn cromatolis_water_body_histogram_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let count = |kind: WaterBodyKind| {
            sim.chunks
                .iter()
                .filter(|chunk| chunk.water_body == Some(kind))
                .count()
        };

        // Exporter: 33,566 corridor cells across 72 substantial systems.
        assert_eq!(count(WaterBodyKind::River), EXPORTED_RIVER_CELLS);
        // Exporter: 24 lagoon basins, 2,815 cells.
        assert_eq!(count(WaterBodyKind::Lagoon), EXPORTED_LAGOON_CELLS);
        // Exporter: 14 lake basins, 13,872 cells, plus the stray
        // `elevated_lakes` cells (see `STRAY_ELEVATED_LAKE_CELLS`).
        assert_eq!(
            count(WaterBodyKind::Lake),
            EXPORTED_LAKE_CELLS + STRAY_ELEVATED_LAKE_CELLS
        );
        // Exporter: 316,682 marine cells.
        assert_eq!(count(WaterBodyKind::Ocean), EXPORTED_MARINE_CELLS);
        // `Sea` needs an offshore band nothing computes yet -- COW-22
        // `C22-3` shipped the bathymetry but no classification from it.
        assert_eq!(count(WaterBodyKind::Sea), 0);

        // And the partition is exactly the authored water footprint: the
        // `water`-mask cells plus the stray `elevated_lakes` ones, with no
        // chunk counted twice and none left over.
        let classified = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.water_body.is_some())
            .count();
        assert_eq!(
            classified,
            AUTHORED_WATER_MASK_CELLS + STRAY_ELEVATED_LAKE_CELLS
        );
        assert_eq!(
            classified,
            EXPORTED_RIVER_CELLS
                + EXPORTED_LAGOON_CELLS
                + EXPORTED_LAKE_CELLS
                + EXPORTED_MARINE_CELLS
                + STRAY_ELEVATED_LAKE_CELLS
        );
    }

    /// The sharpest single assertion in COW-22 `C22-1b`: before it, *no* chunk
    /// on the Cromatolis map was `RiverKind::River` -- every authored corridor
    /// cell lost to the broader `water` mask it sits inside, so the authored
    /// river network existed in the data and nowhere in the simulation.
    #[test]
    #[ignore]
    fn cromatolis_rivers_are_carveable_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let rivers = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.river.river_kind.is_some_and(|kind| kind.is_river()))
            .collect::<Vec<_>>();

        assert_eq!(rivers.len(), CARVEABLE_RIVER_CELLS);

        for chunk in &rivers {
            let Some(RiverKind::River { cross_section }) = chunk.river.river_kind else {
                unreachable!()
            };
            assert!(
                cross_section.x <= CROMATOLIS_MAX_RIVER_WIDTH,
                "cross-section {cross_section:?} exceeds the max river width"
            );
            assert!(
                cross_section.y >= CONFIG.river_min_height,
                "cross-section {cross_section:?} is shallower than the minimum river height"
            );
            assert!(
                cross_section.y <= CROMATOLIS_MAX_RIVER_WIDTH / CONFIG.river_width_to_depth,
                "cross-section {cross_section:?} is deeper than the width-to-depth ratio allows"
            );
            // No river should still be carrying the old flat 3.2 m x 0.25 m
            // ditch the authored map used to give every river on the map.
            assert!(
                cross_section.x > TerrainChunkSize::RECT_SIZE.x as f32 * 0.1,
                "cross-section {cross_section:?} is still the pre-COW-22 ditch"
            );
        }

        // Every carved river flows, and every one has somewhere to flow *to*:
        // `column.rs` panics outright ("How can a river have no downhill?")
        // when it samples a `RiverKind::River` chunk whose `downhill` is
        // `None`, so this is a crash guard, not a tidiness check.
        for chunk in &rivers {
            assert!(
                chunk.downhill.is_some(),
                "a carved river with no downhill neighbour would panic terrain generation"
            );
            assert!(
                chunk.river.velocity.magnitude() > 0.0,
                "a carved river with no velocity: {:?}",
                chunk.river.velocity
            );
        }
    }

    /// The regression that made `local_channel_radius_chunks` necessary.
    ///
    /// Thresholding each chunk's own distance to the nearest bank carves the
    /// *rim* of every wide body -- a rim chunk is one chunk from the bank
    /// however wide the body behind it is -- leaving a 64 m wide, 8 m deep
    /// channel ringing flat lake water, which is precisely the water-wall
    /// geometry the width cap exists to prevent. That counterfactual was
    /// 11,650 rim chunks around 82 wide bodies when it was measured, on the
    /// raster that preceded the marine-shelf terrain package; nothing computes
    /// it today, so it has not been re-measured against the current one.
    ///
    /// Probe: the widest authored water body on the map, centred 17 chunks
    /// from its nearest bank. Not one chunk of it, rim included, may be
    /// carved.
    #[test]
    #[ignore]
    fn cromatolis_wide_water_bodies_are_not_carved_as_rivers_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        // Source-raster coordinates, y-flipped into engine space the same way
        // `cromatolis_v22_relief_and_river_package_regression_against_real_lfs_assets`
        // does.
        let source = Vec2::new(449, 904);
        let radius = 17;
        let centre = Vec2::new(source.x, i32::from(map_size_lg.chunks().y) - 1 - source.y);

        let mut inspected = 0;
        for y in centre.y - radius..=centre.y + radius {
            for x in centre.x - radius..=centre.x + radius {
                if (x - centre.x).pow(2) + (y - centre.y).pow(2) > radius * radius {
                    continue;
                }
                let chunk = &sim.chunks[vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y))];
                inspected += 1;
                assert!(
                    !chunk.river.river_kind.is_some_and(|kind| kind.is_river()),
                    "chunk ({x}, {y}) inside the map's widest water body was carved as a river"
                );
            }
        }
        assert!(
            inspected > 900,
            "the probe disc should cover the whole body"
        );
    }

    /// `WaterBodyKind` and `RiverKind` are allowed to disagree in exactly one
    /// way -- a corridor too wide to carve -- and in no other. Anything else
    /// means the two priority chains have drifted apart.
    #[test]
    #[ignore]
    fn cromatolis_water_body_and_river_kind_agree_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let mut wide_rivers_typed_as_lakes = 0;
        for chunk in sim.chunks.iter() {
            match (chunk.water_body, chunk.river.river_kind) {
                (Some(WaterBodyKind::Ocean | WaterBodyKind::Sea), Some(RiverKind::Ocean)) => {},
                (
                    Some(WaterBodyKind::Lake | WaterBodyKind::Lagoon),
                    Some(RiverKind::Lake { .. }),
                ) => {},
                (Some(WaterBodyKind::River), Some(RiverKind::River { .. })) => {},
                // The one documented disagreement: wider than
                // `CROMATOLIS_MAX_RIVER_WIDTH`, so it is a river ecologically
                // but a flat lake physically.
                (Some(WaterBodyKind::River), Some(RiverKind::Lake { .. })) => {
                    wide_rivers_typed_as_lakes += 1
                },
                (None, None) => {},
                (water_body, river_kind) => panic!(
                    "inconsistent water classification: {water_body:?} vs {river_kind:?} at a \
                     chunk"
                ),
            }
        }
        assert_eq!(
            wide_rivers_typed_as_lakes,
            EXPORTED_RIVER_CELLS - CARVEABLE_RIVER_CELLS
        );
    }

    /// COW-22 `C22-1c`, measured end to end: what the biome histogram looks
    /// like once `is_ocean` alone decides what the sea is, rather than the old
    /// `alt < 0` disjunct that also swallowed every inland body with a bed
    /// painted below sea level.
    ///
    /// The before/after pair this row was originally written against
    /// (`Ocean` 331,778 -> 317,160) was measured on the raster that preceded
    /// the marine-shelf terrain package, and there is no way to re-measure the
    /// "before" half now -- it needed code this row deleted. So the assertions
    /// below state the property instead of the delta: marine biome chunks are
    /// exactly the exporter's marine cells, and `Lake` is exactly every other
    /// classified water chunk.
    ///
    /// Also pins the `get_biome` consequence of `C22-1b`: with
    /// `RiverKind::River` finally reachable and no `BiomeKind::River` to
    /// answer with, a river chunk must still report `Lake` rather than falling
    /// through to a land biome.
    #[test]
    #[ignore]
    fn cromatolis_inland_water_is_no_longer_ocean_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let biome_count = |biome: BiomeKind| {
            sim.chunks
                .iter()
                .filter(|chunk| chunk.get_biome() == biome)
                .count()
        };

        // Marine biome chunks are the marine cells and nothing else: no inland
        // body gets in on the strength of a bed painted below sea level.
        assert_eq!(biome_count(BiomeKind::Ocean), EXPORTED_MARINE_CELLS);
        // ...and every classified water chunk that is not marine lands in
        // `Lake`, the inland bodies that used to read as ocean included.
        assert_eq!(
            biome_count(BiomeKind::Lake),
            AUTHORED_WATER_MASK_CELLS + STRAY_ELEVATED_LAKE_CELLS - EXPORTED_MARINE_CELLS
        );

        // Two mechanisms move the land biomes, and they differ by two orders of
        // magnitude, so keep them apart when reading a failure here.
        //
        // The small one is a CDF knock-on: `pure_water` (which decides whether
        // a chunk takes part in the land-uniform noise CDFs for
        // humidity/flux/temperature) treats `RiverKind::River` as not pure
        // water, so every carveable river chunk joins those CDFs and shifts
        // each land chunk's humidity rank by ~0.001. On its own that moved
        // *eleven* chunks of 1,048,576 across a biome boundary when
        // `RiverKind::River` first became reachable.
        //
        // The large one is the relief itself. Reshaping the seabed and the
        // beach rim re-ranks coastal altitude wholesale, and on the
        // marine-shelf terrain package it moved `Mountain` by 2,526 chunks
        // (-5.7%) and `Savannah` by 4,539 (+2.4%) -- hundreds of times the CDF
        // term. That is why these numbers were re-measured with that package
        // rather than carried over, and why a failure here is far more likely
        // to mean "the relief changed" than "the CDFs drifted".
        //
        // ⚠️ **These numbers were stale before the climate-zone rework, and
        // this assertion was already failing on the branch it re-baselines.**
        // Two separate things moved them:
        //
        // 1. The authored alpine policy introduced `BiomeKind::Snowland` on this map (0
        //    -> 32,983 chunks). Snowland is tested before Mountain and Taiga, so it
        //    took most of both, and the "nothing in Snowland" partition below stopped
        //    holding. That landed without this test being re-run.
        // 2. The climate-zone rework. With three quarters of the map now sitting at a
        //    temperate abstract -0.2 instead of a saturated +1.0: `Savannah` needs
        //    `temp >= 0.3` and all but collapses, `Jungle` needs `temp > 0.45` and
        //    retreats to the painted tropical south, `Taiga`'s `-0.7..-0.3` window
        //    opens on real highland, and the humidity that the evaporation dampener
        //    used to erase at the coast survives, which moves `Forest`/`Grassland`
        //    wholesale.
        //
        // Measured before -> after the rework (both on the post-alpine branch):
        //
        //   Savannah   193,887 ->   2,419    Grassland   25,308 -> 179,722
        //   Jungle     215,727 ->  68,339    Forest     173,668 -> 310,459
        //   Taiga            0 ->  46,052    Mountain    27,997 ->  27,879
        //   Snowland    32,607 ->  32,983    Swamp       12,445 ->  13,786
        //
        // Pinned exactly, deliberately: an exact number is what makes the next
        // re-measure unmissable.
        assert_eq!(biome_count(BiomeKind::Savannah), 2_419);
        assert_eq!(biome_count(BiomeKind::Grassland), 179_722);
        assert_eq!(biome_count(BiomeKind::Taiga), 46_052);
        assert_eq!(biome_count(BiomeKind::Mountain), 27_879);
        assert_eq!(biome_count(BiomeKind::Snowland), 32_983);
        // Banded instead: the biomes where a small move really would be the CDF
        // knock-on rather than a design change, so that an unrelated CDF shift
        // reads as one signal instead of three simultaneous "failures".
        for (biome, measured) in [
            (BiomeKind::Jungle, 68_339.0),
            (BiomeKind::Forest, 310_459.0),
            (BiomeKind::Swamp, 13_786.0),
        ] {
            let counted = biome_count(biome) as f64;
            assert!(
                (counted - measured).abs() / measured < 0.005,
                "{biome:?} moved from the measured {measured} to {counted}, more than the 0.5% \
                 band the CDF knock-on accounts for -- check whether the corridor raster, the \
                 relief or a climate anchor moved"
            );
        }

        // The ten biomes above account for the whole map, with nothing in
        // `Desert` or any other variant. Two things fall out of this for free:
        // a transcription slip in any of the pinned counts above cannot
        // balance, and the slack the three bands carry cannot quietly hide an
        // eleventh biome appearing -- which is exactly how `Snowland` slipped
        // in unnoticed while it was excluded from this list.
        let partitioned: usize = [
            BiomeKind::Ocean,
            BiomeKind::Lake,
            BiomeKind::Savannah,
            BiomeKind::Grassland,
            BiomeKind::Taiga,
            BiomeKind::Mountain,
            BiomeKind::Snowland,
            BiomeKind::Jungle,
            BiomeKind::Forest,
            BiomeKind::Swamp,
        ]
        .into_iter()
        .map(biome_count)
        .sum();
        assert_eq!(partitioned, sim.chunks.len());

        // No water chunk reports a land biome.
        for chunk in sim.chunks.iter() {
            if chunk.water_body.is_some() {
                assert!(
                    matches!(chunk.get_biome(), BiomeKind::Ocean | BiomeKind::Lake),
                    "{:?} water chunk reports {:?}",
                    chunk.water_body,
                    chunk.get_biome()
                );
            }
        }
    }

    /// The two downstream consequences of retyping the inland water that used
    /// to read as ocean, and of making `RiverKind::River` reachable, checked
    /// rather than assumed.
    ///
    /// 1. The 13 `is_ocean()`-gated scatter configs (coral, seagrass, sea
    ///    urchins, ...) must stop growing marine flora in the reclassified
    ///    inland chunks. Their gate is `col.chunk.river.is_ocean()`, so this
    ///    pins the size of that set.
    /// 2. Every water chunk must still be claimed by one of the two Cromatolis
    ///    wildlife manifest entries. Their chunk-level gates are
    ///    `BiomeKind::Ocean` (`cromatolis.ocean`) and `cromatolis_freshwater`
    ///    (`cromatolis.lake`); every generic `*.river`/`*.lake` entry is
    ///    `not_cromatolis`-gated, so a chunk neither entry claims gets *no*
    ///    wildlife at all.
    #[test]
    #[ignore]
    fn cromatolis_water_reclassification_keeps_its_consumers_covered_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();

        // Exactly the marine cells: the inland water that used to read as ocean
        // no longer grows coral. Derived from the constant rather than restated
        // as a literal, so the two cannot drift apart.
        let marine_scatter_chunks = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.river.is_ocean())
            .count();
        assert_eq!(marine_scatter_chunks, EXPORTED_MARINE_CELLS);

        let mut uncovered = 0;
        let mut double_covered = 0;
        for chunk in sim.chunks.iter() {
            if chunk.water_body.is_none() {
                continue;
            }
            let ocean_entry = chunk.get_biome() == BiomeKind::Ocean;
            let lake_entry = crate::layer::wildlife::cromatolis_freshwater(chunk);
            match (ocean_entry, lake_entry) {
                (false, false) => uncovered += 1,
                (true, true) => double_covered += 1,
                _ => {},
            }
        }
        assert_eq!(
            uncovered, 0,
            "{uncovered} water chunks are claimed by neither Cromatolis wildlife entry"
        );
        assert_eq!(
            double_covered, 0,
            "{double_covered} water chunks are claimed by both Cromatolis wildlife entries, \
             double-counting their density"
        );
    }

    // ---- Salinity (COW-22 `C22-4`) ----

    /// A hand-drawn 8x8 water map for the salinity rules.
    struct SalinityFixture {
        map_size_lg: MapSizeLg,
        water_body: Vec<Option<WaterBodyKind>>,
        alt: Vec<Alt>,
        elevated: Vec<bool>,
    }

    impl SalinityFixture {
        /// One character per chunk, row 0 at `y == 0`:
        ///
        /// - `.` dry land
        /// - `O` marine water
        /// - `r` river corridor
        /// - `L` lake
        /// - `G` lagoon
        /// - `E` a lake the authored elevated-lake decree claims
        ///
        /// `alt_of` gives each chunk its altitude from its character and
        /// position, in the same sea-level-is-zero space `WorldSim::generate`
        /// works in.
        fn new(rows: &[&str; 8], alt_of: impl Fn(char, Vec2<i32>) -> Alt) -> Self {
            let map_size_lg = MapSizeLg::new(Vec2 { x: 3, y: 3 }).expect("valid map size");
            let mut water_body = vec![None; map_size_lg.chunks_len()];
            let mut alt = vec![0.0; map_size_lg.chunks_len()];
            let mut elevated = vec![false; map_size_lg.chunks_len()];
            for (y, row) in rows.iter().enumerate() {
                assert_eq!(row.chars().count(), 8, "every fixture row is 8 chunks wide");
                for (x, symbol) in row.chars().enumerate() {
                    let pos = Vec2::new(x as i32, y as i32);
                    let idx = vec2_as_uniform_idx(map_size_lg, pos);
                    water_body[idx] = match symbol {
                        '.' => None,
                        'O' => Some(WaterBodyKind::Ocean),
                        'r' => Some(WaterBodyKind::River),
                        'L' | 'E' => Some(WaterBodyKind::Lake),
                        'G' => Some(WaterBodyKind::Lagoon),
                        other => panic!("unknown fixture symbol {other:?}"),
                    };
                    elevated[idx] = symbol == 'E';
                    alt[idx] = alt_of(symbol, pos);
                }
            }
            Self {
                map_size_lg,
                water_body,
                alt,
                elevated,
            }
        }

        fn idx(&self, x: i32, y: i32) -> usize {
            vec2_as_uniform_idx(self.map_size_lg, Vec2::new(x, y))
        }

        fn salinity(&self) -> Box<[Option<Salinity>]> {
            derive_salinity(self.map_size_lg, &self.water_body, &self.alt, |idx| {
                self.elevated[idx]
            })
        }

        fn river_components(&self) -> (Box<[u32]>, Vec<RiverComponent>) {
            label_river_components(
                self.map_size_lg,
                |idx| self.water_body[idx] == Some(WaterBodyKind::River),
                |idx| {
                    matches!(
                        self.water_body[idx],
                        Some(WaterBodyKind::Ocean | WaterBodyKind::Sea)
                    )
                },
                &self.alt,
            )
        }
    }

    /// Land at +10 m, every kind of water at -1 m. The default for a fixture
    /// whose point is topology rather than height.
    fn flat_alt(symbol: char, _pos: Vec2<i32>) -> Alt { if symbol == '.' { 10.0 } else { -1.0 } }

    /// Land at +10 m, water descending northwards so a channel's head is its
    /// southernmost chunk. The default for a fixture about river gradients.
    fn sloping_alt(symbol: char, pos: Vec2<i32>) -> Alt {
        if symbol == '.' {
            10.0
        } else {
            20.0 - pos.y as Alt
        }
    }

    #[test]
    fn marine_water_is_saline() {
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "OOOOOOOO", "OOOOOOOO", "OOOOOOOO", "........", "........", "........",
                "........",
            ],
            flat_alt,
        );
        let salinity = fixture.salinity();
        for (idx, kind) in fixture.water_body.iter().enumerate() {
            assert_eq!(
                salinity[idx],
                kind.map(|_| Salinity::Saline),
                "every marine chunk is salt and no dry chunk has a salinity"
            );
        }
    }

    #[test]
    fn a_lagoon_fed_by_a_freshwater_channel_is_brackish() {
        // A coastal basin with the sea on one side and a river arriving from
        // inland on the other -- the textbook lagoon.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "OOOOOOOO", "...GG...", "...GG...", "....r...", "....r...", "....r...",
                "....r...",
            ],
            flat_alt,
        );
        let salinity = fixture.salinity();
        assert_eq!(salinity[fixture.idx(3, 2)], Some(Salinity::Brackish));
        assert_eq!(
            salinity[fixture.idx(4, 3)],
            Some(Salinity::Brackish),
            "the whole basin agrees, not just the chunk the channel touches"
        );
    }

    #[test]
    fn a_lagoon_whose_every_endpoint_is_marine_is_saline() {
        // The same basin with the river taken away: nothing fresh reaches it,
        // so it is simply a pocket of the sea.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "OOOOOOOO", "...GG...", "...GG...", "........", "........", "........",
                "........",
            ],
            flat_alt,
        );
        let salinity = fixture.salinity();
        assert_eq!(salinity[fixture.idx(3, 2)], Some(Salinity::Saline));
        assert_eq!(salinity[fixture.idx(4, 3)], Some(Salinity::Saline));
    }

    #[test]
    fn a_lake_that_drains_to_the_sea_is_fresh() {
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "....r...", "....r...", "...LL...", "...LL...", "........", "........",
                "........",
            ],
            flat_alt,
        );
        let salinity = fixture.salinity();
        assert_eq!(salinity[fixture.idx(3, 3)], Some(Salinity::Fresh));
        assert_eq!(salinity[fixture.idx(4, 4)], Some(Salinity::Fresh));
    }

    #[test]
    fn an_endorheic_lake_below_sea_level_is_saline() {
        // A closed depression: a channel leaves it, but that channel dead-ends
        // inland instead of reaching the sea, which is no way out at all.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "........", "........", "...LL...", "...LL...", "....r...", "....r...",
                "........",
            ],
            flat_alt,
        );
        let salinity = fixture.salinity();
        assert_eq!(salinity[fixture.idx(3, 3)], Some(Salinity::Saline));
        assert_eq!(
            salinity[fixture.idx(4, 6)],
            Some(Salinity::Fresh),
            "the dead-end channel itself never meets the sea, so it stays fresh"
        );
    }

    #[test]
    fn an_endorheic_lake_above_sea_level_is_fresh() {
        // Same closed basin, lifted onto a plateau: nothing drains out of it
        // either, but it is not the depression the salt rule is about.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "........", "........", "...LL...", "...LL...", "........", "........",
                "........",
            ],
            |symbol, _| if symbol == '.' { 60.0 } else { 50.0 },
        );
        let salinity = fixture.salinity();
        assert_eq!(salinity[fixture.idx(3, 3)], Some(Salinity::Fresh));
    }

    #[test]
    fn a_river_is_fresh_at_its_head_and_grades_to_salt_at_its_mouth() {
        let fixture = SalinityFixture::new(
            &[
                "OOOO....", "...r....", "...r....", "...r....", "...r....", "...r....", "...r....",
                "...r....",
            ],
            sloping_alt,
        );
        let salinity = fixture.salinity();
        // `y == 1` is the mouth, so a chunk's distance from it is `y - 1`.
        // Expressed against the constants rather than against the numbers they
        // happen to hold: retuning the estuary is not supposed to break a test
        // about the shape of the gradient.
        let mouth_y = 1;
        let last_salt_y = mouth_y + RIVER_SALINE_MOUTH_CHUNKS as i32;
        assert!(
            last_salt_y < 7,
            "the fixture channel has to outlast the salt reach for this test to say anything"
        );
        for y in mouth_y..=last_salt_y {
            assert_eq!(
                salinity[fixture.idx(3, y)],
                Some(Salinity::Saline),
                "chunk (3, {y}) is within {RIVER_SALINE_MOUTH_CHUNKS} of the mouth"
            );
        }
        for y in last_salt_y + 1..=7 {
            assert_eq!(
                salinity[fixture.idx(3, y)],
                Some(Salinity::Brackish),
                "chunk (3, {y}) is past the salt reach but still inside the \
                 {RIVER_BRACKISH_MOUTH_CHUNKS}-chunk mixing reach"
            );
        }
    }

    #[test]
    fn a_river_that_never_reaches_the_sea_is_fresh_along_its_whole_length() {
        let fixture = SalinityFixture::new(
            &[
                "........", "...r....", "...r....", "...r....", "...r....", "...r....", "...r....",
                "...r....",
            ],
            sloping_alt,
        );
        let salinity = fixture.salinity();
        for y in 1..=7 {
            assert_eq!(salinity[fixture.idx(3, y)], Some(Salinity::Fresh));
        }
    }

    #[test]
    fn a_river_whose_source_is_marine_is_salt_along_its_whole_length() {
        // A channel that rises in the sea and returns to it: it never climbs
        // above sea level, so there is no freshwater head anywhere on it -- not
        // even at the far end, which the mouth gradient alone would call fresh.
        let fixture = SalinityFixture::new(
            &[
                "OOOO....", "...r....", "...r....", "...r....", "...r....", "...r....", "...r....",
                "...r....",
            ],
            |symbol, _| if symbol == '.' { 10.0 } else { -1.0 },
        );
        let salinity = fixture.salinity();
        for y in 1..=7 {
            assert_eq!(
                salinity[fixture.idx(3, y)],
                Some(Salinity::Saline),
                "chunk (3, {y}) belongs to a channel with no freshwater head"
            );
        }
    }

    #[test]
    fn a_marine_source_needs_the_channel_to_actually_reach_the_sea() {
        // The same below-sea-level channel with no marine connection is a
        // sunken inland corridor, not an arm of the sea.
        let fixture = SalinityFixture::new(
            &[
                "........", "...r....", "...r....", "...r....", "...r....", "...r....", "...r....",
                "...r....",
            ],
            |symbol, _| if symbol == '.' { 10.0 } else { -1.0 },
        );
        let (_, components) = fixture.river_components();
        assert_eq!(components.len(), 1);
        assert!(components[0].mouths.is_empty());
        assert!(!components[0].source_is_marine);
    }

    #[test]
    fn an_authored_elevated_lake_is_fresh_and_does_not_bridge_two_basins() {
        // The elevated-lake decree marks standing water above sea level: a
        // pond on a shelf at +40 m, with the sea on one side of it and a
        // sunken basin at -1 m on the other. It must not act as a bridge in
        // either direction -- neither making the basin behind it a lagoon, nor
        // giving that basin a way out to the sea, which would mean water
        // running 41 m uphill.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "...GG...", "...EE...", "...LL...", "........", "........", "........",
                "........",
            ],
            |symbol, _| match symbol {
                '.' => 10.0,
                'E' => 40.0,
                _ => -1.0,
            },
        );
        let salinity = fixture.salinity();
        assert_eq!(
            salinity[fixture.idx(3, 1)],
            Some(Salinity::Saline),
            "the coastal basin has no fresh endpoint of its own"
        );
        assert_eq!(
            salinity[fixture.idx(3, 2)],
            Some(Salinity::Fresh),
            "the elevated lake itself"
        );
        assert_eq!(
            salinity[fixture.idx(3, 3)],
            Some(Salinity::Saline),
            "and the basin behind it is a closed depression, because the elevated lake does not \
             connect it to the sea"
        );
    }

    #[test]
    fn river_salinity_grades_by_distance_from_the_mouth() {
        assert_eq!(
            river_salinity_from_mouth_distance(0),
            Salinity::Saline,
            "the mouth itself"
        );
        assert_eq!(
            river_salinity_from_mouth_distance(RIVER_SALINE_MOUTH_CHUNKS),
            Salinity::Saline
        );
        assert_eq!(
            river_salinity_from_mouth_distance(RIVER_SALINE_MOUTH_CHUNKS + 1),
            Salinity::Brackish
        );
        assert_eq!(
            river_salinity_from_mouth_distance(RIVER_BRACKISH_MOUTH_CHUNKS),
            Salinity::Brackish
        );
        assert_eq!(
            river_salinity_from_mouth_distance(RIVER_BRACKISH_MOUTH_CHUNKS + 1),
            Salinity::Fresh
        );
        assert_eq!(
            river_salinity_from_mouth_distance(u32::MAX),
            Salinity::Fresh,
            "a component with no mouth at all"
        );
        const {
            assert!(RIVER_SALINE_MOUTH_CHUNKS < RIVER_BRACKISH_MOUTH_CHUNKS);
        }
    }

    #[test]
    fn river_components_are_separated_and_summarised() {
        // Two channels that never touch, one reaching the sea and one not.
        let fixture = SalinityFixture::new(
            &[
                "OOOOOOOO", "..r..r..", "..r..r..", "..r..r..", "..r.....", "..r.....", "........",
                "........",
            ],
            sloping_alt,
        );
        let (label, components) = fixture.river_components();
        assert_eq!(components.len(), 2, "two disjoint channels");
        assert_ne!(
            label[fixture.idx(2, 1)],
            label[fixture.idx(5, 1)],
            "and the labelling keeps them apart"
        );
        let left = &components[label[fixture.idx(2, 1)] as usize];
        let right = &components[label[fixture.idx(5, 1)] as usize];
        assert_eq!(left.len, 5);
        assert_eq!(right.len, 3);
        // The source is the highest chunk, which `sloping_alt` puts closest to
        // the sea in this fixture.
        assert_eq!(left.source, fixture.idx(2, 1));
        assert_eq!(left.source_alt, 19.0);
        assert_eq!(left.mouths, vec![fixture.idx(2, 1)]);
        assert!(!left.source_is_marine, "its head is well above sea level");
        // Every chunk that is not a river is unlabelled.
        for (idx, kind) in fixture.water_body.iter().enumerate() {
            assert_eq!(
                label[idx] == NO_RIVER_COMPONENT,
                *kind != Some(WaterBodyKind::River)
            );
        }
    }

    // ---- Real-LFS-asset regressions for COW-22 `C22-4` ----
    //
    // These numbers and the exporter constants above this block
    // (`EXPORTED_RIVER_CELLS` and friends) describe the same rasters and move
    // together: both are counts over `cromatolis_v0.bin` and
    // `cromatolis_v0_river_channels.f32le`. A prior revision of this comment
    // noted they were out of sync (measured against different revisions of
    // those two files after `C22-3` regenerated them) -- reconciled in a
    // follow-up (corridor 33,566, marine 316,682, lagoon 2,815, lake
    // unchanged at 13,872, all re-confirmed against the `xindeler-open-world`
    // exporter's own independent measurement, not just this engine's). The
    // biome histogram in
    // `cromatolis_inland_water_is_no_longer_ocean_against_real_lfs_assets`
    // was reconciled in the same pass.

    /// Corridor chunks a component needs before it counts as a river *system*
    /// rather than a puddle or a one-chunk artefact of the raster. At this
    /// threshold the engine's own component labelling finds 72 systems, which
    /// is exactly the count the open-world exporter measured independently.
    const SUBSTANTIAL_RIVER_COMPONENT_CHUNKS: usize = 20;

    /// Every `Salinity` across the real Cromatolis map.
    ///
    /// The partition assertions matter more than any individual count: every
    /// chunk the classification calls water must get exactly one salinity, and
    /// no dry chunk may get one, whatever the rules decide.
    #[test]
    #[ignore]
    fn cromatolis_salinity_histogram_regression_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let count = |salinity: Salinity| {
            sim.chunks
                .iter()
                .filter(|chunk| chunk.salinity == Some(salinity))
                .count()
        };
        let cross = |kind: WaterBodyKind, salinity: Salinity| {
            sim.chunks
                .iter()
                .filter(|chunk| chunk.water_body == Some(kind) && chunk.salinity == Some(salinity))
                .count()
        };

        assert_eq!(count(Salinity::Fresh), 24_526);
        assert_eq!(count(Salinity::Brackish), 7_624);
        assert_eq!(count(Salinity::Saline), 334_787);

        // Per kind, which is where a rule change actually shows up.
        assert_eq!(cross(WaterBodyKind::Ocean, Salinity::Saline), 316_682);
        assert_eq!(cross(WaterBodyKind::Ocean, Salinity::Brackish), 0);
        assert_eq!(cross(WaterBodyKind::Ocean, Salinity::Fresh), 0);
        // The estuary gradient: fresh above it, salt at the waterline.
        assert_eq!(cross(WaterBodyKind::River, Salinity::Fresh), 15_134);
        assert_eq!(cross(WaterBodyKind::River, Salinity::Brackish), 5_471);
        assert_eq!(cross(WaterBodyKind::River, Salinity::Saline), 12_961);
        // Two of the map's fourteen lake basins are closed depressions below
        // sea level; the other twelve drain to the sea.
        assert_eq!(cross(WaterBodyKind::Lake, Salinity::Fresh), 9_392);
        assert_eq!(cross(WaterBodyKind::Lake, Salinity::Saline), 4_482);
        assert_eq!(cross(WaterBodyKind::Lake, Salinity::Brackish), 0);
        // Eleven of the twenty-four lagoon basins have no freshwater channel
        // reaching them at all, so they are simply pockets of the sea.
        assert_eq!(cross(WaterBodyKind::Lagoon, Salinity::Brackish), 2_153);
        assert_eq!(cross(WaterBodyKind::Lagoon, Salinity::Saline), 662);
        assert_eq!(cross(WaterBodyKind::Lagoon, Salinity::Fresh), 0);

        // The partition: exactly the classified water chunks, no more and no
        // fewer.
        let salted = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.salinity.is_some())
            .count();
        let classified = sim
            .chunks
            .iter()
            .filter(|chunk| chunk.water_body.is_some())
            .count();
        assert_eq!(salted, classified);
        assert_eq!(
            salted,
            count(Salinity::Fresh) + count(Salinity::Brackish) + count(Salinity::Saline)
        );
        for chunk in sim.chunks.iter() {
            assert_eq!(
                chunk.water_body.is_some(),
                chunk.salinity.is_some(),
                "{:?} water carries {:?} salinity",
                chunk.water_body,
                chunk.salinity
            );
        }
    }

    /// The baseline that keeps the salt-river exception honest.
    ///
    /// The open-world exporter measured, independently and on the source
    /// raster, that **93 of 93 substantial river systems reach the sea, with
    /// source altitudes of 94-1,095 m**. The engine does not reproduce those
    /// figures exactly and is not expected to: it labels components on the
    /// chunk grid after erosion has reshaped `alt`, and its "the sea" is the
    /// `get_oceans` border flood fill rather than "painted below sea level"
    /// (COW-22 `C22-1c`), so a corridor that ends in an inland basin the
    /// raster painted below sea level counts as landlocked here and as
    /// sea-reaching there. What carries over is the *claim*: a river that is
    /// salt from its own source is a rare exception, not the common case. That
    /// is what the last assertion pins.
    ///
    /// The altitudes here are a re-derivation, not the ones the classifier saw:
    /// `WorldSim` keeps `SimChunk::alt`, which `SimChunk::generate` has already
    /// lowered to the bed for every lake-carved chunk, and a wide corridor is
    /// lake-carved while still being a `WaterBodyKind::River`. So the counts
    /// and the component shapes below are exact -- connectivity and mouths do
    /// not depend on altitude at all -- while the source altitudes and the
    /// marine-sourced count are read off a systematically *lower* altitude
    /// field than production used. That skews in the safe direction: a lower
    /// source can only make `source_is_marine` more likely, so three
    /// marine-sourced components is an upper bound on the figure production
    /// actually derived, and the rarity claim holds a fortiori.
    #[test]
    #[ignore]
    fn cromatolis_river_components_reach_the_sea_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let map_size_lg = sim.map_size_lg();
        // `SimChunk::alt` carries `CONFIG.sea_level`; the classification works
        // in the sea-level-is-zero space `WorldSim::generate` uses.
        let alt = sim
            .chunks
            .iter()
            .map(|chunk| (chunk.alt - CONFIG.sea_level) as Alt)
            .collect::<Vec<_>>();
        let (_, components) = label_river_components(
            map_size_lg,
            |idx| sim.chunks[idx].water_body == Some(WaterBodyKind::River),
            |idx| {
                matches!(
                    sim.chunks[idx].water_body,
                    Some(WaterBodyKind::Ocean | WaterBodyKind::Sea)
                )
            },
            &alt,
        );

        assert_eq!(components.len(), 434, "components of the corridor network");
        assert_eq!(
            components
                .iter()
                .map(|component| component.len)
                .sum::<usize>(),
            sim.chunks
                .iter()
                .filter(|chunk| chunk.water_body == Some(WaterBodyKind::River))
                .count(),
            "the components partition the corridor chunks"
        );

        let substantial = components
            .iter()
            .filter(|component| component.len >= SUBSTANTIAL_RIVER_COMPONENT_CHUNKS)
            .collect::<Vec<_>>();
        assert_eq!(substantial.len(), 72, "substantial river systems");
        assert_eq!(
            substantial
                .iter()
                .filter(|component| !component.mouths.is_empty())
                .count(),
            64,
            "substantial systems reaching the sea; the other eight end in inland water the \
             classification calls a lake"
        );

        let source_alts = substantial
            .iter()
            .map(|component| component.source_alt)
            .collect::<Vec<_>>();
        let lowest = source_alts.iter().copied().fold(f32::INFINITY, f32::min);
        let highest = source_alts
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (lowest - -3.4).abs() < 0.1,
            "lowest substantial source sits at {lowest} m"
        );
        assert!(
            (highest - 1034.5).abs() < 0.1,
            "highest substantial source sits at {highest} m"
        );

        // The claim the exporter's baseline is really about: a river that is
        // salt from its own source stays a rare exception.
        let marine_sourced = components
            .iter()
            .filter(|component| component.source_is_marine)
            .collect::<Vec<_>>();
        assert_eq!(marine_sourced.len(), 3);
        assert_eq!(
            marine_sourced
                .iter()
                .filter(|component| component.len >= SUBSTANTIAL_RIVER_COMPONENT_CHUNKS)
                .count(),
            1,
            "exactly one substantial system rises in the sea"
        );
        let marine_sourced_chunks = marine_sourced
            .iter()
            .map(|component| component.len)
            .sum::<usize>();
        let corridor_chunks = components
            .iter()
            .map(|component| component.len)
            .sum::<usize>();
        assert!(
            (marine_sourced_chunks as f64) / (corridor_chunks as f64) < 0.01,
            "{marine_sourced_chunks} of {corridor_chunks} corridor chunks belong to a \
             marine-sourced system, which is no longer the rare exception the rule assumes"
        );
    }
}
