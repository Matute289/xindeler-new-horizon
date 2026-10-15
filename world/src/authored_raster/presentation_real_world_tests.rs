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
    sim::{FileOpts, GenOpts, WATER_RGB, WorldOpts, water_shade},
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

/// The map as it was before land colours were kept off red zero.
fn map_without_land_clamp(world: &World, index: crate::IndexRef, presented: bool) -> WorldMapMsg {
    world.sim().get_map_opts(index, None, presented, false)
}

/// The world's current maps (presented and table-only) against the maps
/// before the land red clamp: the only differences are land pixels with red
/// zero raised to one (same green and blue), and water pixels are untouched.
/// Returns the number of changed chunks of the presented map.
fn assert_only_land_red_raised(world: &World, index: crate::IndexRef, what: &str) -> usize {
    let (new_p, new_t) = both_maps(world, index);
    let mut changed_first = 0;
    for (presented, new) in [(true, new_p), (false, new_t)] {
        let old = map_without_land_clamp(world, index, presented);
        let sim = world.sim();
        let mut changed = 0usize;
        for posi in 0..sim.map_size_lg().chunks_len() {
            let pos = uniform_idx_as_vec2(sim.map_size_lg(), posi);
            let (n, o) = (map_rgb(&new, pos), map_rgb(&old, pos));
            if n != o {
                changed += 1;
                assert!(
                    o[0] == 0 && n[0] == 1 && n[1] == o[1] && n[2] == o[2],
                    "{what}: {pos:?} changed beyond red 0 -> 1: {o:?} -> {n:?}"
                );
                // Why a River-coloured pixel may be clamped: land colours
                // only change on chunks that are not water.
                let c = sim.get(pos).expect("chunk");
                assert!(
                    c.river.river_kind.is_none() && c.alt >= CONFIG.sea_level,
                    "{what}: {pos:?} changed on a water chunk"
                );
                assert!(
                    !WATER_RGB.contains(&o),
                    "{what}: {pos:?} water colour {o:?} changed"
                );
            }
            // Same alpha and everything else but those pixels.
            assert_eq!(
                new.rgba.get(pos).map(|v| v >> 24),
                old.rgba.get(pos).map(|v| v >> 24)
            );
        }
        println!("{what} (presented {presented}): {changed} chunks of land raised red 0 -> 1");
        if presented {
            changed_first = changed;
        }
    }
    changed_first
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
/// sample column on a ground cell (exact or ring) the presented LOD
/// altitude is within 1 m of the top block the column sampler renders there;
/// samples off the ground layer are unchanged.
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
                    // The rendered column is the authored top block.
                    assert_eq!(Some(col.alt), sim.ground_alt_at(wpos), "at {wpos:?}");
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
    assert!(ring_n > 10, "the ring covers sample columns");
    assert!(ring_err <= 1.0, "ring LOD sample error {ring_err} m > 1 m");
    assert_eq!(other_changed, 0);
}

fn map_rgb(msg: &WorldMapMsg, pos: Vec2<i32>) -> [u8; 3] {
    let [r, g, b, _] = msg.rgba.get(pos).copied().unwrap().to_le_bytes();
    [r, g, b]
}

