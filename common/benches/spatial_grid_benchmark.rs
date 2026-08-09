use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use specs::{Builder, World, WorldExt};
use std::hint::black_box;
use vek::*;
use xindeler_common::util::SpatialGrid;

/// Mirrors phys's per-tick full rebuild (`construct_spatial_grid`,
/// common/systems/src/phys/mod.rs:324): same cell parameters, one insert per
/// entity. Baseline for the Phase 2 incremental-grid work.
pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("spatial_grid_rebuild");
    for &n in &[200usize, 500, 2000] {
        let mut world = World::new();
        let entities: Vec<specs::Entity> = (0..n).map(|_| world.create_entity().build()).collect();
        // Deterministic pseudo-random positions in a 1024x1024 region (a
        // busy town); radius 2 ≈ humanoid scaled_radius + truncation error.
        let positions: Vec<Vec2<i32>> = (0..n as i32)
            .map(|i| Vec2::new((i * 7919) % 1024, (i * 104729) % 1024))
            .collect();

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Parameters from phys/mod.rs:340-342
                let mut grid = SpatialGrid::new(5, 6, 8);
                for (entity, pos) in entities.iter().zip(positions.iter()) {
                    grid.insert(*pos, 2, *entity);
                }
                black_box(&grid);
            })
        });
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
