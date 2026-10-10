use crate::{
    ColumnSample, IndexRef,
    all::ForestKind,
    authored_raster::{AuthoredWater, queries::ChunkWater},
    column::ColumnGen,
    sim::{self, SimChunk},
    util::Sampler,
};
use common::{lottery::Lottery, terrain::TerrainChunkSize, vol::RectVolSize};
use vek::*;

/// A wrapper type that may contain a reference to a generated world. If not,
/// default values will be provided.
///
/// XINDELER: the upstream accessors (`get_alt_approx`,
/// `get_surface_alt_approx`, `get_gradient_approx`) keep their upstream
/// meaning, the sim chunk table. The authored-aware views of the ground
/// (inside authored regions they read the authored ground and water layers,
/// see `WorldSim::ground_alt_at`) are the additive [`Self::ground_alt_at`],
/// [`Self::ground_gradient_at`] and [`Self::surface_alt_at`]; outside every
/// region, and on a world without an authored manifest, they return exactly
/// the table. Site and plot generation (`world/src/site/`) reads the
/// authored-aware ones, so plots stand on the authored ground; spatially
/// extended features (`world/src/layer/`: the cave graph, caverns, authored
/// voids) and civ placement (`world/src/civ/`, `world/src/sim/`) read the
/// table. The test `authored_raster::tests::ground_reads_follow_the_layering_rule`
/// enforces this split per directory: re-run it after every upstream merge, and
/// give each new call it reports the accessor its directory needs (or an
/// allow-list entry with the reason).
pub struct Land<'a> {
    sim: Option<&'a sim::WorldSim>,
}

impl<'a> Land<'a> {
    pub fn empty() -> Self { Self { sim: None } }

    pub fn size(&self) -> Vec2<u32> { self.sim.map_or(Vec2::one(), |s| s.get_size()) }

    pub fn from_sim(sim: &'a sim::WorldSim) -> Self { Self { sim: Some(sim) } }

    pub fn get_interpolated<T>(&self, wpos: Vec2<i32>, f: impl FnMut(&SimChunk) -> T) -> T
    where
        T: Copy + Default + std::ops::Add<Output = T> + std::ops::Mul<f32, Output = T>,
    {
        self.sim
            .and_then(|sim| sim.get_interpolated(wpos, f))
            .unwrap_or_default()
    }

    /// See `WorldSim::get_surface_alt_approx`.
    pub fn get_surface_alt_approx(&self, wpos: Vec2<i32>) -> f32 {
        self.sim
            .map(|sim| sim.get_surface_alt_approx(wpos))
            .unwrap_or(0.0)
    }

    pub fn get_alt_approx(&self, wpos: Vec2<i32>) -> f32 {
        self.sim
            .and_then(|sim| sim.get_alt_approx(wpos))
            .unwrap_or(0.0)
    }

    /// XINDELER: the authored water at `wpos`, if `wpos` is an authored wet
    /// column (see `crate::authored_raster`). `None` everywhere for a world
    /// without authored rasters.
    pub fn authored_water_at(&self, wpos: Vec2<i32>) -> Option<AuthoredWater> {
        self.sim.and_then(|sim| sim.authored_water_at(wpos))
    }

    /// XINDELER: the water facts of the chunk holding `wpos`, authored-aware
    /// (see `WorldSim::chunk_water`).
    pub fn chunk_water_wpos(&self, wpos: Vec2<i32>) -> Option<ChunkWater> {
        self.sim.and_then(|sim| {
            sim.chunk_water(wpos.map(|e| e.div_euclid(TerrainChunkSize::RECT_SIZE.x as i32)))
        })
    }

    /// XINDELER: `river.near_water()` of the chunk holding `wpos`,
    /// authored-aware. `None` outside the map.
    pub fn near_water_at(&self, wpos: Vec2<i32>) -> Option<bool> {
        self.chunk_water_wpos(wpos).map(|w| w.near_water)
    }

    /// XINDELER: [`Self::get_surface_alt_approx`] made authored-aware (see
    /// `WorldSim::surface_alt_at`).
    pub fn surface_alt_at(&self, wpos: Vec2<i32>) -> f32 {
        self.sim.map(|sim| sim.surface_alt_at(wpos)).unwrap_or(0.0)
    }

    /// XINDELER: [`Self::get_alt_approx`] made authored-aware (see
    /// `WorldSim::ground_alt_at`): the authored ground inside authored
    /// regions, the chunk table everywhere else.
    pub fn ground_alt_at(&self, wpos: Vec2<i32>) -> f32 {
        self.sim
            .and_then(|sim| sim.ground_alt_at(wpos))
            .unwrap_or(0.0)
    }

    /// XINDELER: [`Self::get_gradient_approx`] made authored-aware (see
    /// `WorldSim::ground_gradient_at`).
    pub fn ground_gradient_at(&self, wpos: Vec2<i32>) -> f32 {
        self.sim
            .and_then(|sim| sim.ground_gradient_at(wpos))
            .unwrap_or(0.0)
    }

    /// XINDELER: the number of water blocks of an authored wet column at
    /// `wpos` (see [`Self::authored_water_at`]).
    pub fn authored_depth_at(&self, wpos: Vec2<i32>) -> Option<i32> {
        self.authored_water_at(wpos).map(|w| w.depth_blocks())
    }

    pub fn get_downhill(&self, wpos: Vec2<i32>) -> Vec2<i32> {
        self.sim
            .and_then(|sim| sim.get_wpos(wpos))
            .and_then(|c| c.downhill)
            .unwrap_or(Vec2::zero())
    }

    pub fn get_gradient_approx(&self, wpos: Vec2<i32>) -> f32 {
        self.sim
            .and_then(|sim| sim.get_gradient_approx(self.wpos_chunk_pos(wpos)))
            .unwrap_or(0.0)
    }

    pub fn wpos_chunk_pos(&self, wpos: Vec2<i32>) -> Vec2<i32> {
        wpos.map2(TerrainChunkSize::RECT_SIZE, |e, sz| e.div_euclid(sz as i32))
    }

    pub fn get_chunk(&self, chunk_pos: Vec2<i32>) -> Option<&sim::SimChunk> {
        self.sim.and_then(|sim| sim.get(chunk_pos))
    }

    pub fn get_chunk_wpos(&self, wpos: Vec2<i32>) -> Option<&sim::SimChunk> {
        self.sim.and_then(|sim| sim.get_wpos(wpos))
    }

    pub fn get_approx_chunk_terrain_normal(&self, wpos: Vec2<i32>) -> Option<Vec3<f32>> {
        self.sim
            .and_then(|sim| sim.approx_chunk_terrain_normal(wpos))
    }

    pub fn get_nearest_path(
        &self,
        wpos: Vec2<i32>,
    ) -> Option<(f32, Vec2<f32>, sim::Path, Vec2<f32>)> {
        self.sim.and_then(|sim| sim.get_nearest_path(wpos))
    }

    pub fn column_sample<'sample>(
        &'sample self,
        wpos: Vec2<i32>,
        index: IndexRef<'sample>,
    ) -> Option<ColumnSample<'sample>> {
        self.sim
            .and_then(|sim| ColumnGen::new(sim).get((wpos, index, None)))
    }

    pub fn make_forest_lottery(&self, wpos: Vec2<i32>) -> Lottery<Option<ForestKind>> {
        match self.sim {
            Some(sim) => sim.make_forest_lottery(wpos),
            None => Lottery::from(vec![(1.0, None)]),
        }
    }
}
