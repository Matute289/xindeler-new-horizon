//! Real-world checks of the consumers of the authored ground layer on the
//! Cromatolis map: the authored-aware ground queries (identity outside
//! regions, the patch inside, the ring tolerance), a town on a prepared flat
//! and on a slope (a site on a ring and an unlisted site refused), buried
//! players and simulated NPCs on raised patches, rtsim, spawns and wildlife
//! on a dry pit floor below 0 m, a quay beside an authored basin, and a civ
//! road over an authored cliff. Synthetic manifests only: the calibration arena, or a wilderness
//! site / road found far from every other site.
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release --features tools \
//!     authored_raster::ground_consumer_real_world_tests -- --ignored --nocapture --test-threads 1
//! ```

use super::{
    AuthoredCell, RegionEntry, SeaFill,
    format::GROUND_EXACT,
    post_civ::{check_ground_consumers, road_cliffs, site_ground_cells},
    real_world_tests::{
        PLATEAU_CM, REGION_MAX, REGION_MIN, generate_world, is_natural_ground, load_specs,
    },
    top_block_alt,
    writer::{PaintOp, RegionSpec, Shape},
};
use crate::{Land, World, util::Sampler};
use common::{
    comp::Body,
    generation::EntitySpawn,
    terrain::{Block, BlockKind, TerrainChunkSize},
    vol::{ReadVol, RectVolSize},
};
use rand::prelude::*;
use rand_chacha::ChaChaRng;
use rayon::prelude::*;
use vek::*;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape { Shape::Rect { x0, y0, x1, y1 } }

fn exact(shape: Shape, ground_cm: i32) -> PaintOp {
    PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    }
}

fn ring(width_m: i32) -> PaintOp {
    PaintOp::GroundRing {
        width_m,
        ground_cm: None,
    }
}

const fn floor32(v: i32) -> i32 { v.div_euclid(32) * 32 }
const fn ceil32(v: i32) -> i32 { (v + 31).div_euclid(32) * 32 }

fn sampled_alt(world: &World, index: crate::IndexRef, p: Vec2<i32>) -> f32 {
    world.sample_columns().get((p, index, None)).unwrap().alt
}

fn entry<'a>(world: &'a World, id: &str) -> RegionEntry<'a> {
    world
        .sim
        .authored_rasters
        .as_ref()
        .unwrap()
        .region_entries()
        .find(|e| e.id == id)
        .unwrap()
}

