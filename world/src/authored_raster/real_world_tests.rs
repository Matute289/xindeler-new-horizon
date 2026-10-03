//! Real-world checks of the authored water layer on the Cromatolis map.
//!
//! They need the real LFS map (`git lfs pull` against the VPS store), so they
//! are `#[ignore]`d like every other real-asset test in this crate:
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release \
//!     authored_raster::real_world_tests -- --ignored --nocapture
//! ```
//!
//! The synthetic region lives in the calibration arena on the empty plateau
//! around wpos (23264, 25312): no site, landmark, route or water within
//! 600 m, so nothing authored is touched.

use super::{
    AuthoredRasters, LayerKind,
    writer::{PaintOp, RegionSpec, Shape, build_region, manifest},
};
use crate::{
    World,
    sim::{FileOpts, WorldOpts},
    util::Sampler,
};
use common::{
    terrain::{Block, BlockKind, TerrainChunkSize},
    vol::{ReadVol, RectVolSize},
};
use rand::prelude::*;
use rayon::prelude::*;
use std::collections::HashMap;
use vek::*;

const REGION_MIN: (i32, i32) = (22752, 24576);
const REGION_MAX: (i32, i32) = (23936, 25600);
const PLATEAU_CM: i32 = 23_918;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape { Shape::Rect { x0, y0, x1, y1 } }

/// The Stage-1 water scenarios side by side in one region: a 20 m river, a
/// 4 m strip, a 100 m slot canyon (water 6 m over a floor 89 m below the
/// plateau, 2 m bank walls at plateau height), a closed lake and an island
/// with a 30 m strait. Surfaces are written as `block * 100 + 50`.
pub fn arena_spec() -> RegionSpec {
    let water = |shape, surface_block: i32, bed_block: i32| PaintOp::Water {
        shape,
        surface_cm: surface_block * 100 + 50,
        bed_cm: bed_block * 100 + 50,
    };
    RegionSpec {
        id: "arena_stage1".into(),
        min: REGION_MIN,
        max: REGION_MAX,
        feather_m: 32,
        ops: vec![
            // River, 20 m x 400 m, top water block 238, bed 232.
            water(rect(22850.0, 25400.0, 23250.0, 25420.0), 238, 232),
            // Strip, 4 m x 400 m.
            water(rect(22850.0, 25200.0, 23250.0, 25204.0), 238, 232),
            // Slot canyon floor + water, 100 m x 400 m: water top 156, bed 150.
            water(rect(23350.0, 24700.0, 23750.0, 24800.0), 156, 150),
            // Lake 128 x 200 m.
            water(
                Shape::Ellipse {
                    cx: 23500.0,
                    cy: 25300.0,
                    rx: 64.0,
                    ry: 100.0,
                },
                238,
                232,
            ),
            // Strait ring r 50..80 around an island.
            water(
                Shape::Annulus {
                    cx: 23050.0,
                    cy: 24850.0,
                    r_in: 50.0,
                    r_out: 80.0,
                },
                238,
                232,
            ),
            // Island crest 4 m above the plateau.
            PaintOp::Bank {
                shape: Shape::Ellipse {
                    cx: 23050.0,
                    cy: 24850.0,
                    rx: 50.0,
                    ry: 50.0,
                },
                bed_cm: PLATEAU_CM + 400,
            },
            // Banks: a 2 m ring at block 240 (about the plateau) around every
            // body; around the canyon that ring is a vertical 84 m wall.
            PaintOp::BankRing {
                width_m: 2,
                bed_cm: Some(PLATEAU_CM + 100),
            },
        ],
    }
}

