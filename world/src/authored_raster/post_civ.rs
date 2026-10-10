//! The post-civ checks of the authored ground layer: what must hold once the
//! sites are placed and the authored-void index is built, before the world
//! is handed to the server.
//!
//! * **Sites** ([`site_patch_report`]): a site whose bounds come within
//!   [`SITE_PATCH_MARGIN_M`] of a ground region must be listed in that region's
//!   `sites_on_patch` (its layout follows the patch, so listing it is the
//!   reviewed act that re-blesses its layout digest); a listed site must not
//!   stand on ring cells (the authored-aware ground query only approximates
//!   them); and every exact ground column a listed site's plot levelling moves
//!   is counted (reported, not refused: houses level their lots) and handed
//!   to the ground queries ([`SitePatchReport::levelled_columns`], applied by
//!   `WorldSim::set_site_levelling`), so
//!   `WorldSim::ground_alt_at` there answers the levelled height the column
//!   renders, not the patch under it.
//! * **Voids** ([`void_exposure`]): an authored void (cave, interior) whose
//!   authored top comes within [`VOID_EXPOSURE_TOLERANCE_BLOCKS`] of the ground
//!   over it is exposed by the patch; more such columns than the region allows
//!   (`max_exposed_void_columns`, default 0) stop world generation. The
//!   authored-void data has no way to declare a mouth (a column where a void
//!   is meant to meet the surface or the water) yet, so every column counts;
//!   a region where an opening is intended raises its allowance.
//! * **Roads** ([`road_cliffs`]): civ roads are planned on the natural chunk
//!   table, so one can cross an authored cliff. Every road column on an exact
//!   ground cell that steps more than [`ROAD_CLIMB_LIMIT_BLOCKS`] to an
//!   adjacent exact cell is reported (a verifier gates it; the fix is an edit,
//!   not an engine change).
//!
//! Each check enumerates its columns once, samples them in parallel and
//! sorts the result, so its output does not depend on the thread count.
//! Without a manifest, or without a ground layer, nothing runs.
//!
//! What the engine does **not** check: whether NPCs can reach and walk the
//! patch. The road report above only logs; reachability (a flood fill over
//! the rendered blocks) and climbability belong to the offline terrain
//! verifier, which gates a patch before it ships. Loaded NPCs path over the
//! real blocks, so they climb what the patch builds; rtsim's long-range
//! movement and civ's routes use the chunk table and site data, not the
//! authored ground, so an authored cliff across a civ road is a content
//! fix (a ramp in the patch, or an authored route), not an engine one.

use super::{
    AuthoredCell, LoadError, ROAD_CLIMB_LIMIT_BLOCKS, RegionEntry, SITE_PATCH_MARGIN_M,
    VOID_EXPOSURE_TOLERANCE_BLOCKS, format::GROUND_EXACT, top_block_alt,
};
use crate::{IndexRef, Land, World, layer::authored_voids::AuthoredVoids, site::SpawnRules};
use rayon::prelude::*;
use std::collections::BTreeMap;
use tracing::{info, warn};
use vek::*;

const CHUNK: i32 = super::CHUNK;

/// A site near a ground region, as the site check found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteOnPatch {
    /// Stable site key (the site-layout digests' key).
    pub site: String,
    pub region: String,
    /// The site's bounds (wpos).
    pub bounds: Aabr<i32>,
    /// Cells counted (ring cells under the site, or exact cells moved by
    /// levelling); 0 where nothing is counted.
    pub cells: usize,
    /// The first counted cell, by `(y, x)`.
    pub example: Option<Vec2<i32>>,
}

