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

    /// The water facts of a chunk, authored-aware: inside a region from the
    /// raster's per-chunk summary (a chunk is water when at least half its
    /// columns are authored water; it keeps the table's kind when the table
    /// also calls it lake or river, else it is ocean when its surface is at
    /// the ocean's top block and lake otherwise; it is near water when it
    /// or any of its 8 neighbours holds authored water); outside every
    /// region exactly the table's values. `None` outside the map.
    pub fn chunk_water(&self, chunk_pos: Vec2<i32>) -> Option<ChunkWater> {
        let c = self.get(chunk_pos)?;
        let table = ChunkWater {
            river: c.river.is_river(),
            lake: c.river.is_lake(),
            ocean: c.river.is_ocean(),
            underwater: c.is_underwater(),
            near_water: c.river.near_water(),
            water_alt: c.water_alt,
            alt: c.alt,
        };
        let Some(rasters) = self
            .authored_rasters
            .as_ref()
            .filter(|r| r.contains_chunk(chunk_pos))
        else {
            return Some(table);
        };
        let summary = rasters.chunk_summary(chunk_pos);
        let wet = summary.is_some_and(|s| s.wet_majority());
        let near_water = (-1..=1).any(|dy| {
            (-1..=1).any(|dx| {
                rasters
                    .chunk_summary(chunk_pos + Vec2::new(dx, dy))
                    .is_some_and(|s| s.wet_columns > 0)
            })
        });
        let (river, lake, ocean) = match (wet, c.river.river_kind) {
            (false, _) => (false, false, false),
            (true, Some(crate::sim::RiverKind::River { .. })) => (true, false, false),
            (true, Some(crate::sim::RiverKind::Lake { .. })) => (false, true, false),
            (true, Some(crate::sim::RiverKind::Ocean)) => (false, false, true),
            (true, None) => {
                let sea = summary.is_some_and(|s| s.min_surface_block <= SEA_TOP_BLOCK);
                (false, !sea, sea)
            },
        };
        let water_alt = match summary.filter(|_| wet) {
            Some(s) => ((s.max_surface_block + 1) as f32).max(c.alt + 1.0),
            None => crate::CONFIG.sea_level.min(c.alt),
        };
        Some(ChunkWater {
            river,
            lake,
            ocean,
            underwater: wet,
            near_water,
            water_alt,
            alt: c.alt,
        })
    }

    /// Whether a chunk is water (river, lake or ocean), authored-aware
    /// ([`Self::chunk_water`]). `None` outside the map.
    pub fn chunk_is_wet(&self, chunk_pos: Vec2<i32>) -> Option<bool> {
        self.chunk_water(chunk_pos).map(|w| w.wet())
    }

    /// `SimChunk::is_underwater` made authored-aware. `None` outside the map.
    pub fn chunk_is_underwater(&self, chunk_pos: Vec2<i32>) -> Option<bool> {
        self.chunk_water(chunk_pos).map(|w| w.underwater)
    }

    /// `river.near_water()` of the chunk holding `wpos`, authored-aware.
    pub fn near_water_at(&self, wpos: Vec2<i32>) -> Option<bool> {
        self.chunk_water(wpos.map(|e| e.div_euclid(super::CHUNK)))
            .map(|w| w.near_water)
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

    /// `SimChunk::water_alt` made authored-aware ([`Self::chunk_water`]).
    pub fn chunk_water_alt(&self, chunk_pos: Vec2<i32>) -> Option<f32> {
        self.chunk_water(chunk_pos).map(|w| w.water_alt)
    }

    /// Water depth of a chunk for the civ road cost, `water_alt - alt` made
    /// authored-aware: inside a region, the deepest authored water of a
    /// mostly wet chunk, or -8 for a dry one (the "dry" end of that cost);
    /// elsewhere the table's value. `None` outside the map.
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

    /// The chunk as the table would describe it if its water came from the
    /// raster ([`Self::chunk_water`]: river kind, `water_alt`, near-water
    /// neighbours), for predicates written against `SimChunk` that run once
    /// per world (spot placement). A clone only for chunks inside a region;
    /// elsewhere the chunk itself, borrowed.
    pub fn chunk_view(
        &self,
        chunk_pos: Vec2<i32>,
    ) -> Option<std::borrow::Cow<'_, crate::sim::SimChunk>> {
        let chunk = self.get(chunk_pos)?;
        if self.authored_chunk_wet(chunk_pos).is_none() {
            return Some(std::borrow::Cow::Borrowed(chunk));
        }
        let w = self.chunk_water(chunk_pos)?;
        let mut c = chunk.clone();
        c.river.river_kind = if w.ocean {
            Some(crate::sim::RiverKind::Ocean)
        } else if w.river {
            chunk.river.river_kind
        } else if w.lake {
            chunk.river.river_kind.or(Some(crate::sim::RiverKind::Lake {
                neighbor_pass_pos: chunk_pos * super::CHUNK,
            }))
        } else {
            None
        };
        c.river.neighbor_rivers.clear();
        if w.near_water && !w.wet() {
            // A neighbour holds authored water: `near_river()` must say so.
            c.river.neighbor_rivers.push(0);
        }
        c.water_alt = w.water_alt;
        Some(std::borrow::Cow::Owned(c))
    }
}

/// The water facts of one chunk ([`WorldSim::chunk_water`]): the chunk
/// table's predicates, answered from the raster inside authored regions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkWater {
    pub river: bool,
    pub lake: bool,
    pub ocean: bool,
    /// `SimChunk::is_underwater`.
    pub underwater: bool,
    /// `RiverData::near_water`.
    pub near_water: bool,
    pub water_alt: f32,
    pub alt: f32,
}

impl ChunkWater {
    /// River, lake or ocean.
    pub fn wet(&self) -> bool { self.river || self.lake || self.ocean }

    /// `water_alt - alt`.
    pub fn depth(&self) -> f32 { self.water_alt - self.alt }
}

/// `col.chunk.river.is_ocean()` for a column sample, authored-aware: an
/// authored column is ocean when it is authored water at the ocean's top
/// block in a chunk the table does not call lake or river (a sea-level
/// mountain lake or a river mouth stays fresh water).
pub fn column_is_ocean(col: &crate::ColumnSample) -> bool {
    match col.authored {
        Some(a) => {
            matches!(a.cell, AuthoredCell::Wet { surface_block, .. } if surface_block <= SEA_TOP_BLOCK)
                && !col.chunk.river.is_lake()
                && !col.chunk.river.is_river()
        },
        None => col.chunk.river.is_ocean(),
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