/// The ground battery over the arena (an `Auto` half and a
/// `sea_fill: AuthoredOnly` half, never a real settlement): the dry pit at
/// -10 m and the dry depression at -1500 m are not coloured as water on the
/// world map (the table-only map paints them as water), the lake whose
/// surface is 20 m below sea level stays water-coloured (shaded by its
/// authored depth, as it lies over table land), and no other chunk
/// of the map changes colour, natural sea and the `Auto` region included,
/// except authored water the table calls land (here lakes over table land
/// above sea level), which turns water-coloured.
#[test]
#[ignore]
fn world_map_colours_dry_ground_below_sea_level_as_ground() {
    let (mut world, index) = generate_world();
    let index = index.as_index_ref();
    world.sim.set_authored_rasters_for_test(Some(
        load_specs(&super::ground_real_world_tests::ground_battery_specs()).unwrap(),
    ));
    let (new, old) = both_maps(&world, index);
    let sim = world.sim();
    let chunk = |x: i32, y: i32| Vec2::new(x, y) / TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    let is_water = |rgb: [u8; 3]| WATER_RGB.contains(&rgb);

    for (pos, what) in [
        (chunk(23488, 24736), "dry pit -10 m"),
        (chunk(23616, 24704), "dry depression -1500 m"),
    ] {
        let wpos = pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
        let col = world
            .sample_columns()
            .get((wpos, index, None))
            .expect("column on the map");
        println!(
            "{what}: column alt {:.1} m, map colour {:?} (table only {:?})",
            col.alt - CONFIG.sea_level,
            map_rgb(&new, pos),
            map_rgb(&old, pos)
        );
        assert!(col.alt < CONFIG.sea_level, "{what} lies below sea level");
        assert!(sim.chunk_is_authored_dry(pos), "{what} is authored dry");
        assert!(
            is_water(map_rgb(&old, pos)),
            "{what}: the table paints water"
        );
        assert!(!is_water(map_rgb(&new, pos)), "{what}: still water colour");
    }

    let lake = chunk(23488, 24928);
    println!(
        "lake -20 m: map colour {:?}, table {:?}, table alt {:?} sea {}, river {:?}, depth {:?}",
        map_rgb(&new, lake),
        map_rgb(&old, lake),
        sim.get(lake).map(|c| c.alt),
        CONFIG.sea_level,
        sim.get(lake).map(|c| c.river.river_kind.is_some()),
        sim.chunk_water_depth(lake),
    );
    assert!(!sim.chunk_is_authored_dry(lake));
    assert!(is_water(map_rgb(&new, lake)), "the lake keeps water colour");
    // The table calls this chunk land (its altitude is above sea level, no
    // river) though its authored lake and column sample lie below sea level:
    // the table-only map draws water by the column altitude, with the depth
    // of the table's water altitude over it; the presented map shades the
    // authored depth, the same shade as any lake over table land.
    assert!(sim.get(lake).unwrap().alt >= CONFIG.sea_level);
    assert!(sim.get(lake).unwrap().river.river_kind.is_none());
    assert_eq!(
        map_rgb(&new, lake),
        water_shade(sim.chunk_water_depth(lake).unwrap().max(0.0) as f64),
        "the below-sea lake over table land has the authored depth shade"
    );

    let (mut changed, mut changed_bad, mut to_water, mut water) = (0, 0, 0, 0);
    for posi in 0..sim.map_size_lg().chunks_len() {
        let pos = uniform_idx_as_vec2(sim.map_size_lg(), posi);
        water += usize::from(is_water(map_rgb(&new, pos)));
        if new.rgba.get(pos) != old.rgba.get(pos) {
            changed += 1;
            let dry_ok = sim.chunk_is_authored_dry(pos) && !is_water(map_rgb(&new, pos));
            // Authored water the table calls land is the other recolouring:
            // a lake of an `AuthoredOnly` region becomes water-coloured, with
            // the shade of the authored depth the sim reports.
            let lake_ok = sim
                .authored_rasters
                .as_ref()
                .and_then(|r| r.chunk_summary(pos))
                .is_some_and(|s| s.authored_only && s.wet_columns > 0)
                && sim.get(pos).is_some_and(|c| c.river.river_kind.is_none())
                && sim.chunk_water(pos).is_some_and(|w| w.wet())
                && is_water(map_rgb(&new, pos));
            if lake_ok && !dry_ok {
                to_water += 1;
                let depth = sim.chunk_water_depth(pos).unwrap().max(0.0);
                let expect = water_shade(depth as f64);
                // The table calls these chunks land: its altitude is at or
                // above sea level and there is no river (the lake surface
                // itself is 20 m below sea level).
                let table_alt = sim.get(pos).unwrap().alt;
                assert!(
                    table_alt >= CONFIG.sea_level,
                    "to-water chunk {pos:?} is table land (table alt {table_alt})"
                );
                println!(
                    "to-water chunk {pos:?}: depth {depth:.1} m, {:?}",
                    map_rgb(&new, pos)
                );
                assert_eq!(
                    map_rgb(&new, pos),
                    expect,
                    "shade of the authored depth at {pos:?}"
                );
            }
            changed_bad += usize::from(!(dry_ok || lake_ok));
        }
    }
    println!(
        "chunks recoloured: {changed} (unexplained: {changed_bad}); to water: {to_water}; \
         water-coloured chunks: {water}"
    );
    assert!(changed >= 2, "the pit and the depression are recoloured");
    assert_eq!(
        changed_bad, 0,
        "only authored dry chunks turned to ground and authored lakes the table calls land turned \
         to water change colour"
    );
    assert_eq!(
        to_water, 9,
        "the 3x3 chunks of the -20 m lake over table land turn water-coloured"
    );
    assert!(water > 1000, "the natural sea keeps its water colour");
}

