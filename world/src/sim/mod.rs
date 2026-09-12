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
        InverseCdf, cdf_irwin_hall, downhill, get_oceans, local_cells, map_edge_factor,
        uniform_noise, uphill,
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
        map::MapConfig, uniform_idx_as_vec2, vec2_as_uniform_idx,
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
    /// Authored baseline-temperature-curve tuning for the loaded region (if
    /// any), or `AuthoredCromatolisClimate::default()` if none is loaded /
    /// the asset failed to parse. See `cromatolis_baseline_temp`.
    pub(crate) cromatolis_climate: AuthoredCromatolisClimate,
    /// Per-chunk "adjacent to authored water" signal, see
    /// `SimChunk::authored_near_water`.
    authored_near_water: Box<[bool]>,
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

/// One of the authored raster layers a region may ship alongside its base
/// heightmap `.bin`. The asset specifier for a given region + kind is always
/// `"{region.map_asset}_{kind.asset_suffix()}"` (e.g.
/// `"world.map.cromatolis_v0_water"`), matching the convention
/// `xindeler-open-world`'s `export-new-horizon` command already produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthoredLayerKind {
    Routes,
    Vegetation,
    Water,
    ElevatedLakes,
    RiverChannels,
}

impl AuthoredLayerKind {
    /// All layer kinds a region can ship, in the order they're loaded.
    const ALL: [Self; 5] = [
        Self::Routes,
        Self::Vegetation,
        Self::Water,
        Self::ElevatedLakes,
        Self::RiverChannels,
    ];

    fn asset_suffix(self) -> &'static str {
        match self {
            Self::Routes => "routes",
            Self::Vegetation => "vegetation",
            Self::Water => "water",
            Self::ElevatedLakes => "elevated_lakes",
            Self::RiverChannels => "river_channels",
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
    /// Stable region id, independent of the map asset name. Not currently
    /// consumed outside logging, but kept distinct from `map_asset` so a
    /// region can be renamed/re-pointed without changing its identity.
    id: &'static str,
    /// Which authored layers this region ships.
    layers: &'static [AuthoredLayerKind],
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
}];

fn authored_region_for_map_asset(specifier: &str) -> Option<&'static AuthoredRegion> {
    AUTHORED_REGIONS
        .iter()
        .find(|region| region.map_asset == specifier)
}

/// Authored parameters for a region's baseline temperature curve (see
/// `cromatolis_baseline_temp`). Cromatolis-specific tuned content, not a
/// general engine constant, so it lives in a RON asset
/// (`{region.map_asset}_climate`, e.g. `assets/world/map/
/// cromatolis_v0_climate.ron`) rather than a Rust literal -- the same
/// convention this crate already uses for every other authored Cromatolis
/// parameter (settlements, landmarks, bridges, fortifications, ...).
#[derive(Debug, Clone, Copy, Deserialize)]
struct AuthoredCromatolisClimate {
    /// Baseline temperature at sea level, in real degrees Celsius.
    sea_level_temp_c: f32,
    /// How fast the curve cools with altitude, in degrees Celsius per meter
    /// of relief above sea level.
    lapse_rate_c_per_m: f32,
}

