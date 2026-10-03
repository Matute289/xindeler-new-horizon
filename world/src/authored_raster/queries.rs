//! Authored-aware water queries for the consumers that read the sim chunk
//! table instead of generated blocks: rtsim (boats, fish, spawns), civ
//! (placement, road costs), ports, wildlife, admin commands.
//!
//! Inside an authored region these answer from the raster (the water the
//! player sees); everywhere else they return exactly what the chunk table
//! said before authored rasters existed, so every caller outside a region --
//! and every world without a manifest -- behaves bit-identically.

use super::{AuthoredCell, AuthoredColumn, AuthoredWater, SEA_TOP_BLOCK};
use crate::{World, sim::WorldSim};
use vek::*;

/// Where a body of water at a column comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterSource {
    /// An authored raster region (exact, per column).
    Authored,
    /// The sim chunk table (river, lake or ocean of the chunk).
    Sim,
}

/// The water at one column, as [`WorldSim::water_at`] sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterAt {
    /// Whether the column is water.
    pub wet: bool,
    /// Altitude of the top of the water (`SimChunk::water_alt` semantics:
    /// the top face of the top water block), when wet.
    pub surface_alt: Option<f32>,
    /// Altitude of the ground under the water, when wet.
    pub bed_alt: Option<f32>,
    pub source: WaterSource,
}

impl WorldSim {
    /// The authored column at `wpos` (cell, feather, distance to water,
    /// region settings), `None` outside every authored region.
    pub fn authored_column_at(&self, wpos: Vec2<i32>) -> Option<AuthoredColumn> {
        self.authored_rasters.as_ref()?.column(wpos)
    }

    /// The authored cell at `wpos`, `None` outside every authored region.
    pub fn authored_cell_at(&self, wpos: Vec2<i32>) -> Option<AuthoredCell> {
        self.authored_rasters.as_ref()?.cell_at(wpos)
    }

    /// The authored water at `wpos`, if it is an authored wet column.
    pub fn authored_water_at(&self, wpos: Vec2<i32>) -> Option<AuthoredWater> {
        self.authored_rasters.as_ref()?.water_at(wpos)
    }

    /// Water at a column: the authored raster inside a region, else the
    /// chunk table (`river_kind.is_some()`, `water_alt`, `alt`). `None`
    /// outside the map.
    pub fn water_at(&self, wpos: Vec2<i32>) -> Option<WaterAt> {
        if let Some(cell) = self.authored_cell_at(wpos) {
            return Some(match cell {
                AuthoredCell::Wet {
                    surface_block,
                    bed_block,
                } => WaterAt {
                    wet: true,
                    surface_alt: Some((surface_block + 1) as f32),
                    bed_alt: Some((bed_block + 1) as f32),
                    source: WaterSource::Authored,
                },
                _ => WaterAt {
                    wet: false,
                    surface_alt: None,
                    bed_alt: None,
                    source: WaterSource::Authored,
                },
            });
        }
        let chunk = self.get_wpos(wpos)?;
        let wet = chunk.river.river_kind.is_some();
        Some(WaterAt {
            wet,
            surface_alt: wet.then_some(chunk.water_alt),
            bed_alt: wet.then_some(chunk.alt),
            source: WaterSource::Sim,
        })
    }

    /// Whether the column at `wpos` is water ([`Self::water_at`]); `None`
    /// outside the map.
    pub fn is_wet_at(&self, wpos: Vec2<i32>) -> Option<bool> { self.water_at(wpos).map(|w| w.wet) }

    /// `get_surface_alt_approx` made authored-aware: inside a region, the
    /// authored water surface, the authored bank top, or the land (no sim
    /// water there); elsewhere unchanged.
    pub fn surface_alt_at(&self, wpos: Vec2<i32>) -> f32 {
        match self.authored_cell_at(wpos) {
            Some(AuthoredCell::Wet { surface_block, .. }) => (surface_block + 1) as f32,
            Some(AuthoredCell::Bank { bed_block }) => (bed_block + 1) as f32,
            Some(AuthoredCell::None) => self
                .get_alt_approx(wpos)
                .unwrap_or(crate::CONFIG.sea_level)
                .max(crate::CONFIG.sea_level),
            None => self.get_surface_alt_approx(wpos),
        }
    }

    /// Whether a chunk is water at chunk granularity: inside a region, at
    /// least half its columns are authored water; elsewhere the table's
    /// `river_kind.is_some()`. `None` outside the map.
    pub fn chunk_is_wet(&self, chunk_pos: Vec2<i32>) -> Option<bool> {
        let chunk = self.get(chunk_pos)?;
        Some(match self.authored_chunk_wet(chunk_pos) {
            Some(wet) => wet,
            None => chunk.river.river_kind.is_some(),
        })
    }

    /// `SimChunk::is_underwater` made authored-aware (see
    /// [`Self::chunk_is_wet`]). `None` outside the map.
    pub fn chunk_is_underwater(&self, chunk_pos: Vec2<i32>) -> Option<bool> {
        let chunk = self.get(chunk_pos)?;
        Some(match self.authored_chunk_wet(chunk_pos) {
            Some(wet) => wet,
            None => chunk.is_underwater(),
        })
    }

