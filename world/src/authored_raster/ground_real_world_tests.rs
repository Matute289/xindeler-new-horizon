//! Real-world checks of the authored ground layer (Stage 2) on the
//! Cromatolis map: the B-T battery rows on the synthetic calibration arena
//! (the empty plateau of `real_world_tests`, never a real settlement), and
//! the below-sea-level rules against the real sim table.
//!
//! ```text
//! VELOREN_ASSETS=$PWD/assets cargo test -p xindeler-world --release \
//!     authored_raster::ground_real_world_tests -- --ignored --nocapture
//! ```

use super::{
    AuthoredCell, SEA_TOP_BLOCK, SeaFill,
    format::GROUND_EXACT,
    queries::column_is_ocean,
    real_world_tests::{
        PLATEAU_CM, REGION_MAX, REGION_MIN, generate_world, is_natural_ground, load_specs,
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
use vek::*;

/// The arena split in two regions at this x: `Auto` to the west,
/// `AuthoredOnly` to the east.
const SPLIT_X: i32 = 23360;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape { Shape::Rect { x0, y0, x1, y1 } }

fn ground(name: &'static str, shape: Shape, ground_cm: i32) -> (&'static str, PaintOp) {
    (name, PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    })
}

/// The named scenarios of the `Auto` region (painted in order, last wins):
/// B-T6 plaza (the exact base at the plateau height, which the engine renders
/// with +0.6..+7.4 m of noise), B-T1 trenches and B-T2 ridges 1..16 m wide,
/// B-T3 a +10 m plateau and a +3 m step, B-T4 ramps 1:4 and 1:20, B-T5
/// cliffs of 40 m and 90 m, B-T13 a 4 x 4 m pillar at the top of the height
/// profile (block 5140).
fn auto_scenarios() -> Vec<(&'static str, PaintOp)> {
    let p = PLATEAU_CM;
    let mut v = vec![ground(
        "B-T6 plaza",
        rect(22800.0, 24624.0, 23312.0, 25552.0),
        p,
    )];
    for (k, w) in [1.0f32, 2.0, 4.0, 8.0, 16.0].into_iter().enumerate() {
        let x0 = 22850.0 + 40.0 * k as f32;
        v.push(ground(
            "B-T1 trench",
            rect(x0, 24700.0, x0 + w, 24900.0),
            p - 600,
        ));
        v.push(ground(
            "B-T2 ridge",
            rect(x0, 24950.0, x0 + w, 25150.0),
            p + 600,
        ));
    }
    v.push(ground(
        "B-T3 plateau",
        rect(22850.0, 25200.0, 22950.0, 25300.0),
        p + 1000,
    ));
    v.push(ground(
        "B-T3 step",
        rect(23000.0, 25200.0, 23300.0, 25300.0),
        p + 300,
    ));
    v.push(("B-T4 ramp 1:4", PaintOp::GroundPlane {
        shape: rect(23050.0, 24700.0, 23150.0, 24800.0),
        origin: (23050.0, 24700.0),
        origin_cm: p,
        cm_per_m: (25.0, 0.0),
    }));
    v.push(("B-T4 ramp 1:20", PaintOp::GroundPlane {
        shape: rect(23150.0, 24700.0, 23300.0, 24800.0),
        origin: (23150.0, 24700.0),
        origin_cm: p,
        cm_per_m: (0.0, 5.0),
    }));
    v.push(ground(
        "B-T5 cliff 40 m",
        rect(23050.0, 24850.0, 23300.0, 24950.0),
        p + 4000,
    ));
    v.push(ground(
        "B-T5 cliff 90 m",
        rect(23050.0, 25000.0, 23150.0, 25100.0),
        p + 9000,
    ));
    v.push(ground(
        "B-T13 top of the profile",
        rect(23200.0, 25400.0, 23204.0, 25404.0),
        514_050,
    ));
    v
}

/// The `AuthoredOnly` region: the plateau base, B-T14 (a) a dry pit whose
/// floor is block 130 (-10 m), (b) a dry depression at block -1360 (-1500
/// m), B-T13 the bottom of the height profile (block -1860), (c) a lake
/// whose surface is block 120 (-20 m) over a bed at 110, held by the
/// plateau's exact ground around it.
fn authored_only_scenarios() -> Vec<(&'static str, PaintOp)> {
    let p = PLATEAU_CM;
    let lake = rect(23450.0, 24900.0, 23550.0, 25000.0);
    vec![
        ground("B-T14 base", rect(23408.0, 24624.0, 23888.0, 25552.0), p),
        ground(
            "B-T14a dry pit -10 m",
            rect(23450.0, 24700.0, 23550.0, 24800.0),
            13_050,
        ),
        ground(
            "B-T14b dry depression -1500 m",
            rect(23600.0, 24700.0, 23650.0, 24750.0),
            -135_950,
        ),
        ground(
            "B-T13 bottom of the profile",
            rect(23700.0, 24700.0, 23720.0, 24720.0),
            -185_950,
        ),
        ("B-T14c lake -20 m", PaintOp::ClearGround {
            shape: lake.clone(),
        }),
        ("B-T14c lake -20 m", PaintOp::Water {
            shape: lake,
            surface_cm: 12_050,
            bed_cm: 11_050,
        }),
    ]
}

fn spec(id: &str, min: (i32, i32), max: (i32, i32), ops: &[(&str, PaintOp)]) -> RegionSpec {
    RegionSpec::new(
        id,
        min,
        max,
        32,
        ops.iter().map(|(_, op)| op.clone()).collect(),
    )
}

/// Both battery regions: `Auto` west of [`SPLIT_X`], `AuthoredOnly` east.
pub fn ground_battery_specs() -> Vec<RegionSpec> {
    let auto = spec(
        "arena_ground_auto",
        REGION_MIN,
        (SPLIT_X, REGION_MAX.1),
        &auto_scenarios(),
    );
    let mut own = spec(
        "arena_ground_own",
        (SPLIT_X, REGION_MIN.1),
        REGION_MAX,
        &authored_only_scenarios(),
    );
    own.sea_fill = SeaFill::AuthoredOnly;
    vec![auto, own]
}

/// The scenario that painted `wpos` last (its label).
fn scenario_at(wpos: Vec2<i32>) -> &'static str {
    let (px, py) = (wpos.x as f32 + 0.5, wpos.y as f32 + 0.5);
    let inside = |op: &PaintOp| {
        let shape = match op {
            PaintOp::Ground { shape, .. }
            | PaintOp::GroundPlane { shape, .. }
            | PaintOp::Water { shape, .. }
            | PaintOp::ClearGround { shape } => shape,
            _ => return false,
        };
        matches!(*shape, Shape::Rect { x0, y0, x1, y1 } if px >= x0 && px < x1 && py >= y0 && py < y1)
    };
    let list = if wpos.x < SPLIT_X {
        auto_scenarios()
    } else {
        authored_only_scenarios()
    };
    list.iter()
        .rev()
        .find(|(_, op)| inside(op))
        .map_or("none", |(n, _)| *n)
}

