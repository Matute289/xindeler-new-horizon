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
use crate::{
    IndexRef, Land, World, column::ColumnGen, layer::authored_voids::AuthoredVoids,
    site::SpawnRules, util::Sampler,
};
use common::calendar::Calendar;
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
    /// The largest measured change among the counted cells, rounded up;
    /// `None` when none of them could be measured.
    pub largest_blocks: Option<u32>,
    /// Counted cells whose change could not be measured (they count as over
    /// the limit).
    pub unmeasurable: usize,
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
    /// Every ring cell a region allows under a listed site, with the
    /// altitude the column sampler renders there, by site then `(y, x)`.
    pub levelled_ring_columns: Vec<(Vec2<i32>, f32)>,
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

/// A blend (ring) cell under a site, as the cell scan measured it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingCell {
    pub pos: Vec2<i32>,
    /// How far the ring moves the rendered ground at this cell, in blocks:
    /// the column sampler's altitude with the ring against the same column
    /// with the ring turned into an unauthored cell of its region, so
    /// relief, warp, sea clamp, plot levelling (the ring cell is levelled a
    /// second time) and partial-chunk carving are all in it. Infinite (not
    /// measured) when the region allows no ring cells under sites, and when
    /// a column cannot be sampled.
    pub change: f32,
    /// The altitude the sampler renders at this cell (NaN when not measured).
    pub alt: f32,
}

/// The ground cells under one site's footprint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteGroundCells {
    /// Blend (ring) cells, by `(y, x)`.
    pub ring: Vec<RingCell>,
    /// Exact cells whose top block the sites' plot levelling moves (only
    /// when the region levels sites), with the levelled altitude, by `(y, x)`.
    pub levelled_exact: Vec<(Vec2<i32>, f32)>,
}

