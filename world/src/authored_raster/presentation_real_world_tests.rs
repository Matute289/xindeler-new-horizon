//! The world map and LOD altitude on authored ground. Identity without a
//! ground layer (the map message is byte-identical to the table-only map,
//! procedural and Cromatolis) and the LOD sample error on a synthetic patch
//! over the calibration arena (never a real settlement).
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release --features tools \
//!     authored_raster::presentation_real_world_tests -- --ignored --nocapture --test-threads 1
//! ```

use super::{
    AuthoredCell,
    format::GROUND_EXACT,
    real_world_tests::{PLATEAU_CM, REGION_MAX, REGION_MIN, generate_world, load_specs},
    writer::{PaintOp, RegionSpec, Shape},
};
use crate::{
    CONFIG, World,
    sim::{FileOpts, GenOpts, WorldOpts},
    util::Sampler,
};
use common::{
    terrain::{TerrainChunkSize, uniform_idx_as_vec2},
    vol::RectVolSize,
};
use common_net::msg::WorldMapMsg;
use vek::*;

fn map_bytes(msg: &WorldMapMsg) -> Vec<u8> {
    bincode::serde::encode_to_vec(msg, bincode::config::legacy()).unwrap()
}

/// The map message with and without authored ground presentation.
fn both_maps(world: &World, index: crate::IndexRef) -> (WorldMapMsg, WorldMapMsg) {
    (
        world.sim().get_map_with(index, None, true),
        world.sim().get_map_with(index, None, false),
    )
}

/// The `WorldMapMsg::alt` value decoded back to metres (13 bits over
/// `sea_level ..= sea_level + max_height`, as the client decodes it).
fn decoded_alt(msg: &WorldMapMsg, pos: Vec2<i32>) -> f32 {
    let raw = (msg.alt.get(pos).copied().unwrap() >> 3) & 0x1FFF;
    CONFIG.sea_level + raw as f32 / 8191.0 * msg.max_height
}

#[test]
#[ignore]
fn map_message_is_byte_identical_without_a_ground_layer_procedural() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        WorldOpts {
            seed_elements: true,
            world_file: FileOpts::Generate(GenOpts {
                x_lg: 7,
                y_lg: 7,
                ..GenOpts::default()
            }),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    assert!(world.sim().authored_rasters.is_none());
    let (new, old) = both_maps(&world, index.as_index_ref());
    let (new, old) = (map_bytes(&new), map_bytes(&old));
    println!("procedural map message: {} bytes", new.len());
    assert!(new == old, "procedural map message changed");
}

#[test]
#[ignore]
fn map_message_is_byte_identical_without_a_ground_layer_cromatolis() {
    let (mut world, index) = generate_world();
    world.sim.set_authored_rasters_for_test(None);
    let (new, old) = both_maps(&world, index.as_index_ref());
    let (new, old) = (map_bytes(&new), map_bytes(&old));
    println!("Cromatolis (no manifest) map message: {} bytes", new.len());
    assert!(
        new == old,
        "Cromatolis map message changed without a manifest"
    );
}

/// A synthetic ground patch over the arena: a raised plateau, a step, a
/// lowered basin (all above sea level) and a 32 m ring. At every chunk
/// sample column on an exact ground cell the presented LOD altitude is
/// within 1 m of the top block the column sampler renders there; samples off
/// the ground layer are unchanged.
#[test]
#[ignore]
fn lod_altitude_follows_the_ground_patch_at_the_sample_columns() {
    let (mut world, index) = generate_world();
    let index = index.as_index_ref();
    let exact = |x0: f32, y0: f32, x1: f32, y1: f32, ground_cm: i32| PaintOp::Ground {
        shape: Shape::Rect { x0, y0, x1, y1 },
        ground_cm,
        weight: GROUND_EXACT,
    };
    let spec = RegionSpec::new("lod_arena", REGION_MIN, REGION_MAX, 32, vec![
        // Raised 40 m, and a step 25 m up.
        exact(22900.0, 24700.0, 23300.0, 25100.0, PLATEAU_CM + 4_000),
        exact(23300.0, 24700.0, 23500.0, 25100.0, PLATEAU_CM + 2_500),
        // Lowered 60 m (still far above sea level).
        exact(23500.0, 24800.0, 23800.0, 25400.0, PLATEAU_CM - 6_000),
        PaintOp::GroundRing {
            width_m: 32,
            ground_cm: None,
        },
    ]);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec]).unwrap()));
    let (new, old) = both_maps(&world, index);
    let sim = world.sim();
    let rasters = sim.authored_rasters.as_ref().unwrap();
    let step = sim.max_height / 8191.0;
    let (mut exact_n, mut ring_n, mut other_changed) = (0, 0, 0);
    let (mut exact_err, mut exact_err_old, mut ring_err) = (0.0f32, 0.0f32, 0.0f32);
    for posi in 0..sim.map_size_lg().chunks_len() {
        let pos = uniform_idx_as_vec2(sim.map_size_lg(), posi);
        let wpos = pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
        match rasters.cell_at(wpos) {
            Some(AuthoredCell::Ground { weight, .. }) => {
                let col = world
                    .sample_columns()
                    .get((wpos, index, None))
                    .expect("column on the map");
                if col.alt < CONFIG.sea_level {
                    continue;
                }
                let top = col.alt.floor();
                let err = (decoded_alt(&new, pos) - top).abs();
                if weight == GROUND_EXACT {
                    exact_n += 1;
                    exact_err = exact_err.max(err);
                    exact_err_old = exact_err_old.max((decoded_alt(&old, pos) - top).abs());
                } else {
                    ring_n += 1;
                    ring_err = ring_err.max(err);
                }
            },
            _ => other_changed += usize::from(new.alt.get(pos) != old.alt.get(pos)),
        }
    }
    println!(
        "LOD altitude step {step:.3} m; exact samples {exact_n}: max error {exact_err:.3} m \
         (table only: {exact_err_old:.3} m); ring samples {ring_n}: max error {ring_err:.3} m; \
         samples off the ground layer changed: {other_changed}"
    );
    assert!(exact_n > 100, "the patch covers sample columns");
    assert!(
        exact_err <= 1.0,
        "exact LOD sample error {exact_err} m > 1 m"
    );
    assert!(
        exact_err_old > 10.0,
        "the table really differs on the patch"
    );
    assert_eq!(other_changed, 0);
}