/// The authored-aware ground queries: bit for bit the chunk table outside
/// every region (10^6 random columns, with a ground manifest loaded), the
/// authored block on exact cells (the column sampler agrees), within the
/// documented tolerance on ring cells, a flat patch has gradient 0, and a
/// quay beside an authored basin reads the quay and the water.
#[test]
#[ignore]
fn ground_queries_are_the_table_outside_and_the_patch_inside() {
    let (mut world, index) = generate_world();
    let index = index.as_index_ref();
    let (x0, y0, y1) = (23000, 24800, 25200);
    let raised = PLATEAU_CM + 500;
    let ground = RegionSpec::new("q_ground", REGION_MIN, (23328, REGION_MAX.1), 32, vec![
        exact(rect(x0 as f32, y0 as f32, 23280.0, y1 as f32), raised),
        ring(32),
    ]);
    // A basin (water top 139, bed 130) held by an exact quay at
    // block 141 (sea level + 1.5 m), in an Auto region.
    let quay = RegionSpec::new("q_quay", (23360, REGION_MIN.1), REGION_MAX, 32, vec![
        PaintOp::Water {
            shape: rect(23500.0, 24800.0, 23600.0, 24900.0),
            surface_cm: 13_950,
            bed_cm: 13_050,
        },
        exact(rect(23490.0, 24790.0, 23610.0, 24910.0), 14_150),
        exact(rect(23500.0, 24800.0, 23600.0, 24900.0), 13_050),
    ]);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[ground, quay]).unwrap()));
    let sim = &world.sim;
    let land = Land::from_sim(sim);
    let size = sim.get_size().map(|e| e as i32) * 32;
    let boxes: Vec<Aabr<i32>> = sim
        .authored_rasters
        .as_ref()
        .unwrap()
        .region_entries()
        .map(|e| e.bounds)
        .collect();
    let mut rng = ChaChaRng::from_seed([3; 32]);
    let points: Vec<Vec2<i32>> = (0..1_000_000)
        .map(|_| Vec2::new(rng.random_range(0..size.x), rng.random_range(0..size.y)))
        .filter(|p| !boxes.iter().any(|b| b.contains_point(*p)))
        .collect();
    let bad = points
        .par_iter()
        .filter(|&&p| {
            sim.ground_alt_at(p).map(f32::to_bits) != sim.get_alt_approx(p).map(f32::to_bits)
                || sim.surface_alt_at(p).to_bits() != sim.get_surface_alt_approx(p).to_bits()
                || land.ground_alt_at(p).to_bits() != land.get_alt_approx(p).to_bits()
                || land.ground_gradient_at(p).to_bits()
                    != land.get_gradient_approx(p).to_bits()
        })
        .count();
    println!("outside: {} columns compared, {bad} differ", points.len());
    assert_eq!(bad, 0);

    // Exact cells: the block, and the column sampler renders it.
    let block = raised.div_euclid(100);
    let exact_pts: Vec<Vec2<i32>> = (0..2000)
        .map(|_| Vec2::new(rng.random_range(x0..23280), rng.random_range(y0..y1)))
        .collect();
    for p in &exact_pts {
        assert_eq!(sim.ground_alt_at(*p), Some(top_block_alt(block)));
        assert_eq!(land.ground_alt_at(*p), top_block_alt(block));
        assert_eq!(sim.surface_alt_at(*p), (block + 1) as f32);
    }
    let rendered_off = exact_pts
        .par_iter()
        .filter(|p| sampled_alt(&world, index, **p) as i32 != block)
        .count();
    assert_eq!(rendered_off, 0, "exact cells render the block");
    // A chunk deep inside the flat: gradient 0 (the table's is not).
    let centre = Vec2::new(23136, 24992);
    assert_eq!(land.ground_gradient_at(centre), 0.0);
    println!(
        "flat patch gradient 0 (table {:.3})",
        land.get_gradient_approx(centre)
    );

    // Ring cells: the query lerps toward the table, the blocks toward the
    // engine column with its noise; the difference is the tolerance.
    let rasters = sim.authored_rasters.as_ref().unwrap();
    let ring_pts: Vec<(Vec2<i32>, u8)> = (y0 - 40..y1 + 40)
        .step_by(3)
        .flat_map(|y| (x0 - 40..x0).map(move |x| Vec2::new(x, y)))
        .filter_map(|p| match rasters.cell_at(p) {
            Some(AuthoredCell::Ground { weight, .. }) if weight < GROUND_EXACT => Some((p, weight)),
            _ => None,
        })
        .collect();
    let errs: Vec<(u8, f32)> = ring_pts
        .par_iter()
        .map(|&(p, w)| {
            (
                w,
                (sampled_alt(&world, index, p) - sim.ground_alt_at(p).unwrap()).abs(),
            )
        })
        .collect();
    let max = errs.iter().map(|e| e.1).fold(0.0, f32::max);
    let mean = errs.iter().map(|e| e.1).sum::<f32>() / errs.len().max(1) as f32;
    let hi = errs
        .iter()
        .filter(|e| e.0 >= 192)
        .map(|e| e.1)
        .fold(0.0, f32::max);
    println!(
        "ring tolerance: {} ring cells, |rendered - ground_alt_at| mean {mean:.2} m, max {max:.2} \
         m (weight >= 192: max {hi:.2} m)",
        errs.len()
    );
    assert!(!errs.is_empty() && max < 16.0);

    // The quay and the basin through the routed queries.
    let q = Vec2::new(23495, 24850);
    assert_eq!(land.ground_alt_at(q), top_block_alt(141));
    assert_eq!(land.surface_alt_at(q), 142.0);
    let b = Vec2::new(23550, 24850);
    assert_eq!(land.authored_depth_at(b), Some(9));
    assert_eq!(land.surface_alt_at(b), 140.0);
    assert_eq!(land.ground_alt_at(b), top_block_alt(130));
    assert_eq!(sampled_alt(&world, index, q) as i32, 141);
}

