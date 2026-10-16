//! The wall of an authored dry pit on the Cromatolis calibration arena: no
//! water on it, the column's own strata (earth, then natural stone, with rare
//! ore flecks), never the engine's blue underground rock or a sea-filled sand
//! slope.
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

/// An axis-aligned box `[x0, x1) x [y0, y1)` in blocks.
type Rect = (i32, i32, i32, i32);

/// The pit's exact box (152 m, 36 m inside the region's west edge, as the
/// exporter derives it for a 128 m pit) and its chunk range.
const EXACT: Rect = (22788, 25420, 22940, 25572);

fn inside(r: Rect, p: Vec2<i32>) -> bool { p.x >= r.0 && p.x < r.2 && p.y >= r.1 && p.y < r.3 }

fn in_region(p: Vec2<i32>) -> bool {
    p.x >= REGION_MIN.0 && p.x < REGION_MAX.0 && p.y >= REGION_MIN.1 && p.y < REGION_MAX.1
}

/// Whether a colour is the engine's blue underground rock (`stone_col`
/// 90,110,150 is 60 over red): a natural wall colour is never this blue.
fn is_blue(c: Rgb<u8>) -> bool { c.b as i32 - c.r as i32 > 30 }

#[derive(Default, Debug)]
struct Wall {
    columns: u32,
    liquid_columns: u32,
    sea_sand_columns: u32,
    /// Wall blocks (above the neighbouring ground, under the surface layer).
    wall_blocks: u32,
    wall_columns: u32,
    /// Wall blocks by kind.
    kinds: std::collections::BTreeMap<String, u32>,
    /// `Rock`/`Earth` blocks of the wall band with the engine's blue colour,
    /// in any column of the region beside a pit (flagged or not).
    blue_blocks: u32,
    /// Rock wall blocks that are ore flecks.
    ore_blocks: u32,
    /// Wall columns with earth above rock (a transition), only earth, only
    /// rock.
    transition_columns: u32,
    earth_only_columns: u32,
    rock_only_columns: u32,
    /// Wall columns whose earth lies under rock (the wrong way round).
    inverted_columns: u32,
    /// Columns of the region beside a pit's ground (the scan above covered).
    scanned_columns: u32,
    /// Hash of every wall block, for the determinism check.
    digest: u64,
}

