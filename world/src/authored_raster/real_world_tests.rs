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

pub(super) const REGION_MIN: (i32, i32) = (22752, 24576);
pub(super) const REGION_MAX: (i32, i32) = (23936, 25600);
pub(super) const PLATEAU_CM: i32 = 23_918;

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
    RegionSpec::new("arena_stage1", REGION_MIN, REGION_MAX, 32, vec![
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
    ])
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
    load_specs(std::slice::from_ref(spec)).unwrap()
}

/// Build and load several regions (every layer) through the real loader.
pub fn load_specs(specs: &[RegionSpec]) -> Result<AuthoredRasters, super::LoadError> {
    let built: Vec<_> = specs.iter().map(|s| build_region(s).unwrap()).collect();
    let fetch = |id: &str, layer: LayerKind, tx: i32, ty: i32| {
        built
            .iter()
            .find(|r| r.manifest.id == id)
            .and_then(|r| r.tile(layer, tx, ty))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "missing".to_string())
    };
    AuthoredRasters::from_manifest(manifest(&built), Vec2::broadcast(32768), "test", &fetch)
}

/// Every numeric/colour output of a column, as bits.
pub(super) fn column_bits(
    world: &World,
    index: crate::IndexRef,
    wpos: Vec2<i32>,
) -> Option<Vec<u32>> {
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

/// Digest of a generated chunk, block by block, as [`block_class`]es: air,
/// natural ground, each liquid kind, and "decoration" (any other solid block
/// or any sprite). Chunk generation's dynamic RNG and structures' weighted
/// block choices are seeded from the chunk position
/// (`crate::with_deterministic_dynamic_rng`, `crate::choice_rng`). Which
/// decoration kind/colour a tree or structure block gets still partly comes
/// from unseeded RNGs, so decoration is compared by presence, not kind:
/// terrain, water, ground and where every structure/sprite stands are compared
/// exactly, and two region-free runs agree (asserted).
fn chunk_digest(world: &World, index: crate::IndexRef, cpos: Vec2<i32>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut seed = [0u8; 32];
    seed[..4].copy_from_slice(&cpos.x.to_le_bytes());
    seed[4..8].copy_from_slice(&cpos.y.to_le_bytes());
    let (chunk, _) = crate::with_deterministic_dynamic_rng(seed, || {
        world
            .generate_chunk(index, cpos, None, || false, None, None)
            .unwrap()
    });
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let sz = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    chunk.get_min_z().hash(&mut h);
    for z in chunk.get_min_z()..chunk.get_max_z() {
        for y in 0..sz.y {
            for x in 0..sz.x {
                let b = chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty());
                block_class(&b).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Columns and chunks outside the region are bit-identical with and without
/// it, on the same generated world: every `ColumnSample` field of 10 000
/// random columns plus the whole ring around the box, and every block of
/// 10 000 random chunks plus every chunk 1..=3 chunks outside the box.
#[test]
#[ignore]
fn columns_and_chunks_outside_the_region_are_bit_identical() {
    assert_identical_outside(
        load_spec(&arena_spec()),
        REGION_MIN,
        REGION_MAX,
        Vec2::new(23000, 25410),
    );
}

/// Identity gate 3 for the ground layer: the Stage-2 battery regions (Auto
/// and AuthoredOnly, every scenario) leave every column and chunk outside
/// their boxes bit-identical.
#[test]
#[ignore]
fn ground_layer_leaves_everything_outside_its_boxes_identical() {
    assert_identical_outside(
        load_specs(&ground_battery_specs()).unwrap(),
        REGION_MIN,
        REGION_MAX,
        Vec2::new(22900, 25000),
    );
}

/// Columns and chunks outside `rmin..rmax` (the union of the regions'
/// boxes) are bit-identical with and without `rasters`, on the same world:
/// every `ColumnSample` field of 10 000 random columns plus the whole ring
/// around the box, and every block of 10 000 random chunks plus every chunk
/// 1..=3 chunks outside the box. `inside` must be a column the rasters
/// author (or this proves nothing).
pub(super) fn assert_identical_outside(
    rasters: AuthoredRasters,
    rmin: (i32, i32),
    rmax: (i32, i32),
    inside: Vec2<i32>,
) {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let map = world.sim.get_size().map(|e| e as i32) * 32;
    let rmin = Vec2::from(rmin);
    let rmax = Vec2::from(rmax);
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(161);
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
    let cmin = rmin / 32;
    let cmax = rmax / 32;
    let mut chunks: Vec<Vec2<i32>> = Vec::new();
    while chunks.len() < 10_000 {
        let c = Vec2::new(rng.random_range(2..1022), rng.random_range(2..1022));
        if !(c.x >= cmin.x - 3 && c.y >= cmin.y - 3 && c.x < cmax.x + 3 && c.y < cmax.y + 3) {
            chunks.push(c);
        }
    }
    let mut rings: [Vec<Vec2<i32>>; 3] = Default::default();
    for cy in cmin.y - 3..cmax.y + 3 {
        for cx in cmin.x - 3..cmax.x + 3 {
            let d = (cmin.x - cx)
                .max(cx - (cmax.x - 1))
                .max(cmin.y - cy)
                .max(cy - (cmax.y - 1));
            if (1..=3).contains(&d) {
                rings[(d - 1) as usize].push(Vec2::new(cx, cy));
            }
        }
    }
    // Site plots draw some of their decoration from the thread RNG directly
    // (a giant tree's ironwood sprites, camps, ruins: `rand::rng()` in
    // `site/plot/*`), which no test seed reaches; chunks a site's bounds
    // touch are left out of the block comparison (the column comparison above
    // still covers them). Everything else is generated with seeded RNGs and
    // compared exactly.
    let site_chunks: std::collections::HashSet<Vec2<i32>> = index
        .sites
        .values()
        .flat_map(|site| {
            let b = site.bounds();
            let (c0, c1) = (
                b.min.map(|e| e.div_euclid(32)) - 1,
                b.max.map(|e| e.div_euclid(32)) + 1,
            );
            (c0.y..=c1.y).flat_map(move |y| (c0.x..=c1.x).map(move |x| Vec2::new(x, y)))
        })
        .collect();
    chunks.retain(|c| !site_chunks.contains(c));
    let all: Vec<Vec2<i32>> = chunks
        .iter()
        .chain(rings.iter().flatten())
        .copied()
        .collect();
    let sample = |world: &World| -> (Vec<Option<Vec<u32>>>, Vec<u64>) {
        (
            cols.par_iter()
                .map(|p| column_bits(world, index_ref, *p))
                .collect(),
            all.par_iter()
                .map(|c| chunk_digest(world, index_ref, *c))
                .collect(),
        )
    };
    let (cols_a, chunks_a) = sample(&world);
    // The seeded generation really is deterministic: a second run agrees.
    let (_, chunks_a2) = sample(&world);
    assert_eq!(
        chunks_a, chunks_a2,
        "seeded chunk generation is not deterministic"
    );
    world.sim.set_authored_rasters_for_test(Some(rasters));
    let (cols_b, chunks_b) = sample(&world);
    let col_diff = cols_a.iter().zip(&cols_b).filter(|(a, b)| a != b).count();
    let differing: Vec<Vec2<i32>> = all
        .iter()
        .zip(chunks_a.iter().zip(&chunks_b))
        .filter(|(_, (a, b))| a != b)
        .map(|(c, _)| *c)
        .collect();
    println!(
        "columns compared {}, differing {col_diff}; chunks compared {} ({} random chunks off \
         sites + rings of {} / {} / {} chunks at distance 1 / 2 / 3), differing {differing:?}",
        cols.len(),
        all.len(),
        chunks.len(),
        rings[0].len(),
        rings[1].len(),
        rings[2].len()
    );
    assert_eq!(col_diff, 0);
    assert!(differing.is_empty());
    // The region really changed something inside, or this test proves nothing.
    assert!(
        world
            .sim
            .authored_rasters
            .as_ref()
            .unwrap()
            .cell_at(inside)
            .is_some_and(|c| c != super::AuthoredCell::None)
    );
}

/// The synthetic arena manifest passes the load-time consistency check
/// against the real sim table (the arena holds no sim water, and the exporter
/// rule "masks untouched" leaves every authored-wet chunk table-dry, which the
/// default budget allows), and a zero budget for that direction is enforced.
#[test]
#[ignore]
fn arena_manifest_passes_the_consistency_check() {
    let (world, _index) = generate_world();
    let rasters = load_spec(&arena_spec());
    let report = rasters
        .check_consistency_with_sim("test", &world.sim)
        .expect("consistent");
    assert_eq!(report.len(), 1);
    assert!(report[0].authored_dry_table_wet.is_empty());
    assert!(!report[0].authored_wet_table_dry.is_empty());
    let mut strict = arena_spec();
    strict.consistency.max_authored_wet_table_dry_chunks = Some(0);
    let err = load_spec(&strict)
        .check_consistency_with_sim("test", &world.sim)
        .unwrap_err();
    assert!(err.0.contains("dry in the sim table"), "{err}");
    // A sim-wet chunk under authored dry ground: any real lake chunk, with a
    // region boxed around it and no authored water, fails the default budget.
    let lake = (100..900)
        .flat_map(|y| (100..900).map(move |x| Vec2::new(x, y)))
        .find(|c: &Vec2<i32>| world.sim.get(*c).is_some_and(|c| c.river.is_lake()))
        .expect("the map has lakes");
    let min = (lake * 32 - 64).into_tuple();
    let max = (lake * 32 + 96).into_tuple();
    let dry = RegionSpec::new("dry_over_lake", min, max, 0, vec![PaintOp::Bank {
        shape: rect(
            (lake.x * 32 - 32) as f32,
            (lake.y * 32 - 32) as f32,
            (lake.x * 32 - 31) as f32,
            (lake.y * 32 - 31) as f32,
        ),
        bed_cm: 30_000,
    }]);
    let err = load_spec(&dry)
        .check_consistency_with_sim("test", &world.sim)
        .unwrap_err();
    assert!(err.0.contains("the sim table calls water"), "{err}");
}

/// Regional overrides apply on top of authored terrain: a `Damage` crater on
/// an authored river lowers the authored bed by the crater depth and the
/// authored surface stays put, so the crater sits under the water.
#[test]
#[ignore]
fn damage_crater_on_authored_water_sits_under_the_surface() {
    use common::terrain::regional_override::{
        DamageOverride, DamageShape, OverrideRegion, RegionalTerrainOverride, TerrainOverrideId,
        TerrainOverridePayload, TerrainOverrides,
    };
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    world
        .sim
        .set_authored_rasters_for_test(Some(load_spec(&arena_spec())));
    let center = Vec2::new(23050, 25410);
    let overrides = TerrainOverrides {
        version: 1,
        active: vec![RegionalTerrainOverride {
            id: TerrainOverrideId(1),
            region: OverrideRegion::Circle {
                center,
                radius: 12.0,
                edge: 4.0,
            },
            payload: TerrainOverridePayload::Damage(DamageOverride {
                shapes: vec![DamageShape::Crater {
                    max_depth: 3.0,
                    rim_height: 0.0,
                }],
                scorch: 0.0,
                vegetation_mul: 1.0,
                heal_progress: 0.0,
                heal_stages: 1,
                heal_interval: 1.0,
                next_heal_at: 0.0,
            }),
            priority: 0,
            activated_at: 0.0,
            wipe_player_edits: false,
            ephemeral: true,
            transition: Default::default(),
        }],
    };
    let plain = crate::column::ColumnGen::new(&world.sim)
        .get((center, index_ref, None))
        .unwrap();
    let cratered = crate::column::ColumnGen::with_overrides(&world.sim, &overrides)
        .get((center, index_ref, None))
        .unwrap();
    assert_eq!(plain.alt, 232.5, "authored bed");
    assert_eq!(plain.water_level, 238.5, "authored surface");
    assert!(
        (cratered.alt - (232.5 - 3.0)).abs() < 1e-3,
        "crater lowers the authored bed: {}",
        cratered.alt
    );
    assert_eq!(cratered.water_level, 238.5, "the authored surface stays");
}

/// Air 0, natural ground 1, decoration 2 (any other solid block, or a
/// sprite), liquids by kind.
fn block_class(b: &Block) -> u32 {
    if b.is_liquid() || b.kind() == BlockKind::Lava {
        0x100 + b.kind() as u32
    } else if is_natural_ground(b) {
        1
    } else if b.is_filled()
        || b.get_sprite()
            .is_some_and(|s| s != common::terrain::SpriteKind::Empty)
    {
        2
    } else {
        0
    }
}

pub(super) fn is_natural_ground(b: &Block) -> bool {
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
                        super::AuthoredCell::Ground { .. } => {
                            unreachable!("the Stage-1 arena has no ground layer")
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
fn authored_region_generation_cost() { generation_cost(load_spec(&arena_spec())); }

/// [`authored_region_generation_cost`] for the Stage-2 ground battery (exact
/// ground over the whole arena, including the 5 km pillar and the -1860
/// block pit, whose chunks are tall).
#[test]
#[ignore]
fn ground_region_generation_cost() {
    generation_cost(load_specs(&ground_battery_specs()).unwrap());
}

fn generation_cost(rasters: AuthoredRasters) {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
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

/// Identity next to the sim's own water: a region placed beside a real sim
/// river chunk and beside a real sim lake chunk (wilderness, at least 1 km
/// from every site) leaves every column and chunk outside it bit-identical,
/// including the columns whose water comes from those chunks (the river/lake
/// fold, the flood formula and the unfaded relief delta all run there).
#[test]
#[ignore]
fn identity_next_to_a_sim_river_and_a_sim_lake() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let far_from_sites = |c: Vec2<i32>| {
        let w = c * 32;
        index.sites.values().all(|site| {
            let b = site.bounds();
            let dx = (b.min.x - w.x).max(w.x - b.max.x).max(0);
            let dy = (b.min.y - w.y).max(w.y - b.max.y).max(0);
            dx.max(dy) > 1000
        })
    };
    let find = |pred: &dyn Fn(&crate::sim::SimChunk) -> bool| {
        (40..980)
            .step_by(7)
            .flat_map(|y| (40..980).step_by(7).map(move |x| Vec2::new(x, y)))
            .find(|c| {
                world.sim.get(*c).is_some_and(pred)
                    && (2..6).all(|dx| {
                        world
                            .sim
                            .get(*c + Vec2::new(dx, 0))
                            .is_some_and(|n| n.river.river_kind.is_none())
                    })
                    && far_from_sites(*c)
            })
    };
    let river = find(&|c| c.river.is_river()).expect("a wilderness river chunk");
    let lake = find(&|c| c.river.is_lake()).expect("a wilderness lake chunk");
    for (name, water) in [("river", river), ("lake", lake)] {
        // A 4 x 4-chunk region whose west edge is 2 chunks east of the water
        // chunk: the water's own columns, and the band between, stay outside.
        let min = (water + Vec2::new(2, -2)) * 32;
        let max = min + 128;
        let spec = RegionSpec::new(
            format!("beside_{name}"),
            min.into_tuple(),
            max.into_tuple(),
            16,
            vec![PaintOp::Bank {
                shape: rect(
                    (min.x + 40) as f32,
                    (min.y + 40) as f32,
                    (min.x + 80) as f32,
                    (min.y + 80) as f32,
                ),
                bed_cm: ((world.sim.get(water + Vec2::new(4, 0)).unwrap().alt + 3.0) * 100.0)
                    as i32,
            }],
        );
        let rasters = load_spec(&spec);
        let cols: Vec<Vec2<i32>> = (min.y - 192..max.y + 192)
            .step_by(3)
            .flat_map(|y| {
                (min.x - 192..max.x + 192)
                    .step_by(3)
                    .map(move |x| Vec2::new(x, y))
            })
            .filter(|p| !(p.x >= min.x && p.y >= min.y && p.x < max.x && p.y < max.y))
            .collect();
        let cmin = min / 32;
        let cmax = max / 32;
        let chunks: Vec<Vec2<i32>> = (cmin.y - 3..cmax.y + 3)
            .flat_map(|y| (cmin.x - 3..cmax.x + 3).map(move |x| Vec2::new(x, y)))
            .filter(|c| !(c.x >= cmin.x && c.y >= cmin.y && c.x < cmax.x && c.y < cmax.y))
            .collect();
        let sample = |world: &World| -> (Vec<Option<Vec<u32>>>, Vec<u64>) {
            (
                cols.par_iter()
                    .map(|p| column_bits(world, index_ref, *p))
                    .collect(),
                chunks
                    .par_iter()
                    .map(|c| chunk_digest(world, index_ref, *c))
                    .collect(),
            )
        };
        world.sim.set_authored_rasters_for_test(None);
        let a = sample(&world);
        world.sim.set_authored_rasters_for_test(Some(rasters));
        let b = sample(&world);
        let wet_cols = cols
            .iter()
            .filter(|p| {
                world
                    .sample_columns()
                    .get((**p, index_ref, None))
                    .is_some_and(|c| c.water_level > c.alt)
            })
            .count();
        let col_diff = a.0.iter().zip(&b.0).filter(|(x, y)| x != y).count();
        let chunk_diff = a.1.iter().zip(&b.1).filter(|(x, y)| x != y).count();
        println!(
            "{name} at chunk {water:?}: {} columns ({wet_cols} with sim water), differing \
             {col_diff}; {} chunks, differing {chunk_diff}",
            cols.len(),
            chunks.len()
        );
        assert!(wet_cols > 0, "the compared area must hold sim water");
        assert_eq!(col_diff, 0);
        // Chunks 2..3 out are identical; in ring 1, blocks may differ only
        // within the decoration halo: a boulder or tree rooted inside the
        // region (whose terrain changed where the sim's bank pull is now
        // suppressed) reaching across the edge.
        let mut halo = 0;
        for (c, (x, y)) in chunks.iter().zip(a.1.iter().zip(&b.1)) {
            if x == y {
                continue;
            }
            let ring = (cmin.x - c.x)
                .max(c.x - (cmax.x - 1))
                .max(cmin.y - c.y)
                .max(c.y - (cmax.y - 1));
            assert_eq!(ring, 1, "chunk {c:?} {ring} chunks out differs");
            world.sim.set_authored_rasters_for_test(None);
            let before = seeded_chunk(&world, index_ref, *c);
            world
                .sim
                .set_authored_rasters_for_test(Some(load_spec(&spec)));
            let after = seeded_chunk(&world, index_ref, *c);
            for z in
                before.get_min_z().min(after.get_min_z())..before.get_max_z().max(after.get_max_z())
            {
                for yy in 0..32 {
                    for xx in 0..32 {
                        let p = Vec3::new(xx, yy, z);
                        let get = |ch: &common::terrain::TerrainChunk| {
                            ch.get(p).copied().unwrap_or_else(|_| Block::empty())
                        };
                        if block_class(&get(&before)) != block_class(&get(&after)) {
                            let w = c * 32 + Vec2::new(xx, yy);
                            let d = (min.x - w.x)
                                .max(w.x - (max.x - 1))
                                .max(min.y - w.y)
                                .max(w.y - (max.y - 1));
                            halo = halo.max(d);
                        }
                    }
                }
            }
        }
        println!("{name}: ring-1 differences reach {halo} m outside the box");
        assert!(halo <= super::REGION_MARGIN_M, "{halo} m");
    }
}

/// A chunk generated with the seeded RNGs of [`chunk_digest`].
fn seeded_chunk(
    world: &World,
    index: crate::IndexRef,
    cpos: Vec2<i32>,
) -> common::terrain::TerrainChunk {
    let mut seed = [0u8; 32];
    seed[..4].copy_from_slice(&cpos.x.to_le_bytes());
    seed[4..8].copy_from_slice(&cpos.y.to_le_bytes());
    crate::with_deterministic_dynamic_rng(seed, || {
        world
            .generate_chunk(index, cpos, None, || false, None, None)
            .unwrap()
            .0
    })
}

/// Block-by-block editing from the natural map: a 3 x 3 pond written into a
/// real wilderness river chunk, the rest of the region left unauthored.
/// With the box owned (the default) the sim's river chunk is refused by the
/// consistency budget; declared partial, every column except the nine
/// authored ones is bit-identical to the world without the region (the
/// natural river included), and the seam between the pond and the natural
/// water is reported.
#[test]
#[ignore]
fn a_pond_edited_into_a_natural_river() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let far_from_sites = |c: Vec2<i32>| {
        let w = c * 32;
        index.sites.values().all(|site| {
            let b = site.bounds();
            let dx = (b.min.x - w.x).max(w.x - b.max.x).max(0);
            let dy = (b.min.y - w.y).max(w.y - b.max.y).max(0);
            dx.max(dy) > 1000
        })
    };
    let river = (40..980)
        .step_by(7)
        .flat_map(|y| (40..980).step_by(7).map(move |x| Vec2::new(x, y)))
        .find(|c| world.sim.get(*c).is_some_and(|c| c.river.is_river()) && far_from_sites(*c))
        .expect("a wilderness river chunk");
    let centre = river * 32 + 16;
    let natural = world
        .sample_columns()
        .get((centre, index_ref, None))
        .map(|c| (c.alt, c.water_level))
        .unwrap();
    let (natural_alt, natural_level) = natural;
    assert!(
        natural_level > natural_alt,
        "the chunk centre holds natural water"
    );
    // The pond: the natural surface block, bed 2 blocks deeper than the
    // natural ground.
    let surface_block = (natural_level - 0.5).floor() as i32;
    let bed_block = (natural_alt.floor() as i32 - 2).min(surface_block - 1);
    let min = (river - 2) * 32;
    let max = (river + 3) * 32;
    let spec = |partial: bool| {
        let mut s = RegionSpec::new(
            "pond_in_river",
            min.into_tuple(),
            max.into_tuple(),
            16,
            vec![PaintOp::Water {
                shape: rect(
                    (centre.x - 1) as f32,
                    (centre.y - 1) as f32,
                    (centre.x + 2) as f32,
                    (centre.y + 2) as f32,
                ),
                surface_cm: surface_block * 100 + 50,
                bed_cm: bed_block * 100 + 50,
            }],
        );
        s.allow_partial = partial;
        s
    };
    // Owned box: the pond meets unauthored columns -- refused at load.
    let built = vec![build_region(&spec(false)).unwrap()];
    let files: HashMap<(i32, i32), Vec<u8>> = built[0].tiles.iter().cloned().collect();
    let fetch = |_: &str, _: LayerKind, tx: i32, ty: i32| {
        files
            .get(&(tx, ty))
            .cloned()
            .ok_or_else(|| "missing".to_string())
    };
    let e = AuthoredRasters::from_manifest(manifest(&built), Vec2::broadcast(32768), "t", &fetch)
        .unwrap_err();
    println!("owned box: {}", &e.0[..e.0.len().min(220)]);
    assert!(e.0.contains("declare its chunk partial"), "{}", e.0);

    // Partial: loads and passes the hard budget against the real sim.
    let rasters = load_spec(&spec(true));
    let r = rasters.check_consistency_with_sim("t", &world.sim).unwrap();
    println!(
        "partial: {} table-wet partial chunk(s) exempt, {} counted",
        r[0].partial_table_wet.len(),
        r[0].authored_dry_table_wet.len()
    );
    assert!(r[0].partial_table_wet.contains(&river));
    let cols: Vec<Vec2<i32>> = (min.y..max.y)
        .flat_map(|y| (min.x..max.x).map(move |x| Vec2::new(x, y)))
        .collect();
    let sample = |world: &World| -> Vec<Option<Vec<u32>>> {
        cols.par_iter()
            .map(|p| column_bits(world, index_ref, *p))
            .collect()
    };
    world.sim.set_authored_rasters_for_test(None);
    let a = sample(&world);
    let level = |world: &World, p: Vec2<i32>| {
        world
            .sample_columns()
            .get((p, index_ref, None))
            .map(|c| (c.alt, c.water_level))
            .unwrap()
    };
    let ring: Vec<Vec2<i32>> = (-2i32..=2)
        .flat_map(|dy| (-2i32..=2).map(move |dx| Vec2::new(dx, dy)))
        .filter(|d| d.x.abs() == 2 || d.y.abs() == 2)
        .map(|d| centre + d)
        .collect();
    let natural_ring: Vec<(f32, f32)> = ring.iter().map(|p| level(&world, *p)).collect();
    world.sim.set_authored_rasters_for_test(Some(rasters));
    let b = sample(&world);
    let in_pond = |p: &Vec2<i32>| (p.x - centre.x).abs() <= 1 && (p.y - centre.y).abs() <= 1;
    let mut differing = 0;
    let mut pond_differing = 0;
    for (p, (x, y)) in cols.iter().zip(a.iter().zip(&b)) {
        if x != y {
            if in_pond(p) {
                pond_differing += 1;
            } else {
                differing += 1;
            }
        }
    }
    let pond_col = level(&world, centre);
    let step = natural_ring
        .iter()
        .map(|(alt, wl)| {
            let natural_top = if wl > alt { *wl } else { *alt };
            (pond_col.1 - natural_top).abs()
        })
        .fold(0.0f32, f32::max);
    println!(
        "river chunk {river:?}, pond at {centre:?}: surface block {surface_block}, bed block \
         {bed_block} (natural water level {:.2}, ground {:.2}); {} region columns, {differing} \
         differ outside the pond, {pond_differing}/9 pond columns changed; largest step from the \
         pond surface to the natural water/ground around it: {step:.2} m",
        natural_level,
        natural_alt,
        cols.len()
    );
    assert_eq!(differing, 0, "only the authored columns change");
    assert_eq!(pond_col.1, surface_block as f32 + 0.5);
    assert_eq!(pond_col.0, bed_block as f32 + 0.5);
}

pub use super::ground_real_world_tests::ground_battery_specs;

/// The Stage-1 arena manifest (water and banks, no ground layer) renders and
/// answers exactly as before the ground consumers existed: a pinned sha256
/// over every column of the box grown by 64 m (sampled altitude and water
/// level, `surface_alt_at`, the chunk table altitude), every chunk's
/// `chunk_water`, and the block classes of every chunk of the box above
/// z 100 (seeded RNGs; z 100 keeps the deep, unseeded cave decoration out).
/// The constant was computed with the engine before the ground consumers
/// (and agrees with it) and must not move while the manifest has no ground
/// layer. Computed on macOS arm64, like the other committed digests.
#[test]
#[ignore]
fn water_only_arena_output_is_pinned() {
    const PINNED: &str = "c22e38dbccb521bbf59b65937c92000dd03a397b8b19c060623bcfc47e976df9";
    let (mut world, index) = generate_world();
    world
        .sim
        .set_authored_rasters_for_test(Some(load_spec(&arena_spec())));
    let index_ref = index.as_index_ref();
    let (min, max) = (Vec2::from(REGION_MIN) - 64, Vec2::from(REGION_MAX) + 64);
    let rows: Vec<Vec<u8>> = (min.y..max.y)
        .into_par_iter()
        .map(|y| {
            let mut out = Vec::new();
            for x in min.x..max.x {
                let p = Vec2::new(x, y);
                let c = world.sample_columns().get((p, index_ref, None)).unwrap();
                out.extend(c.alt.to_bits().to_le_bytes());
                out.extend(c.water_level.to_bits().to_le_bytes());
                out.extend(world.sim.surface_alt_at(p).to_bits().to_le_bytes());
                out.extend(
                    world
                        .sim
                        .get_alt_approx(p)
                        .unwrap_or(f32::NAN)
                        .to_bits()
                        .to_le_bytes(),
                );
            }
            out
        })
        .collect();
    let (c0, c1) = (min / 32, max / 32);
    let chunks: Vec<Vec2<i32>> = (c0.y..c1.y)
        .flat_map(|y| (c0.x..c1.x).map(move |x| Vec2::new(x, y)))
        .collect();
    let per_chunk: Vec<Vec<u8>> = chunks
        .par_iter()
        .map(|&cpos| {
            let mut out = Vec::new();
            let w = world.sim.chunk_water(cpos).unwrap();
            out.extend([w.river, w.lake, w.ocean, w.underwater, w.near_water].map(u8::from));
            out.extend(w.water_alt.to_bits().to_le_bytes());
            out.extend(w.alt.to_bits().to_le_bytes());
            let chunk = seeded_chunk(&world, index_ref, cpos);
            let sz = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
            for z in chunk.get_min_z().max(100)..chunk.get_max_z() {
                for y in 0..sz.y {
                    for x in 0..sz.x {
                        let b = chunk
                            .get(Vec3::new(x, y, z))
                            .copied()
                            .unwrap_or_else(|_| Block::empty());
                        out.extend(block_class(&b).to_le_bytes());
                    }
                }
            }
            out
        })
        .collect();
    let mut all = Vec::new();
    rows.iter().for_each(|r| all.extend_from_slice(r));
    per_chunk.iter().for_each(|c| all.extend_from_slice(c));
    let digest = super::format::sha256_hex(&all);
    println!(
        "water-only arena: {} columns, {} chunks, sha256 {digest}",
        (max.x - min.x) * (max.y - min.y),
        chunks.len()
    );
    assert_eq!(digest, PINNED);
}
