//! The post-civ checks of the authored ground layer: what must hold once the
//! sites are placed and the authored-void index is built, before the world
//! is handed to the server.
//!
//! * **Sites** ([`site_patch_report`]): a site whose bounds come within
//!   [`SITE_PATCH_MARGIN_M`] of a ground region must be listed in that
//!   region's `sites_on_patch` (its layout follows the patch, so listing it
//!   is the reviewed act that re-blesses its layout digest); a listed site
//!   must not stand on ring cells (the authored-aware ground query only
//!   approximates them); and every exact ground column a listed site's plot
//!   levelling moves is counted (reported, not refused: houses level their
//!   lots).
//! * **Voids** ([`void_exposure`]): an authored void (cave, interior) whose
//!   authored top comes within [`VOID_EXPOSURE_TOLERANCE_BLOCKS`] of the
//!   ground over it is exposed by the patch; more such columns than the
//!   region allows (`max_exposed_void_columns`, default 0) stop world
//!   generation. Declared mouths are exempt; a declared *wet* mouth must not
//!   hold more water than it declares.
//! * **Roads** ([`road_cliffs`]): civ roads are planned on the natural chunk
//!   table, so one can cross an authored cliff. Every road column on an exact
//!   ground cell that steps more than [`ROAD_CLIMB_LIMIT_BLOCKS`] to an
//!   adjacent exact cell is reported (a verifier gates it; the fix is an
//!   edit, not an engine change).
//!
//! Each check enumerates its columns once, samples them in parallel and
//! sorts the result, so its output does not depend on the thread count.
//! Without a manifest, or without a ground layer, nothing runs.