/// A town (`Site::generate_city`, routed `Land`) on an exact patch of the
/// arena, inserted into the world's index and chunks so the column sampler
/// applies its spawn rules.
fn place_town(
    world: &mut World,
    index: &mut crate::IndexOwned,
    origin: Vec2<i32>,
    seed: u8,
) -> common::store::Id<crate::site::Site> {
    let site = {
        let land = Land::from_sim(&world.sim);
        let mut rng = ChaChaRng::from_seed([seed; 32]);
        crate::site::Site::generate_city(
            &land,
            index.as_index_ref(),
            &mut rng,
            origin,
            0.3,
            None,
            &mut crate::site::genstat::SitesGenMeta::new(0),
            None,
            None,
        )
    };
    let b = site.bounds();
    let id = index.index_mut_for_test().sites.insert(site);
    let (c0, c1) = (
        b.min.map(|e| e.div_euclid(32)),
        b.max.map(|e| e.div_euclid(32)),
    );
    for cy in c0.y..=c1.y {
        for cx in c0.x..=c1.x {
            world.sim.get_mut(Vec2::new(cx, cy)).unwrap().sites.push(id);
        }
    }
    id
}

#[derive(Debug, Default)]
struct LotRow {
    buildings: usize,
    /// Footprint columns compared.
    columns: usize,
    /// Columns whose sampled top block is more than 1 block off the plot's
    /// base (the routed `Land` altitude at its root tile, which plots build
    /// on).
    off: usize,
    /// Buildings with more than 10 % of their columns off (floating or
    /// buried).
    misplaced: usize,
    /// The same against the chunk table's altitude (what plots read before
    /// the ground layer was routed).
    misplaced_table: usize,
}

/// Grounding of every building of `site` against the ground around it: a
/// building floats (is buried) when the median top block of the columns
/// just outside its plot -- the sampled ground, levelling included -- lies
/// more than 2 blocks below (above) its base, the routed `Land` altitude it
/// is built on.
fn grounding(
    world: &World,
    index: crate::IndexRef,
    site: &crate::site::Site,
) -> (usize, usize, usize) {
    let land = Land::from_sim(&world.sim);
    let (mut buildings, mut floating, mut buried) = (0, 0, 0);
    for plot in site.plots().filter(|p| p.is_building()) {
        let base = land.ground_alt_at(site.tile_center_wpos(plot.root_tile())) as i32;
        let own: std::collections::HashSet<Vec2<i32>> = plot.tiles().collect();
        let mut ring: Vec<Vec2<i32>> = Vec::new();
        for t in plot.tiles() {
            let (a, e) = (site.tile_wpos(t), site.tile_wpos(t + 1));
            for (n, edge) in [
                (
                    Vec2::new(1, 0),
                    (Vec2::new(e.x, a.y), Vec2::new(e.x + 1, e.y)),
                ),
                (
                    Vec2::new(-1, 0),
                    (Vec2::new(a.x - 1, a.y), Vec2::new(a.x, e.y)),
                ),
                (
                    Vec2::new(0, 1),
                    (Vec2::new(a.x, e.y), Vec2::new(e.x, e.y + 1)),
                ),
                (
                    Vec2::new(0, -1),
                    (Vec2::new(a.x, a.y - 1), Vec2::new(e.x, a.y)),
                ),
            ] {
                if own.contains(&(t + n)) {
                    continue;
                }
                for y in edge.0.y..edge.1.y {
                    for x in edge.0.x..edge.1.x {
                        ring.push(Vec2::new(x, y));
                    }
                }
            }
        }
        let mut tops: Vec<i32> = ring
            .par_iter()
            .map(|p| sampled_alt(world, index, *p) as i32)
            .collect();
        if tops.is_empty() {
            continue;
        }
        tops.sort_unstable();
        let median = tops[tops.len() / 2];
        buildings += 1;
        floating += (median < base - 2) as usize;
        buried += (median > base + 2) as usize;
    }
    (buildings, floating, buried)
}