/// With only `Auto` regions (the battery's `Auto` half over the arena) no
/// chunk is authored dry and the world map image is byte-identical to the
/// table-only one: the dry-ground colour rule never reaches them.
#[test]
#[ignore]
fn map_image_is_byte_identical_with_only_auto_regions() {
    let (mut world, index) = generate_world();
    let mut specs = super::ground_real_world_tests::ground_battery_specs();
    specs.retain(|s| s.sea_fill == super::SeaFill::Auto);
    assert_eq!(specs.len(), 1);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&specs).unwrap()));
    let (new, old) = both_maps(&world, index.as_index_ref());
    let sim = world.sim();
    let dry = (0..sim.map_size_lg().chunks_len())
        .filter(|&i| sim.chunk_is_authored_dry(uniform_idx_as_vec2(sim.map_size_lg(), i)))
        .count();
    println!("Auto regions only: authored dry chunks {dry}");
    assert_eq!(dry, 0);
    assert!(
        new.rgba.raw() == old.rgba.raw(),
        "map image changed with Auto regions"
    );
}

/// Keeping land off red zero changes the map of a procedural world only by
/// raising red 0 -> 1 on land pixels.
#[test]
#[ignore]
fn land_red_clamp_only_raises_red_procedural() {
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
    assert_only_land_red_raised(&world, index.as_index_ref(), "procedural");
}

/// The same for Cromatolis without a manifest (no authored rasters).
#[test]
#[ignore]
fn land_red_clamp_only_raises_red_cromatolis_no_manifest() {
    let (mut world, index) = generate_world();
    world.sim.set_authored_rasters_for_test(None);
    let n = assert_only_land_red_raised(&world, index.as_index_ref(), "Cromatolis no manifest");
    assert!(n > 0, "the real profile has land with red zero");
}

/// The same with only `Auto` regions loaded.
#[test]
#[ignore]
fn land_red_clamp_only_raises_red_auto_only() {
    let (mut world, index) = generate_world();
    let mut specs = super::ground_real_world_tests::ground_battery_specs();
    specs.retain(|s| s.sea_fill == super::SeaFill::Auto);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&specs).unwrap()));
    assert_only_land_red_raised(&world, index.as_index_ref(), "Auto only");
}

/// The client reads a map pixel as water when its red channel is zero (and it
/// is not black). On the real Cromatolis profile map, every pixel it would
/// read as water is a water colour (the depth palette, or the river colour
/// the map draws over a connection): land never reads as water.
#[test]
#[ignore]
fn world_map_land_never_reads_as_water_on_the_client() {
    let (world, index) = generate_world();
    let map = both_maps(&world, index.as_index_ref()).0;
    let sim = world.sim();
    let river = [0, 64, 128];
    let (mut water, mut land) = (0usize, 0usize);
    for posi in 0..sim.map_size_lg().chunks_len() {
        let pos = uniform_idx_as_vec2(sim.map_size_lg(), posi);
        let rgb = map_rgb(&map, pos);
        if rgb[0] == 0 && (rgb[1] > 8 || rgb[2] > 8) {
            water += 1;
            assert!(
                WATER_RGB.contains(&rgb) || rgb == river,
                "{pos:?} reads as water on the client but is not a water colour: {rgb:?}"
            );
            assert!(
                !sim.chunk_is_authored_dry(pos),
                "{pos:?} authored dry ground reads as water: {rgb:?}"
            );
        } else {
            land += 1;
        }
    }
    println!("water {water}, land {land}");
    assert!(water > 1000 && land > 1000);
}
