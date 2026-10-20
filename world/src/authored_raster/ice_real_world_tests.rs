//! Real-world check of the downward-only ice on authored water
//! ([`super::AuthoredColumn::ice_band`]) on the Cromatolis map: the arena
//! lake (128 x 200 m, surface block 238, bed 232) of `real_world_tests`.
//!
//! The plateau is warm, so the test makes the lake's chunks (and their
//! neighbours, which the sampler interpolates) cold itself, as the
//! +4500 m arenas are (their sim temperature sits on the scale's floor).
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release \
//!     authored_raster::ice_real_world_tests -- --ignored --nocapture
//! ```

use super::real_world_tests::{arena_spec, generate_world, load_spec};
use crate::util::Sampler;
use common::{
    terrain::{Block, BlockKind},
    vol::ReadVol,
};
use vek::*;

const SURFACE: i32 = 238;
const BED: i32 = 232;
/// Chunks covering the lake (x 23436..23564, y 25200..25400).
const CHUNKS_X: std::ops::RangeInclusive<i32> = 732..=737;
const CHUNKS_Y: std::ops::RangeInclusive<i32> = 787..=794;

#[test]
#[ignore]
fn cold_authored_lake_freezes_downward_only() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    world
        .sim
        .set_authored_rasters_for_test(Some(load_spec(&arena_spec())));
    for cy in *CHUNKS_Y.start() - 2..=*CHUNKS_Y.end() + 2 {
        for cx in *CHUNKS_X.start() - 2..=*CHUNKS_X.end() + 2 {
            world.sim.get_mut(Vec2::new(cx, cy)).unwrap().temp = -1.0;
        }
    }
    let rasters = world.sim.authored_rasters.as_ref().unwrap();
    let temp = world
        .sample_columns()
        .get((Vec2::new(23500, 25300), index_ref, None))
        .unwrap()
        .temp;
    assert!(
        temp < -0.8,
        "the lake chunk must be colder than the snow line: {temp}"
    );

    let (mut wet, mut iced, mut capped) = (0u32, 0u32, 0u32);
    for cy in CHUNKS_Y {
        for cx in CHUNKS_X {
            let (chunk, _) = world
                .generate_chunk(index_ref, Vec2::new(cx, cy), None, || false, None, None)
                .unwrap();
            let block = |x: i32, y: i32, z: i32| {
                chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty())
            };
            for y in 0..32 {
                for x in 0..32 {
                    let wpos = Vec2::new(cx, cy) * 32 + Vec2::new(x, y);
                    let Some(super::AuthoredCell::Wet {
                        surface_block,
                        bed_block,
                    }) = rasters.column(wpos).map(|c| c.cell)
                    else {
                        continue;
                    };
                    assert_eq!((surface_block, bed_block), (SURFACE, BED));
                    wet += 1;
                    let col = world.sample_columns().get((wpos, index_ref, None)).unwrap();
                    assert!(col.ice_centre + col.ice_depth <= col.water_level);
                    let mut top_fluid = None;
                    let mut ice = Vec::new();
                    for z in (chunk.get_min_z()..chunk.get_max_z()).rev() {
                        let b = block(x, y, z);
                        if b.kind() == BlockKind::Ice {
                            ice.push(z);
                        }
                        if top_fluid.is_none() && (b.is_liquid() || b.kind() == BlockKind::Ice) {
                            top_fluid = Some((z, b.kind()));
                        }
                    }
                    assert!(
                        ice.iter().all(|&z| z <= SURFACE),
                        "{wpos}: ice above the surface block: {ice:?}"
                    );
                    assert!(ice.len() as i32 <= super::ICE_MAX_BLOCKS, "{wpos}: {ice:?}");
                    // A boulder rooted in the water (decorations stay natural)
                    // stands in the surface block: only the ice rules apply.
                    let at_surface = block(x, y, SURFACE);
                    if at_surface.is_filled()
                        && !at_surface.is_liquid()
                        && at_surface.kind() != BlockKind::Ice
                    {
                        continue;
                    }
                    let (top_z, top_kind) = top_fluid.expect("a wet column has liquid or ice");
                    assert_eq!(top_z, SURFACE, "{wpos}: top liquid/ice block");
                    let lowest_ice = ice.last().copied();
                    if let Some(lowest) = lowest_ice {
                        iced += 1;
                        assert_eq!(top_kind, BlockKind::Ice);
                        capped += (block(x, y, lowest - 1).kind() == BlockKind::Water) as u32;
                    } else {
                        assert_eq!(top_kind, BlockKind::Water, "{wpos}");
                    }
                }
            }
        }
    }
    eprintln!("temp {temp}: {wet} wet columns, {iced} with ice, {capped} with water under it");
    assert!(wet > 0 && iced > 0, "the cold lake must freeze somewhere");
    assert_eq!(iced, capped, "liquid stays under every ice cap");
}

/// Outside every region the band is the natural one: centred on the water
/// level.
#[test]
#[ignore]
fn natural_columns_keep_the_water_level_centred_ice_band() {
    let (world, index) = generate_world();
    let index_ref = index.as_index_ref();
    for y in 0..64 {
        for x in 0..64 {
            let wpos = Vec2::new(10_000, 10_000) + Vec2::new(x, y) * 16;
            let col = world.sample_columns().get((wpos, index_ref, None)).unwrap();
            assert_eq!(col.ice_centre.to_bits(), col.water_level.to_bits());
        }
    }
}