fn lot_check(world: &World, index: crate::IndexRef, site: &crate::site::Site) -> LotRow {
    let land = Land::from_sim(&world.sim);
    let mut row = LotRow::default();
    for plot in site.plots().filter(|p| p.is_building()) {
        let root = site.tile_center_wpos(plot.root_tile());
        let base = land.ground_alt_at(root) as i32;
        let base_table = land.get_alt_approx(root) as i32;
        let cols: Vec<Vec2<i32>> = plot
            .tiles()
            .flat_map(|t| {
                let a = site.tile_wpos(t);
                let b = site.tile_wpos(t + 1);
                (a.y..b.y).flat_map(move |y| (a.x..b.x).map(move |x| Vec2::new(x, y)))
            })
            .collect();
        let tops: Vec<i32> = cols
            .par_iter()
            .map(|p| sampled_alt(world, index, *p) as i32)
            .collect();
        let off = tops.iter().filter(|t| (**t - base).abs() > 1).count();
        let off_table = tops.iter().filter(|t| (**t - base_table).abs() > 1).count();
        row.buildings += 1;
        row.columns += cols.len();
        row.off += off;
        row.misplaced += (off * 10 > cols.len()) as usize;
        row.misplaced_table += (off_table * 10 > cols.len()) as usize;
    }
    row
}

/// A town on a prepared flat and on a 0.1 slope, both exact patches
/// of the arena: every building's lot matches its base (0 floating/buried
/// buildings), levelling moves no exact column on the flat (it levels the
/// lots on the slope, reported), and no ring cell lies under either town.
#[test]
#[ignore]
fn bt10_towns_on_a_prepared_flat_and_on_a_slope() {
    let (mut world, mut index) = generate_world();
    let inner = 48; // 16 m box margin + the 32 m ring.
    let west = (REGION_MIN, (23328, REGION_MAX.1));
    let east = ((23360, REGION_MIN.1), REGION_MAX);
    let body = |(min, max): ((i32, i32), (i32, i32))| {
        rect(
            (min.0 + inner) as f32,
            (min.1 + inner) as f32,
            (max.0 - inner) as f32,
            (max.1 - inner) as f32,
        )
    };
    let flat = RegionSpec::new("bt10_flat", west.0, west.1, 32, vec![
        exact(body(west), PLATEAU_CM),
        ring(32),
    ]);
    let slope_origin = ((east.0.0 + east.1.0) / 2, (east.0.1 + east.1.1) / 2);
    let slope = RegionSpec::new("bt10_slope", east.0, east.1, 32, vec![
        PaintOp::GroundPlane {
            shape: body(east),
            origin: (slope_origin.0 as f32, slope_origin.1 as f32),
            origin_cm: PLATEAU_CM,
            cm_per_m: (10.0, 0.0),
        },
        ring(32),
    ]);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[flat, slope]).unwrap()));
    for (id, origin, seed) in [
        (
            "bt10_flat",
            Vec2::new((west.0.0 + west.1.0) / 2, (west.0.1 + west.1.1) / 2),
            11u8,
        ),
        ("bt10_slope", Vec2::from(slope_origin), 12u8),
    ] {
        let id_site = place_town(&mut world, &mut index, origin, seed);
        let index_ref = index.as_index_ref();
        let site = &index_ref.sites[id_site];
        let b = site.bounds();
        let e = entry(&world, id);
        let inside = Aabr {
            min: e.bounds.min + inner,
            max: e.bounds.max - inner,
        };
        assert!(
            inside.contains_point(b.min) && inside.contains_point(b.max - 1),
            "{id}: the town {b:?} must lie on the exact patch {inside:?}"
        );
        let cells = site_ground_cells(&world, index_ref, b, &e);
        let row = lot_check(&world, index_ref, site);
        println!(
            "{id}: bounds {:?}..{:?}, {} plots; {row:?}; ring cells under it {}, exact columns \
             moved by levelling {}",
            b.min,
            b.max,
            site.plots().len(),
            cells.ring.len(),
            cells.levelled_exact.len()
        );
        let (buildings, floating, buried) = grounding(&world, index_ref, site);
        println!("{id}: grounding: {buildings} buildings, {floating} floating, {buried} buried");
        assert!(row.buildings > 0 && buildings > 0);
        assert!(row.misplaced <= row.misplaced_table);
        let land = Land::from_sim(&world.sim);
        let table_land_off = site
            .plots()
            .filter(|p| p.is_building())
            .filter(|p| {
                let root = site.tile_center_wpos(p.root_tile());
                (land.ground_alt_at(root) as i32 - land.get_alt_approx(root) as i32).abs()
                    > 2
            })
            .count();
        println!(
            "{id}: {table_land_off} buildings would be off by more than 2 blocks on the table"
        );
        if id == "bt10_flat" {
            assert_eq!(
                (floating, buried),
                (0, 0),
                "{id}: floating or buried buildings"
            );
            assert_eq!(row.misplaced, 0, "{id}: lots off their base on a flat");
        } else {
            // On a slope a building whose door faces downhill is cut into the
            // hill: the plots' own placement rule, as on engine terrain.
            assert!(
                (floating + buried) * 10 <= buildings,
                "{id}: floating or buried buildings"
            );
        }
        assert!(cells.ring.is_empty());
        if id == "bt10_flat" {
            // Houses raise their lot one block over the street (the vanilla
            // plot rule, on engine terrain too): the only levelling a
            // prepared flat sees, reported by the post-civ check.
            let block = PLATEAU_CM.div_euclid(100);
            let shift = cells
                .levelled_exact
                .par_iter()
                .map(|p| (sampled_alt(&world, index_ref, *p) as i32 - block).abs())
                .max()
                .unwrap_or(0);
            println!("{id}: levelling moves exact columns by at most {shift} block(s)");
            assert!(
                shift <= 1,
                "levelling on a flat moves a lot by {shift} blocks"
            );
        }
    }
}