pub fn generate_world() -> (World, crate::IndexOwned) {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    World::generate(
        0,
        WorldOpts {
            seed_elements: true,
            world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    )
}

pub fn load_spec(spec: &RegionSpec) -> AuthoredRasters {
    let built = vec![build_region(spec).unwrap()];
    let files: HashMap<(i32, i32), Vec<u8>> = built[0].tiles.iter().cloned().collect();
    let fetch = |_: &str, _: LayerKind, tx: i32, ty: i32| {
        files
            .get(&(tx, ty))
            .cloned()
            .ok_or_else(|| "missing".to_string())
    };
    AuthoredRasters::from_manifest(manifest(&built), Vec2::broadcast(32768), &fetch).unwrap()
}

/// Every numeric/colour output of a column, as bits.
fn column_bits(world: &World, index: crate::IndexRef, wpos: Vec2<i32>) -> Option<Vec<u32>> {
    let c = world.sample_columns().get((wpos, index, None))?;
    let mut v = vec![
        c.alt.to_bits(),
        c.riverless_alt.to_bits(),
        c.basement.to_bits(),
        c.chaos.to_bits(),
        c.water_level.to_bits(),
        c.warp_factor.to_bits(),
        c.tree_density.to_bits(),
        c.marble.to_bits(),
        c.rock_density.to_bits(),
        c.temp.to_bits(),
        c.humidity.to_bits(),
        c.spawn_rate.to_bits(),
        c.water_dist.map_or(u32::MAX, f32::to_bits),
        c.gradient.map_or(u32::MAX, f32::to_bits),
        c.cliff_offset.to_bits(),
        c.cliff_height.to_bits(),
        c.ice_depth.to_bits(),
        c.snow_cover as u32,
        c.surface_is_physical as u32,
        c.forest_kind as u32,
    ];
    v.extend(c.surface_color.iter().map(|e| e.to_bits()));
    v.extend(c.sub_surface_color.iter().map(|e| e.to_bits()));
    v.extend(c.water_vel.iter().map(|e| e.to_bits()));
    Some(v)
}

/// Digest of a generated chunk as block classes (air, natural ground,
/// liquid kind, other solid), the classes the terrain probe compares. Sprites
/// and the exact kind/colour of structure blocks are left out: chunk
/// generation rolls some of them from a fresh RNG every time.
fn chunk_digest(world: &World, index: crate::IndexRef, cpos: Vec2<i32>) -> u64 {
    use std::hash::{Hash, Hasher};
    let (chunk, _) = world
        .generate_chunk(index, cpos, None, || false, None, None)
        .unwrap();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let sz = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    for z in chunk.get_min_z()..chunk.get_max_z() {
        for y in 0..sz.y {
            for x in 0..sz.x {
                let b = chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty());
                let v = if b.is_liquid() || b.kind() == BlockKind::Lava {
                    0x100 + b.kind() as u32
                } else if b.is_filled() && is_natural_ground(&b) {
                    1
                } else if b.is_filled() {
                    2
                } else {
                    0
                };
                v.hash(&mut h);
            }
        }
    }
    chunk.get_min_z().hash(&mut h);
    h.finish()
}