/// Survey the chunks around the pits. `pits` are the ground boxes, `wet` the
/// authored water boxes (their liquid is expected, nothing else's).
fn survey(world: &World, index: crate::IndexRef, pits: &[Rect], wet: &[Rect]) -> Wall {
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
                    let wet_here = wet.iter().any(|r| inside(*r, wpos));
                    w.liquid_columns += (liquid && !wet_here) as u32;
                    w.sea_sand_columns +=
                        (liquid && !wet_here && top_kind == Some(BlockKind::Sand)) as u32;
                    // The whole-box scan: no `Rock`/`Earth` block of the band
                    // between a pit's ground top and the column's own surface
                    // layer, in any column of the region beside a pit (flagged
                    // or not), may carry the engine's blue deep-rock colour.
                    if in_region(wpos) && !wet_here {
                        let mut beside = false;
                        let mut near = false;
                        let mut lo = i32::MAX;
                        for r in pits {
                            // The nearest ground cell of the pit box.
                            let n =
                                Vec2::new(wpos.x.clamp(r.0, r.2 - 1), wpos.y.clamp(r.1, r.3 - 1));
                            let d = (wpos - n).map(|e| e.abs()).reduce_max();
                            if (wpos - n).map(|e| e.abs()).sum() == 1 && in_region(n) {
                                // Beside the pit's ground (flagged or not): the
                                // wall band from its top up to the surface.
                                let top = world.sim().ground_alt_at(n).unwrap();
                                lo = lo.min(top.floor() as i32 + 1);
                                beside = true;
                            }
                            near |= d <= 16;
                        }
                        // Within 16 blocks of a pit, a column whose authored
                        // ground sits below the sim's basement (the ring and
                        // feather around the pit): 40 blocks from the surface.
                        let below = near && col.alt < col.basement;
                        if beside || below {
                            w.scanned_columns += 1;
                            // Under the surface layer (grass/earth in the
                            // biome's own colours, which can be teal).
                            let hi = (col.alt - (1.5 + 2.0 * col.chaos)).ceil() as i32 - 1;
                            let lo = if beside { lo } else { hi - 40 };
                            for z in lo.max(z0)..=hi.min(z1 - 1) {
                                let b = block(z);
                                if matches!(b.kind(), BlockKind::Rock | BlockKind::Earth)
                                    && b.get_color()
                                        .is_some_and(|c| is_blue(c) && !is_ore_colour(c))
                                {
                                    w.blue_blocks += 1;
                                    {
                                        eprintln!(
                                            "BLUE beside={beside} below={below} z={z} alt={} \
                                             base={} flagged={} auth={:?} col={:?}",
                                            col.alt,
                                            col.basement,
                                            col.exposed_wall_floor.is_some(),
                                            col.authored.map(|a| (a.settings.sea_fill, a.cell)),
                                            b.get_color()
                                        );
                                    }
                                }
                            }
                        }
                    }
                    let Some(floor) = col.exposed_wall_floor else {
                        continue;
                    };
                    w.wall_columns += 1;
                    // Under the surface layer, above the neighbouring ground.
                    let lo = floor.floor() as i32 + 1;
                    let hi = col.alt as i32 - 6;
                    let (mut top_earth, mut low_rock) = (None::<i32>, None::<i32>);
                    let mut top_rock = None::<i32>;
                    for z in lo.max(z0)..=hi.min(z1 - 1) {
                        let b = block(z);
                        if !b.is_filled() || b.is_liquid() {
                            continue;
                        }
                        w.wall_blocks += 1;
                        *w.kinds.entry(format!("{:?}", b.kind())).or_default() += 1;
                        let c = b.get_color().unwrap_or_default();
                        w.digest = w.digest.wrapping_mul(1_099_511_628_211).wrapping_add(
                            (z as u64) << 32
                                ^ (c.r as u64) << 16
                                ^ (c.g as u64) << 8
                                ^ c.b as u64
                                ^ (b.kind() as u64) << 48,
                        );
                        match b.kind() {
                            BlockKind::Earth => {
                                top_earth = top_earth.max(Some(z));
                            },
                            BlockKind::Rock => {
                                if is_ore_colour(c) {
                                    w.ore_blocks += 1;
                                } else {
                                    low_rock = Some(low_rock.map_or(z, |e| e.min(z)));
                                    top_rock = top_rock.max(Some(z));
                                }
                            },
                            _ => {},
                        }
                    }
                    match (top_earth, top_rock) {
                        (Some(_), Some(_)) => {
                            w.transition_columns += 1;
                            // Earth must start above where the rock does: the
                            // topmost earth is above the lowest rock.
                            if top_earth < low_rock {
                                w.inverted_columns += 1;
                            }
                        },
                        (Some(_), None) => w.earth_only_columns += 1,
                        (None, Some(_)) => w.rock_only_columns += 1,
                        _ => {},
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
        a.wall_blocks += r.wall_blocks;
        a.wall_columns += r.wall_columns;
        a.blue_blocks += r.blue_blocks;
        a.ore_blocks += r.ore_blocks;
        a.transition_columns += r.transition_columns;
        a.earth_only_columns += r.earth_only_columns;
        a.rock_only_columns += r.rock_only_columns;
        a.inverted_columns += r.inverted_columns;
        a.scanned_columns += r.scanned_columns;
        a.digest = a.digest.wrapping_mul(31).wrapping_add(r.digest);
        for (k, v) in r.kinds {
            *a.kinds.entry(k).or_default() += v;
        }
        a
    })
}