/// What [`site_patch_report`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SitePatchReport {
    /// Listed sites near their region's ground.
    pub listed: Vec<SiteOnPatch>,
    /// Sites near a ground region that the region does not list: an error.
    pub unlisted: Vec<SiteOnPatch>,
    /// Listed sites whose footprint holds ring (blend) cells: an error.
    pub ring_under_site: Vec<SiteOnPatch>,
    /// Listed sites whose plot levelling moves exact ground columns
    /// (reported).
    pub levelled_exact: Vec<SiteOnPatch>,
    /// Every exact ground column the listed sites' levelling moves, with the
    /// levelled altitude the column sampler renders there, by site then
    /// `(y, x)`.
    pub levelled_columns: Vec<(Vec2<i32>, f32)>,
    /// `(region, key)` listings that name no site near the region (stale
    /// pins): reported.
    pub stale_listings: Vec<(String, String)>,
}

fn grown(b: Aabr<i32>, by: i32) -> Aabr<i32> {
    Aabr {
        min: b.min - by,
        max: b.max + by,
    }
}

/// Half-open boxes (`max` exclusive) overlap.
fn overlaps(a: Aabr<i32>, b: Aabr<i32>) -> bool {
    a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
}

/// The columns of `a ∩ b` (half-open), row by row.
fn columns(a: Aabr<i32>, b: Aabr<i32>) -> Vec<Vec2<i32>> {
    let min = Vec2::partial_max(a.min, b.min);
    let max = Vec2::partial_min(a.max, b.max);
    (min.y..max.y)
        .flat_map(|y| (min.x..max.x).map(move |x| Vec2::new(x, y)))
        .collect()
}

/// The ground cells under one site's footprint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteGroundCells {
    /// Blend (ring) cells, by `(y, x)`.
    pub ring: Vec<Vec2<i32>>,
    /// Exact cells whose top block the sites' plot levelling moves (only
    /// when the region levels sites), with the levelled altitude, by `(y, x)`.
    pub levelled_exact: Vec<(Vec2<i32>, f32)>,
}

/// The ground cells of region `r` under `bounds` (a site's footprint): its
/// ring cells, and the exact cells whose top block the plot levelling of the
/// sites in their chunk moves -- the column sampler's own rule
/// (`alt + (pref - alt) * factor`, `SpawnRules` of every site of the chunk).
pub fn site_ground_cells(
    world: &World,
    index: IndexRef,
    bounds: Aabr<i32>,
    r: &RegionEntry,
) -> SiteGroundCells {
    let Some(rasters) = world.sim.authored_rasters.as_ref() else {
        return SiteGroundCells::default();
    };
    let land = Land::from_sim(&world.sim);
    // `(column, levelled altitude)`; `None` for a ring cell.
    let counted: Vec<(Vec2<i32>, Option<f32>)> = columns(bounds, r.bounds)
        .par_iter()
        .filter_map(|&wpos| {
            let AuthoredCell::Ground { block, weight } = rasters.cell_at(wpos)? else {
                return None;
            };
            if weight != GROUND_EXACT {
                return Some((wpos, None));
            }
            if !r.site_levelling {
                return None;
            }
            let chunk = world.sim.get_wpos(wpos)?;
            let mut rules = SpawnRules::default();
            for s in &chunk.sites {
                index.sites[*s].spawn_rules(&mut rules, &land, wpos);
            }
            let (pref, factor) = rules.get_preferred_alt();
            let alt = top_block_alt(block);
            let levelled = alt + (pref - alt) * factor.clamped(0.0, 1.0);
            (factor > 0.0 && levelled as i32 != alt as i32).then_some((wpos, Some(levelled)))
        })
        .collect();
    let mut cells = SiteGroundCells::default();
    for (p, levelled) in counted {
        match levelled {
            None => cells.ring.push(p),
            Some(alt) => cells.levelled_exact.push((p, alt)),
        }
    }
    cells
}