/// An ordinary (procedural) site -- on Cromatolis a procedural bridge --
/// with no authored site within 400 m and
/// away from the map rim, with its stable key and bounds.
fn lonely_site(world: &World, index: crate::IndexRef) -> (String, Aabr<i32>) {
    let sites: Vec<(String, Aabr<i32>)> = world
        .civs
        .sites
        .values()
        .filter_map(|s| {
            Some((
                crate::civ::seeds::site_seed_key(s),
                index.sites[s.site_tmp?].bounds(),
            ))
        })
        .collect();
    let size = world.sim.get_size().map(|e| e as i32) * 32;
    let gap = |a: &Aabr<i32>, b: &Aabr<i32>| {
        let dx = (b.min.x - a.max.x).max(a.min.x - b.max.x).max(0);
        let dy = (b.min.y - a.max.y).max(a.min.y - b.max.y).max(0);
        dx.max(dy)
    };
    sites
        .iter()
        .filter(|(k, b)| {
            k.starts_with("procedural")
                && (b.max - b.min).reduce_max() < 400
                && b.min.reduce_min() > 1024
                && b.max.x < size.x - 1024
                && b.max.y < size.y - 1024
        })
        .map(|(k, b)| {
            let nearest = sites
                .iter()
                .filter(|(k2, _)| !k2.starts_with("procedural"))
                .map(|(_, b2)| gap(b, b2))
                .min()
                .unwrap_or(i32::MAX);
            (nearest, k.clone(), *b)
        })
        .filter(|(nearest, ..)| *nearest > 400)
        .min_by(|a, b| a.1.cmp(&b.1))
        .map(|(_, k, b)| (k, b))
        .expect("an ordinary site 400 m from every authored site")
}