/// Columns and chunks outside the region are bit-identical with and without
/// it, on the same generated world.
#[test]
#[ignore]
fn columns_and_chunks_outside_the_region_are_bit_identical() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let rasters = load_spec(&arena_spec());
    let map = world.sim.get_size().map(|e| e as i32) * 32;
    let rmin = Vec2::from(REGION_MIN);
    let rmax = Vec2::from(REGION_MAX);
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(161);
    // 10 000 random columns over the whole map, outside the region, plus a
    // ring of columns hugging its border from outside.
    let mut cols: Vec<Vec2<i32>> = Vec::new();
    while cols.len() < 10_000 {
        let p = Vec2::new(rng.random_range(0..map.x), rng.random_range(0..map.y));
        if !(p.x >= rmin.x && p.y >= rmin.y && p.x < rmax.x && p.y < rmax.y) {
            cols.push(p);
        }
    }
    for x in rmin.x - 2..rmax.x + 2 {
        cols.push(Vec2::new(x, rmin.y - 1));
        cols.push(Vec2::new(x, rmax.y));
    }
    for y in rmin.y..rmax.y {
        cols.push(Vec2::new(rmin.x - 1, y));
        cols.push(Vec2::new(rmax.x, y));
    }
    // Chunks: 10 000 random ones plus every chunk 2..=3 chunks outside the
    // box.
    let cmin = rmin / 32;
    let cmax = rmax / 32;
    let mut chunks: Vec<Vec2<i32>> = Vec::new();
    while chunks.len() < 10_000 {
        let c = Vec2::new(rng.random_range(2..1022), rng.random_range(2..1022));
        if !(c.x >= cmin.x - 3 && c.y >= cmin.y - 3 && c.x < cmax.x + 3 && c.y < cmax.y + 3) {
            chunks.push(c);
        }
    }
    let mut ring1: Vec<Vec2<i32>> = Vec::new();
    for cy in cmin.y - 3..cmax.y + 3 {
        for cx in cmin.x - 3..cmax.x + 3 {
            let d = (cmin.x - cx)
                .max(cx - (cmax.x - 1))
                .max(cmin.y - cy)
                .max(cy - (cmax.y - 1));
            match d {
                2 | 3 => chunks.push(Vec2::new(cx, cy)),
                1 => ring1.push(Vec2::new(cx, cy)),
                _ => {},
            }
        }
    }

    let sample = |world: &World| -> (Vec<Option<Vec<u32>>>, Vec<u64>, Vec<u64>) {
        (
            cols.par_iter()
                .map(|p| column_bits(world, index_ref, *p))
                .collect(),
            chunks
                .par_iter()
                .map(|c| chunk_digest(world, index_ref, *c))
                .collect(),
            ring1
                .par_iter()
                .map(|c| chunk_digest(world, index_ref, *c))
                .collect(),
        )
    };
    // Chunk generation itself is not fully deterministic even at class level
    // (cave decorations roll ores/plants from a fresh RNG and can replace a
    // wood block with an ore sprite; about 0.5 % of chunks), so the chunk
    // gate only uses chunks whose class digest agreed over three runs without
    // the region, and explains any remaining difference below.
    let (cols_a, chunks_a, ring_a) = sample(&world);
    let (_, chunks_a2, ring_a2) = sample(&world);
    let (_, chunks_a3, ring_a3) = sample(&world);
    let stable = |a: &[u64], b: &[u64], c: &[u64]| -> Vec<bool> {
        (0..a.len()).map(|i| a[i] == b[i] && a[i] == c[i]).collect()
    };
    let chunk_stable = stable(&chunks_a, &chunks_a2, &chunks_a3);
    let ring_stable = stable(&ring_a, &ring_a2, &ring_a3);
    world.sim.set_authored_rasters_for_test(Some(rasters));
    let (cols_b, chunks_b, ring_b) = sample(&world);
    let col_diff = cols_a.iter().zip(&cols_b).filter(|(a, b)| a != b).count();
    let differing = |a: &[u64], b: &[u64], stable: &[bool], pos: &[Vec2<i32>]| -> Vec<Vec2<i32>> {
        (0..a.len())
            .filter(|i| stable[*i] && a[*i] != b[*i])
            .map(|i| pos[i])
            .collect()
    };
    let chunk_diff = differing(&chunks_a, &chunks_b, &chunk_stable, &chunks);
    let ring_diff = differing(&ring_a, &ring_b, &ring_stable, &ring1);
    // A chunk that differs with the region may still be one whose generation
    // is merely rarely unstable: regenerate it without the region (up to 32
    // times) and accept it only if one of those runs reproduces the digest
    // seen with the region.
    let rasters = world.sim.authored_rasters.take();
    let explain = |diff: &[Vec2<i32>], with: &dyn Fn(Vec2<i32>) -> u64| -> Vec<Vec2<i32>> {
        diff.iter()
            .copied()
            .filter(|c| {
                let target = with(*c);
                !(0..32).any(|_| chunk_digest(&world, index_ref, *c) == target)
            })
            .collect()
    };
    let digest_with = |c: Vec2<i32>| -> u64 {
        let i = chunks
            .iter()
            .position(|p| *p == c)
            .map(|i| chunks_b[i])
            .or_else(|| ring1.iter().position(|p| *p == c).map(|i| ring_b[i]));
        i.expect("a compared chunk")
    };
    let chunk_unexplained = explain(&chunk_diff, &digest_with);
    let ring_unexplained = explain(&ring_diff, &digest_with);
    world.sim.set_authored_rasters_for_test(rasters);
    let n_stable = chunk_stable.iter().filter(|s| **s).count();
    let n_ring_stable = ring_stable.iter().filter(|s| **s).count();
    println!(
        "columns compared {}, differing {col_diff}; chunks compared {n_stable} stable of {} \
         (random + the 2..3-chunk ring), differing {chunk_diff:?} of which not reproduced by a \
         region-free run {chunk_unexplained:?}; 1-chunk ring {n_ring_stable} stable of {}, \
         differing {ring_diff:?}, not reproduced {ring_unexplained:?} (informational: layers \
         there read columns inside the region)",
        cols.len(),
        chunks.len(),
        ring1.len()
    );
    assert_eq!(col_diff, 0);
    assert!(chunk_unexplained.is_empty());
    assert!(
        n_stable * 10 >= chunks.len() * 9,
        "too few stable chunks to judge"
    );
    // The region really changed something inside, or this test proves nothing.
    let inside = Vec2::new(23300, 25410);
    assert_ne!(
        world.sim.authored_rasters.as_ref().unwrap().column(inside),
        None
    );
}