/// The site check (see the module doc). Ordered by site key, then region.
pub fn site_patch_report(world: &World, index: IndexRef) -> SitePatchReport {
    let mut report = SitePatchReport::default();
    let Some(rasters) = world.sim.authored_rasters.as_ref() else {
        return report;
    };
    let regions: Vec<RegionEntry> = rasters.region_entries().filter(|r| r.has_ground).collect();
    if regions.is_empty() {
        return report;
    }
    let mut sites: Vec<(String, Aabr<i32>)> = world
        .civs
        .sites
        .values()
        .filter_map(|s| {
            let id = s.site_tmp?;
            Some((
                crate::civ::seeds::site_seed_key(s),
                index.sites[id].bounds(),
            ))
        })
        .collect();
    sites.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for r in &regions {
        for (key, bounds) in &sites {
            if !overlaps(grown(*bounds, SITE_PATCH_MARGIN_M), r.bounds) {
                continue;
            }
            let near = SiteOnPatch {
                site: key.clone(),
                region: r.id.to_string(),
                bounds: *bounds,
                cells: 0,
                example: None,
            };
            if !r.sites_on_patch.contains(key) {
                report.unlisted.push(near);
                continue;
            }
            let cells = site_ground_cells(world, index, *bounds, r);
            let tally = |hits: &[Vec2<i32>]| SiteOnPatch {
                cells: hits.len(),
                example: hits.first().copied(),
                ..near.clone()
            };
            if !cells.ring.is_empty() {
                report.ring_under_site.push(tally(&cells.ring));
            }
            if !cells.levelled_exact.is_empty() {
                let moved: Vec<Vec2<i32>> = cells.levelled_exact.iter().map(|c| c.0).collect();
                report.levelled_exact.push(tally(&moved));
                report.levelled_columns.extend(cells.levelled_exact);
            }
            report.listed.push(near);
        }
        for key in r.sites_on_patch {
            if !report
                .listed
                .iter()
                .any(|l| &l.site == key && l.region == r.id)
            {
                report.stale_listings.push((r.id.to_string(), key.clone()));
            }
        }
    }
    report
}

/// One region's result of [`void_exposure`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionVoidExposure {
    pub region: String,
    pub bounds: Aabr<i32>,
    /// `max_exposed_void_columns` of the region.
    pub allowed: u32,
    /// Exposed void columns.
    pub exposed: usize,
    /// Per authored feature: exposed columns and the first one, by `(y, x)`.
    pub features: Vec<(String, usize, Vec2<i32>)>,
}

impl RegionVoidExposure {
    pub fn is_error(&self) -> bool { self.exposed > self.allowed as usize }
}

/// The void check (see the module doc), per ground region.
pub(crate) fn void_exposure(
    world: &World,
    voids: Option<&AuthoredVoids>,
) -> Vec<RegionVoidExposure> {
    let (Some(rasters), Some(voids)) = (world.sim.authored_rasters.as_ref(), voids) else {
        return Vec::new();
    };
    rasters
        .region_entries()
        .filter(|r| r.has_ground)
        .map(|r| {
            let chunks: Vec<Vec2<i32>> = rasters
                .ground_chunks(r.id)
                .into_iter()
                .filter(|c| !voids.in_chunk(*c * CHUNK).is_empty())
                .collect();
            // `(column, feature index)`: no name is cloned per hit.
            let mut hits: Vec<(Vec2<i32>, u16)> = chunks
                .par_iter()
                .flat_map_iter(|&c| {
                    let origin = c * CHUNK;
                    let bucket = voids.in_chunk(origin);
                    let mut out = Vec::new();
                    for y in 0..CHUNK {
                        for x in 0..CHUNK {
                            let wpos = origin + Vec2::new(x, y);
                            let ground_top = match rasters.cell_at(wpos) {
                                Some(AuthoredCell::Ground {
                                    block,
                                    weight: GROUND_EXACT,
                                }) => block,
                                Some(AuthoredCell::Ground { .. }) => {
                                    match world.sim.ground_alt_at(wpos) {
                                        Some(a) => a as i32,
                                        None => continue,
                                    }
                                },
                                _ => continue,
                            };
                            bucket.for_each_authored_top(wpos, |top, feature| {
                                if top.ceil() as i32 + VOID_EXPOSURE_TOLERANCE_BLOCKS >= ground_top
                                {
                                    out.push((wpos, feature));
                                }
                            });
                        }
                    }
                    out
                })
                .collect();
            hits.sort_unstable_by_key(|&(p, f)| (p.y, p.x, f));
            hits.dedup();
            let mut columns: Vec<Vec2<i32>> = hits.iter().map(|h| h.0).collect();
            columns.dedup();
            // Per feature: count and first column (hits are sorted by column).
            let mut per_feature: BTreeMap<&str, (usize, Vec2<i32>)> = BTreeMap::new();
            for &(wpos, feature) in &hits {
                per_feature
                    .entry(voids.feature_name(feature))
                    .or_insert((0, wpos))
                    .0 += 1;
            }
            let features = per_feature
                .into_iter()
                .map(|(name, (n, first))| (name.to_string(), n, first))
                .collect();
            RegionVoidExposure {
                region: r.id.to_string(),
                bounds: r.bounds,
                allowed: r.max_exposed_void_columns,
                exposed: columns.len(),
                features,
            }
        })
        .collect()
}