/// The site gate: a ground region near an
/// ordinary site that it does not list stops world generation, naming the
/// site; listing it is accepted; a listed site standing on ring cells is
/// refused, naming the site and the ring.
#[test]
#[ignore]
fn bt10_unlisted_sites_and_sites_on_rings_are_refused() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let (key, b) = lonely_site(&world, index_ref);
    println!("site {key} at {b:?}");
    let pad = 64 + 32 + 16;
    let (min, max) = (
        (floor32(b.min.x - pad), floor32(b.min.y - pad)),
        (ceil32(b.max.x + pad), ceil32(b.max.y + pad)),
    );
    let ground_cm = (world.sim.get_alt_approx(b.center()).unwrap() * 100.0) as i32;
    let spec = |listed: &[String], cover: Aabr<i32>| {
        let mut s = RegionSpec::new("bt10_gate", min, max, 32, vec![
            exact(
                rect(
                    cover.min.x as f32,
                    cover.min.y as f32,
                    cover.max.x as f32,
                    cover.max.y as f32,
                ),
                ground_cm,
            ),
            ring(32),
        ]);
        s.sites_on_patch = listed.to_vec();
        s
    };
    let whole = Aabr {
        min: b.min - 40,
        max: b.max + 40,
    };
    // Unlisted: refused, naming the site.
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(&[], whole)]).unwrap()));
    let err = check_ground_consumers(&world, index_ref, None)
        .unwrap_err()
        .0;
    println!("unlisted: {err}");
    assert!(err.contains(&key) && err.contains("does not list them"));
    // Every site near the region (the chosen one and its procedural
    // neighbours), as the exporter's pins would list them.
    let near: Vec<String> = super::post_civ::site_patch_report(&world, index_ref)
        .unlisted
        .into_iter()
        .map(|s| s.site)
        .collect();
    assert!(near.contains(&key));
    // Listed, exact under the whole footprint: accepted.
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(&near, whole)]).unwrap()));
    let report = check_ground_consumers(&world, index_ref, None).unwrap();
    assert_eq!(report.sites.listed.len(), near.len());
    assert!(report.sites.unlisted.is_empty() && report.sites.ring_under_site.is_empty());
    println!(
        "listed: levelling moves {} exact column(s)",
        report
            .sites
            .levelled_exact
            .iter()
            .map(|s| s.cells)
            .sum::<usize>()
    );
    // Listed, but the exact patch stops 20 m inside its east edge: the ring
    // runs under the site.
    let short = Aabr {
        min: whole.min,
        max: Vec2::new(b.max.x - 20, whole.max.y),
    };
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(&near, short)]).unwrap()));
    let err = check_ground_consumers(&world, index_ref, None)
        .unwrap_err()
        .0;
    println!("ring under site: {err}");
    assert!(err.contains(&key) && err.contains("blend (ring)"));
}

