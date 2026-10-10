//! Real-world checks of the authored ground layer's blend ring and seams
//! (Stage 2b) on the Cromatolis map: B-T7 (the ring between an exact patch
//! and the engine terrain), B-S1 (exact dunes), B-T8 (an exact patch beside a
//! natural river in a partial chunk), B-T14 (d) (a dry pit beside the sea,
//! with and without a dyke), and identity gate 3 for the ring. Synthetic
//! manifests only: the calibration arena, or a wilderness river / open sea
//! found far from every site.
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release \
//!     authored_raster::ground_seam_real_world_tests -- --ignored --nocapture
//! ```

use super::{
    AuthoredCell, SEA_TOP_BLOCK, SeaFill,
    format::GROUND_EXACT,
    queries::{check_ground_seams, ground_seams},
    real_world_tests::{
        PLATEAU_CM, REGION_MAX, REGION_MIN, assert_identical_outside, column_bits, generate_world,
        is_natural_ground, load_specs,
    },
    top_block_alt,
    writer::{PaintOp, RegionSpec, Shape},
};
use crate::{World, util::Sampler};
use common::{
    terrain::{Block, BlockKind},
    vol::ReadVol,
};
use rayon::prelude::*;
use std::collections::HashMap;
use vek::*;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape { Shape::Rect { x0, y0, x1, y1 } }

fn exact(shape: Shape, ground_cm: i32) -> PaintOp {
    PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    }
}

const fn floor32(v: i32) -> i32 { v.div_euclid(32) * 32 }
const fn ceil32(v: i32) -> i32 { (v + 31).div_euclid(32) * 32 }

/// B-T7: a 96 x 96 m exact plateau per ring width (x of its west edge), at
/// [`PATCH_Y`], each in its own region of the arena.
const RINGS: [(i32, i32); 4] = [(8, 22850), (16, 23100), (32, 23400), (64, 23700)];
const PATCH_Y: i32 = 24700;
const PATCH: i32 = 96;
/// The plateau stands this many blocks above the engine's terrain around it.
const STEP_BLOCKS: i32 = 7;

/// The region of one B-T7 patch: the plateau at `ground_cm` and its ring
/// (the master continuing the plateau flat, as `make_exact` over a flat
/// master gives), in a chunk-aligned box that keeps the 16 m margin.
fn ring_spec(width: i32, x0: i32, ground_cm: i32) -> RegionSpec {
    let pad = width + 16;
    RegionSpec::new(
        format!("bt7_ring_{width}"),
        (floor32(x0 - pad), floor32(PATCH_Y - pad)),
        (ceil32(x0 + PATCH + pad), ceil32(PATCH_Y + PATCH + pad)),
        32,
        vec![
            exact(
                rect(
                    x0 as f32,
                    PATCH_Y as f32,
                    (x0 + PATCH) as f32,
                    (PATCH_Y + PATCH) as f32,
                ),
                ground_cm,
            ),
            PaintOp::GroundRing {
                width_m: width,
                ground_cm: None,
            },
        ],
    )
}

/// B-S1 at S2: exact dunes 2 m high (amplitude) with a 20 m wavelength along
/// x, one 1 m strip per column, on the arena plateau.
const DUNES: (i32, i32, i32, i32) = (22900, 25120, 23300, 25310);

fn dune_cm(x: i32) -> i32 {
    let phase = (x as f64 + 0.5) / 20.0 * std::f64::consts::TAU;
    PLATEAU_CM + (200.0 * phase.sin()).round() as i32
}

fn dunes_spec() -> RegionSpec {
    let (x0, y0, x1, y1) = DUNES;
    RegionSpec::new(
        "bs1_dunes",
        (floor32(x0 - 16), floor32(y0 - 16)),
        (ceil32(x1 + 16), ceil32(y1 + 16)),
        32,
        (x0..x1)
            .map(|x| {
                exact(
                    rect(x as f32, y0 as f32, (x + 1) as f32, y1 as f32),
                    dune_cm(x),
                )
            })
            .collect(),
    )
}

/// Top solid block of a sampled column (`block.rs`: `z as i32 <= alt as i32`).
fn top(world: &World, index: crate::IndexRef, p: Vec2<i32>) -> i32 {
    world.sample_columns().get((p, index, None)).unwrap().alt as i32
}

