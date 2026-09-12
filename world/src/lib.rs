#![expect(
    clippy::option_map_unit_fn,
    clippy::blocks_in_conditions,
    clippy::identity_op,
    clippy::needless_pass_by_ref_mut //until we find a better way for specs
)]
#![expect(clippy::branches_sharing_code)] // TODO: evaluate
#![deny(clippy::clone_on_ref_ptr)]
#![feature(option_zip)]
#![cfg_attr(feature = "simd", feature(portable_simd))]

mod all;
mod biome_profile;
mod block;
pub mod canvas;
pub mod civ;
mod column;
pub mod config;
pub mod index;
pub mod land;
pub mod layer;
pub mod pathfinding;
pub mod sim;
pub mod sim2;
pub mod site;
pub mod util;

// Reexports
pub use crate::{
    all::ForestKind,
    biome_profile::{BiomeProfile, BiomeProfiles},
    canvas::{Canvas, CanvasInfo},
    config::{CONFIG, Features},
    land::Land,
    layer::PathLocals,
};
pub use block::BlockGen;
use civ::WorldCivStage;
pub use column::{ColumnSample, ResolvedBiomeProfile};
pub use common::terrain::site::{DungeonKindMeta, SettlementKindMeta};
pub use index::{IndexOwned, IndexRef};
use sim::{SimChunk, WorldSimStage};

use crate::{
    column::ColumnGen,
    index::Index,
    layer::spot::SpotGenerate,
    site::{SiteKind, SpawnRules},
    util::{Grid, Sampler},
};
use common::{
    assets::{self, BoxedError, FileAsset, load_ron},
    calendar::Calendar,
    comp::Content,
    generation::{ChunkSupplement, EntityInfo, EntitySpawn, SpecialEntity},
    lod,
    map::{Marker, MarkerKind},
    resources::TimeOfDay,
    rtsim::TerrainResource,
    spiral::Spiral2d,
    spot::Spot,
    terrain::{
        Block, BlockKind, CoordinateConversions, SpriteKind, TerrainChunk, TerrainChunkMeta,
        TerrainChunkSize, TerrainGrid, TerrainOverrides,
    },
    vol::{ReadVol, RectVolSize, WriteVol},
};
use common_base::prof_span;
use common_net::msg::{WorldMapMsg, world_msg};
use enum_map::EnumMap;
use rand::{RngExt, prelude::*};
use rand_chacha::ChaCha8Rng;
use serde::Deserialize;
use std::{borrow::Cow, time::Duration};
use vek::*;

#[cfg(all(feature = "be-dyn-lib", feature = "use-dyn-lib"))]
compile_error!("Can't use both \"be-dyn-lib\" and \"use-dyn-lib\" features at once");

#[cfg(feature = "use-dyn-lib")]
use {common_dynlib::LoadedLib, lazy_static::lazy_static, std::sync::Arc, std::sync::Mutex};

#[cfg(feature = "use-dyn-lib")]
lazy_static! {
    pub static ref LIB: Arc<Mutex<Option<LoadedLib>>> =
        common_dynlib::init("xindeler-world", "world", &[]);
}

#[cfg(feature = "use-dyn-lib")]
pub fn init() { lazy_static::initialize(&LIB); }

#[derive(Debug)]
pub enum Error {
    Other(String),
}

#[derive(Debug)]
pub enum WorldGenerateStage {
    WorldSimGenerate(WorldSimStage),
    WorldCivGenerate(WorldCivStage),
    EconomySimulation,
    SpotGeneration,
}

pub struct World {
    sim: sim::WorldSim,
    civs: civ::Civs,
}

#[derive(Deserialize)]
pub struct Colors {
    pub deep_stone_color: (u8, u8, u8),
    pub block: block::Colors,
    pub column: column::Colors,
    pub layer: layer::Colors,
}

impl FileAsset for Colors {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

impl World {
    pub fn empty() -> (Self, IndexOwned) {
        let index = Index::new(0);
        (
            Self {
                sim: sim::WorldSim::empty(),
                civs: civ::Civs::default(),
            },
            IndexOwned::new(index),
        )
    }

