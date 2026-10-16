//! The wall of an authored dry pit on the Cromatolis calibration arena: no
//! water on it and the ground's own material, never the engine's deep stone
//! (blue) or a sea-filled sand slope.
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release --features tools \
//!     authored_raster::pit_wall_real_world_tests -- --ignored --nocapture --test-threads 1
//! ```

use super::{
    SeaFill,
    format::GROUND_EXACT,
    real_world_tests::{REGION_MAX, REGION_MIN, generate_world, load_specs},
    writer::{PaintOp, RegionSpec, Shape},
};
use crate::{World, util::Sampler};
use common::{
    terrain::{Block, BlockKind},
    vol::ReadVol,
};
use rayon::prelude::*;
use vek::*;

/// The pit's exact box (152 m, 36 m inside the region's west edge, as the
/// exporter derives it for a 128 m pit) and its chunk range.
const EXACT: (i32, i32, i32, i32) = (22788, 25420, 22940, 25572);

#[derive(Default, Debug)]
struct Wall {
    columns: u32,
    liquid_columns: u32,
    sea_sand_columns: u32,
    /// Wall blocks (above the neighbouring ground, under the surface layer)
    /// that are not the surface block kind.
    foreign_blocks: u32,
    wall_blocks: u32,
    colour_mismatch: u32,
    kinds: std::collections::BTreeMap<String, u32>,
    wall_columns: u32,
}

fn survey(world: &World, index: crate::IndexRef) -> Wall {
    let chunks: Vec<Vec2<i32>> = (22752 / 32 - 1..=22944 / 32 + 1)
        .flat_map(|x| (25376 / 32..=25600 / 32 + 1).map(move |y| Vec2::new(x, y)))
        .collect();
    let rows: Vec<Wall> = chunks
        .par_iter()
        .map(|c| {
            let mut w = Wall::default();
            let (chunk, _) = world
                .generate_chunk(index, *c, None, || false, None, None)
                .unwrap();
            let (z0, z1) = (chunk.get_min_z(), chunk.get_max_z());
            for y in 0..32 {
                for x in 0..32 {
                    let wpos = c * 32 + Vec2::new(x, y);
                    let col = world.sample_columns().get((wpos, index, None)).unwrap();
                    let block = |z: i32| {
                        chunk
                            .get(Vec3::new(x, y, z))
                            .copied()
                            .unwrap_or_else(|_| Block::empty())
                    };
                    w.columns += 1;
                    let mut liquid = false;
                    let mut top_kind = None;
                    for z in (z0..z1).rev() {
                        let b = block(z);
                        liquid |= b.is_liquid();
                        if top_kind.is_none() && b.is_filled() {
                            top_kind = Some(b.kind());
                        }
                    }
                    w.liquid_columns += liquid as u32;
                    w.sea_sand_columns += (liquid && top_kind == Some(BlockKind::Sand)) as u32;
                    let Some(floor) = col.exposed_wall_floor else {
                        continue;
                    };
                    w.wall_columns += 1;
                    // Under the surface layer, above the neighbouring ground.
                    let lo = floor.floor() as i32 + 1;
                    let hi = col.alt as i32 - 6;
                    for z in lo.max(z0)..=hi.min(z1 - 1) {
                        let b = block(z);
                        if !b.is_filled() || b.is_liquid() {
                            continue;
                        }
                        w.wall_blocks += 1;
                        let want = col.surface_color.map(|e| (e * 255.0) as u8);
                        if !matches!(b.kind(), BlockKind::Grass | BlockKind::Snow) {
                            *w.kinds.entry(format!("{:?}", b.kind())).or_default() += 1;
                            w.foreign_blocks += 1;
                        } else if b.get_color() != Some(want) {
                            w.colour_mismatch += 1;
                        }
                    }
                }
            }
            w
        })
        .collect();
    rows.into_iter().fold(Wall::default(), |mut a, r| {
        a.columns += r.columns;
        a.liquid_columns += r.liquid_columns;
        a.sea_sand_columns += r.sea_sand_columns;
        a.foreign_blocks += r.foreign_blocks;
        a.wall_blocks += r.wall_blocks;
        a.colour_mismatch += r.colour_mismatch;
        a.wall_columns += r.wall_columns;
        for (k, v) in r.kinds {
            *a.kinds.entry(k).or_default() += v;
        }
        a
    })
}

fn pit_spec(floor_cm: i32) -> RegionSpec {
    let (x0, y0, x1, y1) = EXACT;
    let mut s = RegionSpec::new("pit_wall", REGION_MIN, REGION_MAX, 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: x0 as f32,
                y0: y0 as f32,
                x1: x1 as f32,
                y1: y1 as f32,
            },
            ground_cm: floor_cm,
            weight: GROUND_EXACT,
        },
    ]);
    s.sea_fill = SeaFill::AuthoredOnly;
    s
}

/// Pits at -10 m and -1500 m (blocks 130 and -1360) in an `AuthoredOnly`
/// region whose chunk table holds the pit's chunks below the sea, as the
/// exporter's composed table does beside a deep depression: the unauthored
/// columns around the pit interpolate toward it and sit below sea level.
/// Every column around the pit is dry; the wall's exposed face is the
/// ground's own surface block (kind and colour), not the engine's blue deep
/// stone; natural sea elsewhere in the same world keeps its water.
#[test]
#[ignore]
fn pit_walls_are_dry_and_the_grounds_own_material() {
    let (mut world, index) = generate_world();
    let ir = index.as_index_ref();
    // The table's pit chunks below the sea (chunk x 712..717, y 794..800).
    for cy in 794..800 {
        for cx in 712..717 {
            world.sim.get_mut(Vec2::new(cx, cy)).unwrap().alt = 120.0;
        }
    }
    for floor_cm in [13_050, -135_950] {
        world
            .sim
            .set_authored_rasters_for_test(Some(load_specs(&[pit_spec(floor_cm)]).unwrap()));
        let w = survey(&world, ir);
        println!("floor {floor_cm} cm: {w:#?}");
        assert!(w.wall_blocks > 10_000, "no wall surveyed: {w:?}");
        assert_eq!(w.liquid_columns, 0, "water around the pit: {w:?}");
        assert_eq!(w.sea_sand_columns, 0, "{w:?}");
        assert_eq!(w.foreign_blocks, 0, "wall is not ground material: {w:?}");
        assert_eq!(w.colour_mismatch, 0, "{w:?}");
    }
    // Counter-test: natural sea in the same world still holds water.
    let sea = (40..980)
        .step_by(5)
        .flat_map(|y| (40..980).step_by(5).map(move |x| Vec2::new(x, y)))
        .find(|c| {
            world
                .sim
                .get(*c)
                .is_some_and(|n| n.river.is_ocean() && n.alt < 100.0)
        })
        .expect("a sea chunk");
    let (chunk, _) = world
        .generate_chunk(ir, sea, None, || false, None, None)
        .unwrap();
    let mut water = 0;
    for y in 0..32 {
        for x in 0..32 {
            for z in chunk.get_min_z()..chunk.get_max_z() {
                water += chunk
                    .get(Vec3::new(x, y, z))
                    .is_ok_and(|b| b.kind() == BlockKind::Water) as u32;
            }
        }
    }
    println!("natural sea chunk {sea:?}: {water} water blocks");
    assert!(water > 1000);
}