/// The plateau height of B-T7: [`STEP_BLOCKS`] above the median engine block
/// on the four rings' outer edges (no manifest loaded).
fn bt7_ground_cm(world: &World, index: crate::IndexRef) -> i32 {
    let mut tops: Vec<i32> = RINGS
        .iter()
        .flat_map(|&(w, x0)| {
            (0..PATCH).step_by(4).flat_map(move |o| {
                [
                    Vec2::new(x0 - w, PATCH_Y + o),
                    Vec2::new(x0 + PATCH - 1 + w, PATCH_Y + o),
                    Vec2::new(x0 + o, PATCH_Y - w),
                    Vec2::new(x0 + o, PATCH_Y + PATCH - 1 + w),
                ]
            })
        })
        .collect::<Vec<_>>()
        .par_iter()
        .map(|p| top(world, index, *p))
        .collect();
    tops.sort_unstable();
    (tops[tops.len() / 2] + STEP_BLOCKS) * 100 + 50
}

/// One B-T7 transect: from the last exact column (`k = 0`) straight out
/// through the ring to the first engine column (`k = width`).
struct Transect {
    blocks: Vec<i32>,
    engine_outer: i32,
}

fn transects(x0: i32) -> Vec<(Vec2<i32>, Vec2<i32>)> {
    let mut v = Vec::new();
    // Along every edge, away from the corners (where the ring is a disc).
    for o in (8..PATCH - 8).step_by(2) {
        v.push((Vec2::new(x0 + PATCH - 1, PATCH_Y + o), Vec2::new(1, 0)));
        v.push((Vec2::new(x0, PATCH_Y + o), Vec2::new(-1, 0)));
        v.push((Vec2::new(x0 + o, PATCH_Y + PATCH - 1), Vec2::new(0, 1)));
        v.push((Vec2::new(x0 + o, PATCH_Y), Vec2::new(0, -1)));
    }
    v
}

#[derive(Debug, Default)]
struct RingRow {
    transects: usize,
    /// The top block never rises from the plateau edge to the engine
    /// terrain (the plateau stands above it).
    monotone: usize,
    /// Largest step between adjacent columns, edge to engine.
    max_step: i32,
    /// Transects whose largest step is within `ceil(delta / width) + 1`
    /// blocks (`delta` = plateau block - engine block at the outer edge).
    within_bound: usize,
    worst: Option<String>,
    exact_edge: usize,
}