    pub fn generate(
        seed: u32,
        opts: sim::WorldOpts,
        threadpool: &rayon::ThreadPool,
        report_stage: &(dyn Fn(WorldGenerateStage) + Send + Sync),
    ) -> (Self, IndexOwned) {
        prof_span!("World::generate");
        // NOTE: Generating index first in order to quickly fail if the color manifest
        // is broken.
        threadpool.install(|| {
            let mut index = Index::new(seed);
            let calendar = opts.calendar.clone();

            let mut sim = sim::WorldSim::generate(seed, opts, threadpool, &|stage| {
                report_stage(WorldGenerateStage::WorldSimGenerate(stage))
            });

            let civs =
                civ::Civs::generate(seed, &mut sim, &mut index, calendar.as_ref(), &|stage| {
                    report_stage(WorldGenerateStage::WorldCivGenerate(stage))
                });

            report_stage(WorldGenerateStage::EconomySimulation);
            sim2::simulate(&mut index, &mut sim);

            report_stage(WorldGenerateStage::SpotGeneration);
            Spot::generate(&mut sim);

            (Self { sim, civs }, IndexOwned::new(index))
        })
    }

    pub fn sim(&self) -> &sim::WorldSim { &self.sim }

    pub fn civs(&self) -> &civ::Civs { &self.civs }

    pub fn tick(&self, _dt: Duration) {
        // TODO
    }

    pub fn get_map_data(&self, index: IndexRef, threadpool: &rayon::ThreadPool) -> WorldMapMsg {
        prof_span!("World::get_map_data");
        threadpool.install(|| {
            WorldMapMsg {
                pois: self
                    .civs()
                    .pois
                    .iter()
                    .map(|(_, poi)| world_msg::PoiInfo {
                        name: poi.name.clone(),
                        kind: match &poi.kind {
                            civ::PoiKind::Peak(alt) => world_msg::PoiKind::Peak(*alt),
                            civ::PoiKind::Biome(size) => world_msg::PoiKind::Lake(*size),
                        },
                        wpos: poi.loc * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                    })
                    .collect(),
                sites: self
                    .civs()
                    .sites
                    .values()
                    .filter_map(|site| Some((site.kind.marker()?, site)))
                    .map(|(marker, site)| {
                        Marker::at(
                            (site.center * TerrainChunkSize::RECT_SIZE.map(|e| e as i32)).as_(),
                        )
                        .with_kind(marker)
                        .with_site_id(site.site_tmp.map(|i| i.id()))
                        .with_label(site.site_tmp.map(|id| {
                            Content::Plain(index.sites[id].name().unwrap_or("").to_string())
                        }))
                    })
                    .chain(
                        layer::cave::surface_entrances(&Land::from_sim(self.sim()), index)
                            .map(|wpos| Marker::at(wpos.as_()).with_kind(MarkerKind::Cave)),
                    )
                    .collect(),
                possible_starting_sites: {
                    const STARTING_SITE_COUNT: usize = 5;

                    let mut candidates = self
                        .civs()
                        .sites
                        .iter()
                        .filter_map(|(_, civ_site)| Some((civ_site, civ_site.site_tmp?)))
                        // An authored settlement explicitly excluded from player-start selection
                        // (e.g. for lore/terrain reasons) must never be a candidate at all --
                        // unlike the score=0 cases below (which still compete for a slot if
                        // fewer than `STARTING_SITE_COUNT` alternatives exist), this is a hard
                        // exclusion regardless of how many other candidates there are.
                        .filter(|(civ_site, _)| civ_site.is_eligible_as_starting_site())
                        .map(|(civ_site, site_id)| {
                            // Score the site according to how suitable it is to be a starting site

                            let site = &index.sites[site_id];
                            let mut score = match site.kind {
                                Some(SiteKind::Refactor) => 2.0,
                                Some(kind)
                                    if matches!(
                                        kind.meta(),
                                        Some(common::terrain::SiteKindMeta::Settlement(_))
                                    ) =>
                                {
                                    1.0
                                },
                                // Non-town sites should not be chosen as starting sites and get a
                                // score of 0
                                _ => return (site_id.id(), 0.0),
                            };

                            /// Optimal number of plots in a starter town
                            const OPTIMAL_STARTER_TOWN_SIZE: f32 = 30.0;

                            // Prefer sites of a medium size
                            let plots = site.plots().len() as f32;
                            let size_score = if plots > OPTIMAL_STARTER_TOWN_SIZE {
                                1.0 + (1.0
                                    / (1.0 + ((plots - OPTIMAL_STARTER_TOWN_SIZE) / 15.0).powi(3)))
                            } else {
                                (2.05
                                    / (1.0 + ((OPTIMAL_STARTER_TOWN_SIZE - plots) / 15.0).powi(5)))
                                    - 0.05
                            }
                            .max(0.01);

                            score *= size_score;

                            // Prefer sites that are close to the centre of the world
                            let pos_score = (10.0
                                / (1.0
                                    + (civ_site
                                        .center
                                        .map2(self.sim().get_size(), |e, sz| {
                                            (e as f32 / sz as f32 - 0.5).abs() * 2.0
                                        })
                                        .reduce_partial_max())
                                    .powi(6)
                                        * 25.0))
                                .max(0.02);
                            score *= pos_score;

                            // Check if neighboring biomes are beginner friendly
                            let mut chunk_scores = 2.0;
                            for (chunk, distance) in
                                Spiral2d::with_radius(10).filter_map(|rel_pos| {
                                    let chunk_pos = civ_site.center + rel_pos * 2;
                                    self.sim()
                                        .get(chunk_pos)
                                        .zip(Some(rel_pos.as_::<f32>().magnitude()))
                                })
                            {
                                let weight = 1.0 / (distance * std::f32::consts::TAU + 1.0);
                                let chunk_difficulty = 20.0
                                    / (20.0 + chunk.get_biome().difficulty().pow(4) as f32 / 5.0);
                                // let chunk_difficulty = 1.0 / chunk.get_biome().difficulty() as
                                // f32;

                                chunk_scores *= 1.0 - weight + chunk_difficulty * weight;
                            }

                            score *= chunk_scores;

                            (site_id.id(), score)
                        })
                        .collect::<Vec<_>>();
                    candidates.sort_by_key(|(_, score)| -(*score * 1000.0) as i32);
                    candidates
                        .into_iter()
                        .map(|(site_id, _)| site_id)
                        .take(STARTING_SITE_COUNT)
                        .collect()
                },
                ..self.sim.get_map(index, self.sim().calendar.as_ref())
            }
        })
    }