/// The ground cells of region `r` under `bounds` (a site's footprint): its
/// ring cells, and the exact cells whose top block the plot levelling of the
/// sites in their chunk moves -- the column sampler's own rule
/// (`alt + (pref - alt) * factor`, `SpawnRules` of every site of the chunk).
/// When the region allows ring cells under sites, each ring cell is measured
/// by sampling its column twice (see [`RingCell::change`]).
pub fn site_ground_cells(
    world: &World,
    index: IndexRef,
    calendar: Option<&Calendar>,
    bounds: Aabr<i32>,
    r: &RegionEntry,
) -> SiteGroundCells {
    let Some(rasters) = world.sim.authored_rasters.as_ref() else {
        return SiteGroundCells::default();
    };
    let land = Land::from_sim(&world.sim);
    let measure_ring = r.ring_under_sites_max_change_blocks.is_some();
    let with_ring = ColumnGen::new(&world.sim);
    let without_ring = ColumnGen::without_ring(&world.sim);
    // The sites' preferred altitude and blend factor at a column.
    let plot_rule = |wpos: Vec2<i32>| -> Option<(f32, f32)> {
        let chunk = world.sim.get_wpos(wpos)?;
        let mut rules = SpawnRules::default();
        for s in &chunk.sites {
            index.sites[*s].spawn_rules(&mut rules, &land, wpos);
        }
        Some(rules.get_preferred_alt())
    };
    let cols = columns(bounds, r.bounds);
    if measure_ring {
        let ring_cells = cols
            .par_iter()
            .filter(|&&p| {
                matches!(
                    rasters.cell_at(p),
                    Some(AuthoredCell::Ground { weight, .. }) if weight != GROUND_EXACT
                )
            })
            .count();
        info!(
            region = r.id,
            ring_cells, "Measuring ring cells under a site (two column samples each)"
        );
    }
    let counted: Vec<(Vec2<i32>, Counted)> = cols
        .par_iter()
        .filter_map(|&wpos| {
            let AuthoredCell::Ground { block, weight } = rasters.cell_at(wpos)? else {
                return None;
            };
            if weight != GROUND_EXACT {
                let (change, alt) = if measure_ring {
                    match (
                        with_ring.get((wpos, index, calendar)),
                        without_ring.get((wpos, index, calendar)),
                    ) {
                        (Some(a), Some(b)) => ((a.alt - b.alt).abs(), a.alt),
                        // Cannot be measured, so it counts.
                        _ => (f32::INFINITY, f32::NAN),
                    }
                } else {
                    (f32::INFINITY, f32::NAN)
                };
                return Some((wpos, Counted::Ring { change, alt }));
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
    for (pos, c) in counted {
        match c {
            Counted::Ring { change, alt } => cells.ring.push(RingCell { pos, change, alt }),
            Counted::Levelled(alt) => cells.levelled_exact.push((pos, alt)),
        }
    }
    cells
}

/// A ground column under a site that the check counts.
enum Counted {
    /// A ring cell ([`RingCell`]).
    Ring { change: f32, alt: f32 },
    /// An exact cell the plot levelling moves, with the levelled altitude.
    Levelled(f32),
}

/// The site check (see the module doc). Ordered by site key, then region.
pub fn site_patch_report(
    world: &World,
    index: IndexRef,
    calendar: Option<&Calendar>,
) -> SitePatchReport {
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
                site_ground_cells(world, index, calendar, *bounds, r)
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

/// Ring cells of one site counted on one side of the allowance.
#[derive(Default)]
struct RingTally {
    count: usize,
    first: Option<Vec2<i32>>,
    /// The largest measured change.
    largest: f32,
    unmeasurable: usize,
}

impl RingTally {
    fn add(&mut self, c: &RingCell) {
        self.count += 1;
        self.first.get_or_insert(c.pos);
        if c.change.is_finite() {
            self.largest = self.largest.max(c.change);
        } else {
            self.unmeasurable += 1;
        }
    }

    fn site(&self, near: &SiteOnPatch, limit: Option<u32>) -> SiteOnPatch {
        SiteOnPatch {
            cells: self.count,
            example: self.first,
            ring_change: limit.map(|limit_blocks| RingChange {
                limit_blocks,
                largest_blocks: (self.count > self.unmeasurable)
                    .then(|| self.largest.ceil() as u32),
                unmeasurable: self.unmeasurable,
            }),
            ..near.clone()
        }
    }
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
    // One pass over the ring cells: those within the allowance (their
    // rendered altitude is kept for the ground queries) and the others.
    let limit = r.ring_under_sites_max_change_blocks;
    let (mut allowed, mut refused) = (RingTally::default(), RingTally::default());
    let mut allowed_alts = Vec::new();
    for c in &cells.ring {
        if limit.is_some_and(|l| c.change <= l as f32) {
            allowed.add(c);
            allowed_alts.push((c.pos, c.alt));
        } else {
            refused.add(c);
        }
    }
    if refused.count > 0 {
        report.ring_under_site.push(refused.site(&near, limit));
    }
    if allowed.count > 0 {
        report
            .ring_allowed_under_site
            .push(allowed.site(&near, limit));
        report.levelled_ring_columns.extend(allowed_alts);
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
                let what = match c.largest_blocks {
                    Some(largest) => format!(
                        "{} cell(s) move the ground by more than the {} block(s) the region \
                         allows, by up to {largest} block(s){}",
                        s.cells,
                        c.limit_blocks,
                        if c.unmeasurable > 0 {
                            format!(", {} of them unmeasurable", c.unmeasurable)
                        } else {
                            String::new()
                        }
                    ),
                    None => format!(
                        "{} cell(s) are unmeasurable against the {} block(s) the region allows",
                        s.cells, c.limit_blocks
                    ),
                };
                format!(
                    "{} (region '{}', {what}, e.g. {:?})",
                    s.site, s.region, s.example
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
    calendar: Option<&Calendar>,
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
        sites: site_patch_report(world, index, calendar),
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
    use crate::authored_raster::{
        AuthoredColumn, DryTerrain, EngineColumn, RegionSettings, SeaFill,
    };

    const KEY: &str = "procedural:town:10,20";
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

    /// The report of one site standing on ring cells with the given changes
    /// (rendered altitude `100 + index`).
    fn file(r: &RegionEntry, key: &str, changes: &[f32]) -> SitePatchReport {
        let ring = changes
            .iter()
            .enumerate()
            .map(|(i, &change)| RingCell {
                pos: Vec2::new(200 + i as i32, 300),
                change,
                alt: 100.0 + i as f32,
            })
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
        assert!(report.levelled_ring_columns.is_empty());
        let errors = site_errors(&report);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0],
            "1 listed site(s) stand on blend (ring) cells, where the ground query only \
             approximates the rendered terrain: procedural:town:10,20 (region 'pit', 2 cell(s), \
             e.g. Some(Vec2 { x: 200, y: 300 })). Grow the exact footprint over the site"
        );
        // A measured change does not matter while the allowance is off.
        let report = file(&r, KEY, &[0.0, 0.5]);
        assert_eq!(report.ring_under_site.len(), 1);
        assert_eq!(report.ring_under_site[0].ring_change, None);
    }

    #[test]
    fn ring_cells_within_the_allowance_are_accepted_and_handed_to_the_ground_queries() {
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
                largest_blocks: Some(3),
                unmeasurable: 0,
            })
        );
        // The rendered altitudes of the allowed cells.
        assert!(report.levelled_columns.is_empty());
        assert_eq!(report.levelled_ring_columns, vec![
            (Vec2::new(200, 300), 100.0),
            (Vec2::new(201, 300), 101.0),
            (Vec2::new(202, 300), 102.0),
        ]);
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
    }

    #[test]
    fn a_cell_that_cannot_be_measured_is_refused_and_says_so() {
        let listed = vec![KEY.to_string()];
        let r = region(&listed, Some(3));
        for bad in [f32::INFINITY, f32::NAN] {
            let report = file(&r, KEY, &[0.0, bad]);
            assert_eq!(report.ring_under_site[0].cells, 1);
            let msg = site_errors(&report).remove(0);
            assert!(
                msg.contains("1 cell(s) are unmeasurable against the 3 block(s)"),
                "{msg}"
            );
            assert!(!msg.contains(&u32::MAX.to_string()), "{msg}");
        }
        let report = file(&r, KEY, &[5.0, f32::INFINITY]);
        let msg = site_errors(&report).remove(0);
        assert!(
            msg.contains(
                "2 cell(s) move the ground by more than the 3 block(s) the region allows, by up \
                 to 5 block(s), 1 of them unmeasurable"
            ),
            "{msg}"
        );
    }

    #[test]
    fn an_unlisted_site_stays_strict_whatever_the_allowance() {
        let listed = vec!["procedural:town:1,2".to_string()];
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

    fn settings() -> RegionSettings {
        RegionSettings {
            suppress_procedural_in_water: false,
            exclude_procedural_margin_m: 0.0,
            aquatic_profile: None,
            sea_fill: SeaFill::Auto,
            site_levelling: true,
        }
    }

    /// The change `AuthoredColumn::resolve` makes at a ring cell of weight
    /// `weight` and target block `block`, against the same column without
    /// the ring, on a noise-free column of table altitude `table` under a
    /// plot `(pref, factor)`; and the closed form it should follow.
    fn resolved_change(table: f32, block: i32, weight: u8, plot: (f32, f32)) -> (f32, f32) {
        let level = |a: f32| a + (plot.0 - a) * plot.1;
        let natural = level(table);
        let column = AuthoredColumn {
            weight: 1.0,
            cell: AuthoredCell::Ground { block, weight },
            water_dist: None,
            ground_dist: None,
            settings: settings(),
            natural: false,
        };
        let engine = EngineColumn {
            alt: natural,
            water_level: 0.0,
            water_dist: None,
            warp_factor: 0.0,
            cliff_offset: 0.0,
            riverless_alt: natural,
        };
        let dry = DryTerrain {
            riverless_alt: natural,
            riverless_alt_delta: 0.0,
            warp: 0.0,
            max_warp: 0.0,
            base_sea_level: 0.0,
            site_prefer_alt: plot,
        };
        let ring = column.resolve(engine, dry).alt;
        let base = column.without_ring().resolve(engine, dry).alt;
        assert_eq!(
            base, natural,
            "a column without its ring is the natural one"
        );
        let target = top_block_alt(block);
        let w = weight as f32 / GROUND_EXACT as f32;
        let closed = plot.1 * (plot.0 - natural) + (1.0 - plot.1) * w * (target - natural);
        ((ring - base).abs(), closed.abs())
    }

    #[test]
    fn the_rendered_ring_change_follows_the_levelling_rule_including_the_second_levelling() {
        for factor in [0.0, 0.25, 0.5, 1.0] {
            for weight in [0u8, 1, 100, 254] {
                let (resolved, closed) = resolved_change(100.0, 130, weight, (50.0, factor));
                assert!(
                    (resolved - closed).abs() < 1e-3,
                    "factor {factor} weight {weight}: resolve {resolved}, closed form {closed}"
                );
            }
        }
        // Weight 0, half levelled: the second levelling alone, whatever the
        // target (natural 75, plot 50).
        let (resolved, _) = resolved_change(100.0, 70, 0, (50.0, 0.5));
        assert!((resolved - 12.5).abs() < 1e-3, "{resolved}");
        // A fully levelled plot ignores the ring.
        let (resolved, _) = resolved_change(100.0, 130, 200, (50.0, 1.0));
        assert!(resolved.abs() < 1e-3, "{resolved}");
    }

    #[test]
    fn without_ring_only_turns_blend_cells_into_unauthored_ones() {
        let column = |weight| AuthoredColumn {
            weight: 1.0,
            cell: AuthoredCell::Ground { block: 10, weight },
            water_dist: None,
            ground_dist: None,
            settings: settings(),
            natural: false,
        };
        assert_eq!(column(128).without_ring().cell, AuthoredCell::None);
        assert_eq!(column(GROUND_EXACT).without_ring(), column(GROUND_EXACT));
    }
}