/// B-T7, B-S1 and identity gate 3 for the ring: rings of 8, 16, 32 and 64 m
/// around a plateau 7 blocks above the engine's terrain are monotone and
/// never step more than `ceil(7 / ring) + 1` blocks between adjacent
/// columns; the exact cells stay exact; exact dunes keep their amplitude
/// exactly.
#[test]
#[ignore]
fn blend_ring_meets_the_engine_terrain_within_the_step_bound() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let h_cm = bt7_ground_cm(&world, index_ref);
    let h = h_cm.div_euclid(100);
    let paths: Vec<(i32, i32, Vec<(Vec2<i32>, Vec2<i32>)>)> = RINGS
        .iter()
        .map(|&(w, x0)| (w, x0, transects(x0)))
        .collect();
    let line = |(start, dir): (Vec2<i32>, Vec2<i32>), w: i32| -> Vec<Vec2<i32>> {
        (0..=w).map(|k| start + dir * k).collect()
    };
    let engine: HashMap<i32, Vec<Vec<i32>>> = paths
        .iter()
        .map(|(w, _, ts)| {
            (
                *w,
                ts.par_iter()
                    .map(|t| {
                        line(*t, *w)
                            .iter()
                            .map(|p| top(&world, index_ref, *p))
                            .collect()
                    })
                    .collect(),
            )
        })
        .collect();
    let mut specs: Vec<RegionSpec> = RINGS
        .iter()
        .map(|&(w, x0)| ring_spec(w, x0, h_cm))
        .collect();
    specs.push(dunes_spec());
    let rasters = load_specs(&specs).expect("the B-T7 / B-S1 regions load");
    rasters
        .check_consistency_with_sim("test", &world.sim)
        .expect("the battery passes the sim-table checks");
    world.sim.set_authored_rasters_for_test(Some(rasters));
    let mut table = std::collections::BTreeMap::new();
    for (w, _, ts) in &paths {
        let mut row = RingRow::default();
        let measured: Vec<Transect> = ts
            .par_iter()
            .zip(&engine[w])
            .map(|(t, eng)| Transect {
                blocks: line(*t, *w)
                    .iter()
                    .map(|p| top(&world, index_ref, *p))
                    .collect(),
                engine_outer: eng[*w as usize],
            })
            .collect();
        for (t, path) in measured.iter().zip(ts) {
            row.transects += 1;
            row.exact_edge += (t.blocks[0] == h) as usize;
            row.monotone += t.blocks.windows(2).all(|p| p[1] <= p[0]) as usize;
            let step = t
                .blocks
                .windows(2)
                .map(|p| (p[1] - p[0]).abs())
                .max()
                .unwrap();
            let delta = (h - t.engine_outer).abs();
            let bound = (delta + w - 1) / w + 1;
            row.within_bound += (step <= bound) as usize;
            if step > row.max_step {
                row.max_step = step;
                row.worst = Some(format!(
                    "{:?} dir {:?}: blocks {:?} (engine outer {}, bound {bound})",
                    path.0, path.1, t.blocks, t.engine_outer
                ));
            }
        }
        table.insert(*w, row);
    }
    // B-S1: exact dunes, sampler and generated blocks.
    let (x0, y0, x1, y1) = DUNES;
    let dune_cols: Vec<Vec2<i32>> = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| Vec2::new(x, y)))
        .collect();
    let dune_sampler_exact = dune_cols
        .par_iter()
        .filter(|p| {
            let c = world.sample_columns().get((**p, index_ref, None)).unwrap();
            c.alt == top_block_alt(dune_cm(p.x).div_euclid(100)) && c.warp_factor == 0.0
        })
        .count();
    let dune_chunks: Vec<Vec2<i32>> = {
        let mut v: Vec<Vec2<i32>> = dune_cols
            .iter()
            .map(|p| p.map(|e| e.div_euclid(32)))
            .collect();
        v.sort_unstable_by_key(|c| (c.y, c.x));
        v.dedup();
        v
    };
    let (dune_undecorated, dune_top_exact) = dune_chunks
        .par_iter()
        .map(|c| {
            let (chunk, _) = world
                .generate_chunk(index_ref, *c, None, || false, None, None)
                .unwrap();
            let block = |x: i32, y: i32, z: i32| {
                chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty())
            };
            let (z0, z1) = (chunk.get_min_z(), chunk.get_max_z());
            let (mut undecorated, mut exact_top) = (0usize, 0usize);
            for y in 0..32 {
                for x in 0..32 {
                    let w = c * 32 + Vec2::new(x, y);
                    if !(x0..x1).contains(&w.x) || !(y0..y1).contains(&w.y) {
                        continue;
                    }
                    let b = dune_cm(w.x).div_euclid(100);
                    let decorated = (z0..=b.min(z1 - 1)).any(|z| {
                        let k = block(x, y, z);
                        (k.is_filled()
                            && !k.is_liquid()
                            && !is_natural_ground(&k)
                            && k.kind() != BlockKind::Ice)
                            || k.get_sprite()
                                .is_some_and(|s| s != common::terrain::SpriteKind::Empty)
                    });
                    if decorated {
                        continue;
                    }
                    undecorated += 1;
                    let t = (z0..z1).rev().find(|z| is_natural_ground(&block(x, y, *z)));
                    exact_top += (t == Some(b)) as usize;
                }
            }
            (undecorated, exact_top)
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    println!(
        "B-T7 plateau block {h} (+{STEP_BLOCKS} over the engine's median): {table:#?}\nB-S1 \
         dunes: {} columns, sampler exact {dune_sampler_exact}, undecorated {dune_undecorated}, \
         top exact {dune_top_exact}",
        dune_cols.len()
    );
    for (w, row) in &table {
        assert_eq!(row.exact_edge, row.transects, "ring {w}: exact edge moved");
        assert_eq!(
            row.within_bound, row.transects,
            "ring {w}: a step over ceil(delta / ring) + 1: {:?}",
            row.worst
        );
        assert_eq!(row.monotone, row.transects, "ring {w}: not monotone");
    }
    assert_eq!(
        dune_sampler_exact,
        dune_cols.len(),
        "B-S1: sampler not exact"
    );
    assert!(dune_undecorated > dune_cols.len() * 9 / 10);
    // Carving after the sampler (cave mouths) is the only allowed deviation.
    assert!(
        dune_top_exact * 1000 >= dune_undecorated * 999,
        "B-S1: {dune_top_exact} of {dune_undecorated} undecorated tops exact"
    );
}