#[derive(Default, Debug, Clone)]
struct Row {
    columns: u32,
    /// `ColumnSample::alt` is the authored top block's altitude, warp 0.
    sampler_exact: u32,
    /// A structure block (tree root, boulder: a non-natural block, or a
    /// natural-kind block above the sampler's surface) or a sprite sits at or
    /// below the authored top block, in place of ground: counted, not judged.
    decorated: u32,
    /// Undecorated columns whose topmost natural ground block is the
    /// authored block.
    top_exact: u32,
    /// Undecorated columns whose top is *below* the authored block: a layer
    /// that carves after the column sampler (procedural cave mouths from
    /// below, which Stage 2 keeps; civ paths, which are routed onto exact
    /// cells by a later slice). Reported, split by whether a path runs there.
    carved: u32,
    carved_on_path: u32,
    /// Undecorated columns whose top is *above* the authored block: engine
    /// relief leaking through. Must be 0.
    raised: u32,
    /// Water blocks in a dry column (above sea level, or anywhere in an
    /// `AuthoredOnly` region).
    water_in_dry: u32,
    /// Wet columns whose top water block is the authored surface.
    water_top_exact: u32,
    wet: u32,
}

impl Row {
    fn add(&mut self, o: &Row) {
        self.columns += o.columns;
        self.sampler_exact += o.sampler_exact;
        self.decorated += o.decorated;
        self.top_exact += o.top_exact;
        self.carved += o.carved;
        self.carved_on_path += o.carved_on_path;
        self.raised += o.raised;
        self.water_in_dry += o.water_in_dry;
        self.water_top_exact += o.water_top_exact;
        self.wet += o.wet;
    }
}

