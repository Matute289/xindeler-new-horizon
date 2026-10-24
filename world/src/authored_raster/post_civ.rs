//! The post-civ checks of the authored ground layer: what must hold once the
//! sites are placed and the authored-void index is built, before the world
//! is handed to the server.
//!
//! * **Sites** ([`site_patch_report`]): a site whose bounds come within
//!   [`SITE_PATCH_MARGIN_M`] of a ground region must be listed in that region's
//!   `sites_on_patch` (its layout follows the patch, so listing it is the
//!   reviewed act that re-blesses its layout digest); a listed site must not
//!   stand on ring cells (the authored-aware ground query only approximates
//!   them), unless the region allows it with
//!   `ring_under_sites_max_change_blocks` and the ring moves the ground by no
//!   more than that; and every exact ground column a listed site's plot
//!   levelling moves is counted (reported, not refused: houses level their
//!   lots) and handed to the ground queries
//!   ([`SitePatchReport::levelled_columns`], applied by
//!   `WorldSim::set_site_levelling`), so `WorldSim::ground_alt_at` there
//!   answers the levelled height the column renders, not the patch under it.
//! * **Voids** ([`void_exposure`]): an authored void (cave, interior) whose
//!   authored top comes within [`VOID_EXPOSURE_TOLERANCE_BLOCKS`] of the ground
//!   over it is exposed by the patch; more such columns than the region allows
//!   (`max_exposed_void_columns`, default 0) stop world generation. The
//!   authored-void data has no way to declare a mouth (a column where a void is
//!   meant to meet the surface or the water) yet, so every column counts; a
//!   region where an opening is intended raises its allowance.
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
    /// For ring cells under a site of a region that allows them: the limit
    /// and the largest change among the counted cells.
    pub ring_change: Option<RingChange>,
}

/// How far the counted ring cells under a site move the ground, against the
/// region's allowance (`RegionManifest::ring_under_sites_max_change_blocks`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingChange {
    pub limit_blocks: u32,
    /// The largest change among the counted cells, rounded up.
    pub largest_blocks: u32,
}

/// What [`site_patch_report`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SitePatchReport {
    /// Listed sites near their region's ground.
    pub listed: Vec<SiteOnPatch>,
    /// Sites near a ground region that the region does not list: an error.
    pub unlisted: Vec<SiteOnPatch>,
    /// Listed sites whose footprint holds ring (blend) cells the region does
    /// not allow (all of them without an allowance, those that move the
    /// ground more than the allowance with one): an error.
    pub ring_under_site: Vec<SiteOnPatch>,
    /// Listed sites whose ring cells the region allows (within its
    /// `ring_under_sites_max_change_blocks`): reported.
    pub ring_allowed_under_site: Vec<SiteOnPatch>,
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
    /// Blend (ring) cells, by `(y, x)`, with how far the ring moves the
    /// ground there, in blocks ([`ring_change`]). Measured only when the
    /// region allows ring cells under sites; otherwise infinite (every ring
    /// cell counts, unmeasured).
    pub ring: Vec<(Vec2<i32>, f32)>,
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
    let measure_ring = r.ring_under_sites_max_change_blocks.is_some();
    // The sites' preferred altitude and blend factor at a column.
    let plot_rule = |wpos: Vec2<i32>| -> Option<(f32, f32)> {
        let chunk = world.sim.get_wpos(wpos)?;
        let mut rules = SpawnRules::default();
        for s in &chunk.sites {
            index.sites[*s].spawn_rules(&mut rules, &land, wpos);
        }
        Some(rules.get_preferred_alt())
    };
    let counted: Vec<(Vec2<i32>, Counted)> = columns(bounds, r.bounds)
        .par_iter()
        .filter_map(|&wpos| {
            let AuthoredCell::Ground { block, weight } = rasters.cell_at(wpos)? else {
                return None;
            };
            if weight != GROUND_EXACT {
                let change = if measure_ring {
                    let plot = if r.site_levelling {
                        plot_rule(wpos)
                    } else {
                        Some((0.0, 0.0))
                    };
                    match (plot, world.sim.get_alt_approx(wpos)) {
                        (Some(plot), Some(table)) => {
                            ring_change(table, top_block_alt(block), weight, plot)
                        },
                        // Outside the map: cannot be measured, so it counts.
                        _ => f32::INFINITY,
                    }
                } else {
                    f32::INFINITY
                };
                return Some((wpos, Counted::Ring(change)));
            }
            if !r.site_levelling {
                return None;
            }
            let (pref, factor) = plot_rule(wpos)?;
            let alt = top_block_alt(block);
            let levelled = alt + (pref - alt) * factor.clamped(0.0, 1.0);
            (factor > 0.0 && levelled as i32 != alt as i32)
                .then_some((wpos, Counted::Levelled(levelled)))
        })
        .collect();
    let mut cells = SiteGroundCells::default();
    for (p, c) in counted {
        match c {
            Counted::Ring(change) => cells.ring.push((p, change)),
            Counted::Levelled(alt) => cells.levelled_exact.push((p, alt)),
        }
    }
    cells
}