/// Identity gate 3 with the blend ring: the B-T7 and B-S1 regions leave
/// every column and chunk outside the arena box bit-identical.
#[test]
#[ignore]
fn blend_rings_leave_everything_outside_their_boxes_identical() {
    let mut specs: Vec<RegionSpec> = RINGS
        .iter()
        .map(|&(w, x0)| ring_spec(w, x0, PLATEAU_CM + STEP_BLOCKS * 100))
        .collect();
    specs.push(dunes_spec());
    let (w, x0) = RINGS[2];
    assert_identical_outside(
        load_specs(&specs).unwrap(),
        REGION_MIN,
        REGION_MAX,
        Vec2::new(x0 + PATCH + w / 2, PATCH_Y + PATCH / 2),
    );
}

/// Sites farther than `margin` from chunk `c` (wpos).
fn far_from_sites(index: &crate::IndexOwned, c: Vec2<i32>, margin: i32) -> bool {
    let w = c * 32;
    index.sites.values().all(|site| {
        let b = site.bounds();
        let dx = (b.min.x - w.x).max(w.x - b.max.x).max(0);
        let dy = (b.min.y - w.y).max(w.y - b.max.y).max(0);
        dx.max(dy) > margin
    })
}

/// Natural `(alt, water_level)` of the columns of chunks `c - 1 ..= c + 1`.
fn natural_columns(
    world: &World,
    index: crate::IndexRef,
    c: Vec2<i32>,
) -> HashMap<Vec2<i32>, (f32, f32)> {
    let ps: Vec<Vec2<i32>> = ((c.y - 1) * 32..(c.y + 2) * 32)
        .flat_map(|y| ((c.x - 1) * 32..(c.x + 2) * 32).map(move |x| Vec2::new(x, y)))
        .collect();
    ps.par_iter()
        .map(|p| {
            let s = world.sample_columns().get((*p, index, None)).unwrap();
            (*p, (s.alt, s.water_level))
        })
        .collect()
}