/// What [`road_cliffs`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoadCliffReport {
    /// Road columns on exact ground cells.
    pub road_columns: usize,
    /// Those that step more than [`ROAD_CLIMB_LIMIT_BLOCKS`] to an adjacent
    /// exact ground cell: `(column, largest step)`, by `(y, x)`.
    pub steep: Vec<(Vec2<i32>, i32)>,
}

/// The road check (see the module doc).
pub fn road_cliffs(world: &World) -> RoadCliffReport {
    let Some(rasters) = world.sim.authored_rasters.as_ref() else {
        return RoadCliffReport::default();
    };
    let exact_block = |p: Vec2<i32>| match rasters.cell_at(p) {
        Some(AuthoredCell::Ground {
            block,
            weight: GROUND_EXACT,
        }) => Some(block),
        _ => None,
    };
    let chunks: Vec<Vec2<i32>> = rasters
        .region_entries()
        .filter(|r| r.has_ground)
        .flat_map(|r| rasters.ground_chunks(r.id))
        .collect();
    let found: Vec<(Vec2<i32>, i32)> = chunks
        .par_iter()
        .flat_map_iter(|&c| {
            let origin = c * CHUNK;
            let mut out = Vec::new();
            // No way passes within a chunk of this one: nothing to look at.
            let centre = origin + CHUNK / 2;
            if world
                .sim
                .get_nearest_path(centre)
                .is_none_or(|(dist, _, path, _)| dist > CHUNK as f32 + path.width)
            {
                return out;
            }
            for y in 0..CHUNK {
                for x in 0..CHUNK {
                    let wpos = origin + Vec2::new(x, y);
                    let Some(block) = exact_block(wpos) else {
                        continue;
                    };
                    if !world
                        .sim
                        .get_nearest_path(wpos)
                        .is_some_and(|(dist, _, path, _)| dist < path.width)
                    {
                        continue;
                    }
                    let step = [
                        Vec2::new(1, 0),
                        Vec2::new(-1, 0),
                        Vec2::new(0, 1),
                        Vec2::new(0, -1),
                    ]
                    .into_iter()
                    .filter_map(|d| exact_block(wpos + d))
                    .map(|n| (n - block).abs())
                    .max()
                    .unwrap_or(0);
                    out.push((wpos, step));
                }
            }
            out
        })
        .collect();
    let mut steep: Vec<(Vec2<i32>, i32)> = found
        .iter()
        .copied()
        .filter(|&(_, step)| step > ROAD_CLIMB_LIMIT_BLOCKS)
        .collect();
    steep.sort_unstable_by_key(|(p, _)| (p.y, p.x));
    RoadCliffReport {
        road_columns: found.len(),
        steep,
    }
}

/// Everything the post-civ ground checks found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroundConsumerReport {
    pub sites: SitePatchReport,
    pub voids: Vec<RegionVoidExposure>,
    pub roads: RoadCliffReport,
}