/// An ore fleck colour of the wall (`WALL_ORES`).
fn is_ore_colour(c: Rgb<u8>) -> bool {
    [(150, 92, 62), (92, 150, 100), (212, 178, 62), (38, 38, 44)].contains(&(c.r, c.g, c.b))
}

fn rect((x0, y0, x1, y1): Rect) -> Shape {
    Shape::Rect {
        x0: x0 as f32,
        y0: y0 as f32,
        x1: x1 as f32,
        y1: y1 as f32,
    }
}

/// An `AuthoredOnly` region over the arena box with one exact ground box per
/// `(box, floor in cm)` and the given water boxes `(box, surface_cm, bed_cm)`.
fn spec(pits: &[(Rect, i32)], water: &[(Rect, i32, i32)]) -> RegionSpec {
    let mut ops: Vec<PaintOp> = pits
        .iter()
        .map(|&(r, ground_cm)| PaintOp::Ground {
            shape: rect(r),
            ground_cm,
            weight: GROUND_EXACT,
        })
        .collect();
    ops.extend(water.iter().map(|&(r, surface_cm, bed_cm)| PaintOp::Water {
        shape: rect(r),
        surface_cm,
        bed_cm,
    }));
    if let Some(&(_, surface_cm, _)) = water.first() {
        // The loader wants every wet cell contained by banks.
        ops.push(PaintOp::BankRing {
            width_m: 2,
            bed_cm: Some(surface_cm),
        });
    }
    let mut s = RegionSpec::new("pit_wall", REGION_MIN, REGION_MAX, 32, ops);
    s.sea_fill = SeaFill::AuthoredOnly;
    s
}

fn pit_spec(floor_cm: i32) -> RegionSpec { spec(&[(EXACT, floor_cm)], &[]) }

/// The chunk table of the shipped world holds no deep depression here, so the
/// pit's chunks (chunk x 711..717, y 794..800) are set below the sea by hand:
/// that is the exporter's composed table beside a deep pit, whose
/// interpolation drags the unauthored columns around it under the sea. The
/// real world is otherwise unchanged.
fn fake_deep_pit_table(world: &mut World) {
    for cy in 794..800 {
        for cx in 711..717 {
            world.sim.get_mut(Vec2::new(cx, cy)).unwrap().alt = 120.0;
        }
    }
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
fn pit_walls_are_dry_with_natural_strata_and_no_blue_rock() {
    let (mut world, index) = generate_world();
    let ir = index.as_index_ref();
    fake_deep_pit_table(&mut world);
    for floor_cm in [13_050, -135_950] {
        world
            .sim
            .set_authored_rasters_for_test(Some(load_specs(&[pit_spec(floor_cm)]).unwrap()));
        let w = survey(&world, ir, &[EXACT], &[]);
        println!("floor {floor_cm} cm: {w:#?}");
        assert!(w.wall_blocks > 10_000, "no wall surveyed: {w:?}");
        assert_eq!(w.liquid_columns, 0, "water around the pit: {w:?}");
        assert_eq!(w.sea_sand_columns, 0, "{w:?}");
        assert_wall_strata("pit", &w);
        assert!(w.scanned_columns > 500, "{w:?}");
        // Same chunks twice: the same blocks (position-hash ore and mix).
        let again = survey(&world, ir, &[EXACT], &[]);
        assert_eq!(w.digest, again.digest, "wall blocks are not deterministic");
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

/// The strata contract of a surveyed pit: earth near the top turning to stone
/// below on columns with soil, stone straight away on stone-heavy ones, no
/// blue rock anywhere beside the pit, and ore flecks sparse.
fn assert_wall_strata(name: &str, w: &Wall) {
    assert_eq!(w.blue_blocks, 0, "{name}: blue rock on a wall: {w:?}");
    assert_eq!(w.inverted_columns, 0, "{name}: stone above earth: {w:?}");
    assert!(
        w.transition_columns > 0,
        "{name}: no earth-to-stone wall: {w:?}"
    );
    assert!(
        w.earth_only_columns + w.transition_columns + w.rock_only_columns > 0,
        "{name}: {w:?}"
    );
    let rock = w.kinds.get("Rock").copied().unwrap_or(0);
    assert!(rock > 0, "{name}: no stone at depth: {w:?}");
    assert!(
        (w.ore_blocks as f32) < rock as f32 * 0.01,
        "{name}: ore is not sparse: {w:?}"
    );
}

/// Run one layout through the survey and check the pit invariants: no water but
/// the authored one, no blue rock
/// in any wall block of the region beside a pit (flagged or not).
fn check_layout(name: &str, pits: &[(Rect, i32)], water: &[(Rect, i32, i32)]) {
    let (mut world, index) = generate_world();
    let ir = index.as_index_ref();
    fake_deep_pit_table(&mut world);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[spec(pits, water)]).unwrap()));
    let boxes: Vec<Rect> = pits.iter().map(|p| p.0).collect();
    let wet: Vec<Rect> = water.iter().map(|w| w.0).collect();
    let w = survey(&world, ir, &boxes, &wet);
    println!("{name}: {w:#?}");
    assert!(w.scanned_columns > 200, "{name}: nothing scanned: {w:?}");
    assert_eq!(w.liquid_columns, 0, "{name}: stray water: {w:?}");
    assert_eq!(w.sea_sand_columns, 0, "{name}: {w:?}");
    assert_eq!(w.blue_blocks, 0, "{name}: blue rock on a wall: {w:?}");
    assert_eq!(w.inverted_columns, 0, "{name}: {w:?}");
}