/// B-T8: a 3 x 3 m exact patch beside a natural river, in a partial chunk.
/// At (or above) the river's top water block it loads, the river columns
/// stay bit-identical and the patch holds the water; 2 blocks below it the
/// post-civ seam check finds the water walls and refuses it, naming them.
#[test]
#[ignore]
fn exact_patch_beside_a_natural_river_in_a_partial_chunk() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    // Wilderness river chunks, far from every site, until one has a 3 x 3
    // block of dry columns whose outer edge touches its water.
    let candidates: Vec<Vec2<i32>> = (40..980)
        .step_by(7)
        .flat_map(|y| (40..980).step_by(7).map(move |x| Vec2::new(x, y)))
        .filter(|c| world.sim.get(*c).is_some_and(|c| c.river.is_river()))
        .filter(|c| far_from_sites(&index, *c, 1000))
        .collect();
    let wet = |m: &HashMap<Vec2<i32>, (f32, f32)>, p: Vec2<i32>| {
        m.get(&p).is_some_and(|&(alt, level)| level > alt + 1.0)
    };
    let found = candidates.iter().take(40).find_map(|c| {
        let m = natural_columns(&world, index_ref, *c);
        (c.y * 32 + 2..c.y * 32 + 28)
            .flat_map(|y| (c.x * 32 + 2..c.x * 32 + 28).map(move |x| Vec2::new(x, y)))
            .find(|p| {
                let inner = (0..3).all(|j| (0..3).all(|i| !wet(&m, p + Vec2::new(i, j))));
                let edge = (0..3).any(|k| {
                    [
                        Vec2::new(-1, k),
                        Vec2::new(3, k),
                        Vec2::new(k, -1),
                        Vec2::new(k, 3),
                    ]
                    .iter()
                    .any(|d| wet(&m, p + *d))
                });
                inner && edge
            })
            .map(|p| (*c, p, m))
    });
    let (river, p, natural) = found.expect("a wilderness river edge");
    // The river's top water block beside the patch.
    let water_top = (0..3)
        .flat_map(|k| {
            [
                Vec2::new(-1, k),
                Vec2::new(3, k),
                Vec2::new(k, -1),
                Vec2::new(k, 3),
            ]
        })
        .filter_map(|d| {
            let (alt, level) = natural[&(p + d)];
            (level > alt + 1.0).then(|| level.ceil() as i32 - 1)
        })
        .max()
        .unwrap();
    let spec = |block: i32| {
        let mut s = RegionSpec::new(
            "bt8_river_edge",
            ((river - 2) * 32).into_tuple(),
            ((river + 3) * 32).into_tuple(),
            16,
            vec![exact(
                rect(p.x as f32, p.y as f32, (p.x + 3) as f32, (p.y + 3) as f32),
                block * 100 + 50,
            )],
        );
        s.allow_partial = true;
        s
    };
    let box_cols: Vec<Vec2<i32>> = ((river.y - 2) * 32..(river.y + 3) * 32)
        .flat_map(|y| ((river.x - 2) * 32..(river.x + 3) * 32).map(move |x| Vec2::new(x, y)))
        .filter(|q| !(q.x >= p.x && q.y >= p.y && q.x < p.x + 3 && q.y < p.y + 3))
        .collect();
    let before: Vec<_> = box_cols
        .par_iter()
        .map(|q| column_bits(&world, index_ref, *q))
        .collect();
    // At the water's top block: loads, the river is untouched.
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(water_top)]).unwrap()));
    let ok = ground_seams(&world, index_ref, None);
    let after: Vec<_> = box_cols
        .par_iter()
        .map(|q| column_bits(&world, index_ref, *q))
        .collect();
    let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
    let patch_tops: Vec<i32> = (0..9)
        .map(|k| top(&world, index_ref, p + Vec2::new(k % 3, k / 3)))
        .collect();
    println!(
        "B-T8 river chunk {river:?}, patch {p:?}, water top block {water_top}: seam pairs {}, \
         walls {}, patch tops {patch_tops:?}, other columns of the box changed: {changed} of {}",
        ok.pairs,
        ok.walls.len(),
        box_cols.len()
    );
    assert!(ok.pairs >= 1, "the patch meets the natural map");
    assert!(ok.walls.is_empty(), "{:?}", ok.walls);
    assert!(
        check_ground_seams(&world, index_ref, None).is_ok(),
        "loads at the water's top block"
    );
    assert_eq!(
        changed, 0,
        "river and every other natural column bit-identical"
    );
    assert!(patch_tops.iter().all(|t| *t == water_top), "patch exact");
    // Two blocks below the water: refused, the walls named.
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(water_top - 2)]).unwrap()));
    let low = ground_seams(&world, index_ref, None);
    let err = check_ground_seams(&world, index_ref, None).unwrap_err();
    println!(
        "B-T8 refusal: {} wall(s), message: {}",
        low.walls.len(),
        err.0
    );
    assert!(!low.walls.is_empty());
    assert!(low.walls.iter().all(|w| w.water_top > w.ground_top));
    assert!(err.0.contains("would stand as a wall"), "{err}");
    assert!(
        err.0.contains(&format!("{:?}", low.walls[0].ground)),
        "{err}"
    );
}