fn is_natural_ground(b: &Block) -> bool {
    matches!(
        b.kind(),
        BlockKind::Rock
            | BlockKind::WeakRock
            | BlockKind::Earth
            | BlockKind::Grass
            | BlockKind::Sand
            | BlockKind::Snow
    )
}

/// Inside the region the column sampler follows the raster exactly (wet:
/// ground and water level; bank: ground, plus at most one block of snow;
/// elsewhere: no water above sea level), and the generated blocks hold no
/// water above any authored surface or outside the authored footprint. On
/// wet columns whose water column no tree, boulder or sprite reaches, the
/// top water block and the bed are exactly the authored blocks.
#[test]
#[ignore]
fn authored_water_renders_exactly_in_the_arena() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let spec = arena_spec();
    world
        .sim
        .set_authored_rasters_for_test(Some(load_spec(&spec)));
    let rasters = world.sim.authored_rasters.as_ref().unwrap();
    let base_sea_level = crate::CONFIG.sea_level - 1.0 + 0.01;
    let cmin = Vec2::from(REGION_MIN) / 32;
    let cmax = Vec2::from(REGION_MAX) / 32;
    let chunks: Vec<Vec2<i32>> = (cmin.y..cmax.y)
        .flat_map(|cy| (cmin.x..cmax.x).map(move |cx| Vec2::new(cx, cy)))
        .collect();
    #[derive(Default, Debug)]
    struct Count {
        wet: u32,
        wet_sampler_exact: u32,
        wet_water_above_surface: u32,
        wet_clear: u32,
        wet_clear_exact: u32,
        bank: u32,
        bank_sampler_exact: u32,
        bank_water: u32,
        none: u32,
        none_sampler_dry: u32,
        none_water: u32,
    }
    let t = std::time::Instant::now();
    let counts: Vec<(Count, Vec<String>)> = chunks
        .par_iter()
        .map(|c| {
            let (chunk, _) = world
                .generate_chunk(index_ref, *c, None, || false, None, None)
                .unwrap();
            let mut n = Count::default();
            let mut ex = Vec::new();
            let block = |x: i32, y: i32, z: i32| {
                chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty())
            };
            for y in 0..32 {
                for x in 0..32 {
                    let wpos = c * 32 + Vec2::new(x, y);
                    let col = world.sample_columns().get((wpos, index_ref, None)).unwrap();
                    let water_above = |z0: i32| {
                        (z0..chunk.get_max_z()).any(|z| block(x, y, z).kind() == BlockKind::Water)
                    };
                    match rasters.column(wpos).unwrap().cell {
                        super::AuthoredCell::Wet {
                            surface_block,
                            bed_block,
                        } => {
                            n.wet += 1;
                            n.wet_sampler_exact += (col.alt == bed_block as f32 + 0.5
                                && col.water_level == surface_block as f32 + 0.5)
                                as u32;
                            let above = water_above(surface_block + 1);
                            n.wet_water_above_surface += above as u32;
                            let clear = (bed_block + 1..=surface_block + 1).all(|z| {
                                let b = block(x, y, z);
                                b.kind() == BlockKind::Water || (z > surface_block && b.is_air())
                            });
                            if clear {
                                n.wet_clear += 1;
                                let exact = is_natural_ground(&block(x, y, bed_block));
                                n.wet_clear_exact += exact as u32;
                                if !exact && ex.len() < 4 {
                                    ex.push(format!("wet {wpos:?}: bed block not ground"));
                                }
                            }
                        },
                        super::AuthoredCell::Bank { bed_block } => {
                            n.bank += 1;
                            n.bank_sampler_exact += (col.alt >= bed_block as f32 + 0.5
                                && col.alt <= bed_block as f32 + 1.5
                                && col.water_level == base_sea_level)
                                as u32;
                            let w = water_above(super::SEA_TOP_BLOCK);
                            n.bank_water += w as u32;
                            if w && ex.len() < 4 {
                                ex.push(format!("bank {wpos:?}: water"));
                            }
                        },
                        super::AuthoredCell::None => {
                            n.none += 1;
                            n.none_sampler_dry += (col.water_level == base_sea_level) as u32;
                            let w = water_above(super::SEA_TOP_BLOCK);
                            n.none_water += w as u32;
                            if w && ex.len() < 4 {
                                ex.push(format!("none {wpos:?}: water"));
                            }
                        },
                    }
                }
            }
            (n, ex)
        })
        .collect();
    let mut n = Count::default();
    let mut examples = Vec::new();
    for (c, ex) in counts {
        n.wet += c.wet;
        n.wet_sampler_exact += c.wet_sampler_exact;
        n.wet_water_above_surface += c.wet_water_above_surface;
        n.wet_clear += c.wet_clear;
        n.wet_clear_exact += c.wet_clear_exact;
        n.bank += c.bank;
        n.bank_sampler_exact += c.bank_sampler_exact;
        n.bank_water += c.bank_water;
        n.none += c.none;
        n.none_sampler_dry += c.none_sampler_dry;
        n.none_water += c.none_water;
        examples.extend(ex);
    }
    println!(
        "{} chunks in {:?}: {n:#?}; examples {examples:#?}",
        chunks.len(),
        t.elapsed()
    );
    assert!(
        n.wet > 50_000,
        "the scenarios produced {} wet columns",
        n.wet
    );
    assert_eq!(n.wet_sampler_exact, n.wet);
    assert_eq!(n.bank_sampler_exact, n.bank);
    assert_eq!(n.none_sampler_dry, n.none);
    assert_eq!(n.wet_water_above_surface, 0);
    assert_eq!(n.bank_water, 0);
    assert_eq!(n.none_water, 0);
    assert_eq!(n.wet_clear_exact, n.wet_clear);
    assert!(
        n.wet_clear as f64 >= 0.95 * n.wet as f64,
        "only {} of {} wet columns are free of trees/boulders",
        n.wet_clear,
        n.wet
    );
}