use super::{
    AuthoredCell, LoadError, ROAD_CLIMB_LIMIT_BLOCKS, RegionEntry, SITE_PATCH_MARGIN_M,
    VOID_EXPOSURE_TOLERANCE_BLOCKS, format::GROUND_EXACT, top_block_alt,
};
use crate::{IndexRef, Land, World, layer::authored_voids::AuthoredVoids, site::SpawnRules};
use rayon::prelude::*;
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    let land = Land::from_sim(&world.sim);
    let mut sites: Vec<(String, Aabr<i32>)> = world
        .civs
        .sites
        .values()
        .filter_map(|s| {
            let id = s.site_tmp?;
            Some((crate::civ::seeds::site_seed_key(s), index.sites[id].bounds()))
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
            let cols = columns(*bounds, r.bounds);
            // Ring cells under the site, and exact cells its levelling moves.
            let counted: Vec<(Vec2<i32>, bool, bool)> = cols
                .par_iter()
                .filter_map(|&wpos| {
                    let AuthoredCell::Ground { block, weight } = rasters.cell_at(wpos)? else {
                        return None;
                    };
                    if weight != GROUND_EXACT {
                        return Some((wpos, true, false));
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
                    (factor > 0.0 && levelled as i32 != alt as i32).then_some((wpos, false, true))
                })
                .collect();
            let tally = |pick: fn(&(Vec2<i32>, bool, bool)) -> bool| {
                let hits: Vec<Vec2<i32>> = counted.iter().filter(|c| pick(c)).map(|c| c.0).collect();
                SiteOnPatch {
                    cells: hits.len(),
                    example: hits.first().copied(),
                    ..near.clone()
                }
            };
            let ring = tally(|c| c.1);
            if ring.cells > 0 {
                report.ring_under_site.push(ring);
            }
            let moved = tally(|c| c.2);
            if moved.cells > 0 {
                report.levelled_exact.push(moved);
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

/// A declared mouth of an authored void: a column where the void is meant
/// to meet the surface (or the water over it). The authored-void data has no
/// format for mouths yet, so no shipped map declares one; the rule and its
/// fixture are ready for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoidMouth {
    pub column: Vec2<i32>,
    /// A wet mouth (the void meets authored or natural water here): the
    /// deepest water, in blocks, the mouth's column may hold. The water then
    /// ends at the mouth: a curtain at the entrance, the cave behind it dry.
    pub max_water_depth_blocks: Option<i32>,
}

/// One region's result of [`void_exposure`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionVoidExposure {
    pub region: String,
    pub bounds: Aabr<i32>,
    /// `max_exposed_void_columns` of the region.
    pub allowed: u32,
    /// Exposed void columns (outside declared mouths).
    pub exposed: usize,
    /// Per authored feature: exposed columns and the first one, by `(y, x)`.
    pub features: Vec<(String, usize, Vec2<i32>)>,
    /// Declared wet mouths holding more water than they declare.
    pub flooded_mouths: Vec<(Vec2<i32>, i32)>,
}

impl RegionVoidExposure {
    pub fn is_error(&self) -> bool {
        self.exposed > self.allowed as usize || !self.flooded_mouths.is_empty()
    }
}

/// The void check (see the module doc), per ground region.
pub(crate) fn void_exposure(
    world: &World,
    voids: Option<&AuthoredVoids>,
    mouths: &[VoidMouth],
) -> Vec<RegionVoidExposure> {
    let (Some(rasters), Some(voids)) = (world.sim.authored_rasters.as_ref(), voids) else {
        return Vec::new();
    };
    let is_mouth = |p: Vec2<i32>| mouths.iter().any(|m| m.column == p);
    rasters
        .region_entries()
        .filter(|r| r.has_ground)
        .map(|r| {
            let chunks: Vec<Vec2<i32>> = rasters
                .ground_chunks(r.id)
                .into_iter()
                .filter(|c| !voids.in_chunk(*c * CHUNK).is_empty())
                .collect();
            let mut hits: Vec<(Vec2<i32>, String)> = chunks
                .par_iter()
                .flat_map_iter(|&c| {
                    let origin = c * CHUNK;
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
                            if is_mouth(wpos) {
                                continue;
                            }
                            for (top, feature) in voids.authored_tops_at_column(wpos) {
                                if top.ceil() as i32 + VOID_EXPOSURE_TOLERANCE_BLOCKS >= ground_top
                                {
                                    out.push((wpos, feature.to_string()));
                                }
                            }
                        }
                    }
                    out
                })
                .collect();
            hits.sort_unstable_by(|a, b| (a.0.y, a.0.x, &a.1).cmp(&(b.0.y, b.0.x, &b.1)));
            hits.dedup();
            let mut columns: Vec<Vec2<i32>> = hits.iter().map(|h| h.0).collect();
            columns.dedup();
            let mut features: Vec<(String, usize, Vec2<i32>)> = Vec::new();
            for (wpos, feature) in &hits {
                match features.iter_mut().find(|f| &f.0 == feature) {
                    Some(f) => f.1 += 1,
                    None => features.push((feature.clone(), 1, *wpos)),
                }
            }
            features.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let flooded_mouths = mouths
                .iter()
                .filter(|m| r.bounds.contains_point(m.column))
                .filter_map(|m| {
                    let max = m.max_water_depth_blocks?;
                    let w = world.sim.water_at(m.column)?;
                    let depth = (w.surface_alt? - w.bed_alt?).round() as i32;
                    (depth > max).then_some((m.column, depth))
                })
                .collect();
            RegionVoidExposure {
                region: r.id.to_string(),
                bounds: r.bounds,
                allowed: r.max_exposed_void_columns,
                exposed: columns.len(),
                features,
                flooded_mouths,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroundConsumerReport {
    pub sites: SitePatchReport,
    pub voids: Vec<RegionVoidExposure>,
    pub roads: RoadCliffReport,
}

/// The three checks as start-up rules: an unlisted site near a ground
/// region, a listed site on ring cells, more exposed void columns than a
/// region allows, or a flooded wet mouth stop world generation; moved exact
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
        .is_some_and(|r| r.region_entries().any(|e| e.has_ground));
    if !has_ground {
        return Ok(GroundConsumerReport::default());
    }
    let report = GroundConsumerReport {
        sites: site_patch_report(world, index),
        // No authored-void format declares mouths yet.
        voids: void_exposure(world, voids, &[]),
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
        if v.exposed > v.allowed as usize {
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
        if !v.flooded_mouths.is_empty() {
            errors.push(format!(
                "region '{}': declared wet void mouth(s) hold more water than declared: {:?}",
                v.region, v.flooded_mouths
            ));
        }
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