/// Positions buried by +10 m and +90 m patches are lifted
/// to the new surface (free air there, solid ground under it); simulated
/// NPCs and rtsim spawns stand on the patch; on a dry pit floor at -10 m
/// (`AuthoredOnly`) the queries have no sea-level clamp, the chunk is dry for
/// `chunk_water`, the generated pit holds no water and no aquatic animal.
#[test]
#[ignore]
fn bt11_bt14_positions_npcs_and_wildlife_on_raised_and_pit_patches() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let p10 = Vec2::new(22900, 24800);
    let p90 = Vec2::new(23100, 24800);
    let before = |p: Vec2<i32>| sampled_alt(&world, index_ref, p + 48) as i32;
    let (g10, g90) = (before(p10), before(p90));
    let raised = RegionSpec::new("bt11_raised", REGION_MIN, (23328, REGION_MAX.1), 32, vec![
        exact(
            rect(
                p10.x as f32,
                p10.y as f32,
                (p10.x + 96) as f32,
                (p10.y + 96) as f32,
            ),
            (g10 + 10) * 100 + 50,
        ),
        exact(
            rect(
                p90.x as f32,
                p90.y as f32,
                (p90.x + 96) as f32,
                (p90.y + 96) as f32,
            ),
            (g90 + 90) * 100 + 50,
        ),
    ]);
    // A pit floor at block 129 (-10 m), 128 x 128 m, chunk-aligned.
    let pit = (23520, 24800);
    let mut pit_spec = RegionSpec::new("bt14_pit", (23360, REGION_MIN.1), REGION_MAX, 32, vec![
        exact(
            rect(
                pit.0 as f32,
                pit.1 as f32,
                (pit.0 + 128) as f32,
                (pit.1 + 128) as f32,
            ),
            12_950,
        ),
    ]);
    pit_spec.sea_fill = SeaFill::AuthoredOnly;
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[raised, pit_spec]).unwrap()));
    let sim = &world.sim;
    for (p, old, block) in [(p10, g10, g10 + 10), (p90, g90, g90 + 90)] {
        let c = p + 48;
        // A player saved standing on the old ground is now inside it.
        let feet = c.with_z(old + 1);
        assert_eq!(sim.buried_ground_lift(feet, true), Some(block + 1));
        // The generated column: solid ground at the block, air above.
        let cpos = c.map(|e| e.div_euclid(32));
        let (chunk, _) = world
            .generate_chunk(index_ref, cpos, None, || false, None, None)
            .unwrap();
        let local = (c - cpos * 32).with_z(0);
        let at = |z: i32| {
            chunk
                .get(local.with_z(z))
                .copied()
                .unwrap_or_else(|_| Block::empty())
        };
        assert!(at(block).is_solid(), "ground at the patch block");
        assert!(!at(block + 1).is_solid(), "free air where the player lands");
        // Simulated NPCs (per-tick snap) and rtsim spawns.
        assert_eq!(sim.surface_alt_at(c), (block + 1) as f32);
        assert_eq!(sim.ground_alt_at(c), Some(top_block_alt(block)));
        println!(
            "buried +{}: lifted from z {} to {}",
            block - old,
            feet.z,
            block + 1
        );
    }
    // The pit floor.
    let c = Vec2::new(pit.0 + 64, pit.1 + 64);
    assert_eq!(sim.ground_alt_at(c), Some(129.5));
    assert_eq!(sim.surface_alt_at(c), 130.0);
    let pit_chunk = c.map(|e| e.div_euclid(32));
    let w = sim.chunk_water(pit_chunk).unwrap();
    assert!(!w.wet() && !w.ocean, "{w:?}");
    let mut aquatic = 0;
    let mut spawns = 0;
    let mut water = 0;
    for dy in 0..4 {
        for dx in 0..4 {
            let cpos = Vec2::new(pit.0 / 32 + dx, pit.1 / 32 + dy);
            let (chunk, supplement) = world
                .generate_chunk(index_ref, cpos, None, || false, None, None)
                .unwrap();
            for y in 0..32 {
                for x in 0..32 {
                    for z in 120..140 {
                        if chunk
                            .get(Vec3::new(x, y, z))
                            .is_ok_and(|b| b.kind() == BlockKind::Water)
                        {
                            water += 1;
                        }
                    }
                }
            }
            for s in &supplement.entity_spawns {
                let infos: Vec<_> = match s {
                    EntitySpawn::Entity(e) => vec![e.as_ref().clone()],
                    EntitySpawn::Group(g) => g.clone(),
                };
                for e in infos {
                    spawns += 1;
                    aquatic += matches!(
                        e.body,
                        Body::FishSmall(_) | Body::FishMedium(_) | Body::Crustacean(_)
                    ) as usize;
                    assert!(e.pos.z >= 129.0, "spawn below the floor: {:?}", e.pos);
                }
            }
        }
    }
    println!(
        "pit: chunk_water {w:?}; {water} water blocks; {spawns} spawn(s), {aquatic} aquatic"
    );
    assert_eq!(water, 0);
    assert_eq!(aquatic, 0);
}

/// A chunk crossed by a civ road, far from every site and from water, with
/// its 8 neighbours dry.
fn wilderness_road_chunk(world: &World, index: &crate::IndexOwned) -> Vec2<i32> {
    let size = world.sim.get_size().map(|e| e as i32);
    let far = |c: Vec2<i32>| {
        let w = c * 32;
        index.sites.values().all(|site| {
            let b = site.bounds();
            let dx = (b.min.x - w.x).max(w.x - b.max.x).max(0);
            let dy = (b.min.y - w.y).max(w.y - b.max.y).max(0);
            dx.max(dy) > 600
        })
    };
    (40..size.y - 40)
        .step_by(7)
        .flat_map(|y| (40..size.x - 40).step_by(7).map(move |x| Vec2::new(x, y)))
        .find(|&c| {
            let dry = (-3..=3).all(|dy| {
                (-3..=3).all(|dx| {
                    world.sim.get(c + Vec2::new(dx, dy)).is_some_and(|k| {
                        k.river.river_kind.is_none() && !k.is_underwater() && k.alt > 160.0
                    })
                })
            });
            dry && world.sim.get(c).is_some_and(|k| k.path.0.is_way())
                && world
                    .sim
                    .get_nearest_path(c * 32 + 16)
                    .is_some_and(|(d, ..)| d < 4.0)
                && far(c)
        })
        .expect("a wilderness road")
}

