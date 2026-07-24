use common::{terrain::TerrainGrid, vol::SampleVol};
use criterion::{Criterion, criterion_group, criterion_main};
use std::{hint::black_box, sync::Arc};
use vek::*;
use world::{World, sim};
use xindeler_voxygen::mesh::terrain::{MAX_LIGHT_DIST, SUNLIGHT, calc_light};

const CENTER: Vec2<i32> = Vec2 { x: 512, y: 512 };
const GEN_SIZE: i32 = 3;

pub fn criterion_benchmark(c: &mut Criterion) {
    let pool = rayon::ThreadPoolBuilder::new().build().unwrap();
    // Generate chunks here to test
    let (world, index) = World::generate(
        sim::DEFAULT_WORLD_SEED,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset(sim::DEFAULT_WORLD_MAP.into()),
            calendar: None,
        },
        &pool,
        &|_| {},
    );
    let mut terrain = TerrainGrid::new(
        world.sim().map_size_lg(),
        Arc::new(world.sim().generate_oob_chunk()),
    )
    .unwrap();
    let index = index.as_index_ref();
    (0..GEN_SIZE)
        .flat_map(|x| (0..GEN_SIZE).map(move |y| Vec2::new(x, y)))
        .map(|offset| offset + CENTER)
        .map(|pos| {
            (
                pos,
                world
                    .generate_chunk(index, pos, None, || false, None)
                    .unwrap(),
            )
        })
        .for_each(|(key, chunk)| {
            terrain.insert(key, Arc::new(chunk.0));
        });

    // Sample chunk (1,1) + 1-block borders, same math as meshing_benchmark L51-79
    let chunk_pos = Vec2::new(1, 1) + CENTER;
    let aabr = Aabr {
        min: chunk_pos.map2(TerrainGrid::chunk_size(), |e, sz| e * sz as i32 - 1),
        max: chunk_pos.map2(TerrainGrid::chunk_size(), |e, sz| (e + 1) * sz as i32 + 1),
    };
    let volume = terrain.sample(aabr).unwrap();
    let min_z = volume
        .iter()
        .fold(i32::MAX, |min, (_, chunk)| chunk.get_min_z().min(min));
    let max_z = volume
        .iter()
        .fold(i32::MIN, |max, (_, chunk)| chunk.get_max_z().max(max));
    let range = Aabb {
        min: Vec3::from(aabr.min) + Vec3::unit_z() * (min_z - 1),
        max: Vec3::from(aabr.max) + Vec3::unit_z() * (max_z + 1),
    };

    // 16 synthetic glow seeds spread through the chunk interior
    let glow_seeds: Vec<(Vec3<i32>, u8)> = (0..16)
        .map(|i| {
            let off = Vec3::new(
                MAX_LIGHT_DIST + (i % 4) * 6,
                MAX_LIGHT_DIST + (i / 4) * 6,
                range.size().d / 2,
            );
            (range.min + off, 10)
        })
        .collect();

    let mut group = c.benchmark_group("light");
    group.sample_size(20);
    group.bench_function("sunlight", |b| {
        b.iter(|| {
            black_box(calc_light(
                true,
                SUNLIGHT,
                black_box(range),
                &volume,
                core::iter::empty(),
            ))
        })
    });
    group.bench_function("glow_empty", |b| {
        b.iter(|| {
            black_box(calc_light(
                false,
                0,
                black_box(range),
                &volume,
                core::iter::empty(),
            ))
        })
    });
    group.bench_function("glow_seeded", |b| {
        b.iter(|| {
            black_box(calc_light(
                false,
                0,
                black_box(range),
                &volume,
                glow_seeds.iter().copied(),
            ))
        })
    });
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
