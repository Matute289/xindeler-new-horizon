//! Authored settlement layouts (`cromatolis_v0_settlement_layouts.ron`)
//! against the real authored world: every plot of a layout settlement stays
//! in its footprint and off every other site, the world's own generation
//! matches a direct one, and building counts hold over many seeds.
//!
//! Needs the real Cromatolis LFS assets; `#[ignore]`d like the rest of
//! this module. Run in release:
//!
//! ```text
//! VELOREN_ASSETS=... cargo test -p xindeler-world --release --lib \
//!     settlement_layouts -- --ignored --nocapture
//! ```
//!
//! `XINDELER_LAYOUT_VARIANTS=<n>` changes the number of seeds the spread
//! test measures (default 64).

use super::{site_layouts::generate_cromatolis, *};
use crate::{
    civ::{generate_authored_city_with_seed, seeds::layout_variant_seed},
    site::layout::{Footprint, Zone},
};
use std::fmt::Write as _;

/// The settlements `cromatolis_v0_settlement_layouts.ron` gives a layout.
const LAYOUT_SITES: &[&str] = &["site.kalthis", "site.duren"];

fn generated_site<'a>(world: &World, index: &'a IndexOwned, site_id: &str) -> &'a site::Site {
    let civ_site = world
        .civs()
        .sites
        .values()
        .find(|site| site.authored_id() == Some(site_id))
        .unwrap_or_else(|| panic!("no authored site {site_id}"));
    index.sites.get(civ_site.site_tmp.expect("generated"))
}

fn building_roots(site: &site::Site) -> impl Iterator<Item = Vec2<i32>> + '_ {
    site.plots()
        .filter(|plot| plot.is_building())
        .map(|plot| plot.root_tile())
}

/// Everything about a layout that a regression should notice, in a stable
/// order: each plot's kind, root and bounds.
fn plot_list(site: &site::Site) -> Vec<String> {
    let mut plots: Vec<String> = site
        .plots()
        .map(|plot| {
            let b = plot.find_bounds();
            format!("{} {:?} {:?}", plot.kind(), plot.root_tile(), b)
        })
        .collect();
    plots.sort();
    plots
}

/// The world's own Kalthis and Duren are exactly what generating them
/// directly with their own seed and layout gives: the layout reaches the
/// generator through world generation, and the first draw already meets the
/// size band (no re-draw replaced it).
#[test]
#[ignore]
fn world_generation_hands_each_layout_to_its_settlement() {
    let (world, index) = generate_cromatolis(None);
    for &site_id in LAYOUT_SITES {
        let in_world = generated_site(&world, &index, site_id);
        let (direct, layout) =
            generate_authored_city_with_seed(&world, index.as_index_ref(), site_id, None, true);
        assert!(layout.is_some(), "{site_id} has no layout");
        assert_eq!(
            plot_list(in_world),
            plot_list(&direct),
            "{site_id}: the world's layout is not its first draw with its layout"
        );
    }
}