/// Two pits sharing a wall: the shallower one's side facing the deeper one is
/// a wall above the deeper pit's floor.
#[test]
#[ignore]
fn two_adjacent_pits_have_natural_strata_walls() {
    check_layout(
        "adjacent",
        &[
            ((22788, 25420, 22864, 25572), 13_050),
            ((22864, 25420, 22940, 25572), -135_950),
        ],
        &[],
    );
}

/// A pit with a river running along its east side: the river keeps its water,
/// nothing else holds any, and the pit's other walls show the natural strata.
#[test]
#[ignore]
fn pit_beside_a_river_keeps_only_the_rivers_water() {
    check_layout("river", &[(EXACT, 13_050)], &[(
        (22940, 25420, 22960, 25572),
        13_050,
        12_750,
    )]);
}

/// A pit whose box lies at the closest the loader allows to the region's west
/// edge (16 m): the columns outside the
/// region are the natural world's and are not scanned; every column inside it
/// obeys the same invariants.
#[test]
#[ignore]
fn pit_at_the_region_border_has_natural_strata_walls() {
    check_layout(
        "border",
        &[(
            (REGION_MIN.0 + 16, 25420, REGION_MIN.0 + 168, 25572),
            13_050,
        )],
        &[],
    );
}

/// An `AuthoredOnly` region whose edge touches the natural ocean without
/// authoring its shore (the exporter must author it, see [`SeaFill`]). The
/// observed behaviour, pinned here: every unauthored column of the region is
/// dry, its altitude the engine's (under the sea), with solid ground to the
/// bottom of the chunk and no void or hole; the natural chunk next to it
/// keeps its sea, so the water ends in a vertical face at the region's edge.
#[test]
#[ignore]
fn region_edge_on_the_natural_sea_is_a_dry_strip_beside_a_water_face() {
    let (mut world, index) = generate_world();
    let ir = index.as_index_ref();
    // A 3 x 3 block of ocean chunks.
    let ocean = |c: Vec2<i32>| {
        world
            .sim
            .get(c)
            .is_some_and(|n| n.river.is_ocean() && n.alt < 100.0)
    };
    let centre = (40..980)
        .step_by(3)
        .flat_map(|y| (40..980).step_by(3).map(move |x| Vec2::new(x, y)))
        .find(|c| (-3..=3).all(|dy| (-3..=3).all(|dx| ocean(c + Vec2::new(dx, dy)))))
        .expect("an open sea");
    let (cmin, cmax) = (centre - 2, centre + 3);
    let bmin = (cmin.x * 32, cmin.y * 32);
    let bmax = (cmax.x * 32, cmax.y * 32);
    // Only a strip well inside the west side is authored (a dry mound); the rest of
    // the region, and its whole shore, is not.
    let mut s = RegionSpec::new("sea_edge", bmin, bmax, 32, vec![PaintOp::Ground {
        shape: rect((bmin.0 + 32, bmin.1 + 32, bmin.0 + 64, bmax.1 - 32)),
        ground_cm: 8_000,
        weight: GROUND_EXACT,
    }]);
    s.sea_fill = SeaFill::AuthoredOnly;
    // Columns with a void under their topmost solid block (caves of the
    // natural world included), per owned column, and the owned columns with
    // water: first without the region (the baseline), then with it.
    let scan = |world: &World| {
        let mut holes = std::collections::BTreeSet::new();
        let mut liquid_cols = 0u32;
        for cy in cmin.y..cmax.y {
            for cx in cmin.x..cmax.x {
                let c = Vec2::new(cx, cy);
                let (chunk, _) = world
                    .generate_chunk(ir, c, None, || false, None, None)
                    .unwrap();
                let (z0, z1) = (chunk.get_min_z(), chunk.get_max_z());
                for y in 0..32 {
                    for x in 0..32 {
                        let filled = |z: i32| {
                            chunk
                                .get(Vec3::new(x, y, z))
                                .is_ok_and(|b| b.is_filled() && !b.is_liquid())
                        };
                        liquid_cols += (z0..z1)
                            .any(|z| chunk.get(Vec3::new(x, y, z)).is_ok_and(|b| b.is_liquid()))
                            as u32;
                        let hole = match (z0..z1).rev().find(|&z| filled(z)) {
                            Some(top) => (z0..top).any(|z| !filled(z)),
                            None => true,
                        };
                        if hole {
                            holes.insert(((c * 32).x + x, (c * 32).y + y));
                        }
                    }
                }
            }
        }
        (holes, liquid_cols)
    };
    let (baseline_holes, baseline_liquid) = scan(&world);
    world
        .sim
        .set_authored_rasters_for_test(Some(load_specs(&[s]).unwrap()));
    let (holes, region_liquid) = scan(&world);
    println!(
        "region of {} columns: baseline {baseline_liquid} with water, {} with a void; authored \
         {region_liquid} with water, {} with a void",
        (cmax - cmin).product() * 1024,
        baseline_holes.len(),
        holes.len()
    );
    assert!(baseline_liquid > 20_000, "the baseline is open sea");
    assert_eq!(region_liquid, 0, "no sea inside the AuthoredOnly region");
    assert!(
        holes.is_subset(&baseline_holes),
        "the dry columns have no void the natural ones lack: {} new",
        holes.difference(&baseline_holes).count()
    );
    // The natural sea outside still holds water (the vertical face).
    let (outside, _) = world
        .generate_chunk(
            ir,
            Vec2::new(cmin.x - 1, centre.y),
            None,
            || false,
            None,
            None,
        )
        .unwrap();
    let water: u32 = (0..32)
        .flat_map(|y| (0..32).map(move |x| (x, y)))
        .map(|(x, y)| {
            (outside.get_min_z()..outside.get_max_z())
                .any(|z| outside.get(Vec3::new(x, y, z)).is_ok_and(|b| b.is_liquid()))
                as u32
        })
        .sum();
    assert!(water > 500, "the natural sea beside the region: {water}");
}
