//! Authored-aware water queries for the consumers that read the sim chunk
//! table instead of generated blocks: rtsim (boats, fish, spawns), civ
//! (placement, road costs), ports, wildlife, admin commands.
//!
//! Inside an authored region these answer from the raster (the water the
//! player sees); everywhere else they return exactly what the chunk table
//! said before authored rasters existed, so every caller outside a region --
//! and every world without a manifest -- behaves bit-identically. Unauthored
//! columns of a partial chunk are the natural map, so there too the table
//! answers.

use super::{AuthoredCell, AuthoredColumn, AuthoredWater, SEA_TOP_BLOCK, SeaFill};
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
        if let Some((cell, sea_fill, _)) = self
            .authored_rasters
            .as_ref()
            .and_then(|r| r.cell_detail_at(wpos))
            .filter(|&(cell, _, partial)| !(partial && cell == AuthoredCell::None))
        {
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
                // A sea floor (`SeaFill::Auto` ground below sea level): the
                // sea fills it.
                AuthoredCell::Ground { block, .. }
                    if sea_fill == SeaFill::Auto && block < SEA_TOP_BLOCK =>
                {
                    WaterAt {
                        wet: true,
                        surface_alt: Some(crate::CONFIG.sea_level),
                        bed_alt: Some((block + 1) as f32),
                        source: WaterSource::Authored,
                    }
                },
                AuthoredCell::Bank { .. } | AuthoredCell::Ground { .. } | AuthoredCell::None => {
                    WaterAt {
                        wet: false,
                        surface_alt: None,
                        bed_alt: None,
                        source: WaterSource::Authored,
                    }
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
        match self
            .authored_cell_at(wpos)
            .filter(|c| !self.natural_unauthored(*c, wpos))
        {
            Some(AuthoredCell::Wet { surface_block, .. }) => (surface_block + 1) as f32,
            Some(AuthoredCell::Bank { bed_block }) => (bed_block + 1) as f32,
            // Not routed to the ground layer yet: consumers keep the table
            // (the natural map the sim was built from) until the
            // authored-aware ground query lands; ground cells are dry land.
            Some(AuthoredCell::Ground { .. } | AuthoredCell::None) => self
                .get_alt_approx(wpos)
                .unwrap_or(crate::CONFIG.sea_level)
                .max(crate::CONFIG.sea_level),
            None => self.get_surface_alt_approx(wpos),
        }
    }

    /// An unauthored column of a partial chunk: the natural map.
    fn natural_unauthored(&self, cell: AuthoredCell, wpos: Vec2<i32>) -> bool {
        cell == AuthoredCell::None
            && self
                .authored_rasters
                .as_ref()
                .is_some_and(|r| r.natural_at(wpos))
    }

    /// The water facts of a chunk, authored-aware: inside a region from the
    /// raster's per-chunk summary (a chunk is water when at least half its
    /// columns are water -- authored water, plus, in a partial chunk the
    /// table calls water, its unauthored natural columns; it keeps the
    /// table's kind when the table also calls it lake or river, else it is
    /// ocean when its surface is at the ocean's top block and lake
    /// otherwise; it is near water when it or any of its 8 neighbours holds
    /// authored water, or, for a partial chunk, when the table says so);
    /// outside every region, and for a partial chunk without authored cells,
    /// exactly the table's values. `None` outside the map.
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
        let natural = rasters.natural_at(chunk_pos * super::CHUNK);
        if natural && summary.is_none() {
            return Some(table);
        }
        let all = (super::CHUNK * super::CHUNK) as u32;
        // Unauthored columns that keep the natural water.
        let natural_wet = if natural && table.wet() {
            all - summary.map_or(0, |s| s.authored_columns)
        } else {
            0
        };
        let wet = (summary.map_or(0, |s| s.water_columns()) + natural_wet) * 2 >= all;
        let near_water = (natural && table.near_water)
            || (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    rasters
                        .chunk_summary(chunk_pos + Vec2::new(dx, dy))
                        .is_some_and(|s| s.water_columns() > 0)
                })
            });
        let (river, lake, ocean) = match (wet, c.river.river_kind) {
            (false, _) => (false, false, false),
            (true, Some(crate::sim::RiverKind::River { .. })) => (true, false, false),
            (true, Some(crate::sim::RiverKind::Lake { .. })) => (false, true, false),
            (true, Some(crate::sim::RiverKind::Ocean)) => (false, false, true),
            (true, None) => {
                let sea = summary.is_some_and(|s| {
                    s.min_surface_block <= SEA_TOP_BLOCK || s.sea_ground_columns > 0
                });
                (false, !sea, sea)
            },
        };
        let authored_alt = summary
            .filter(|s| s.wet_columns > 0)
            .map(|s| ((s.max_surface_block + 1) as f32).max(c.alt + 1.0));
        let water_alt = match (wet, natural_wet > 0, authored_alt) {
            (false, ..) => crate::CONFIG.sea_level.min(c.alt),
            (true, true, a) => a.map_or(c.water_alt, |a| a.max(c.water_alt)),
            (true, false, a) => a.unwrap_or(c.water_alt),
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
        let Some(rasters) = self
            .authored_rasters
            .as_ref()
            .filter(|r| r.contains_chunk(chunk_pos))
        else {
            return Some(chunk.water_alt - chunk.alt);
        };
        let summary = rasters.chunk_summary(chunk_pos);
        let natural = rasters.natural_at(chunk_pos * super::CHUNK);
        if natural && summary.is_none() {
            return Some(chunk.water_alt - chunk.alt);
        }
        let w = self.chunk_water(chunk_pos)?;
        Some(if !w.wet() {
            -8.0
        } else if natural && chunk.river.river_kind.is_some() {
            // The natural water is (part of) this chunk's water.
            w.water_alt - chunk.alt
        } else {
            summary.map_or(0.0, |s| {
                // A sea floor without authored water lies under the sea.
                let top = if s.wet_columns > 0 {
                    s.max_surface_block
                } else {
                    SEA_TOP_BLOCK
                };
                (top - s.min_ground_block()) as f32
            })
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
        let natural_untouched = self.authored_rasters.as_ref().is_some_and(|r| {
            r.natural_at(chunk_pos * super::CHUNK) && r.chunk_summary(chunk_pos).is_none()
        });
        if self.authored_chunk_wet(chunk_pos).is_none() || natural_untouched {
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
        let natural = self
            .authored_rasters
            .as_ref()
            .is_some_and(|r| r.natural_at(chunk_pos * super::CHUNK));
        if !natural {
            // Inside an owned box only authored water exists.
            c.river.neighbor_rivers.clear();
        }
        if w.near_water && !w.wet() && c.river.neighbor_rivers.is_empty() {
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
/// block, or exact ground below it (a sea floor, which the sea fills), in a
/// chunk the table does not call lake or river (a sea-level mountain lake or
/// a river mouth stays fresh water) and in a [`SeaFill::Auto`] region: in a
/// [`SeaFill::AuthoredOnly`] region authored water is a lake at any altitude
/// and ground is dry.
pub fn column_is_ocean(col: &crate::ColumnSample) -> bool {
    match col.authored {
        Some(a) => {
            let sea = match a.cell {
                AuthoredCell::Wet { surface_block, .. } => surface_block <= SEA_TOP_BLOCK,
                AuthoredCell::Ground { block, .. } => block < SEA_TOP_BLOCK,
                AuthoredCell::Bank { .. } | AuthoredCell::None => false,
            };
            sea && a.settings.sea_fill == SeaFill::Auto
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
