//! Authored-aware water and ground queries for the consumers that read the
//! sim chunk table instead of generated blocks: rtsim (boats, fish, spawns,
//! simulated NPCs), sites and plots (through `Land`), ports, wildlife, admin
//! commands.
//!
//! The one routing point for the ground is [`WorldSim::ground_alt_at`]:
//! `Land::ground_alt_at` and `Land::ground_gradient_at` delegate to it, and
//! [`WorldSim::surface_alt_at`] adds the water on top. The upstream accessors
//! (`WorldSim::get_alt_approx`, `Land::get_alt_approx`, the gradient and
//! surface ones) stay the chunk table (the natural map the sim was built
//! from): consumers whose result reaches beyond the sampled point (the cave
//! graph, caverns, authored voids) and civ placement read them (see the
//! `Land` doc for the per-directory rule and its test).
//!
//! Inside an authored region these answer from the raster (the water the
//! player sees); everywhere else they return exactly what the chunk table
//! said before authored rasters existed, so every caller outside a region --
//! and every world without a manifest -- behaves bit-identically. Unauthored
//! columns of a partial chunk are the natural map, so there too the table
//! answers.

use super::{
    AuthoredCell, AuthoredColumn, AuthoredWater, SEA_TOP_BLOCK, SeaFill, format::GROUND_EXACT,
    top_block_alt,
};
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

    /// The authored-aware ground altitude at a column: the single
    /// implementation every consumer that must stand on the real ground reads
    /// (`Land::ground_alt_at`, so site and plot generation, rtsim spawns, LOD
    /// objects).
    ///
    /// * exact ground, bank, wet bed: the authored top block's altitude
    ///   ([`top_block_alt`], the column sampler's convention), with no
    ///   sea-level clamp (a dry pit below 0 m is its floor); on an exact
    ///   ground column a listed site's plot levelling moves, the levelled
    ///   altitude the column renders (`AuthoredRasters::site_levelled_alt`);
    /// * blend (ring) ground of weight `w`: `lerp(table, block, w / 255)`. The
    ///   rendered ring lerps toward the engine column *with* its noise instead,
    ///   so on ring cells this differs from the blocks by up to the engine's
    ///   relief times `1 - w` (rings belong outside sites; the post-civ site
    ///   check refuses one under a listed site);
    /// * everything else, and every column outside every region: the chunk
    ///   table ([`Self::get_alt_approx`]), bit for bit.
    ///
    /// `None` outside the map.
    pub fn ground_alt_at(&self, wpos: Vec2<i32>) -> Option<f32> {
        let rasters = self.authored_rasters.as_ref();
        match rasters.and_then(|r| r.cell_at(wpos)) {
            Some(AuthoredCell::Wet { bed_block, .. } | AuthoredCell::Bank { bed_block }) => {
                Some(top_block_alt(bed_block))
            },
            Some(AuthoredCell::Ground {
                block,
                weight: GROUND_EXACT,
            }) => Some(
                rasters
                    .and_then(|r| r.site_levelled_alt(wpos))
                    .unwrap_or_else(|| top_block_alt(block)),
            ),
            Some(AuthoredCell::Ground { block, weight }) => Some(Lerp::lerp(
                self.get_alt_approx(wpos)?,
                top_block_alt(block),
                weight as f32 / GROUND_EXACT as f32,
            )),
            Some(AuthoredCell::None) | None => self.get_alt_approx(wpos),
        }
    }

    /// `get_gradient_approx` of the chunk holding `wpos`, authored-aware: for
    /// a chunk whose centre column is a ground cell, the steepest of the
    /// slopes from its centre to the four neighbouring chunk centres over
    /// [`Self::ground_alt_at`] (the table's downhill neighbour would miss a
    /// ramp or a prepared flat); otherwise the table's value. `None` outside
    /// the map.
    pub fn ground_gradient_at(&self, wpos: Vec2<i32>) -> Option<f32> {
        let chunk_pos = wpos.map(|e| e.div_euclid(super::CHUNK));
        let centre = chunk_pos * super::CHUNK + super::CHUNK / 2;
        if !matches!(
            self.authored_rasters
                .as_ref()
                .and_then(|r| r.cell_at(centre)),
            Some(AuthoredCell::Ground { .. })
        ) {
            return self.get_gradient_approx(chunk_pos);
        }
        let here = self.ground_alt_at(centre)?;
        Some(
            [
                Vec2::new(1, 0),
                Vec2::new(-1, 0),
                Vec2::new(0, 1),
                Vec2::new(0, -1),
            ]
            .into_iter()
            .filter_map(|d| self.ground_alt_at(centre + d * super::CHUNK))
            .map(|n| (here - n).abs() / super::CHUNK as f32)
            .fold(0.0, f32::max),
        )
    }

    /// `get_surface_alt_approx` made authored-aware: inside a region, the
    /// authored water surface, the authored bank or ground top (a sea floor
    /// of a [`SeaFill::Auto`] region: the sea's surface over it), or the land
    /// (no sim water there); elsewhere unchanged. Simulated rtsim NPCs are
    /// snapped to it every tick.
    pub fn surface_alt_at(&self, wpos: Vec2<i32>) -> f32 {
        let sea = crate::CONFIG.sea_level;
        let table_land = || self.get_alt_approx(wpos).unwrap_or(sea).max(sea);
        match self
            .authored_rasters
            .as_ref()
            .and_then(|r| r.cell_detail_at(wpos))
            .filter(|&(cell, _, partial)| !(partial && cell == AuthoredCell::None))
        {
            Some((AuthoredCell::Wet { surface_block, .. }, ..)) => (surface_block + 1) as f32,
            Some((AuthoredCell::Bank { bed_block }, ..)) => (bed_block + 1) as f32,
            Some((AuthoredCell::Ground { block, weight }, sea_fill, _)) => {
                let top = self.exact_ground_top(wpos, block) as f32;
                let land = if weight == GROUND_EXACT {
                    top
                } else {
                    Lerp::lerp(table_land(), top, weight as f32 / GROUND_EXACT as f32)
                };
                match sea_fill {
                    // Below sea level the sea fills it: its surface.
                    SeaFill::Auto => land.max(sea),
                    // Dry at any altitude.
                    SeaFill::AuthoredOnly => land,
                }
            },
            Some((AuthoredCell::None, ..)) => table_land(),
            None => self.get_surface_alt_approx(wpos),
        }
    }

    /// Login spawn-fix for a position saved before a ground patch raised the
    /// ground over it: when `feet` lies inside solid terrain (`solid`) in a
    /// ground-cell column, the z to search from instead -- one block above the
    /// authored ground ([`Self::ground_alt_at`] on a ring cell) -- so the
    /// normal ground search starts on the new surface rather than in a cave
    /// below it or more than its reach under it. `None` (search from `feet`
    /// as before) everywhere else: outside every region, on water, bank and
    /// unauthored columns, when the feet are in free space (a cave or an
    /// interior under the patch), or when the surface is not above the feet.
    pub fn buried_ground_lift(&self, feet: Vec3<i32>, solid: bool) -> Option<i32> {
        if !solid {
            return None;
        }
        let z = match self.authored_rasters.as_ref()?.cell_at(feet.xy())? {
            AuthoredCell::Ground {
                block,
                weight: GROUND_EXACT,
            } => self.exact_ground_top(feet.xy(), block),
            AuthoredCell::Ground { .. } => self.ground_alt_at(feet.xy())?.floor() as i32 + 1,
            AuthoredCell::Wet { .. } | AuthoredCell::Bank { .. } | AuthoredCell::None => {
                return None;
            },
        };
        (z > feet.z).then_some(z)
    }

    /// The z just above the top block of an exact ground column: the
    /// authored block, or the levelled one where a listed site's plot
    /// levelling moves it.
    fn exact_ground_top(&self, wpos: Vec2<i32>, block: i32) -> i32 {
        self.authored_rasters
            .as_ref()
            .and_then(|r| r.site_levelled_alt(wpos))
            .map_or(block, |alt| alt.floor() as i32)
            + 1
    }

    /// Record the exact ground columns listed sites' plot levelling moves
    /// (see `AuthoredRasters::set_site_levelling`); a no-op without authored
    /// rasters.
    pub(crate) fn set_site_levelling(
        &mut self,
        columns: impl IntoIterator<Item = (Vec2<i32>, f32)>,
    ) {
        if let Some(r) = self.authored_rasters.as_mut() {
            r.set_site_levelling(columns);
        }
    }

    /// The water facts of a chunk, authored-aware: inside a region from the
    /// raster's per-chunk summary (a chunk is water when at least half its
    /// columns are water -- authored water and sea floors, plus, in a partial
    /// chunk the table calls water, its unauthored natural columns and the
    /// ring cells that fade into them; exact ground, banks and the ground of
    /// a [`SeaFill::AuthoredOnly`] region are dry at any altitude, so
    /// reclaimed land stops being water once it is mostly land; it keeps the
    /// table's kind when the table also calls it lake or river, else it is
    /// ocean when its surface is at the ocean's top block and lake
    /// otherwise -- always lake in a [`SeaFill::AuthoredOnly`] region, whose
    /// only water is authored; it is near water when it or any of its 8
    /// neighbours holds authored water, or, for a partial chunk, when the
    /// table says so);
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
            all - summary.map_or(0, |s| s.authored_columns - s.fading_blend_columns)
        } else {
            0
        };
        let authored_only = summary.is_some_and(|s| s.authored_only);
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
            // The region's own water, not the sea's.
            (true, Some(crate::sim::RiverKind::Ocean)) if authored_only && natural_wet == 0 => {
                (false, true, false)
            },
            (true, Some(crate::sim::RiverKind::Ocean)) => (false, false, true),
            (true, None) => {
                let sea = !authored_only
                    && summary.is_some_and(|s| {
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

/// One water wall found by [`ground_seams`]: natural (or sea) water beside a
/// ground cell stands higher than everything the ground cell holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeamWall {
    pub ground: Vec2<i32>,
    /// Top solid block of the ground cell (its own water top, when the sea
    /// fills it, if higher).
    pub ground_top: i32,
    pub neighbour: Vec2<i32>,
    /// Top water block of the neighbour.
    pub water_top: i32,
}

/// What [`ground_seams`] looked at and found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeamReport {
    /// `(ground cell, neighbour)` pairs compared.
    pub pairs: usize,
    /// Distinct columns sampled.
    pub columns_sampled: usize,
    /// Every water wall, sorted by ground cell then neighbour.
    pub walls: Vec<SeamWall>,
}

/// Top solid block and top water block (when the column holds water above
/// its ground) of a column sample, with `block.rs`'s rules: `z` is solid when
/// `z as i32 <= alt as i32`, and water when not solid and `z < water_level`.
fn column_tops(col: &crate::ColumnSample) -> (i32, Option<i32>) {
    let solid = col.alt as i32;
    let water = col.water_level.ceil() as i32 - 1;
    (solid, (water > solid).then_some(water))
}

/// The post-civ seam check of the ground layer: every ground cell (exact or
/// blend) that meets the natural map -- an unauthored column of a partial
/// chunk, or, around [`SeaFill::AuthoredOnly`] ground below the ocean's top
/// block, any unauthored column -- is compared with that neighbour through
/// the column sampler (sites are placed, so levelling and `Damage` overrides
/// are in): a neighbour whose water stands above everything the ground cell
/// holds is a water wall. Only the seam pairs are sampled, deduplicated and
/// in parallel; the order of the result does not depend on the thread count.
pub fn ground_seams(
    world: &World,
    index: crate::IndexRef,
    calendar: Option<&common::calendar::Calendar>,
) -> SeamReport {
    use crate::util::Sampler;
    use rayon::prelude::*;
    let Some(rasters) = world.sim.authored_rasters.as_ref() else {
        return SeamReport::default();
    };
    let pairs = rasters.ground_seam_pairs();
    if pairs.is_empty() {
        return SeamReport::default();
    }
    let mut cols: Vec<Vec2<i32>> = pairs.iter().flat_map(|&(g, n)| [g, n]).collect();
    cols.sort_unstable_by_key(|p| (p.y, p.x));
    cols.dedup();
    let sampler = world.sample_columns();
    let tops: Vec<Option<(i32, Option<i32>)>> = cols
        .par_iter()
        .map(|&p| sampler.get((p, index, calendar)).map(|c| column_tops(&c)))
        .collect();
    let top_at = |p: Vec2<i32>| {
        cols.binary_search_by_key(&(p.y, p.x), |q| (q.y, q.x))
            .ok()
            .and_then(|i| tops[i])
    };
    let mut walls: Vec<SeamWall> = pairs
        .iter()
        .filter_map(|&(g, n)| {
            let (g_solid, g_water) = top_at(g)?;
            let (_, n_water) = top_at(n)?;
            let ground_top = g_water.map_or(g_solid, |w| w.max(g_solid));
            let water_top = n_water?;
            (water_top > ground_top).then_some(SeamWall {
                ground: g,
                ground_top,
                neighbour: n,
                water_top,
            })
        })
        .collect();
    walls.sort_unstable_by_key(|w| (w.ground.y, w.ground.x, w.neighbour.y, w.neighbour.x));
    SeamReport {
        pairs: pairs.len(),
        columns_sampled: cols.len(),
        walls,
    }
}

/// [`ground_seams`] as a start-up rule: any water wall stops world
/// generation, with the first walls and the fixes in the message.
pub(crate) fn check_ground_seams(
    world: &World,
    index: crate::IndexRef,
    calendar: Option<&common::calendar::Calendar>,
) -> Result<SeamReport, super::LoadError> {
    let report = ground_seams(world, index, calendar);
    if report.walls.is_empty() {
        if report.pairs > 0 {
            tracing::info!(
                pairs = report.pairs,
                columns = report.columns_sampled,
                "Authored ground meets the natural map without a water wall"
            );
        }
        return Ok(report);
    }
    let first: Vec<String> = report
        .walls
        .iter()
        .take(10)
        .map(|w| {
            format!(
                "{:?} (ground top block {}) beside {:?} (water top block {})",
                w.ground, w.ground_top, w.neighbour, w.water_top
            )
        })
        .collect();
    Err(super::LoadError(format!(
        "authored rasters: {} ground cell edge(s) meet natural water that stands higher than the \
         ground (the water would stand as a wall), first: {}. Raise the ground there to the \
         water's top block at least (a dyke), author the water in the region, or keep the patch \
         away from the natural water",
        report.walls.len(),
        first.join("; ")
    )))
}