/// B-T14 (d): a dry pit (sea_fill: AuthoredOnly, floor at block 130, 10 m
/// below sea level) dug into open sea far from sites, in partial chunks. Its
/// cells meet the natural sea directly: refused at the seam check. With a 2
/// m dyke of exact ground at block 141 around it the sea meets the dyke
/// (higher than the sea's top block 139): it loads, and the pit floor is dry
/// and exact in the generated blocks.
#[test]
#[ignore]
fn a_dry_pit_beside_the_sea_needs_a_dyke() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let sea = (40..980)
        .step_by(5)
        .flat_map(|y| (40..980).step_by(5).map(move |x| Vec2::new(x, y)))
        .find(|c| {
            (-3..=3).all(|dy| {
                (-3..=3).all(|dx| {
                    world
                        .sim
                        .get(*c + Vec2::new(dx, dy))
                        .is_some_and(|n| n.river.is_ocean() && n.alt < 100.0)
                })
            }) && far_from_sites(&index, *c, 600)
        })
        .expect("open sea far from sites");
    let o = sea * 32;
    let pit = rect(
        (o.x + 8) as f32,
        (o.y + 8) as f32,
        (o.x + 24) as f32,
        (o.y + 24) as f32,
    );
    let spec = |dyke: bool| {
        let mut ops = Vec::new();
        if dyke {
            ops.push(exact(
                rect(
                    (o.x + 6) as f32,
                    (o.y + 6) as f32,
                    (o.x + 26) as f32,
                    (o.y + 26) as f32,
                ),
                14_150,
            ));
        }
        ops.push(exact(pit.clone(), 13_050));
        let mut s = RegionSpec::new(
            "bt14d_pit",
            ((sea - 2) * 32).into_tuple(),
            ((sea + 3) * 32).into_tuple(),
            16,
            ops,
        );
        s.allow_partial = true;
        s.sea_fill = SeaFill::AuthoredOnly;
        s
    };
    // No dyke: the pit's edge cells meet the sea.
    let open = load_specs(&[spec(false)]).unwrap();
    open.check_consistency_with_sim("test", &world.sim)
        .expect("load-time rules pass (the wall needs the sampler)");
    world.sim.set_authored_rasters_for_test(Some(open));
    let walls = ground_seams(&world, index_ref, None);
    let err = check_ground_seams(&world, index_ref, None).unwrap_err();
    println!(
        "B-T14d sea chunk {sea:?}, no dyke: {} pairs, {} walls: {}",
        walls.pairs,
        walls.walls.len(),
        err.0
    );
    assert_eq!(walls.walls.len(), walls.pairs, "every pit edge is a wall");
    assert!(
        walls
            .walls
            .iter()
            .all(|w| w.water_top == SEA_TOP_BLOCK && w.ground_top == 130)
    );
    // With the dyke: loads; the pit floor is dry and exact.
    let diked = load_specs(&[spec(true)]).unwrap();
    diked
        .check_consistency_with_sim("test", &world.sim)
        .expect("load-time rules pass");
    world.sim.set_authored_rasters_for_test(Some(diked));
    let ok = check_ground_seams(&world, index_ref, None).expect("a dyke holds the sea");
    let (chunk, _) = world
        .generate_chunk(index_ref, sea, None, || false, None, None)
        .unwrap();
    let (z0, z1) = (chunk.get_min_z(), chunk.get_max_z());
    let mut floor_exact = 0;
    let mut water_in_pit = 0;
    let mut dyke_exact = 0;
    for y in 6..26 {
        for x in 6..26 {
            let b = |z| {
                chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty())
            };
            let in_pit = (8..24).contains(&x) && (8..24).contains(&y);
            let t = (z0..z1).rev().find(|z| is_natural_ground(&b(*z)));
            if in_pit {
                floor_exact += (t == Some(130)) as u32;
                water_in_pit += (z0..z1).any(|z| b(z).kind() == BlockKind::Water) as u32;
            } else {
                dyke_exact += (t == Some(141)) as u32;
            }
        }
    }
    println!(
        "B-T14d with the dyke: {} pairs, walls {}; pit floor exact {floor_exact}/256, water \
         columns in the pit {water_in_pit}, dyke tops exact {dyke_exact}/144",
        ok.pairs,
        ok.walls.len()
    );
    assert!(ok.pairs > 0 && ok.walls.is_empty());
    assert_eq!(water_in_pit, 0, "the pit is dry");
    assert!(floor_exact >= 250, "pit floor exact ({floor_exact}/256)");
    assert!(dyke_exact >= 140, "dyke exact ({dyke_exact}/144)");
    assert!(matches!(
        world.sim.authored_cell_at(o + 16),
        Some(AuthoredCell::Ground { block: 130, .. })
    ));
}