/// A ground column under a site that the check counts.
enum Counted {
    /// A ring cell, with [`ring_change`] of it.
    Ring(f32),
    /// An exact cell the plot levelling moves, with the levelled altitude.
    Levelled(f32),
}

/// How far, in blocks, a ring cell of blend weight `weight` moves the
/// rendered ground away from the natural site ground in its column.
///
/// `table` is the chunk table's altitude, `target` the authored block's
/// altitude, `plot` the sites' `(preferred altitude, factor)` there. The
/// column sampler levels the natural column (`natural`), blends it toward the
/// target by the ring weight, and levels the ring cell once more, so
/// `natural = L(table)` and `ring = L(natural + w * (target - natural))` with
/// `L(x) = x + (pref - x) * factor`. Under a fully levelled plot (factor 1)
/// both are the preferred altitude and the change is 0; a plot nowhere near
/// levelled (factor 0) sees the ring's own `w * |target - natural|`; in
/// between the change also holds the second levelling the ring cell gets
/// (`factor * |pref - natural|`, even at weight 0), which is why the measure
/// is the rendered change and not just the distance to the target.
pub fn ring_change(table: f32, target: f32, weight: u8, (pref, factor): (f32, f32)) -> f32 {
    let level = |a: f32| a + (pref - a) * factor.clamped(0.0, 1.0);
    let natural = level(table);
    let ring = level(natural + (target - natural) * (weight as f32 / GROUND_EXACT as f32));
    (ring - natural).abs()
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
            report_site(&mut report, r, key, *bounds, || {
                site_ground_cells(world, index, *bounds, r)
            });
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

/// Files one site that is near region `r` in `report`. `cells` gives the
/// ground cells under the site; it is not called for an unlisted site.
fn report_site(
    report: &mut SitePatchReport,
    r: &RegionEntry,
    key: &str,
    bounds: Aabr<i32>,
    cells: impl FnOnce() -> SiteGroundCells,
) {
    let near = SiteOnPatch {
        site: key.to_string(),
        region: r.id.to_string(),
        bounds,
        cells: 0,
        example: None,
        ring_change: None,
    };
    if !r.sites_on_patch.iter().any(|k| k == key) {
        report.unlisted.push(near);
        return;
    }
    let cells = cells();
    let tally = |hits: &[Vec2<i32>]| SiteOnPatch {
        cells: hits.len(),
        example: hits.first().copied(),
        ..near.clone()
    };
    let limit = r.ring_under_sites_max_change_blocks;
    let ring_tally = |hits: &[(Vec2<i32>, f32)]| SiteOnPatch {
        ring_change: limit.map(|limit_blocks| RingChange {
            limit_blocks,
            largest_blocks: hits.iter().map(|h| h.1).fold(0.0, f32::max).ceil() as u32,
        }),
        ..tally(&hits.iter().map(|h| h.0).collect::<Vec<_>>())
    };
    // A cell is allowed only when its change is known and within the limit.
    let (allowed, refused): (Vec<_>, Vec<_>) = cells
        .ring
        .iter()
        .copied()
        .partition(|c| limit.is_some_and(|l| c.1 <= l as f32));
    if !refused.is_empty() {
        report.ring_under_site.push(ring_tally(&refused));
    }
    if !allowed.is_empty() {
        report.ring_allowed_under_site.push(ring_tally(&allowed));
    }
    if !cells.levelled_exact.is_empty() {
        let moved: Vec<Vec2<i32>> = cells.levelled_exact.iter().map(|c| c.0).collect();
        report.levelled_exact.push(tally(&moved));
        report.levelled_columns.extend(cells.levelled_exact);
    }
    report.listed.push(near);
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

fn site_names(v: &[SiteOnPatch], with_cells: bool) -> String {
    v.iter()
        .take(10)
        .map(|s| {
            if let Some(c) = s.ring_change.filter(|_| with_cells) {
                format!(
                    "{} (region '{}', {} cell(s) move the ground by more than the {} block(s) the \
                     region allows, by up to {} block(s), e.g. {:?})",
                    s.site, s.region, s.cells, c.limit_blocks, c.largest_blocks, s.example
                )
            } else if with_cells {
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
}

/// The start-up errors of the site check: sites near a region that does not
/// list them, and listed sites on ring cells the region does not allow.
fn site_errors(sites: &SitePatchReport) -> Vec<String> {
    let mut errors = Vec::new();
    if !sites.unlisted.is_empty() {
        errors.push(format!(
            "{} site(s) come within {SITE_PATCH_MARGIN_M} m of a ground region that does not list \
             them in sites_on_patch: {}. Their layout would follow every patch edit: list them \
             (and re-bless their layout digests) or move the region",
            sites.unlisted.len(),
            site_names(&sites.unlisted, false)
        ));
    }
    if !sites.ring_under_site.is_empty() {
        errors.push(format!(
            "{} listed site(s) stand on blend (ring) cells, where the ground query only \
             approximates the rendered terrain: {}. Grow the exact footprint over the site",
            sites.ring_under_site.len(),
            site_names(&sites.ring_under_site, true)
        ));
    }
    errors
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
    for s in &report.sites.ring_allowed_under_site {
        info!(
            site = s.site,
            region = s.region,
            cells = s.cells,
            largest_change_blocks = ?s.ring_change.map(|c| c.largest_blocks),
            "Ring (blend) cells under a listed site, within the region's \
             ring_under_sites_max_change_blocks"
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
    errors.extend(site_errors(&report.sites));
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "settlement:kalthis";
    const BOUNDS: Aabr<i32> = Aabr {
        min: Vec2::new(100, 100),
        max: Vec2::new(900, 900),
    };

    fn region(listed: &[String], limit: Option<u32>) -> RegionEntry<'_> {
        RegionEntry {
            id: "pit",
            bounds: Aabr {
                min: Vec2::new(0, 0),
                max: Vec2::new(1024, 1024),
            },
            digest: "",
            has_ground: true,
            sites_on_patch: listed,
            max_exposed_void_columns: 0,
            site_levelling: true,
            ring_under_sites_max_change_blocks: limit,
        }
    }

    /// The report of one site standing on ring cells with the given changes.
    fn file(r: &RegionEntry, key: &str, changes: &[f32]) -> SitePatchReport {
        let ring = changes
            .iter()
            .enumerate()
            .map(|(i, c)| (Vec2::new(200 + i as i32, 300), *c))
            .collect();
        let mut report = SitePatchReport::default();
        report_site(&mut report, r, key, BOUNDS, || SiteGroundCells {
            ring,
            levelled_exact: Vec::new(),
        });
        report
    }

    #[test]
    fn without_an_allowance_any_ring_cell_under_a_listed_site_is_refused() {
        let listed = vec![KEY.to_string()];
        let r = region(&listed, None);
        // What the cell scan gives without an allowance: unmeasured.
        let report = file(&r, KEY, &[f32::INFINITY, f32::INFINITY]);
        assert_eq!(report.ring_under_site.len(), 1);
        assert!(report.ring_allowed_under_site.is_empty());
        let errors = site_errors(&report);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0],
            "1 listed site(s) stand on blend (ring) cells, where the ground query only \
             approximates the rendered terrain: settlement:kalthis (region 'pit', 2 cell(s), e.g. \
             Some(Vec2 { x: 200, y: 300 })). Grow the exact footprint over the site"
        );
        // A measured change does not matter while the allowance is off.
        let report = file(&r, KEY, &[0.0, 0.5]);
        assert_eq!(report.ring_under_site.len(), 1);
        assert_eq!(report.ring_under_site[0].ring_change, None);
    }

    #[test]
    fn ring_cells_within_the_allowance_are_accepted() {
        let listed = vec![KEY.to_string()];
        let r = region(&listed, Some(3));
        let report = file(&r, KEY, &[0.0, 2.5, 3.0]);
        assert!(report.ring_under_site.is_empty());
        assert!(site_errors(&report).is_empty());
        assert_eq!(report.listed.len(), 1);
        assert_eq!(report.ring_allowed_under_site.len(), 1);
        let allowed = &report.ring_allowed_under_site[0];
        assert_eq!(allowed.cells, 3);
        assert_eq!(
            allowed.ring_change,
            Some(RingChange {
                limit_blocks: 3,
                largest_blocks: 3,
            })
        );
    }

    #[test]
    fn one_ring_cell_over_the_allowance_is_refused_naming_site_and_count() {
        let listed = vec![KEY.to_string()];
        let r = region(&listed, Some(3));
        let report = file(&r, KEY, &[1.0, 2.0, 3.5, 3.0]);
        assert_eq!(report.ring_under_site.len(), 1);
        assert_eq!(report.ring_under_site[0].cells, 1);
        assert_eq!(report.ring_allowed_under_site[0].cells, 3);
        let errors = site_errors(&report);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(KEY), "{}", errors[0]);
        assert!(errors[0].contains("blend (ring)"), "{}", errors[0]);
        assert!(
            errors[0].contains(
                "1 cell(s) move the ground by more than the 3 block(s) the region allows, by up \
                 to 4 block(s)"
            ),
            "{}",
            errors[0]
        );
        // A cell that cannot be measured counts as over the limit.
        for bad in [f32::INFINITY, f32::NAN] {
            let report = file(&r, KEY, &[0.0, bad]);
            assert_eq!(report.ring_under_site[0].cells, 1);
        }
    }

    #[test]
    fn an_unlisted_site_stays_strict_whatever_the_allowance() {
        let listed = vec!["settlement:other".to_string()];
        let r = region(&listed, Some(1000));
        let mut report = SitePatchReport::default();
        report_site(&mut report, &r, KEY, BOUNDS, || {
            unreachable!("an unlisted site's cells are never looked at")
        });
        assert_eq!(report.unlisted.len(), 1);
        assert!(report.listed.is_empty() && report.ring_allowed_under_site.is_empty());
        let errors = site_errors(&report);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not list"), "{}", errors[0]);
        assert!(errors[0].contains(KEY), "{}", errors[0]);
    }

    #[test]
    fn ring_change_follows_the_levelling_rule() {
        // No levelling: the ring's own blend toward the target.
        let got = ring_change(100.0, 110.0, 128, (0.0, 0.0));
        assert!((got - 10.0 * (128.0 / 255.0)).abs() < 1e-4, "{got}");
        assert_eq!(ring_change(100.0, 90.0, 0, (0.0, 0.0)), 0.0);
        // A fully levelled plot ignores the ring.
        assert_eq!(ring_change(100.0, 130.0, 200, (101.0, 1.0)), 0.0);
        // Half levelled: the natural ground is already half way to the plot
        // altitude, and the ring cell is levelled once more.
        let natural: f32 = 100.0 + (110.0 - 100.0) * 0.5;
        let ring = natural + (130.0 - natural) * (200.0 / 255.0);
        let ring = ring + (110.0 - ring) * 0.5;
        let got = ring_change(100.0, 130.0, 200, (110.0, 0.5));
        assert!((got - (ring - natural).abs()).abs() < 1e-4, "{got}");
    }

    #[test]
    fn ring_change_counts_the_second_levelling_of_a_ring_cell() {
        // Even a ring cell of weight 0 is levelled again on top of the
        // natural column's levelling: half way to the plot altitude (50)
        // from the natural 75 is 12.5 blocks, whatever the target.
        let got = ring_change(100.0, 70.0, 0, (50.0, 0.5));
        assert!((got - 12.5).abs() < 1e-4, "{got}");
        let got = ring_change(100.0, 999.0, 0, (50.0, 0.5));
        assert!((got - 12.5).abs() < 1e-4, "{got}");
    }
}