/// Generation cost of the chunks of the arena region with and without the
/// region (every chunk of the request inside a region: the worst case).
/// Single-threaded and interleaved so both sides see the same machine state;
/// prints p50/p99 per chunk and fails only on a gross regression.
#[test]
#[ignore]
fn authored_region_generation_cost() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let rasters = load_spec(&arena_spec());
    let cmin = Vec2::from(REGION_MIN) / 32;
    let cmax = Vec2::from(REGION_MAX) / 32;
    let chunks: Vec<Vec2<i32>> = (cmin.y..cmax.y)
        .flat_map(|cy| (cmin.x..cmax.x).map(move |cx| Vec2::new(cx, cy)))
        .collect();
    let time = |world: &World, c: Vec2<i32>| {
        let t = std::time::Instant::now();
        let _ = world
            .generate_chunk(index_ref, c, None, || false, None, None)
            .unwrap();
        t.elapsed().as_secs_f64() * 1e3
    };
    // Warm up both paths (tile decode, distance fields, caches).
    world.sim.set_authored_rasters_for_test(Some(rasters));
    for c in &chunks {
        time(&world, *c);
    }
    let rasters = world.sim.authored_rasters.take();
    let (mut without, mut with) = (Vec::new(), Vec::new());
    let mut rasters = rasters;
    for round in 0..2 {
        for c in &chunks {
            for side in [round % 2 == 0, round % 2 != 0] {
                if side {
                    world.sim.set_authored_rasters_for_test(rasters.take());
                    with.push(time(&world, *c));
                    rasters = world.sim.authored_rasters.take();
                } else {
                    without.push(time(&world, *c));
                }
            }
        }
    }
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * p) as usize]
    };
    let (w50, w99) = (pct(&mut without, 0.5), pct(&mut without, 0.99));
    let (r50, r99) = (pct(&mut with, 0.5), pct(&mut with, 0.99));
    println!(
        "{} chunks x 2 rounds, ms per chunk: without region p50 {w50:.2} p99 {w99:.2}; with \
         region p50 {r50:.2} p99 {r99:.2} ({:+.1} % / {:+.1} %)",
        chunks.len(),
        (r50 / w50 - 1.0) * 100.0,
        (r99 / w99 - 1.0) * 100.0
    );
    assert!(r50 < w50 * 1.25 && r99 < w99 * 1.25);
}