/// Generate `chunks` and judge every column that the rasters fix exactly.
fn judge(
    world: &World,
    index: crate::IndexRef,
    chunks: &[Vec2<i32>],
) -> (
    std::collections::BTreeMap<&'static str, Row>,
    Vec<String>,
    f64,
) {
    let rasters = world.sim.authored_rasters.as_ref().unwrap();
    let t = std::time::Instant::now();
    let per_chunk: Vec<(Vec<(&'static str, Row)>, Vec<String>)> = chunks
        .par_iter()
        .map(|c| {
            let (chunk, _) = world
                .generate_chunk(index, *c, None, || false, None, None)
                .unwrap();
            let block = |x: i32, y: i32, z: i32| {
                chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty())
            };
            let mut rows = Vec::new();
            let mut ex = Vec::new();
            for y in 0..32 {
                for x in 0..32 {
                    let wpos = c * 32 + Vec2::new(x, y);
                    let Some(acol) = rasters.column(wpos) else {
                        continue;
                    };
                    let Some(exact) = acol.cell.exact_ground_block() else {
                        continue;
                    };
                    let col = world.sample_columns().get((wpos, index, None)).unwrap();
                    let mut r = Row {
                        columns: 1,
                        ..Default::default()
                    };
                    r.sampler_exact +=
                        (col.alt == top_block_alt(exact) && col.warp_factor == 0.0) as u32;
                    let cap = col.alt as i32;
                    let (z0, z1) = (chunk.get_min_z(), chunk.get_max_z());
                    // Only what replaces the ground counts: a structure block
                    // (tree root, boulder) or a sprite (bones, graves) at or
                    // below the authored top block. Canopies and trunks above
                    // it do not touch the ground.
                    let decorated = (z0..=exact.min(z1 - 1)).any(|z| {
                        let b = block(x, y, z);
                        let structure = b.is_filled()
                            && !b.is_liquid()
                            && (!is_natural_ground(&b) || z > cap)
                            && b.kind() != BlockKind::Ice;
                        let sprite = b
                            .get_sprite()
                            .is_some_and(|s| s != common::terrain::SpriteKind::Empty);
                        structure || sprite
                    });
                    let top = (z0..=cap.min(z1 - 1))
                        .rev()
                        .find(|z| is_natural_ground(&block(x, y, *z)));
                    let dry = !acol.cell.is_wet()
                        && (acol.settings.sea_fill == SeaFill::AuthoredOnly
                            || exact >= SEA_TOP_BLOCK);
                    if dry {
                        r.water_in_dry +=
                            (z0..z1).any(|z| block(x, y, z).kind() == BlockKind::Water) as u32;
                    }
                    if let AuthoredCell::Wet { surface_block, .. } = acol.cell {
                        r.wet += 1;
                        let wtop = (z0..z1)
                            .rev()
                            .find(|z| block(x, y, *z).kind() == BlockKind::Water);
                        r.water_top_exact += (wtop == Some(surface_block)) as u32;
                    }
                    if decorated {
                        r.decorated += 1;
                    } else if top == Some(exact) {
                        r.top_exact += 1;
                    } else if top.is_some_and(|t| t < exact) {
                        r.carved += 1;
                        r.carved_on_path += col.path.is_some_and(|(d, ..)| d < 16.0) as u32;
                    } else {
                        r.raised += 1;
                    }
                    if !decorated && top != Some(exact) && ex.len() < 4 {
                        ex.push(format!(
                            "{wpos:?} ({}): authored top block {exact}, natural ground top \
                             {top:?}, sampler alt {}",
                            scenario_at(wpos),
                            col.alt
                        ));
                    }
                    if r.sampler_exact == 0 && ex.len() < 4 {
                        ex.push(format!(
                            "{wpos:?} ({}): sampler alt {} warp {} for block {exact}",
                            scenario_at(wpos),
                            col.alt,
                            col.warp_factor
                        ));
                    }
                    rows.push((scenario_at(wpos), r));
                }
            }
            (rows, ex)
        })
        .collect();
    let secs = t.elapsed().as_secs_f64();
    let mut table = std::collections::BTreeMap::<&'static str, Row>::new();
    let mut examples = Vec::new();
    for (rows, ex) in per_chunk {
        for (name, r) in rows {
            table.entry(name).or_default().add(&r);
        }
        examples.extend(ex);
    }
    (table, examples, secs)
}

/// B-T1...B-T6, B-T13 and the load-time B-T14 cases: on every exact ground
/// cell the column sampler returns the authored block (no spline, noise,
/// cliff, warp, mesa or registration), and on every column no structure
/// occupies the topmost natural ground block is exactly `floor(cm / 100)`;
/// `AuthoredOnly` ground below sea level is dry, the lake below sea level has
/// its authored surface.
#[test]
#[ignore]
fn ground_battery_renders_exactly_in_the_arena() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let rasters = load_specs(&ground_battery_specs()).unwrap();
    rasters
        .check_consistency_with_sim("test", &world.sim)
        .expect("the battery passes the sim-table checks");
    world.sim.set_authored_rasters_for_test(Some(rasters));
    let cmin = Vec2::from(REGION_MIN) / 32;
    let cmax = Vec2::from(REGION_MAX) / 32;
    let chunks: Vec<Vec2<i32>> = (cmin.y..cmax.y)
        .flat_map(|cy| (cmin.x..cmax.x).map(move |cx| Vec2::new(cx, cy)))
        .collect();
    let (table, examples, secs) = judge(&world, index_ref, &chunks);
    println!(
        "{} chunks in {secs:.1} s; per scenario: {table:#?}; examples {examples:#?}",
        chunks.len()
    );
    let mut all = Row::default();
    for r in table.values() {
        all.add(r);
    }
    for (name, r) in &table {
        assert!(r.columns > 0, "{name}: no columns");
        assert_eq!(r.sampler_exact, r.columns, "{name}: sampler not exact");
        assert_eq!(
            r.raised, 0,
            "{name}: engine relief above the authored ground"
        );
        assert_eq!(
            r.top_exact + r.carved,
            r.columns - r.decorated,
            "{name}: undecorated top block neither exact nor carved"
        );
        assert!(
            r.top_exact > 0 || r.columns < 64,
            "{name}: every column decorated"
        );
        assert_eq!(r.water_in_dry, 0, "{name}: water in dry columns");
        assert_eq!(r.water_top_exact, r.wet, "{name}: water top not exact");
    }
    assert!(all.columns > 800_000, "{} exact columns", all.columns);
    // Carving after the sampler (cave mouths, paths) is rare on the plateau.
    assert!(
        all.carved as f64 <= 0.01 * (all.columns - all.decorated) as f64,
        "{} of {} undecorated exact columns carved",
        all.carved,
        all.columns - all.decorated
    );
}