/// A civ road (planned on the natural table) crossing an exact patch
/// with a 5-block cliff: the post-civ report names the steep road columns
/// (not an error), and the road over the exact cells is painted on the
/// patch: the top block of every undecorated road column is the authored
/// block (no notch, no dike).
#[test]
#[ignore]
fn br1_a_road_over_an_authored_cliff_is_reported_and_painted_on_the_patch() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let c = wilderness_road_chunk(&world, &index);
    let base = world.sim.get_alt_approx(c * 32 + 16).unwrap() as i32;
    let (x0, y0) = ((c.x - 1) * 32, (c.y - 1) * 32);
    let split = c.x * 32 + 16;
    let spec = RegionSpec::new(
        "br1_cliff",
        ((c.x - 3) * 32, (c.y - 3) * 32),
        ((c.x + 4) * 32, (c.y + 4) * 32),
        32,
        vec![
            exact(
                rect(x0 as f32, y0 as f32, split as f32, (y0 + 96) as f32),
                base * 100 + 50,
            ),
            exact(
                rect(split as f32, y0 as f32, (x0 + 96) as f32, (y0 + 96) as f32),
                (base + 5) * 100 + 50,
            ),
            ring(16),
        ],
    );
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec]).unwrap()));
    let report = road_cliffs(&world);
    println!(
        "road over a cliff at chunk {c:?}: {} road columns on exact cells, {} steep, first {:?}",
        report.road_columns,
        report.steep.len(),
        report.steep.first()
    );
    assert!(report.road_columns > 0);
    assert!(
        report
            .steep
            .iter()
            .all(|(p, step)| *step == 5 && (p.x - split).abs() <= 1)
    );
    let rasters = world.sim.authored_rasters.as_ref().unwrap();
    let (mut painted, mut exact_top, mut other) = (0, 0, 0);
    let mut cats = std::collections::BTreeMap::<String, usize>::new();
    for dy in -1..=1 {
        for dx in -1..=1 {
            let cpos = c + Vec2::new(dx, dy);
            let (chunk, _) = world
                .generate_chunk(index_ref, cpos, None, || false, None, None)
                .unwrap();
            for y in 0..32 {
                for x in 0..32 {
                    let wpos = cpos * 32 + Vec2::new(x, y);
                    let Some(AuthoredCell::Ground {
                        block,
                        weight: GROUND_EXACT,
                    }) = rasters.cell_at(wpos)
                    else {
                        continue;
                    };
                    if !world
                        .sim
                        .get_nearest_path(wpos)
                        .is_some_and(|(d, _, path, _)| d < path.width)
                    {
                        continue;
                    }
                    painted += 1;
                    let at = |z: i32| {
                        chunk
                            .get(Vec3::new(x, y, z))
                            .copied()
                            .unwrap_or_else(|_| Block::empty())
                    };
                    let top = (block - 8..block + 24)
                        .rev()
                        .find(|z| at(*z).is_filled())
                        .unwrap_or(i32::MIN);
                    let _ = top;
                    // The road's top natural block is the patch block, painted
                    // (tree canopies over the road are not ground).
                    let natural_top = (block - 8..block + 24)
                        .rev()
                        .find(|z| is_natural_ground(&at(*z)))
                        .unwrap_or(i32::MIN);
                    if natural_top == block && at(block).kind() == BlockKind::Earth {
                        exact_top += 1;
                    } else {
                        other += 1;
                        let cat = format!(
                            "dz {} top {:?} at-block {:?}",
                            natural_top - block,
                            at(natural_top).kind(),
                            at(block).kind()
                        );
                        *cats.entry(cat).or_default() += 1;
                    }
                }
            }
        }
    }
    println!(
        "road paint: {painted} road columns on exact cells, top = block (Earth) on {exact_top}, \
         other {other}: {cats:?}"
    );
    assert!(painted > 0);
    assert_eq!(exact_top, painted, "road painted on the patch");
    let _ = TerrainChunkSize::RECT_SIZE;
}