/// The three checks as start-up rules: an unlisted site near a ground
/// region, a listed site on ring cells, or more exposed void columns than a
/// region allows stop world generation; moved exact
/// cells, stale listings and roads over authored cliffs are logged.
pub(crate) fn check_ground_consumers(
    world: &World,
    index: IndexRef,
    voids: Option<&AuthoredVoids>,
) -> Result<GroundConsumerReport, LoadError> {
    let has_ground = world
        .sim
        .authored_rasters
        .as_ref()
        .is_some_and(|r| r.has_ground_layer());
    if !has_ground {
        return Ok(GroundConsumerReport::default());
    }
    let report = GroundConsumerReport {
        sites: site_patch_report(world, index),
        voids: void_exposure(world, voids),
        roads: road_cliffs(world),
    };
    for s in &report.sites.levelled_exact {
        info!(
            site = s.site,
            region = s.region,
            columns = s.cells,
            example = ?s.example,
            "Site plot levelling moves exact authored ground columns (site_levelling)"
        );
    }
    for (region, key) in &report.sites.stale_listings {
        warn!(
            region,
            site = key,
            "sites_on_patch lists a site that is not near the region's ground"
        );
    }
    if !report.roads.steep.is_empty() {
        warn!(
            road_columns = report.roads.road_columns,
            steep = report.roads.steep.len(),
            first = ?&report.roads.steep[..report.roads.steep.len().min(10)],
            "Civ roads cross authored ground steeper than {ROAD_CLIMB_LIMIT_BLOCKS} blocks between \
             adjacent columns (planned on the natural map): add a ramp to the patch or author the \
             route"
        );
    }
    let mut errors = Vec::new();
    let names = |v: &[SiteOnPatch], with_cells: bool| -> String {
        v.iter()
            .take(10)
            .map(|s| {
                if with_cells {
                    format!(
                        "{} (region '{}', {} cell(s), e.g. {:?})",
                        s.site, s.region, s.cells, s.example
                    )
                } else {
                    format!(
                        "{} (region '{}', bounds {:?}..{:?})",
                        s.site, s.region, s.bounds.min, s.bounds.max
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !report.sites.unlisted.is_empty() {
        errors.push(format!(
            "{} site(s) come within {SITE_PATCH_MARGIN_M} m of a ground region that does not list \
             them in sites_on_patch: {}. Their layout would follow every patch edit: list them \
             (and re-bless their layout digests) or move the region",
            report.sites.unlisted.len(),
            names(&report.sites.unlisted, false)
        ));
    }
    if !report.sites.ring_under_site.is_empty() {
        errors.push(format!(
            "{} listed site(s) stand on blend (ring) cells, where the ground query only \
             approximates the rendered terrain: {}. Grow the exact footprint over the site",
            report.sites.ring_under_site.len(),
            names(&report.sites.ring_under_site, true)
        ));
    }
    for v in report.voids.iter().filter(|v| v.is_error()) {
        let features: Vec<String> = v
            .features
            .iter()
            .take(10)
            .map(|(f, n, p)| format!("{f} ({n} column(s), e.g. {p:?})"))
            .collect();
        errors.push(format!(
            "region '{}' (box {:?}..{:?}): the ground comes within \
             {VOID_EXPOSURE_TOLERANCE_BLOCKS} block(s) of authored voids on {} column(s) \
             (max_exposed_void_columns {}): {}. Raise the ground over them, move the void, or \
             raise the region's max_exposed_void_columns where an opening is intended",
            v.region,
            v.bounds.min,
            v.bounds.max,
            v.exposed,
            v.allowed,
            features.join(", ")
        ));
    }
    if errors.is_empty() {
        let listed = report.sites.listed.len();
        let exposed: usize = report.voids.iter().map(|v| v.exposed).sum();
        info!(
            listed_sites = listed,
            exposed_void_columns = exposed,
            road_columns = report.roads.road_columns,
            "Authored ground: sites, voids and roads checked"
        );
        Ok(report)
    } else {
        Err(LoadError(format!(
            "authored rasters: {}",
            errors.join("; ")
        )))
    }
}