impl FileAsset for AuthoredCromatolisClimate {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
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
            sea_level_temp_c: 36.0,
            lapse_rate_c_per_m: 0.023,
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
            authored_water_layer,
            authored_elevated_lakes_layer,
            authored_river_channels_layer,
        ) = if let Some(region) = authored_region {
            (
                load_authored_layer(region, AuthoredLayerKind::Routes),
                load_authored_layer(region, AuthoredLayerKind::Vegetation),
                load_authored_layer(region, AuthoredLayerKind::Water),
                load_authored_layer(region, AuthoredLayerKind::ElevatedLakes),
                load_authored_layer(region, AuthoredLayerKind::RiverChannels),
            )
        } else {
            (None, None, None, None, None)
        };
        // Not a raster layer (`AuthoredLayerKind`), so loaded separately: a
        // couple of scalar tuning values, not a per-chunk array.
        let cromatolis_climate = authored_region
            .and_then(|region| {
                let specifier = format!("{}_climate", region.map_asset);
                match AuthoredCromatolisClimate::load_owned(&specifier) {
                    Ok(climate) => Some(climate),
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
            .unwrap_or_default();
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
            // Only used when the erosion sim didn't already detect a river at a
            // river-channel-masked tile. Matches `CONFIG.river_min_height` so
            // `river.near_water()`/biome logic treats authored rivers consistently
            // with procedurally-detected ones.
            let authored_river_cross_section = Vec2::new(
                TerrainChunkSize::RECT_SIZE.x as f32 * 0.1,
                CONFIG.river_min_height,
            );
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
                        is_elevated_lake,
                        is_water_body,
                        is_river_channel,
                        is_ocean: is_ocean[idx],
                        alt_below_sea_level: alt[idx] < 0.0,
                        existing_river_kind: river.river_kind,
                        neighbor_pass_pos,
                        authored_river_cross_section,
                    })
                };
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
        let authored_near_water: Box<[bool]> = if authored_cromatolis_v0 {
            let mask_hit = |layer: &Option<Box<[f32]>>, idx: usize| {
                layer.as_ref().is_some_and(|values| {
                    authored_layer_value_for_cromatolis_v0(map_size_lg, idx, values)
                        >= AUTHORED_WATER_THRESHOLD
                })
            };
            (0..map_size_lg.chunks_len())
                .into_par_iter()
                .map(|posi| {
                    let pos = uniform_idx_as_vec2(map_size_lg, posi);
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
                            if mask_hit(&authored_water_layer, nidx)
                                || mask_hit(&authored_elevated_lakes_layer, nidx)
                                || mask_hit(&authored_river_channels_layer, nidx)
                            {
                                return true;
                            }
                        }
                    }
                    false
                })
                .collect::<Vec<_>>()
                .into_boxed_slice()
        } else {
            vec![false; map_size_lg.chunks_len()].into_boxed_slice()
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
            cromatolis_climate,
            authored_near_water,
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
                                c.tree_density *= 1.0 - warp;
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
        self.gen_ctx
            .structure_gen
            .iter(wpos_min, wpos_max)
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

/// Inputs to [`authored_river_kind_override`], grouped into a struct rather
/// than several positional `bool` parameters.
struct AuthoredRiverKindInputs {
    is_elevated_lake: bool,
    is_water_body: bool,
    is_river_channel: bool,
    is_ocean: bool,
    alt_below_sea_level: bool,
    existing_river_kind: Option<RiverKind>,
    neighbor_pass_pos: Vec2<i32>,
    authored_river_cross_section: Vec2<f32>,
}

/// Decides the authored `RiverKind` for one chunk from its (already
/// mask-value-thresholded) authored water flags. Each authored mask keeps
/// its own semantics instead of being collapsed into one generic "is wet"
/// check: an elevated lake wins over the broader water-body mask, which
/// wins over a river channel, which wins over leaving the tile dry. A river
/// channel tile keeps the erosion sim's own `RiverKind::River` (with its
/// physically-derived cross-section) when the sim already computed one;
/// only chunks the sim didn't already flag as a river fall back to
/// `authored_river_cross_section`.
fn authored_river_kind_override(inputs: AuthoredRiverKindInputs) -> Option<RiverKind> {
    let AuthoredRiverKindInputs {
        is_elevated_lake,
        is_water_body,
        is_river_channel,
        is_ocean,
        alt_below_sea_level,
        existing_river_kind,
        neighbor_pass_pos,
        authored_river_cross_section,
    } = inputs;

    if is_elevated_lake {
        Some(RiverKind::Lake { neighbor_pass_pos })
    } else if is_water_body {
        if is_ocean || alt_below_sea_level {
            Some(RiverKind::Ocean)
        } else {
            // Flagged as water but neither below sea level nor tagged
            // `elevated_lakes` -- still real standing water.
            Some(RiverKind::Lake { neighbor_pass_pos })
        }
    } else if is_river_channel {
        match existing_river_kind {
            Some(RiverKind::River { .. }) => existing_river_kind,
            _ => Some(RiverKind::River {
                cross_section: authored_river_cross_section,
            }),
        }
    } else {
        None
    }
}

/// Humidity floor for `SimChunk::get_biome`'s `Swamp` branch (also requires
/// `authored_near_water`). Picked empirically against real Cromatolis
/// hydrology, not guessed: sampling `generate_cromatolis_world()` (real LFS
/// assets pulled from the VPS) over the 17,921 land chunks flagged
/// `authored_near_water` gives min=0.250, p10=0.501, p25=0.637, median=0.736,
/// p75=0.846, p90=0.930, max=0.970 -- i.e. land next to authored water skews
/// heavily humid already, with only a short tail down near the general-map
/// floor of 0.25. 0.6 sits below the p25 (keeps most of the near-water
/// population, matching how common coastal/riverine wetlands are meant to be
/// for a Caribbean-coast map) while still excluding the driest sliver (the
/// bottom ~10-25%, likely narrow river mouths on otherwise arid stretches
/// rather than real wetland). End result measured via
/// `cromatolis_swamp_coverage_regression_against_real_lfs_assets`: Swamp
/// covers 1.368% of the full Cromatolis chunk grid -- rare but genuinely
/// present, not the ~0% the biome had before this row (COW-4). The
/// pre-existing commented-out threshold (0.8, with no water-proximity gate
/// at all) would have kept `Swamp` effectively unreachable: it sat behind
/// `Forest`/`Jungle` in the old branch order, and both of those already
/// claim most tiles humid enough to clear 0.8.
const SWAMP_HUMIDITY_THRESHOLD: f32 = 0.6;

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
/// `climate` is the authored, region-specific tuning
/// (`AuthoredCromatolisClimate`, loaded from `cromatolis_v0_climate.ron`):
/// `sea_level_temp_c` is a hot tropical coastal baseline (`close(...,
/// CONFIG.desert_temp, ...)`/`(0.9..1.0).contains(&chunk.temp)`-style site
/// predicates elsewhere in worldgen need *some* chunks to reach the hot end
/// of the abstract scale, and the lowest-altitude chunks are the only ones
/// this curve ever makes that hot); `lapse_rate_c_per_m` is deliberately
/// steeper than Earth's ~6.5 °C/km average tropospheric lapse rate (still
/// within the range real lapse rates span with humidity/region -- the dry
/// adiabatic rate alone is ~9.8 °C/km): Cromatolis's actual relief tops out
/// around 1.3 km above sea level, and a literal Earth-average rate over
/// only that much relief would cool the highlands by less than 9 °C total,
/// leaving the whole map clustered in the warm end of the scale and
/// largely reproducing the "no usable cold/middle band" problem this curve
/// exists to fix -- just gradually instead of via a hard clamp. The
/// steeper rate lets Cromatolis's real, modest relief span the scale's
/// full practical range end to end.
fn cromatolis_baseline_temp(alt_pre: f32, climate: AuthoredCromatolisClimate) -> f32 {
    let temp_c = climate.sea_level_temp_c - alt_pre.max(0.0) * climate.lapse_rate_c_per_m;
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
        if gen_cdf.authored_cromatolis_v0 {
            temp = cromatolis_baseline_temp(alt_pre, gen_cdf.cromatolis_climate);
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
                    let vegetation =
                        authored_layer_value_for_cromatolis_v0(map_size_lg, posi, vegetation);
                    let altitude_tree_factor = if alt_pre < 520.0 {
                        1.0
                    } else if alt_pre < 760.0 {
                        Lerp::lerp(1.0, 0.45, (alt_pre - 520.0) / 240.0)
                    } else if alt_pre < 970.0 {
                        Lerp::lerp(0.45, 0.04, (alt_pre - 760.0) / 210.0)
                    } else {
                        0.0
                    };
                    if is_underwater {
                        0.0
                    } else if temp < 0.0 {
                        vegetation * 0.02
                    } else {
                        vegetation * altitude_tree_factor
                    }
                });
        if let Some(vegetation_density) = authored_vegetation_density
            && temp >= 0.0
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
            tree_density = if temp < 0.0 {
                vegetation_density.min(0.04)
            } else {
                vegetation_density.powf(1.55)
            };
            if temp >= 0.0 && vegetation_density > 0.82 {
                tree_density = tree_density.max(0.90);
            }
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
        } else if self.authored_region_id != Some(CROMATOLIS_V0_REGION_ID)
            && self.temp < CONFIG.snow_temp
        {
            // Cromatolis is lore-authored as tropical/caribbean -- no real
            // snow -- so this check is scoped to that specific region rather
            // than "any authored region is loaded" (COW-2 debt: a future
            // region shouldn't silently inherit Cromatolis's no-snow rule).
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
            // authored region is loaded" -- same COW-2 debt note). Without
            // this, `cromatolis_baseline_temp`'s hot coastal end (needed so
            // some chunks reach the `(0.9..1.0)` band several dungeon-site
            // predicates require) drives real low-altitude coastal humidity
            // down to ~0 via the evaporation dampener a few lines above
            // (`humidity *= (1.0 - (temp - CONFIG.tropical_temp).max(0.0) /
            // ...)`), which would otherwise satisfy this branch on ordinary
            // tropical coastline and paint it as literal desert terrain.
            BiomeKind::Desert
        } else if self.authored_near_water && self.humidity > SWAMP_HUMIDITY_THRESHOLD {
            // Gated on real hydrology (`authored_near_water`, not humidity
            // alone) -- a swamp is wet ground *near standing/flowing water*,
            // not just any humid open area (that's Jungle/Savannah/
            // Grassland's territory). Uses the authored water/elevated-lake/
            // river-channel masks directly (`authored_near_water`) rather
            // than `RiverData::near_water`: real Cromatolis LFS data shows
            // every authored river-channel pixel also sits inside the
            // broader `water` mask, so `authored_river_kind_override`'s
            // elevated-lake > water-body > river-channel priority order
            // means no chunk ever actually resolves to `RiverKind::River`
            // there today (COW-3 behavior, out of this row's scope to
            // change) -- `RiverData::near_water` would therefore never see a
            // land tile as "near" anything, only chunks that are themselves
            // Ocean/Lake (and those are already claimed by the branches
            // above, before this one runs). Checked *before* Jungle/Forest
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
    fn registry_resolves_cromatolis_map_asset_with_all_five_layers() {
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
        // already rely on, extended here to cover the 3 new layers too.
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

    // ---- Authored river-kind priority (elevated lake > water body > river
    // channel > nothing) ----

    fn river_kind_inputs() -> AuthoredRiverKindInputs {
        AuthoredRiverKindInputs {
            is_elevated_lake: false,
            is_water_body: false,
            is_river_channel: false,
            is_ocean: false,
            alt_below_sea_level: false,
            existing_river_kind: None,
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
    fn water_mask_below_sea_level_or_ocean_becomes_ocean() {
        let below_sea_level = authored_river_kind_override(AuthoredRiverKindInputs {
            is_water_body: true,
            alt_below_sea_level: true,
            ..river_kind_inputs()
        });
        assert_eq!(below_sea_level, Some(RiverKind::Ocean));

        let ocean_connected = authored_river_kind_override(AuthoredRiverKindInputs {
            is_water_body: true,
            is_ocean: true,
            ..river_kind_inputs()
        });
        assert_eq!(ocean_connected, Some(RiverKind::Ocean));
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

    #[test]
    fn river_channel_mask_preserves_an_already_computed_river() {
        let existing = Some(RiverKind::River {
            cross_section: Vec2::new(9.0, 9.0),
        });
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            existing_river_kind: existing,
            ..river_kind_inputs()
        });
        // Not replaced with the default cross-section -- the erosion sim's
        // physically-derived one wins.
        assert_eq!(kind, existing);
    }

    #[test]
    fn river_channel_mask_falls_back_to_the_default_cross_section() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            is_river_channel: true,
            existing_river_kind: None,
            ..river_kind_inputs()
        });
        assert_eq!(
            kind,
            Some(RiverKind::River {
                cross_section: Vec2::new(1.0, 2.0)
            })
        );
    }

    #[test]
    fn no_mask_flagged_clears_any_previous_river_kind() {
        let kind = authored_river_kind_override(AuthoredRiverKindInputs {
            // Nothing flagged, even though this chunk previously computed as
            // ocean -- the authored masks are authoritative for the region.
            existing_river_kind: Some(RiverKind::Ocean),
            ..river_kind_inputs()
        });
        assert_eq!(kind, None);
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
            .map(|alt_pre| cromatolis_baseline_temp(alt_pre as f32, climate))
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
        let mut prev = cromatolis_baseline_temp(-100.0, climate);
        for alt_pre in (0..3000).step_by(10) {
            let temp = cromatolis_baseline_temp(alt_pre as f32, climate);
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
            let temp = cromatolis_baseline_temp(alt_pre, climate);
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
    /// desert wildlife density formulas away from Cromatolis. Before that
    /// gate existed, the ungated `world.wildlife.spawn.desert.hot` formula
    /// (`close(chunk.temp, CONFIG.desert_temp + 0.2, 0.3)`, no humidity
    /// check) was nonzero for ~86% of the real generated Cromatolis grid --
    /// this reproduces that exact formula and asserts the gate zeroes it
    /// out everywhere.
    #[test]
    #[ignore]
    fn cromatolis_desert_wildlife_density_stays_gated_out_against_real_lfs_assets() {
        let sim = generate_cromatolis_world();
        let ungated_hits = sim
            .chunks
            .iter()
            .filter(|c| (c.temp - (CONFIG.desert_temp + 0.2)).abs() < 0.3)
            .count();
        let gated_hits = sim
            .chunks
            .iter()
            .filter(|c| {
                crate::layer::wildlife::not_cromatolis(c) > 0.0
                    && (c.temp - (CONFIG.desert_temp + 0.2)).abs() < 0.3
            })
            .count();
        assert!(
            ungated_hits as f64 / sim.chunks.len() as f64 > 0.5,
            "sanity check failed: expected the ungated formula to still hit a large fraction of \
             the map (regenerating this baseline confirms the gate is doing real work), got \
             {ungated_hits}/{}",
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
}