/// B-T12: exact ground 20 m below sea level in a `sea_fill: Auto` region is
/// a sea floor where the sim table calls the chunks ocean (exact under sea
/// water, `column_is_ocean`), and is refused at start-up inland, with a
/// message that names `sea_fill: AuthoredOnly`.
#[test]
#[ignore]
fn sea_floor_below_sea_level_in_ocean_chunks_and_refused_inland() {
    let (mut world, index) = generate_world();
    let index_ref = index.as_index_ref();
    let far_from_sites = |c: Vec2<i32>| {
        let w = c * 32;
        index.sites.values().all(|site| {
            let b = site.bounds();
            let dx = (b.min.x - w.x).max(w.x - b.max.x).max(0);
            let dy = (b.min.y - w.y).max(w.y - b.max.y).max(0);
            dx.max(dy) > 600
        })
    };
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
            }) && far_from_sites(*c)
        })
        .expect("open sea far from sites");
    let floor = |id: &str, centre: Vec2<i32>| {
        let mut s = RegionSpec::new(
            id,
            ((centre - 3) * 32).into_tuple(),
            ((centre + 4) * 32).into_tuple(),
            32,
            vec![PaintOp::Ground {
                shape: rect(
                    ((centre.x - 1) * 32) as f32,
                    ((centre.y - 1) * 32) as f32,
                    ((centre.x + 2) * 32) as f32,
                    ((centre.y + 2) * 32) as f32,
                ),
                ground_cm: 12_050,
                weight: GROUND_EXACT,
            }],
        );
        // The patch edits the natural sea: its unauthored columns keep it.
        s.allow_partial = true;
        s
    };
    let ocean = load_specs(&[floor("sea_floor", sea)]).unwrap();
    ocean
        .check_consistency_with_sim("test", &world.sim)
        .expect("a sea floor in ocean chunks is fine");
    world.sim.set_authored_rasters_for_test(Some(ocean));
    let (table, examples, _) = judge(&world, index_ref, &[sea]);
    println!("sea floor at chunk {sea:?}: {table:#?} {examples:#?}");
    let r = &table["none"];
    assert_eq!(r.columns, 32 * 32);
    assert_eq!(r.sampler_exact, r.columns);
    assert_eq!(r.raised, 0);
    assert_eq!(r.top_exact + r.carved, r.columns - r.decorated);
    let col = world
        .sample_columns()
        .get((sea * 32 + 16, index_ref, None))
        .unwrap();
    assert!(col.water_level > col.alt + 18.0, "sea water over the floor");
    assert!(column_is_ocean(&col), "a sea floor is ocean");
    // The water queries agree with the rendered sea.
    let w = world.sim.water_at(sea * 32 + 16).unwrap();
    assert!(w.wet, "{w:?}");
    assert_eq!(w.bed_alt, Some(121.0));
    let cw = world.sim.chunk_water(sea).unwrap();
    assert!(cw.ocean && cw.underwater, "{cw:?}");
    // The same patch inland (the arena plateau): refused, naming the fix.
    let inland = load_specs(&[floor("inland", Vec2::new(23200, 25100) / 32)]).unwrap();
    let e = inland
        .check_consistency_with_sim("test", &world.sim)
        .unwrap_err();
    assert!(e.0.contains("sea_fill: AuthoredOnly"), "{e}");
}