/// Every building, plaza and field of a layout settlement lies inside its
/// footprint (every tile of the plot), and none touches another site's
/// plots. Roads are left out on both sides (they may cross), and so is the
/// settlement's naval port, which the layout does not place.
#[test]
#[ignore]
fn layout_settlements_stay_in_their_footprint_and_clear_of_other_sites() {
    let (world, index) = generate_cromatolis(None);
    let mut problems = Vec::new();
    for &site_id in LAYOUT_SITES {
        let site = generated_site(&world, &index, site_id);
        let (_, layout) =
            generate_authored_city_with_seed(&world, index.as_index_ref(), site_id, None, true);
        let footprint = Footprint::new(&layout.unwrap(), site.origin);
        for plot in site.plots() {
            let zone = match plot.kind() {
                PlotKind::Road(_) | PlotKind::NavalPort(_) => continue,
                PlotKind::FarmField(_) | PlotKind::Barn(_) => Zone::Rural,
                _ => Zone::Built,
            };
            if let Some(tile) = plot.tiles().find(|&tile| !footprint.allows(tile, zone)) {
                problems.push(format!(
                    "{site_id}: {} at {:?} uses tile {tile:?} outside its footprint",
                    plot.kind(),
                    plot.root_tile()
                ));
            }
        }

        // Every tile of this settlement's plots (roads aside), and for each
        // other site's plot tile, the tiles of ours its 6 x 6 blocks touch.
        // The naval port is placed by its own shoreline search, which the
        // layout does not govern.
        let own: std::collections::HashMap<Vec2<i32>, String> = site
            .plots()
            .filter(|plot| !matches!(plot.kind(), PlotKind::Road(_) | PlotKind::NavalPort(_)))
            .flat_map(|plot| {
                plot.tiles()
                    .map(move |tile| (tile, plot.kind().to_string()))
            })
            .collect();
        let tile_size = (site.tile_wpos(Vec2::one()) - site.tile_wpos(Vec2::zero())).x;
        for other in index.sites.values() {
            if std::ptr::eq(other, site) {
                continue;
            }
            for plot in other.plots() {
                if matches!(plot.kind(), PlotKind::Road(_)) {
                    continue;
                }
                // A citadel claims a square of tiles but builds only within
                // its radius of the site origin: check that disc instead.
                if let PlotKind::Citadel(citadel) = plot.kind() {
                    let radius = citadel.radius() as f32;
                    for (&tile, kind) in &own {
                        let rect = Aabr {
                            min: site.tile_wpos(tile),
                            max: site.tile_wpos(tile + 1),
                        };
                        let nearest = rect.projected_point(other.origin);
                        if nearest.as_::<f32>().distance(other.origin.as_()) < radius {
                            problems.push(format!(
                                "{site_id}: {kind} tile {tile:?} is inside citadel {:?}",
                                other.name()
                            ));
                        }
                    }
                    continue;
                }
                for tile in plot.tiles() {
                    let min = other.tile_wpos(tile) - site.origin;
                    let max = min + tile_size - 1;
                    let (lo, hi) = (
                        min.map(|e| e.div_euclid(tile_size)),
                        max.map(|e| e.div_euclid(tile_size)),
                    );
                    for y in lo.y..=hi.y {
                        for x in lo.x..=hi.x {
                            if let Some(kind) = own.get(&Vec2::new(x, y)) {
                                problems.push(format!(
                                    "{site_id}: {kind} tile {:?} overlaps {} of {:?}",
                                    Vec2::new(x, y),
                                    plot.kind(),
                                    other.name()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

fn percentile(sorted: &[usize], p: f64) -> usize {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn spread(values: &[usize]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mean = sorted.iter().sum::<usize>() as f64 / sorted.len() as f64;
    let sd = (sorted
        .iter()
        .map(|&v| (v as f64 - mean).powi(2))
        .sum::<f64>()
        / sorted.len() as f64)
        .sqrt();
    format!(
        "min {} p10 {} median {} p90 {} max {} (sd {sd:.1})",
        sorted[0],
        percentile(&sorted, 0.1),
        percentile(&sorted, 0.5),
        percentile(&sorted, 0.9),
        sorted[sorted.len() - 1],
    )
}

/// The spread of building counts over many seeds, with and without the
/// layout, overall and per ward against each ward's target; printed for
/// review. Asserts what the layout guarantees today: every seed reaches the
/// settlement's size band on its first draw, so the size-band re-draw is
/// never needed for a layout settlement.
#[test]
#[ignore]
fn layout_settlements_reach_their_band_on_every_seed() {
    let variants: u32 = std::env::var("XINDELER_LAYOUT_VARIANTS")
        .ok()
        .map_or(64, |v| v.parse().unwrap());
    let (world, index) = generate_cromatolis(None);
    let mut report = String::new();
    let mut below_band = Vec::new();
    for &site_id in LAYOUT_SITES {
        let civ_site = world
            .civs()
            .sites
            .values()
            .find(|site| site.authored_id() == Some(site_id))
            .unwrap();
        let key = crate::civ::seeds::site_seed_key(civ_site);
        let (category, size) = civ_site.authored_category_and_size().unwrap();
        let band = crate::civ::seeds::min_buildings_for(category, size).unwrap();
        let mut with = Vec::new();
        let mut without = Vec::new();
        let mut per_ward: Vec<Vec<usize>> = Vec::new();
        let mut layout = None;
        for k in 0..variants {
            let seed = Some(layout_variant_seed(0, &key, k));
            let (site, l) =
                generate_authored_city_with_seed(&world, index.as_index_ref(), site_id, seed, true);
            let l = l.unwrap();
            let footprint = Footprint::new(&l, site.origin);
            let buildings = crate::civ::seeds::building_count(&site);
            if buildings < band {
                below_band.push(format!("{site_id} variant {k}: {buildings} < {band}"));
            }
            with.push(buildings);
            for (ward, count) in footprint
                .buildings_per_ward(building_roots(&site))
                .into_iter()
                .enumerate()
            {
                if per_ward.len() <= ward {
                    per_ward.push(Vec::new());
                }
                per_ward[ward].push(count);
            }
            layout = Some(l);
            let (site, _) = generate_authored_city_with_seed(
                &world,
                index.as_index_ref(),
                site_id,
                seed,
                false,
            );
            without.push(crate::civ::seeds::building_count(&site));
        }
        let layout = layout.unwrap();
        let usable = Footprint::new(&layout, generated_site(&world, &index, site_id).origin)
            .usable_tiles_per_ward();
        let in_world = crate::civ::seeds::building_count(generated_site(&world, &index, site_id));
        let _ = writeln!(
            report,
            "{site_id}: target {} band {band}, world seed {in_world} buildings, {variants} seeds",
            layout.target_buildings()
        );
        let _ = writeln!(report, "  with layout:    {}", spread(&with));
        let _ = writeln!(report, "  without layout: {}", spread(&without));
        for ((ward, counts), usable) in layout.wards.iter().zip(&per_ward).zip(&usable) {
            // A tile is 6 x 6 blocks.
            let area = (ward.a.1 - ward.a.0) * (ward.b.1 - ward.b.0) / 36.0;
            let _ = writeln!(
                report,
                "  {:<18} target {:>3}, {:>3.0}% of its area usable: {}",
                ward.id,
                ward.target_buildings,
                100.0 * *usable as f64 / area,
                spread(counts)
            );
        }
    }
    println!("{report}");
    assert!(below_band.is_empty(), "{}", below_band.join("\n"));
}

/// A settlement with an authored layout keeps the rtsim population of the
/// layout it would have generated without one, so the larger town brings no
/// more NPCs; every other site's population is still its own plot count.
///
/// The expected plot counts are the layouts Kalthis and Duren generated
/// before settlement layouts existed (`cromatolis_site_layout_digests.txt`
/// as committed then). A terrain edit that re-rolls either town's
/// procedural layout changes them; re-measure, don't just re-approve.
#[test]
#[ignore]
fn layout_settlements_keep_their_procedural_population() {
    const PROCEDURAL_PLOTS: &[(&str, usize)] = &[("site.kalthis", 116), ("site.duren", 133)];
    let (world, index) = generate_cromatolis(None);
    for &(site_id, plots) in PROCEDURAL_PLOTS {
        let site = generated_site(&world, &index, site_id);
        assert_eq!(site.population_plots(), plots, "{site_id}");
        assert!(
            site.plots().len() > plots,
            "{site_id}: premise, the layout grew it"
        );
    }
    let layout_sites: Vec<_> = LAYOUT_SITES
        .iter()
        .map(|&id| generated_site(&world, &index, id) as *const site::Site)
        .collect();
    for (_, site) in index.sites.iter() {
        if layout_sites.contains(&(site as *const site::Site)) {
            continue;
        }
        assert_eq!(
            site.population_plots(),
            site.plots().len(),
            "{:?}",
            site.name()
        );
    }
}