    /// Inside a region: whether the chunk is mostly authored water. `None`
    /// outside every region.
    pub fn authored_chunk_wet(&self, chunk_pos: Vec2<i32>) -> Option<bool> {
        let rasters = self.authored_rasters.as_ref()?;
        rasters.contains_chunk(chunk_pos).then(|| {
            rasters
                .chunk_summary(chunk_pos)
                .is_some_and(|s| s.wet_majority())
        })
    }

    /// `SimChunk::water_alt` made authored-aware: inside a region, the
    /// highest authored surface of a mostly wet chunk, or the sea level (no
    /// water above it there); elsewhere the table's value. `None` outside
    /// the map.
    pub fn chunk_water_alt(&self, chunk_pos: Vec2<i32>) -> Option<f32> {
        let chunk = self.get(chunk_pos)?;
        Some(match self.authored_chunk_wet(chunk_pos) {
            Some(true) => self
                .authored_rasters
                .as_ref()
                .and_then(|r| r.chunk_summary(chunk_pos))
                .map_or(crate::CONFIG.sea_level, |s| {
                    (s.max_surface_block + 1) as f32
                }),
            Some(false) => crate::CONFIG.sea_level,
            None => chunk.water_alt,
        })
    }

    /// Water depth of a chunk, `water_alt - alt` made authored-aware: inside
    /// a region, the deepest authored water of a mostly wet chunk, or a dry
    /// value (-8, the deepest "dry" the road cost cares about); elsewhere the
    /// table's value. `None` outside the map.
    pub fn chunk_water_depth(&self, chunk_pos: Vec2<i32>) -> Option<f32> {
        let chunk = self.get(chunk_pos)?;
        Some(match self.authored_chunk_wet(chunk_pos) {
            Some(true) => self
                .authored_rasters
                .as_ref()
                .and_then(|r| r.chunk_summary(chunk_pos))
                .map_or(0.0, |s| (s.max_surface_block - s.min_bed_block) as f32),
            Some(false) => -8.0,
            None => chunk.water_alt - chunk.alt,
        })
    }

    /// The chunk as the sim table would describe it if its water came from
    /// the raster: inside a region, a mostly wet chunk reads underwater (its
    /// `water_alt` lifted to the authored surface) and any other reads dry
    /// (no river kind, `water_alt` not above `alt`). For predicates written
    /// against `SimChunk` (spawn filters). Outside every region: the chunk
    /// itself, borrowed.
    pub fn chunk_with_authored_water(
        &self,
        chunk_pos: Vec2<i32>,
    ) -> Option<std::borrow::Cow<'_, crate::sim::SimChunk>> {
        let chunk = self.get(chunk_pos)?;
        Some(match self.authored_chunk_wet(chunk_pos) {
            None => std::borrow::Cow::Borrowed(chunk),
            Some(wet) => {
                let mut c = chunk.clone();
                if wet {
                    let surface = self.chunk_water_alt(chunk_pos).unwrap_or(c.water_alt);
                    c.water_alt = surface.max(c.alt + 1.0);
                } else {
                    c.river.river_kind = None;
                    c.water_alt = c.water_alt.min(c.alt);
                }
                std::borrow::Cow::Owned(c)
            },
        })
    }

    /// Whether authored water of a chunk sits at the ocean's level.
    pub fn authored_chunk_is_sea(&self, chunk_pos: Vec2<i32>) -> bool {
        self.authored_rasters
            .as_ref()
            .and_then(|r| r.chunk_summary(chunk_pos))
            .is_some_and(|s| s.wet_majority() && s.min_surface_block <= SEA_TOP_BLOCK)
    }
}

impl World {
    /// Water at a column, authored-aware ([`WorldSim::water_at`]).
    pub fn water_at(&self, wpos: Vec2<i32>) -> Option<WaterAt> { self.sim.water_at(wpos) }

    /// Whether the column at `wpos` is water, authored-aware; `None` outside
    /// the map.
    pub fn is_wet_at(&self, wpos: Vec2<i32>) -> Option<bool> { self.sim.is_wet_at(wpos) }
}

/// No authored settlement may stand on a mostly-wet authored chunk: the
/// settlement data and the authored water disagree, and the site would be
/// generated under water. Checked once, after civ generation.
pub(crate) fn check_authored_sites(
    sim: &WorldSim,
    civs: &crate::civ::Civs,
) -> Result<(), super::LoadError> {
    if sim.authored_rasters.is_none() {
        return Ok(());
    }
    let bad: Vec<String> = civs
        .sites
        .values()
        .filter(|site| site.is_authored_settlement())
        .filter(|site| sim.authored_chunk_wet(site.center) == Some(true))
        .map(|site| format!("{:?} at chunk {:?}", site.kind, site.center))
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(super::LoadError(format!(
            "authored rasters: {} authored settlement(s) stand on authored water: {}",
            bad.len(),
            bad.join(", ")
        )))
    }
}