    pub fn sample_columns(
        &self,
    ) -> impl Sampler<
        '_,
        Index = (Vec2<i32>, IndexRef<'_>, Option<&'_ Calendar>),
        Sample = Option<ColumnSample<'_>>,
    > + '_ {
        ColumnGen::new(&self.sim)
    }

    pub fn sample_blocks(&self) -> BlockGen<'_> { BlockGen::new(ColumnGen::new(&self.sim)) }

    /// Same as [`Self::sample_blocks`], but -- when `overrides` is `Some` --
    /// applies active regional terrain overrides' column-level effects (see
    /// `common::terrain::regional_override` and `column::ColumnGen`).
    pub fn sample_blocks_with_overrides<'a>(
        &'a self,
        overrides: Option<&'a TerrainOverrides>,
    ) -> BlockGen<'a> {
        BlockGen::new(match overrides {
            Some(overrides) => ColumnGen::with_overrides(&self.sim, overrides),
            None => ColumnGen::new(&self.sim),
        })
    }

    /// Find a position that's accessible to a player at the given world
    /// position by searching blocks vertically.
    ///
    /// If `ascending` is `true`, we try to find the highest accessible position
    /// instead of the lowest.
    pub fn find_accessible_pos(
        &self,
        index: IndexRef,
        spawn_wpos: Vec2<i32>,
        ascending: bool,
    ) -> Vec3<f32> {
        let chunk_pos = TerrainGrid::chunk_key(spawn_wpos);

        // Unwrapping because generate_chunk only returns err when should_continue evals
        // to true
        let (tc, _cs) = self
            .generate_chunk(index, chunk_pos, None, || false, None, None)
            .unwrap();

        tc.find_accessible_pos(spawn_wpos, ascending)
    }

    #[expect(clippy::result_unit_err)]
    pub fn generate_chunk(
        &self,
        index: IndexRef,
        chunk_pos: Vec2<i32>,
        rtsim_resources: Option<EnumMap<TerrainResource, f32>>,
        // TODO: misleading name
        mut should_continue: impl FnMut() -> bool,
        time: Option<(TimeOfDay, Calendar)>,
        overrides: Option<&TerrainOverrides>,
    ) -> Result<(TerrainChunk, ChunkSupplement), ()> {
        let calendar = time.as_ref().map(|(_, cal)| cal);

        // Filtered ONCE per `generate_chunk` call to just the overrides that
        // actually touch this chunk (an AABB-then-exact `touches_chunk`
        // check per override, done here rather than per column). Both the
        // column-level sampler below and the chunk-level `Cow<SimChunk>`
        // patch further down reuse this same small subset, so a chunk
        // nowhere near any active override never has `ColumnGen::get`
        // scanning the full, potentially server-wide, active-overrides list
        // once per block-column -- and a chunk that IS touched only ever
        // gets filtered once, not once per column either.
        let touching_overrides: Option<TerrainOverrides> = overrides.and_then(|overrides| {
            let active: Vec<_> = overrides
                .overrides_touching_chunk(chunk_pos, TerrainChunkSize::RECT_SIZE)
                .cloned()
                .collect();
            (!active.is_empty()).then_some(TerrainOverrides {
                version: overrides.version,
                active,
            })
        });

        let mut sampler = self.sample_blocks_with_overrides(touching_overrides.as_ref());

        let chunk_wpos2d = chunk_pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
        let chunk_center_wpos2d = chunk_wpos2d + TerrainChunkSize::RECT_SIZE.map(|e| e as i32 / 2);
        let grid_border = 4;
        let zcache_grid = Grid::populate_from(
            TerrainChunkSize::RECT_SIZE.map(|e| e as i32) + grid_border * 2,
            |offs| sampler.get_z_cache(chunk_wpos2d - grid_border + offs, index, calendar),
        );

        let air = Block::air(SpriteKind::Empty);
        let stone = Block::new(
            BlockKind::Rock,
            zcache_grid
                .get(grid_border + TerrainChunkSize::RECT_SIZE.map(|e| e as i32) / 2)
                .and_then(|zcache| zcache.as_ref())
                .map(|zcache| zcache.sample.stone_col)
                .unwrap_or_else(|| index.colors.deep_stone_color.into()),
        );

        let (base_z, sim_chunk) = match self
            .sim
            /*.get_interpolated(
                chunk_pos.map2(chunk_size2d, |e, sz: u32| e * sz as i32 + sz as i32 / 2),
                |chunk| chunk.get_base_z(),
            )
            .and_then(|base_z| self.sim.get(chunk_pos).map(|sim_chunk| (base_z, sim_chunk))) */
            .get_base_z(chunk_pos)
        {
            Some(base_z) => (base_z as i32, self.sim.get(chunk_pos).unwrap()),
            // Some((base_z, sim_chunk)) => (base_z as i32, sim_chunk),
            None => {
                // NOTE: This is necessary in order to generate a handful of chunks at the edges
                // of the map.
                return Ok((self.sim().generate_oob_chunk(), ChunkSupplement::default()));
            },
        };

        // Chunk-level (as opposed to `ColumnGen::get`'s per-column) regional
        // terrain override patch: applied ONCE per `generate_chunk` call, at
        // this chunk's center, and consumed everywhere below that would
        // otherwise read the raw `sim_chunk` for its biome label,
        // wildlife-density closures, or forest-species lottery (`get_biome`,
        // `apply_wildlife_supplement`, `layer::tree`'s real call site).
        // `Cow::Borrowed` (zero-cost) unless an override actually touches
        // this chunk.
        let sim_chunk: Cow<SimChunk> = match touching_overrides.as_ref() {
            Some(overrides) => {
                let (temp, humidity, tree_density_mul, damage, biome_governing) = overrides
                    .climate_tree_density_damage_and_profile_at(
                        chunk_center_wpos2d,
                        sim_chunk.temp,
                        sim_chunk.humidity,
                    );
                // NOTE: `(*sim_chunk).clone()`, not `sim_chunk.clone()` --
                // `sim_chunk` is already `&SimChunk` here, and `&T` is
                // itself always `Clone` (a cheap pointer copy) regardless of
                // whether `T` is; without the explicit deref this would
                // silently clone the reference instead of the chunk.
                let mut patched: SimChunk = (*sim_chunk).clone();
                patched.temp = temp;
                patched.humidity = humidity;
                // `damage.vegetation_mul` reuses the exact same multiplier
                // mechanism `ClimateOverride::tree_density_mul` already
                // threads through here, rather than a parallel path.
                patched.tree_density *= tree_density_mul * damage.vegetation_mul;
                // Best-effort: only affects this chunk's informational
                // metadata (minimap coloring, etc, via `TerrainChunkMeta`
                // below) at the chunk CENTER -- the real per-column terrain
                // height change happens in `ColumnGen::get`, not here.
                patched.alt += damage.rim - damage.depth;
                // Resolve a governing `BiomeProfile` override's catalog
                // entry (same lookup `ColumnGen::get` does, at chunk
                // granularity here) and patch `forest_kind` so
                // `ColumnSample::forest_kind`/this chunk's biome metadata
                // agree with what `layer::tree`'s per-position override
                // (see `world/src/layer/tree.rs`) actually places. This is
                // a DETERMINISTIC representative pick (highest-weight entry
                // in the profile's own list), not a seeded random draw --
                // unlike the real per-tree placement, nothing here needs to
                // vary tree-to-tree, it just needs a single label for the
                // whole chunk.
                if let Some(governing) = biome_governing
                    && let Some(profile) = index
                        .biome_profiles
                        .entries
                        .iter()
                        .find(|profile| profile.id == governing.profile.profile)
                {
                    patched.tree_density *= Lerp::lerp(
                        1.0,
                        profile.tree_density_mul,
                        governing.blend * governing.profile.intensity.clamp(0.0, 1.0),
                    );
                    if let Some((forest_kind, _)) =
                        profile.forest.iter().max_by(|(_, a), (_, b)| {
                            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                        })
                    {
                        patched.forest_kind = *forest_kind;
                    }
                }
                Cow::Owned(patched)
            },
            None => Cow::Borrowed(sim_chunk),
        };
        let sim_chunk: &SimChunk = &sim_chunk;

        let meta = TerrainChunkMeta::new(
            sim_chunk.get_location_name(&index.sites, &self.civs.pois, chunk_center_wpos2d),
            sim_chunk.get_biome(),
            sim_chunk.alt,
            sim_chunk.tree_density,
            sim_chunk.river.is_river(),
            sim_chunk.river.near_water(),
            sim_chunk.river.velocity,
            sim_chunk.temp,
            sim_chunk.humidity,
            sim_chunk
                .sites
                .iter()
                .filter(|id| {
                    index.sites[**id]
                        .origin
                        .as_::<f32>()
                        .distance_squared(chunk_center_wpos2d.as_::<f32>())
                        <= index.sites[**id].radius().powi(2)
                })
                .min_by_key(|id| {
                    index.sites[**id]
                        .origin
                        .as_::<i64>()
                        .distance_squared(chunk_center_wpos2d.as_::<i64>())
                })
                .map(|id| index.sites[*id].meta().unwrap_or_default()),
            self.sim.approx_chunk_terrain_normal(chunk_pos),
            sim_chunk.rockiness,
            sim_chunk.cliff_height,
        );

        let mut chunk = TerrainChunk::new(base_z, stone, air, meta);

        for y in 0..TerrainChunkSize::RECT_SIZE.y as i32 {
            for x in 0..TerrainChunkSize::RECT_SIZE.x as i32 {
                if should_continue() {
                    return Err(());
                };

                let offs = Vec2::new(x, y);

                let z_cache = match zcache_grid.get(grid_border + offs) {
                    Some(Some(z_cache)) => z_cache,
                    _ => continue,
                };

                let (min_z, max_z) = z_cache.get_z_limits();

                (base_z..min_z as i32).for_each(|z| {
                    let _ = chunk.set(Vec3::new(x, y, z), stone);
                });

                (min_z as i32..max_z as i32).for_each(|z| {
                    let lpos = Vec3::new(x, y, z);
                    let wpos = Vec3::from(chunk_wpos2d) + lpos;

                    if let Some(block) = sampler.get_with_z_cache(wpos, Some(z_cache)) {
                        let _ = chunk.set(lpos, block);
                    }
                });
            }
        }

        let sample_get = |offs| {
            zcache_grid
                .get(grid_border + offs)
                .and_then(Option::as_ref)
                .map(|zc| &zc.sample)
        };

        // Only use for rng affecting dynamic elements like chests and entities!
        let mut dynamic_rng = ChaCha8Rng::from_seed(rand::rng().random());

        // Apply layers (paths, caves, etc.)
        let mut canvas = Canvas {
            info: CanvasInfo {
                chunk_pos,
                wpos: chunk_pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                column_grid: &zcache_grid,
                column_grid_border: grid_border,
                chunks: &self.sim,
                index,
                chunk: sim_chunk,
                calendar,
                overrides: touching_overrides.as_ref(),
            },
            chunk: &mut chunk,
            entity_spawns: Vec::new(),
            rtsim_resource_blocks: Vec::new(),
        };

        if index.features.train_tracks && !sim_chunk.authored_cromatolis_v0 {
            layer::apply_trains_to(&mut canvas, &self.sim, sim_chunk, chunk_center_wpos2d);
        }

        if index.features.caverns {
            layer::apply_caverns_to(&mut canvas, &mut dynamic_rng);
        }
        if index.features.caves {
            layer::apply_caves_to(&mut canvas, &mut dynamic_rng);
        }
        if sim_chunk.authored_cromatolis_v0 {
            layer::apply_cromatolis_interiors_to(&mut canvas);
            layer::apply_cromatolis_cave_features_to(&mut canvas);
            layer::apply_cromatolis_local_aerial_features_to(&mut canvas);
        }
        if index.features.rocks {
            layer::apply_rocks_to(&mut canvas, &mut dynamic_rng);
        }
        if index.features.shrubs {
            layer::apply_shrubs_to(&mut canvas, &mut dynamic_rng);
        }
        if index.features.trees {
            layer::apply_trees_to(&mut canvas, &mut dynamic_rng, calendar);
        }
        // Not gated behind any `index.features` flag: a `Damage` override's
        // debris field is an explicit, admin/event-activated regional
        // effect, not an ambient world-gen feature toggle -- it has nothing
        // to do with the trees/scatter features immediately around it.
        layer::apply_terrain_damage_to(&mut canvas, &mut dynamic_rng);
        if index.features.scatter {
            layer::apply_scatter_to(&mut canvas, &mut dynamic_rng, calendar);
        }
        if index.features.paths {
            layer::apply_paths_to(&mut canvas);
        }
        if index.features.spots {
            layer::apply_spots_to(&mut canvas, &mut dynamic_rng);
        }
        // layer::apply_coral_to(&mut canvas);

        // Apply site generation
        sim_chunk
            .sites
            .iter()
            .for_each(|site| index.sites[*site].render(&mut canvas, &mut dynamic_rng));

        let mut rtsim_resource_blocks = std::mem::take(&mut canvas.rtsim_resource_blocks);
        let mut supplement = ChunkSupplement {
            entity_spawns: std::mem::take(&mut canvas.entity_spawns),
            rtsim_max_resources: Default::default(),
        };
        drop(canvas);

        let gen_entity_pos = |dynamic_rng: &mut ChaCha8Rng| {
            let lpos2d = TerrainChunkSize::RECT_SIZE
                .map(|sz| dynamic_rng.random::<u32>().rem_euclid(sz) as i32);
            let mut lpos = Vec3::new(
                lpos2d.x,
                lpos2d.y,
                sample_get(lpos2d).map(|s| s.alt as i32 - 32).unwrap_or(0),
            );

            while let Some(block) = chunk.get(lpos).ok().copied().filter(Block::is_solid) {
                lpos.z += block.solid_height().ceil() as i32;
            }

            (Vec3::from(chunk_wpos2d) + lpos).map(|e: i32| e as f32) + 0.5
        };

        if sim_chunk.contains_waypoint {
            let waypoint_pos = gen_entity_pos(&mut dynamic_rng);
            let mut spawn_rules = SpawnRules::default();
            for site in sim_chunk.sites.iter().map(|site| &index.sites[*site]) {
                site.spawn_rules(
                    &mut spawn_rules,
                    &Land::from_sim(&self.sim),
                    waypoint_pos.xy().as_(),
                );
            }
            if spawn_rules.waypoints {
                supplement.add_entity_spawn(EntitySpawn::Entity(Box::new(
                    EntityInfo::at(waypoint_pos).into_special(SpecialEntity::Waypoint),
                )));
            }
        }

        // Apply layer supplement
        layer::wildlife::apply_wildlife_supplement(
            &mut dynamic_rng,
            chunk_wpos2d,
            sample_get,
            &chunk,
            index,
            sim_chunk,
            &mut supplement,
            time.as_ref(),
        );

        // Apply site supplementary information
        sim_chunk.sites.iter().for_each(|site| {
            index.sites[*site].apply_supplement(&mut dynamic_rng, chunk_wpos2d, &mut supplement)
        });

        // Finally, defragment to minimize space consumption.
        chunk.defragment();

        // Before we finish, we check candidate rtsim resource blocks, deduplicating
        // positions and only keeping those that actually do have resources.
        // Although this looks potentially very expensive, only blocks that are rtsim
        // resources (i.e: a relatively small number of sprites) are processed here.
        if let Some(rtsim_resources) = rtsim_resources {
            rtsim_resource_blocks.sort_unstable_by_key(|pos| pos.into_array());
            rtsim_resource_blocks.dedup();
            for wpos in rtsim_resource_blocks {
                let _ = chunk.map(wpos - chunk_wpos2d.with_z(0), |block| {
                    if let Some(res) = block.get_rtsim_resource() {
                        // Note: this represents the upper limit, not the actual number spanwed, so
                        // we increment this before deciding whether we're going to spawn the
                        // resource.
                        supplement.rtsim_max_resources[res] += 1;

                        debug_assert!(
                            0.0 <= rtsim_resources[res] && rtsim_resources[res] <= 1.0,
                            "The rtsim resource {res:?} has the value '{}', which is not in the \
                             expected range of 0.0..=1.0. When registering a block with the \
                             sprite `{:?}`, with the damage `{:?}`.",
                            rtsim_resources[res],
                            block.get_sprite(),
                            block.get_attr::<common::terrain::sprite::Damage>().ok(),
                        );

                        // Throw a dice to determine whether this resource should actually spawn
                        // TODO: Don't throw a dice, try to generate the *exact* correct number
                        if dynamic_rng.random_bool(rtsim_resources[res].clamp(0.0, 1.0) as f64) {
                            block
                        } else {
                            block.into_vacant()
                        }
                    } else {
                        block
                    }
                });
            }
        }

        Ok((chunk, supplement))
    }

    // Zone coordinates
    pub fn get_lod_zone(&self, pos: Vec2<i32>, index: IndexRef) -> lod::Zone {
        let min_wpos = pos.map(lod::to_wpos);
        let max_wpos = (pos + 1).map(lod::to_wpos);

        let mut objects = Vec::new();

        // Add trees
        prof_span!(guard, "add trees");
        objects.extend(
            &mut self
                .sim()
                .get_area_trees(min_wpos, max_wpos)
                .filter_map(|attr| {
                    ColumnGen::new(self.sim())
                        .get((attr.pos, index, self.sim().calendar.as_ref()))
                        .filter(|col| layer::tree::tree_valid_at(attr.pos, col, None, attr.seed))
                        .zip(Some(attr))
                })
                .filter_map(|(col, tree)| {
                    Some(lod::Object {
                        kind: match tree.forest_kind {
                            all::ForestKind::Dead => lod::ObjectKind::Dead,
                            all::ForestKind::Pine => lod::ObjectKind::Pine,
                            all::ForestKind::Mangrove => lod::ObjectKind::Mangrove,
                            all::ForestKind::Acacia => lod::ObjectKind::Acacia,
                            all::ForestKind::Birch => lod::ObjectKind::Birch,
                            all::ForestKind::Redwood => lod::ObjectKind::Redwood,
                            all::ForestKind::Baobab => lod::ObjectKind::Baobab,
                            all::ForestKind::Frostpine => lod::ObjectKind::Frostpine,
                            all::ForestKind::Palm => lod::ObjectKind::Palm,
                            _ => lod::ObjectKind::GenericTree,
                        },
                        pos: {
                            let rpos = tree.pos - min_wpos;
                            if rpos.is_any_negative() {
                                return None;
                            } else {
                                rpos.map(|e| e as i16).with_z(col.alt as i16)
                            }
                        },
                        flags: lod::InstFlags::empty()
                            | if col.snow_cover {
                                lod::InstFlags::SNOW_COVERED
                            } else {
                                lod::InstFlags::empty()
                            }
                            // Apply random rotation
                            | lod::InstFlags::from_bits(((tree.seed % 4) as u8) << 2).expect("This shouldn't set unknown bits"),
                        color: {
                            let field = crate::util::RandomField::new(tree.seed);
                            let lerp = field.get_f32(Vec3::from(tree.pos)) * 0.8 + 0.1;
                            let sblock = tree.forest_kind.leaf_block();

                            crate::all::leaf_color(index, tree.seed, lerp, &sblock)
                                .unwrap_or(Rgb::black())
                        },
                    })
                }),
        );
        drop(guard);

        // Add structures
        objects.extend(
            index
                .sites
                .iter()
                .filter(|(_, site)| {
                    site.origin
                        .map2(min_wpos.zip(max_wpos), |e, (min, max)| e >= min && e < max)
                        .reduce_and()
                })
                .flat_map(|(_, site)| {
                    site.plots().filter_map(|plot| match &plot.kind {
                        site::plot::PlotKind::House(h) => Some((
                            site.tile_wpos(plot.root_tile),
                            h.roof_color(),
                            lod::ObjectKind::House,
                        )),
                        site::plot::PlotKind::GiantTree(t) => Some((
                            site.tile_wpos(plot.root_tile),
                            t.leaf_color(),
                            lod::ObjectKind::GiantTree,
                        )),
                        site::plot::PlotKind::Haniwa(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::Haniwa,
                        )),
                        site::plot::PlotKind::DesertCityMultiPlot(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::Desert,
                        )),
                        site::plot::PlotKind::DesertCityArena(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::Arena,
                        )),
                        site::plot::PlotKind::SavannahHut(_)
                        | site::plot::PlotKind::SavannahWorkshop(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::SavannahHut,
                        )),
                        site::plot::PlotKind::SavannahAirshipDock(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::SavannahAirshipDock,
                        )),
                        site::plot::PlotKind::TerracottaPalace(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::TerracottaPalace,
                        )),
                        site::plot::PlotKind::TerracottaHouse(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::TerracottaHouse,
                        )),
                        site::plot::PlotKind::TerracottaYard(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::TerracottaYard,
                        )),
                        site::plot::PlotKind::AirshipDock(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::AirshipDock,
                        )),
                        site::plot::PlotKind::CoastalHouse(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::CoastalHouse,
                        )),
                        site::plot::PlotKind::CoastalWorkshop(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::CoastalWorkshop,
                        )),
                        site::plot::PlotKind::CoastalAirshipDock(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::CoastalAirshipDock,
                        )),
                        site::plot::PlotKind::DesertCityAirshipDock(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::DesertCityAirshipDock,
                        )),
                        site::plot::PlotKind::CliffTownAirshipDock(_) => Some((
                            site.tile_wpos(plot.root_tile),
                            Rgb::black(),
                            lod::ObjectKind::CliffTownAirshipDock,
                        )),
                        _ => None,
                    })
                })
                .filter_map(|(wpos2d, color, model)| {
                    ColumnGen::new(self.sim())
                        .get((wpos2d, index, self.sim().calendar.as_ref()))
                        .zip(Some((wpos2d, color, model)))
                })
                .map(|(column, (wpos2d, color, model))| lod::Object {
                    kind: model,
                    pos: (wpos2d - min_wpos)
                        .map(|e| e as i16)
                        .with_z(self.sim().get_alt_approx(wpos2d).unwrap_or(0.0) as i16),
                    flags: if column.snow_cover {
                        lod::InstFlags::SNOW_COVERED
                    } else {
                        lod::InstFlags::empty()
                    },
                    color,
                }),
        );

        lod::Zone { objects }
    }

    // determine waypoint name
    pub fn get_location_name(&self, index: IndexRef, wpos2d: Vec2<i32>) -> Option<String> {
        let chunk_pos = wpos2d.wpos_to_cpos();
        let sim_chunk = self.sim.get(chunk_pos)?;
        sim_chunk.get_location_name(&index.sites, &self.civs.pois, wpos2d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requires the real Cromatolis LFS assets to be pulled locally (`git lfs
    /// pull` against the VPS store); not run automated, matching this
    /// crate's existing precedent for tests whose meaningful assertion
    /// depends on real, environment-specific data. Recommended command:
    /// `cargo test -p xindeler-world
    /// ineligible_authored_settlements_never_appear_as_possible_starting_sites
    /// -- --ignored`
    #[test]
    #[ignore]
    fn ineligible_authored_settlements_never_appear_as_possible_starting_sites() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (mut world, index) = World::generate(
            0,
            sim::WorldOpts {
                seed_elements: true,
                world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();

        // No settlement in the real export is currently marked
        // `start_eligible: false`, so this test exercises the exclusion
        // mechanism directly: take the *baseline* `possible_starting_sites`
        // result, pick a real authored settlement that's actually part of
        // it (not just any authored settlement -- most of the 62 wouldn't
        // rank in the top slots anyway, so excluding an arbitrary one
        // wouldn't move the result), force-exclude it, and confirm it drops
        // out.
        let baseline = world.get_map_data(index_ref, &threadpool);
        let site_tmp_to_civ_site_id: std::collections::HashMap<_, _> = world
            .civs
            .sites
            .iter()
            .filter_map(|(civ_site_id, site)| Some((site.site_tmp?.id(), civ_site_id)))
            .collect();
        let target_site_id = *baseline
            .possible_starting_sites
            .iter()
            .find(|site_tmp| {
                site_tmp_to_civ_site_id
                    .get(site_tmp)
                    .is_some_and(|&civ_site_id| {
                        world
                            .civs
                            .sites
                            .get(civ_site_id)
                            .is_authored_starting_settlement()
                    })
            })
            .expect(
                "at least one real authored settlement must rank as a possible starting site for \
                 this test to be meaningful",
            );
        let target_civ_site_id = site_tmp_to_civ_site_id[&target_site_id];

        world
            .civs
            .sites
            .get_mut(target_civ_site_id)
            .set_start_eligible_for_test(false);

        let after_exclusion = world.get_map_data(index_ref, &threadpool);
        assert!(
            !after_exclusion
                .possible_starting_sites
                .contains(&target_site_id),
            "an authored settlement explicitly marked start_eligible: false was still returned as \
             a possible starting site"
        );
    }
}
