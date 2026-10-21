//! Asset-free tests of the authored raster loader, validator and sampler.
//! The real-world identity and battery checks live in
//! `crate::cromatolis_generation_tests` (they need the LFS map).

use super::{
    writer::{PaintOp, RegionSpec, Shape, build_region, manifest},
    *,
};
use std::collections::HashMap as StdHashMap;

const MAP: Vec2<i32> = Vec2::new(32768, 32768);

fn load(regions: &[writer::BuiltRegion]) -> Result<AuthoredRasters, LoadError> {
    let files: StdHashMap<(String, i32, i32), Vec<u8>> = regions
        .iter()
        .flat_map(|r| {
            r.tiles
                .iter()
                .map(|((tx, ty), b)| ((r.manifest.id.clone(), *tx, *ty), b.clone()))
        })
        .collect();
    let ground: StdHashMap<(String, i32, i32), Vec<u8>> = regions
        .iter()
        .flat_map(|r| {
            r.ground_tiles
                .iter()
                .map(|((tx, ty), b)| ((r.manifest.id.clone(), *tx, *ty), b.clone()))
        })
        .collect();
    let fetch = move |id: &str, layer: LayerKind, tx: i32, ty: i32| {
        match layer {
            LayerKind::Water => &files,
            LayerKind::Ground => &ground,
        }
        .get(&(id.to_string(), tx, ty))
        .cloned()
        .ok_or_else(|| format!("missing tile {id} {tx} {ty}"))
    };
    AuthoredRasters::from_manifest(manifest(regions), MAP, "test", &fetch)
}

fn region(
    id: &str,
    min: (i32, i32),
    max: (i32, i32),
    feather_m: i32,
    ops: Vec<PaintOp>,
) -> RegionSpec {
    RegionSpec::new(id, min, max, feather_m, ops)
}

/// A 20 m river along x at y = 1100..1120 (x 1056..1504, a chunk inside
/// the box at each end), water top block 238 over bed
/// 232, a 2 m bank ring at 239 m.
fn river_spec() -> RegionSpec {
    region("river", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1056.0,
                y0: 1100.0,
                x1: 1504.0,
                y1: 1120.0,
            },
            surface_cm: 23_868,
            bed_cm: 23_268,
        },
        PaintOp::BankRing {
            width_m: 2,
            bed_cm: Some(23_918),
        },
    ])
}

#[test]
fn columns_outside_every_region_are_none() {
    let ar = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    for p in [
        Vec2::new(1023, 1100),
        Vec2::new(1536, 1100),
        Vec2::new(1200, 1023),
        Vec2::new(1200, 1216),
        Vec2::new(0, 0),
        Vec2::new(-5, 99_999),
    ] {
        assert_eq!(ar.column(p), None, "{p:?}");
    }
    assert!(ar.column(Vec2::new(1024, 1024)).is_some());
    assert!(ar.column(Vec2::new(1535, 1215)).is_some());
}

#[test]
fn cells_quantise_with_integer_arithmetic() {
    let ar = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    let wet = ar.column(Vec2::new(1300, 1110)).unwrap();
    assert_eq!(wet.cell, AuthoredCell::Wet {
        surface_block: 238,
        bed_block: 232
    });
    assert_eq!(wet.water_dist, Some(0.0));
    let bank = ar.column(Vec2::new(1300, 1120)).unwrap();
    assert_eq!(bank.cell, AuthoredCell::Bank { bed_block: 239 });
    assert_eq!(bank.water_dist, Some(1.0));
    let none = ar.column(Vec2::new(1300, 1150)).unwrap();
    assert_eq!(none.cell, AuthoredCell::None);
    // 1150 is 31 columns north of the last wet row (1119): chamfer 3-4 is
    // exact along an axis.
    assert_eq!(none.water_dist, Some(31.0));
    assert_eq!(ar.column(Vec2::new(1300, 1200)).unwrap().water_dist, None);
    assert_eq!(
        ar.water_at(Vec2::new(1300, 1110)),
        Some(AuthoredWater {
            surface_block: 238,
            bed_block: 232,
        })
    );
    assert_eq!(ar.water_at(Vec2::new(1300, 1120)), None);
}

#[test]
fn negative_centimetres_floor_towards_minus_infinity() {
    // Bank cells may not go below the ocean, so test the arithmetic directly.
    let raw = format::WaterTile {
        origin: Vec2::zero(),
        base_cm: -250,
        surface: vec![format::NONE; TILE_CELLS].into(),
        bed: {
            let mut b = vec![format::NONE; TILE_CELLS];
            b[0] = 0; // -250 cm
            b[1] = 249; // -1 cm
            b[2] = 250; // 0 cm
            b.into()
        },
    };
    assert_eq!(cell_of(Some(&raw), None, 0), AuthoredCell::Bank {
        bed_block: -3
    });
    assert_eq!(cell_of(Some(&raw), None, 1), AuthoredCell::Bank {
        bed_block: -1
    });
    assert_eq!(cell_of(Some(&raw), None, 2), AuthoredCell::Bank {
        bed_block: 0
    });
}

#[test]
fn feather_weight_runs_from_the_edge_to_the_core() {
    let ar = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    let w = |x| ar.column(Vec2::new(x, 1170)).unwrap().weight;
    assert!(w(1024) > 0.0 && w(1024) < 0.01, "edge column {}", w(1024));
    assert!(w(1040) > 0.45 && w(1040) < 0.55, "mid feather {}", w(1040));
    assert_eq!(w(1056), 1.0);
    assert_eq!(w(1300), 1.0);
    assert!(w(1535) < 0.01);
    let mut prev = 0.0;
    for x in 1024..1060 {
        assert!(w(x) >= prev, "monotone at {x}");
        prev = w(x);
    }
}

const SETTINGS: RegionSettings = RegionSettings {
    suppress_procedural_in_water: true,
    exclude_procedural_margin_m: 0.0,
    aquatic_profile: None,
    sea_fill: SeaFill::Auto,
    site_levelling: true,
};

fn engine() -> EngineColumn {
    EngineColumn {
        alt: 244.3,
        water_level: 241.2,
        water_dist: Some(-3.0),
        warp_factor: 0.7,
        cliff_offset: 2.0,
        riverless_alt: 245.0,
    }
}

fn dry() -> DryTerrain {
    DryTerrain {
        riverless_alt: 245.0,
        riverless_alt_delta: 3.0,
        warp: 1.5,
        max_warp: 1.0,
        base_sea_level: 139.01,
        site_prefer_alt: (0.1, f32::NEG_INFINITY),
    }
}

#[test]
fn resolve_wet_and_bank_are_exact_at_any_weight() {
    for weight in [0.001, 0.5, 1.0] {
        let wet = AuthoredColumn {
            settings: SETTINGS,
            weight,
            cell: AuthoredCell::Wet {
                surface_block: 238,
                bed_block: 232,
            },
            water_dist: Some(0.0),
            ground_dist: None,
            natural: false,
        }
        .resolve(engine(), dry());
        assert_eq!(wet.alt, 232.5);
        assert_eq!(wet.water_level, 238.5);
        assert_eq!(wet.riverless_alt, 232.5, "paths and trees see the bed");
        assert_eq!(wet.warp_factor, 0.0);
        assert_eq!(wet.cliff_offset, 0.0);
        assert_eq!(wet.alt as i32, 232, "top ground block");
        assert_eq!((wet.water_level - 0.5) as i32, 238);
        // Water fills z < water_level: 238 is water, 239 is not.
        assert!(238.0 < wet.water_level && 239.0 >= wet.water_level);
        let bank = AuthoredColumn {
            settings: SETTINGS,
            weight,
            cell: AuthoredCell::Bank { bed_block: 239 },
            water_dist: Some(1.0),
            ground_dist: None,
            natural: false,
        }
        .resolve(engine(), dry());
        assert_eq!(bank.alt, 239.5);
        assert_eq!(bank.water_level, 139.01);
        assert_eq!(bank.riverless_alt, 239.5);
    }
}

/// The blocks `Ice` takes in a column for a band, the way `BlockGen` decides
/// it: `|z - centre| < half_depth`, over the water blocks `bed + 1..=surface`.
fn ice_blocks(band: (f32, f32), bed: i32, surface: i32) -> Vec<i32> {
    (bed + 1..=surface + 3)
        .filter(|&z| (z as f32 - band.0).abs() < band.1)
        .collect()
}

#[test]
fn authored_ice_band_only_grows_downward_from_the_surface() {
    let wet = |surface_block, bed_block| AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::Wet {
            surface_block,
            bed_block,
        },
        water_dist: Some(0.0),
        ground_dist: None,
        natural: false,
    };
    for (surface, bed) in [(238, 232), (238, 237), (238, 236), (4639, 4633), (12, 5)] {
        let col = wet(surface, bed);
        let wl = surface as f32 + 0.5;
        // Every natural half-depth the engine can produce (up to ~3.5 + cliff).
        for step in 0..=160 {
            let d = step as f32 * 0.05;
            let (centre, half) = col.ice_band(wl, d).unwrap();
            let ice = ice_blocks((centre, half), bed, surface);
            let t = ice.len() as i32;
            assert!(
                ice.iter().all(|&z| z <= surface),
                "ice above the surface block {surface} at d={d}: {ice:?}"
            );
            assert!(t <= ICE_MAX_BLOCKS, "{t} ice blocks at d={d}");
            // Contiguous from the surface block down, and a liquid block left.
            assert!(ice.iter().rev().zip(0..).all(|(&z, k)| z == surface - k));
            assert!(
                surface - bed - t >= 1,
                "no liquid left under the ice at d={d}"
            );
            // The natural band's part below the surface, truncated.
            let natural_below = (bed + 1..=surface)
                .filter(|&z| (z as f32 - wl).abs() < d)
                .count() as i32;
            assert_eq!(t, natural_below.min(ICE_MAX_BLOCKS).min(surface - bed - 1));
            assert!(
                half == 0.0 || centre + half <= wl,
                "band top over the water"
            );
        }
    }
    // A column that does not freeze stays unfrozen.
    let (c, h) = wet(238, 232).ice_band(238.5, 0.0).unwrap();
    assert!(ice_blocks((c, h), 232, 238).is_empty());
    // Warm natural half-depth values the noise gives (<= 0.5) never freeze.
    let (c, h) = wet(238, 232).ice_band(238.5, 0.5).unwrap();
    assert!(ice_blocks((c, h), 232, 238).is_empty());
}

#[test]
fn one_block_authored_ice_counts_as_ice_for_wildlife() {
    let col = AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::Wet {
            surface_block: 238,
            bed_block: 232,
        },
        water_dist: Some(0.0),
        ground_dist: None,
        natural: false,
    };
    // Natural half-depth 1.0 -> one block of ice below the surface block.
    let (centre, half) = col.ice_band(238.5, 1.0).unwrap();
    assert_eq!(half, 0.5);
    let gate = crate::layer::wildlife::is_ice_column;
    // The authored wet one-block band counts as ice (when under water).
    assert!(gate(half, Some(&col), true));
    assert!(!gate(half, Some(&col), false));
    assert!(!gate(0.0, Some(&col), true));
    // Natural columns keep the base threshold: 0.3 and 0.5 are not ice, 0.6 is.
    assert!(!gate(0.3, None, true));
    assert!(!gate(0.5, None, true));
    assert!(gate(0.6, None, true));
    // An authored dry cell is as natural as no authored column at all.
    let dry = AuthoredColumn {
        cell: AuthoredCell::None,
        ..col
    };
    assert!(!gate(0.3, Some(&dry), true));
    assert!(!gate(0.5, Some(&dry), true));
    assert!(gate(0.6, Some(&dry), true));
    // The Ice spawn height (`ice_centre + 1 + ice_depth`) is one block over
    // the top of the band, i.e. over the drawn surface block.
    assert_eq!(centre + 1.0 + half, 238.5 + 1.0);
}

#[test]
fn flooded_authored_water_keeps_ice_in_the_drawn_surface_block() {
    let col = AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::Wet {
            surface_block: 238,
            bed_block: 232,
        },
        water_dist: Some(0.0),
        ground_dist: None,
        natural: false,
    };
    // A regional flood lifted the water level well above the drawn surface.
    let wl = 238.5 + 6.0;
    for d in [0.6_f32, 1.5, 2.5, 3.5] {
        let (centre, half) = col.ice_band(wl, d).unwrap();
        let ice = ice_blocks((centre, half), 232, 238 + 8);
        assert!(!ice.is_empty() && ice.iter().all(|&z| z <= 238), "{ice:?}");
        assert!(centre + half <= 238.5, "band top over the drawn surface");
    }
}

#[test]
fn z_limit_covers_every_flooded_authored_water_voxel() {
    let col = AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::Wet {
            surface_block: 238,
            bed_block: 232,
        },
        water_dist: Some(0.0),
        ground_dist: None,
        natural: false,
    };
    for rise in [0.0_f32, 2.0, 5.0, 6.5, 12.0, 40.0] {
        let wl = 238.5 + rise;
        for d in [0.0_f32, 0.6, 1.5, 3.5] {
            let (centre, half) = col.ice_band(wl, d).unwrap();
            // The caller fills `min_z..max_z as i32`; the ground bound is below
            // the drawn surface here.
            let max = crate::block::z_max(240.0, wl, half);
            // The original form of the bound, unaffected by the ice band's centre.
            assert_eq!(max, (wl + 2.0 + half).max(240.0));
            assert!(max >= wl + 2.0, "bound below the water column: {max}");
            let max_z = max as i32;
            for z in 232..=wl as i32 + 3 {
                let wet = (z as f32) < wl;
                let ice = (z as f32 - centre).abs() < half;
                assert!(
                    !(wet || ice) || z < max_z,
                    "voxel {z} (wl {wl}, d {d}) is skipped by max_z {max_z}"
                );
            }
        }
    }
}

#[test]
fn ice_band_is_authored_wet_only() {
    let col = |cell| AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell,
        water_dist: Some(1.0),
        ground_dist: None,
        natural: false,
    };
    assert_eq!(
        col(AuthoredCell::Bank { bed_block: 239 }).ice_band(139.01, 2.0),
        None
    );
    assert_eq!(
        col(AuthoredCell::Ground {
            block: 239,
            weight: GROUND_EXACT
        })
        .ice_band(139.01, 2.0),
        None
    );
    assert_eq!(col(AuthoredCell::None).ice_band(139.01, 2.0), None);
}

#[test]
fn resolve_none_blends_engine_into_dry_terrain() {
    let col = |weight, water_dist| AuthoredColumn {
        settings: SETTINGS,
        weight,
        cell: AuthoredCell::None,
        water_dist,
        ground_dist: None,
        natural: false,
    };
    // Weight 0 is the engine's terrain, never its water.
    let r = col(0.0, None).resolve(engine(), dry());
    assert_eq!(r.alt, engine().alt);
    assert_eq!(r.water_level, 139.01);
    // The water level is never blended (only authored water in a region).
    assert_eq!(col(0.4, None).resolve(engine(), dry()).water_level, 139.01);
    assert_eq!(r.water_dist, Some(-3.0));
    // Core, far from authored water: full warp, the dry terrain.
    let r = col(1.0, None).resolve(engine(), dry());
    assert_eq!(r.alt, 245.0 + 3.0 + 1.5);
    assert_eq!(r.warp_factor, 1.0);
    assert_eq!(r.water_dist, None);
    // Core, 32 m from authored water: half the warp.
    let r = col(1.0, Some(32.0)).resolve(engine(), dry());
    assert_eq!(r.alt, 245.0 + 1.5 + 0.75);
    // Below sea level the dry terrain is held at the engine's floor.
    let low = DryTerrain {
        riverless_alt: 100.0,
        riverless_alt_delta: 0.0,
        warp: 0.0,
        ..dry()
    };
    assert_eq!(col(1.0, None).resolve(engine(), low).alt, 139.51);
}

fn expect_error(regions: Vec<writer::BuiltRegion>, needle: &str) {
    match load(&regions) {
        Ok(_) => panic!("expected an error containing {needle:?}"),
        Err(e) => assert!(e.0.contains(needle), "{needle:?} not in {}", e.0),
    }
}

#[test]
fn water_next_to_unauthored_ground_is_refused() {
    let mut spec = river_spec();
    spec.ops.pop();
    expect_error(vec![build_region(&spec).unwrap()], "write a bank cell");
}

#[test]
fn water_above_its_bank_is_refused() {
    let mut spec = river_spec();
    spec.ops[1] = PaintOp::BankRing {
        width_m: 2,
        bed_cm: Some(23_799),
    };
    expect_error(vec![build_region(&spec).unwrap()], "would stand as a wall");
}

#[test]
fn a_bank_level_with_the_water_holds_it() {
    let mut spec = river_spec();
    spec.ops[1] = PaintOp::BankRing {
        width_m: 1,
        bed_cm: None,
    };
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    assert_eq!(
        ar.column(Vec2::new(1300, 1120)).unwrap().cell,
        AuthoredCell::Bank { bed_block: 238 }
    );
}

#[test]
fn water_and_banks_below_the_ocean_are_refused() {
    let low_water = region("low", (1024, 1024), (1280, 1280), 0, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1110.0,
                y1: 1110.0,
            },
            surface_cm: 13_899,
            bed_cm: 13_000,
        },
        PaintOp::BankRing {
            width_m: 1,
            bed_cm: Some(14_000),
        },
    ]);
    expect_error(vec![build_region(&low_water).unwrap()], "below the ocean");
    let low_bank = region("low", (1024, 1024), (1280, 1280), 0, vec![PaintOp::Bank {
        shape: Shape::Rect {
            x0: 1100.0,
            y0: 1100.0,
            x1: 1110.0,
            y1: 1110.0,
        },
        bed_cm: 13_850,
    }]);
    expect_error(vec![build_region(&low_bank).unwrap()], "would flood");
    // The ocean's own level is fine.
    let sea = region("sea", (1024, 1024), (1280, 1280), 0, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1110.0,
                y1: 1110.0,
            },
            surface_cm: 13_950,
            bed_cm: 13_000,
        },
        PaintOp::BankRing {
            width_m: 1,
            bed_cm: None,
        },
    ]);
    assert!(load(&[build_region(&sea).unwrap()]).is_ok());
}

#[test]
fn a_dry_bed_is_refused() {
    let spec = region("dry", (1024, 1024), (1280, 1280), 0, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1110.0,
                y1: 1110.0,
            },
            surface_cm: 20_050,
            bed_cm: 20_000,
        },
        PaintOp::BankRing {
            width_m: 1,
            bed_cm: None,
        },
    ]);
    expect_error(vec![build_region(&spec).unwrap()], "is not below surface");
}

#[test]
fn authored_cells_must_keep_the_margin_from_the_box_edge() {
    // The river running the box's whole length: its ends touch the edge.
    let mut spec = river_spec();
    spec.ops[0] = PaintOp::Water {
        shape: Shape::Rect {
            x0: 1024.0,
            y0: 1100.0,
            x1: 1536.0,
            y1: 1120.0,
        },
        surface_cm: 23_868,
        bed_cm: 23_268,
    };
    expect_error(
        vec![build_region(&spec).unwrap()],
        "come within 0 m of the box",
    );
    expect_error(
        vec![build_region(&spec).unwrap()],
        "Enlarge the box to at least min (992, 1024) max (1568, 1216)",
    );
    // Exactly the margin is fine; one metre less is not.
    let pond = |x0: f32| {
        region("pond", (1024, 1024), (1280, 1280), 0, vec![
            PaintOp::Water {
                shape: Shape::Rect {
                    x0,
                    y0: 1100.0,
                    x1: x0 + 10.0,
                    y1: 1110.0,
                },
                surface_cm: 23_868,
                bed_cm: 23_268,
            },
            PaintOp::BankRing {
                width_m: 2,
                bed_cm: Some(23_918),
            },
        ])
    };
    assert!(load(&[build_region(&pond(1042.0)).unwrap()]).is_ok());
    expect_error(
        vec![build_region(&pond(1041.0)).unwrap()],
        "come within 15 m of the box",
    );
}

#[test]
fn natural_decorations_are_kept_by_default() {
    let m: RegionManifest = ron::from_str(
        "(id: \"r\", min: (1024, 1024), max: (1280, 1280), feather_m: 0, tile_size_m: 256, \
         cell_size_m: 1, layers: [Water], tiles: [])",
    )
    .unwrap();
    assert!(!m.suppress_procedural_in_water);
    assert_eq!(m.exclude_procedural_margin_m, 0);
    assert!(!m.allow_partial && m.allow_partial_chunks.is_empty());
    assert!(!river_spec().suppress_procedural_in_water);
}

/// Block-by-block editing from the natural map: a 3 x 3 pond edited into an
/// existing (natural) river chunk, everything else in the region left alone.
#[test]
fn a_small_edit_inside_a_natural_river_chunk() {
    let pond = |partial: Vec<(i32, i32)>, ring: bool| {
        let mut ops = vec![PaintOp::Water {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1103.0,
                y1: 1103.0,
            },
            surface_cm: 23_868,
            bed_cm: 23_268,
        }];
        if ring {
            ops.push(PaintOp::BankRing {
                width_m: 1,
                bed_cm: Some(23_918),
            });
        }
        let mut spec = region("pond", (1024, 1024), (1184, 1184), 16, ops);
        spec.allow_partial_chunks = partial;
        spec
    };
    // The table's natural river runs through chunk (34, 34) (wpos
    // 1088..1120), which holds the pond.
    let table = |c: Vec2<i32>| Some(c == Vec2::new(34, 34));
    // 1. Owned box (the default): the pond's water meets unauthored columns.
    expect_error(
        vec![build_region(&pond(vec![], false)).unwrap()],
        "declare its chunk partial",
    );
    // 2. Owned box, pond walled by banks: loads, but the river chunk is table-wet
    //    and not fully authored -> the hard budget refuses it.
    let ar = load(&[build_region(&pond(vec![], true)).unwrap()]).unwrap();
    let e = ar.check_consistency("t", table).unwrap_err();
    assert!(
        e.0.contains("1 chunk(s) the sim table calls water"),
        "{}",
        e.0
    );
    // 3. The chunk declared partial: loads (the pond meets the natural columns: 12
    //    seam sides), passes the budget, and every other column of that chunk is
    //    the natural engine column, untouched.
    let ar = load(&[build_region(&pond(vec![(34, 34)], false)).unwrap()]).unwrap();
    let r = ar.check_consistency("t", table).unwrap();
    assert_eq!(r[0].partial_table_wet, vec![Vec2::new(34, 34)]);
    assert!(r[0].authored_dry_table_wet.is_empty());
    let natural = ar.column(Vec2::new(1095, 1101)).unwrap();
    assert_eq!(natural.cell, AuthoredCell::None);
    assert!(natural.natural);
    assert_eq!(
        apply(Some(natural), engine(), dry(), |l| l + 100.0),
        engine()
    );
    assert_eq!(natural.resolve(engine(), dry()), engine());
    let wet = ar.column(Vec2::new(1101, 1101)).unwrap();
    assert_eq!(wet.cell, AuthoredCell::Wet {
        surface_block: 238,
        bed_block: 232
    });
    // Outside the partial chunk the box still owns its columns.
    let owned = ar.column(Vec2::new(1130, 1101)).unwrap();
    assert!(!owned.natural);
    assert_eq!(owned.resolve(engine(), dry()).water_level, 139.01);
    // The whole region partial: same, everywhere.
    let mut spec = pond(vec![], false);
    spec.allow_partial = true;
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    assert!(ar.column(Vec2::new(1130, 1101)).unwrap().natural);
    assert!(ar.check_consistency("t", |_| Some(true)).is_ok());
}

#[test]
fn region_geometry_is_validated() {
    let ok = |min, max, feather| region("r", min, max, feather, vec![]);
    let build = |s: RegionSpec| vec![build_region(&s).unwrap()];
    expect_error(build(ok((1000, 1024), (1280, 1280), 0)), "not aligned");
    expect_error(build(ok((32, 1024), (288, 1280), 0)), "map rim");
    expect_error(
        build(ok((32768 - 96, 1024), (32768 - 32, 1280), 0)),
        "map rim",
    );
    expect_error(build(ok((1024, 1024), (1088, 1280), 40)), "feather_m");
    expect_error(build(ok((1024, 1024), (1024, 1280), 0)), "empty box");
    let a = build_region(&ok((1024, 1024), (1280, 1280), 0)).unwrap();
    let mut b = build_region(&ok((1248, 1024), (1536, 1280), 0)).unwrap();
    b.manifest.id = "b".into();
    expect_error(vec![a, b], "overlaps region 'r'");
    let a = build_region(&ok((1024, 1024), (1280, 1280), 0)).unwrap();
    let mut b = build_region(&ok((1280, 1024), (1536, 1280), 0)).unwrap();
    b.manifest.id = "b".into();
    assert!(load(&[a, b]).is_ok(), "adjacent regions are allowed");
    let mut bad = build_region(&ok((1024, 1024), (1280, 1280), 0)).unwrap();
    bad.manifest.id = "Bad-Id".into();
    expect_error(vec![bad], "[a-z0-9_]");
}

#[test]
fn tiles_are_checked_against_the_manifest() {
    let good = build_region(&river_spec()).unwrap();
    assert!(!good.tiles.is_empty());
    // Wrong checksum.
    let mut r = build_region(&river_spec()).unwrap();
    r.manifest.tiles[0].sha256 = "00".repeat(32);
    expect_error(vec![r], "does not match the manifest");
    // Corrupt bytes with a matching checksum.
    let mut r = build_region(&river_spec()).unwrap();
    r.tiles[0].1[0] = b'Y';
    r.manifest.tiles[0].sha256 = format::sha256_hex(&r.tiles[0].1);
    expect_error(vec![r], "bad magic");
    // Missing tile file.
    let mut r = build_region(&river_spec()).unwrap();
    r.tiles.remove(0);
    expect_error(vec![r], "missing tile");
    // Listed twice.
    let mut r = build_region(&river_spec()).unwrap();
    let dup = r.manifest.tiles[0].clone();
    r.manifest.tiles.push(dup);
    expect_error(vec![r], "listed twice");
    // Outside the grid.
    let mut r = build_region(&river_spec()).unwrap();
    r.manifest.tiles[0].tx = 7;
    expect_error(vec![r], "outside the region grid");
    // A tile moved to another slot: header origin disagrees.
    let mut r = build_region(&river_spec()).unwrap();
    let (tx, ty) = r.tiles[0].0;
    let other = if tx == 0 { 1 } else { 0 };
    r.manifest.tiles[0].tx = other;
    r.tiles[0].0 = (other, ty);
    r.manifest.tiles.truncate(1);
    r.tiles.truncate(1);
    expect_error(vec![r], "does not match the manifest position");
}

#[test]
fn manifest_schema_and_layers_are_strict() {
    let r = build_region(&river_spec()).unwrap();
    let mut m = manifest(std::slice::from_ref(&r));
    m.schema = 2;
    let fetch = |_: &str, _: LayerKind, _: i32, _: i32| Err::<Vec<u8>, _>("unused".to_string());
    assert!(
        AuthoredRasters::from_manifest(m, MAP, "test", &fetch)
            .unwrap_err()
            .0
            .contains("schema")
    );
    // Unknown fields and layers are parse errors, never ignored.
    let text = ron::ser::to_string(&manifest(std::slice::from_ref(&r))).unwrap();
    let unknown_layer = text.replacen("layers:[Water]", "layers:[Water,Lava]", 1);
    assert_ne!(unknown_layer, text, "test needs the compact RON spelling");
    assert!(ron::from_str::<Manifest>(&unknown_layer).is_err());
    let unknown_field = text.replacen("schema:1", "schema:1,colour:3", 1);
    assert!(ron::from_str::<Manifest>(&unknown_field).is_err());
    assert_eq!(ron::from_str::<Manifest>(&text).unwrap(), manifest(&[r]));
}

#[test]
fn building_the_same_raster_twice_gives_the_same_bytes() {
    let a = build_region(&river_spec()).unwrap();
    let b = build_region(&river_spec()).unwrap();
    assert_eq!(
        a.tiles.iter().map(|t| &t.1).collect::<Vec<_>>(),
        b.tiles.iter().map(|t| &t.1).collect::<Vec<_>>()
    );
    assert_eq!(a.manifest, b.manifest);
}

#[test]
fn distance_crosses_tile_borders() {
    // Water only in tile (0, 0) near its east edge; a column in tile (1, 0)
    // must still see it.
    let spec = region("tiles", (1024, 1024), (1536, 1280), 0, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1272.0,
                y0: 1100.0,
                x1: 1280.0,
                y1: 1110.0,
            },
            surface_cm: 20_050,
            bed_cm: 19_000,
        },
        PaintOp::BankRing {
            width_m: 1,
            bed_cm: None,
        },
    ]);
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    // x = 1290 lies in tile 1; nearest wet column is x = 1279: 11 m.
    assert_eq!(
        ar.column(Vec2::new(1290, 1105)).unwrap().water_dist,
        Some(11.0)
    );
    // Tile (1, 0) holds no authored cell but tile (0, 0) is listed.
    let built = build_region(&spec).unwrap();
    assert_eq!(
        built.tiles.len(),
        2,
        "the bank ring reaches x = 1280 in tile 1"
    );
}

#[test]
fn concurrent_first_use_decodes_once_and_agrees() {
    let ar = std::sync::Arc::new(load(&[build_region(&river_spec()).unwrap()]).unwrap());
    let reference: Vec<_> = (1024..1536)
        .map(|x| ar.column(Vec2::new(x, 1110 + x % 9)))
        .collect();
    let ar2 = std::sync::Arc::new(load(&[build_region(&river_spec()).unwrap()]).unwrap());
    std::thread::scope(|s| {
        for _ in 0..8 {
            let ar2 = &ar2;
            let reference = &reference;
            s.spawn(move || {
                for (k, x) in (1024..1536).enumerate() {
                    assert_eq!(ar2.column(Vec2::new(x, 1110 + x % 9)), reference[k]);
                }
            });
        }
    });
}

#[test]
fn constants_agree_with_the_engine() {
    // The column sampler's ocean: `z < sea_level - 1 + 0.01` is water.
    assert_eq!(SEA_TOP_BLOCK, 139);
    assert!((SEA_TOP_BLOCK as f32) < crate::CONFIG.sea_level - 1.0 + 0.01);
    assert!(((SEA_TOP_BLOCK + 1) as f32) >= crate::CONFIG.sea_level - 1.0 + 0.01);
    assert_eq!(DIST_CAP_M as f32, crate::column::WATER_WARP_FADE_M);
    // The ground field reuses the water field's apron and u8 encoding.
    assert!(GROUND_WARP_FADE_M <= DIST_CAP_M && DIST_CAP_M * 3 < NO_DIST as i32);
}

#[test]
fn apply_reapplies_the_flood_on_top_of_authored_water_only() {
    let wet = AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::Wet {
            surface_block: 238,
            bed_block: 232,
        },
        water_dist: Some(0.0),
        ground_dist: None,
        natural: false,
    };
    let flood = |l: f32| l.max(240.0);
    assert_eq!(apply(Some(wet), engine(), dry(), flood).water_level, 240.0);
    assert_eq!(
        apply(None, engine(), dry(), flood),
        engine(),
        "no region: untouched"
    );
}

#[test]
fn procedural_suppression_follows_the_region_settings() {
    let col = |cell, d, suppress, margin| AuthoredColumn {
        settings: RegionSettings {
            suppress_procedural_in_water: suppress,
            exclude_procedural_margin_m: margin,
            aquatic_profile: None,
            sea_fill: SeaFill::Auto,
            site_levelling: true,
        },
        weight: 1.0,
        cell,
        water_dist: d,
        ground_dist: None,
        natural: false,
    };
    let wet = AuthoredCell::Wet {
        surface_block: 238,
        bed_block: 232,
    };
    let bank = AuthoredCell::Bank { bed_block: 240 };
    assert!(col(wet, Some(0.0), true, 0.0).procedural_suppressed());
    assert!(!col(wet, Some(0.0), false, 0.0).procedural_suppressed());
    assert!(!col(bank, Some(1.0), true, 0.0).procedural_suppressed());
    assert!(col(bank, Some(3.0), true, 4.0).procedural_suppressed());
    assert!(!col(bank, Some(5.0), true, 4.0).procedural_suppressed());
    assert!(!col(AuthoredCell::None, None, true, 4.0).procedural_suppressed());
}

#[test]
fn cell_at_matches_column_and_skips_the_distance_field() {
    let ar = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    for x in (1024..1536).step_by(7) {
        for y in (1024..1216).step_by(3) {
            let p = Vec2::new(x, y);
            assert_eq!(ar.cell_at(p), ar.column(p).map(|c| c.cell));
        }
    }
    assert_eq!(ar.cell_at(Vec2::new(0, 0)), None);
}

#[test]
fn consistency_is_checked_both_ways_against_a_budget() {
    let ar = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    // Region (1024..1536) x (1024..1216) = chunks 32..48 x 32..38; water in
    // chunk row 34 (y 1100..1119), chunks 33..47 (x 1056..1504), and banks
    // around it.
    let table_all_dry = |_: Vec2<i32>| Some(false);
    let r = ar.check_consistency("t", table_all_dry).unwrap();
    assert_eq!(
        r[0].authored_wet_table_dry.len(),
        14,
        "default budget: allowed"
    );
    assert!(r[0].authored_dry_table_wet.is_empty());
    // A sim lake in the unauthored north row.
    let table_lake_north = |c: Vec2<i32>| Some(c.y == 37);
    let e = ar.check_consistency("t", table_lake_north).unwrap_err();
    assert!(
        e.0.contains("16 chunk(s) the sim table calls water"),
        "{}",
        e.0
    );
    // The sim also calls the river row water (masks painted): that row is
    // only partly authored (a 20 m river in a 32 m chunk row), so the sim's
    // sunk terrain shows there too.
    let table_river = |c: Vec2<i32>| Some(c.y == 34);
    let e = ar.check_consistency("t", table_river).unwrap_err();
    assert!(
        e.0.contains("16 chunk(s) the sim table calls water"),
        "{}",
        e.0
    );
    assert!(
        e.0.contains("allow_partial_chunks"),
        "the error names the partial-authoring opt-in"
    );
    // Declared partial (the table's river is the natural one being edited):
    // exempt, reported.
    let mut spec = river_spec();
    spec.allow_partial_chunks = (32..48).map(|x| (x, 34)).collect();
    let r = load(&[build_region(&spec).unwrap()])
        .unwrap()
        .check_consistency("t", table_river)
        .unwrap();
    assert_eq!(r[0].partial_table_wet.len(), 16);
    assert!(r[0].authored_dry_table_wet.is_empty());
    let mut spec = river_spec();
    spec.allow_partial = true;
    let ar_all = load(&[build_region(&spec).unwrap()]).unwrap();
    assert!(ar_all.check_consistency("t", table_river).is_ok());
    assert!(ar_all.check_consistency("t", table_lake_north).is_ok());
    // A partial chunk outside the box is refused.
    let mut spec = river_spec();
    spec.allow_partial_chunks = vec![(48, 34)];
    expect_error(vec![build_region(&spec).unwrap()], "outside the box");
    let mut spec = river_spec();
    spec.consistency.max_authored_dry_table_wet_chunks = 32;
    spec.consistency.max_authored_wet_table_dry_chunks = Some(0);
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let e = ar.check_consistency("t", table_all_dry).unwrap_err();
    assert!(e.0.contains("dry in the sim table"), "{}", e.0);
    assert!(
        ar.check_consistency("t", |c: Vec2<i32>| Some(c.y == 34 || c.y == 37))
            .is_ok()
    );
    // A fully authored chunk may be water in the table.
    let full = region("full", (992, 992), (1088, 1088), 0, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1024.0,
                y0: 1024.0,
                x1: 1056.0,
                y1: 1056.0,
            },
            surface_cm: 20_050,
            bed_cm: 19_050,
        },
        PaintOp::BankRing {
            width_m: 2,
            bed_cm: Some(20_150),
        },
    ]);
    let ar = load(&[build_region(&full).unwrap()]).unwrap();
    assert!(
        ar.check_consistency("t", |c| Some(c == Vec2::new(32, 32)))
            .is_ok()
    );
}

#[test]
fn the_memory_budget_is_checked_before_any_tile_is_read() {
    let tiles_needed = RESIDENT_BUDGET_BYTES / (TILE_CELLS * 4) + 1;
    let side = (tiles_needed as f64).sqrt().ceil() as i32 * TILE_SIZE;
    let mut built = build_region(&region(
        "huge",
        (1024, 1024),
        (1024 + side, 1024 + side),
        0,
        vec![],
    ))
    .unwrap();
    built.manifest.tiles = (0..tiles_needed as i32)
        .map(|k| TileManifest {
            layer: LayerKind::Water,
            tx: k % (side / TILE_SIZE),
            ty: k / (side / TILE_SIZE),
            sha256: "00".repeat(32),
        })
        .collect();
    let fetch = |_: &str, _: LayerKind, _: i32, _: i32| -> Result<Vec<u8>, String> {
        panic!("a tile was read before the budget check")
    };
    let e = AuthoredRasters::from_manifest(manifest(&[built]), MAP, "test", &fetch).unwrap_err();
    assert!(e.0.contains("engine's limit"), "{}", e.0);
    assert!(e.0.starts_with("authored rasters [test]"), "{}", e.0);
}

#[test]
fn water_layer_blocks_outside_the_engine_range_are_refused() {
    // A lake 2e9 cm up (20 million blocks): far outside the blocks the
    // engine can hold, whatever the sea rule says.
    let lake = |surface_cm, bed_cm| {
        let mut s = region("lake", (1024, 1024), (1536, 1216), 32, vec![
            PaintOp::Water {
                shape: Shape::Rect {
                    x0: 1100.0,
                    y0: 1100.0,
                    x1: 1200.0,
                    y1: 1120.0,
                },
                surface_cm,
                bed_cm,
            },
            PaintOp::BankRing {
                width_m: 2,
                bed_cm: Some(surface_cm + 50),
            },
        ]);
        s.sea_fill = SeaFill::AuthoredOnly;
        s
    };
    expect_error(
        vec![build_region(&lake(2_000_000_000, 1_999_999_400)).unwrap()],
        "outside the blocks",
    );
    // The same lake inside the range loads.
    assert!(load(&[build_region(&lake(23_868, 23_268)).unwrap()]).is_ok());
}

#[test]
fn a_garbage_region_box_fails_cleanly_instead_of_overflowing() {
    let mut built = build_region(&river_spec()).unwrap();
    built.manifest.min = (i32::MIN, i32::MIN);
    built.manifest.max = (i32::MAX, i32::MAX);
    let fetch = |_: &str, _: LayerKind, _: i32, _: i32| -> Result<Vec<u8>, String> {
        Err("no tiles".to_string())
    };
    let e = AuthoredRasters::from_manifest(manifest(&[built]), MAP, "test", &fetch).unwrap_err();
    assert!(e.0.contains("too large to be a region"), "{}", e.0);
}

#[test]
fn unknown_aquatic_profiles_and_bad_margins_are_refused() {
    let mut spec = river_spec();
    spec.aquatic_ecology_profile = Some("no_such_profile".into());
    expect_error(
        vec![build_region(&spec).unwrap()],
        "aquatic_ecology_profile",
    );
    let mut spec = river_spec();
    spec.aquatic_ecology_profile = Some("mountain_river".into());
    assert!(load(&[build_region(&spec).unwrap()]).is_ok());
    let mut spec = river_spec();
    spec.exclude_procedural_margin_m = 65;
    expect_error(
        vec![build_region(&spec).unwrap()],
        "exclude_procedural_margin_m",
    );
}

/// Cost of the per-column lookups (`cargo test --release ... -- --ignored
/// --nocapture column_lookup_cost`): outside every region (the hot path of
/// every world), inside a region without tiles, on wet and on dry cells.
#[test]
#[ignore]
fn column_lookup_cost() {
    let river = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    let empty =
        load(&[build_region(&region("empty", (1024, 1024), (1536, 1216), 32, vec![])).unwrap()])
            .unwrap();
    river.prewarm();
    let time = |name: &str, ar: &AuthoredRasters, pts: &[Vec2<i32>], cell_only: bool| {
        let n = 20;
        let t = std::time::Instant::now();
        let mut acc = 0u32;
        for _ in 0..n {
            for p in pts {
                if cell_only {
                    acc += ar.cell_at(*p).is_some() as u32;
                } else {
                    acc += ar.column(std::hint::black_box(*p)).is_some() as u32;
                }
            }
        }
        let ns = t.elapsed().as_nanos() as f64 / (n * pts.len()) as f64;
        println!("{name}: {ns:.1} ns per lookup ({acc})");
    };
    let outside: Vec<_> = (0..100_000)
        .map(|k| Vec2::new(5000 + k % 300, 5000 + k / 300))
        .collect();
    let inside: Vec<_> = (0..100_000)
        .map(|k| Vec2::new(1100 + k % 400, 1030 + (k / 400) % 180))
        .collect();
    let wet: Vec<_> = (0..100_000)
        .map(|k| Vec2::new(1030 + k % 500, 1100 + (k / 500) % 20))
        .collect();
    time("outside every region", &river, &outside, false);
    time("inside a region with no tiles", &empty, &inside, false);
    time("inside the river region (mixed)", &river, &inside, false);
    time("wet cells", &river, &wet, false);
    time("cell_at (mixed)", &river, &inside, true);
    let plateau = load(&[build_region(&plateau_spec()).unwrap()]).unwrap();
    time("exact ground cells", &plateau, &inside, false);
    // Two regions in opposite corners of the map: the union box covers the
    // map, so every column pays the region scan.
    let corners = load(&[
        build_region(&region("sw", (1024, 1024), (1536, 1216), 32, vec![])).unwrap(),
        build_region(&region(
            "ne",
            (31_232, 31_232),
            (31_744, 31_424),
            32,
            vec![],
        ))
        .unwrap(),
    ])
    .unwrap();
    let middle: Vec<_> = (0..100_000)
        .map(|k| Vec2::new(16_000 + k % 300, 16_000 + k / 300))
        .collect();
    time(
        "two corner regions, columns between them",
        &corners,
        &middle,
        false,
    );
    let t = std::time::Instant::now();
    let fresh = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    fresh.prewarm();
    println!("load + prewarm of the river region: {:?}", t.elapsed());
}

#[test]
fn the_process_wide_counter_follows_loaded_manifests() {
    // Other tests load manifests concurrently: only compare this one's share.
    let a = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    let share = a.resident_bytes;
    assert!(share > 0);
    assert!(global_resident_bytes() >= share);
    drop(a);
    // A refused manifest leaves nothing reserved: the huge one of
    // `the_memory_budget_is_checked_before_any_tile_is_read` fails before
    // reserving, and a bad tile fails after reserving and must release.
    let mut r = build_region(&river_spec()).unwrap();
    r.manifest.tiles[0].sha256 = "00".repeat(32);
    let before = global_resident_bytes();
    assert!(load(&[r]).is_err());
    // Concurrent tests may load and drop meanwhile; the failed load itself
    // must not leave its reservation behind.
    assert!(global_resident_bytes() <= before + share * 4);
}

#[test]
fn the_process_wide_cap_refuses_with_a_clear_error() {
    // A cap below what is already reserved plus the request is refused and
    // reserves nothing; the error names the override.
    let e = ReservedBytes::reserve(3 << 20, 2 << 20)
        .err()
        .expect("over the cap");
    assert!(e.contains("process-wide cap of 2 MiB"), "{e}");
    assert!(e.contains("XINDELER_AUTHORED_RASTERS_MAX_MIB"), "{e}");
    let r = ReservedBytes::reserve(1 << 20, usize::MAX).expect("under the cap");
    assert!(global_resident_bytes() >= 1 << 20);
    drop(r);
}

// --------------------------------------------------------------------------
// Stage 2: the ground layer
// --------------------------------------------------------------------------

/// A 200 x 100 m exact plateau at 251.37 m (block 251) with a 1 m trench,
/// a 1 m ridge and a 1:4 ramp, inside a ground-only region.
fn plateau_spec() -> RegionSpec {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    region("plateau", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Ground {
            shape: rect(1100.0, 1050.0, 1300.0, 1150.0),
            ground_cm: 25_137,
            weight: GROUND_EXACT,
        },
        // 1 m wide trench, 3 m deep.
        PaintOp::Ground {
            shape: rect(1150.0, 1050.0, 1151.0, 1150.0),
            ground_cm: 24_837,
            weight: GROUND_EXACT,
        },
        // 1 m wide ridge, 2 m high.
        PaintOp::Ground {
            shape: rect(1200.0, 1050.0, 1201.0, 1150.0),
            ground_cm: 25_337,
            weight: GROUND_EXACT,
        },
        // A ramp rising 25 cm per metre in x.
        PaintOp::GroundPlane {
            shape: rect(1250.0, 1050.0, 1300.0, 1150.0),
            origin: (1250.0, 1050.0),
            origin_cm: 25_137,
            cm_per_m: (25.0, 0.0),
        },
    ])
}

fn ground_col(cell: AuthoredCell, sea_fill: SeaFill) -> AuthoredColumn {
    AuthoredColumn {
        settings: RegionSettings {
            sea_fill,
            ..SETTINGS
        },
        weight: 0.3,
        cell,
        water_dist: Some(12.0),
        ground_dist: None,
        natural: false,
    }
}

/// Which authored columns hold water over their ground (what rtsim spawns
/// and its water pass treat as water for land bodies).
#[test]
fn flooded_columns_are_wet_cells_and_sea_filled_dry_cells() {
    let wet = AuthoredCell::Wet {
        surface_block: 145,
        bed_block: 140,
    };
    let pit = AuthoredCell::Ground {
        block: SEA_TOP_BLOCK - 10,
        weight: GROUND_EXACT,
    };
    let ring_pit = AuthoredCell::Ground {
        block: SEA_TOP_BLOCK - 1,
        weight: 100,
    };
    let at_sea_top = AuthoredCell::Ground {
        block: SEA_TOP_BLOCK,
        weight: GROUND_EXACT,
    };
    let low_bank = AuthoredCell::Bank {
        bed_block: SEA_TOP_BLOCK - 3,
    };
    for (cell, auto, authored_only) in [
        (wet, true, true),
        (pit, true, false),
        (ring_pit, true, false),
        (at_sea_top, false, false),
        (low_bank, true, false),
        (AuthoredCell::None, false, false),
    ] {
        assert_eq!(
            ground_col(cell, SeaFill::Auto).is_flooded(),
            auto,
            "{cell:?} Auto"
        );
        assert_eq!(
            ground_col(cell, SeaFill::AuthoredOnly).is_flooded(),
            authored_only,
            "{cell:?} AuthoredOnly"
        );
    }
}

#[test]
fn ground_cells_are_read_exactly() {
    let built = build_region(&plateau_spec()).unwrap();
    assert_eq!(built.manifest.layers, vec![LayerKind::Ground]);
    assert!(built.tiles.is_empty() && !built.ground_tiles.is_empty());
    let ar = load(&[built]).unwrap();
    let cell = |x, y| ar.cell_at(Vec2::new(x, y)).unwrap();
    assert_eq!(cell(1120, 1100), AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT
    });
    assert_eq!(cell(1150, 1100), AuthoredCell::Ground {
        block: 248,
        weight: GROUND_EXACT
    });
    assert_eq!(cell(1149, 1100), AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT
    });
    assert_eq!(cell(1200, 1100), AuthoredCell::Ground {
        block: 253,
        weight: GROUND_EXACT
    });
    // Ramp: centre of column 1260 is 10.5 m in: 25137 + 262.5 -> 25400 cm.
    assert_eq!(cell(1260, 1100), AuthoredCell::Ground {
        block: 254,
        weight: GROUND_EXACT
    });
    assert_eq!(cell(1099, 1100), AuthoredCell::None);
    assert_eq!(cell(1120, 1100).exact_ground_block(), Some(251));
    assert_eq!(ar.water_at(Vec2::new(1120, 1100)), None);
    // The chunk summary and the stone floor see the ground.
    let s = ar.chunk_summary(Vec2::new(1150 / 32, 1100 / 32)).unwrap();
    assert_eq!(s.ground_columns, s.authored_columns);
    assert_eq!(s.wet_columns, 0);
    assert_eq!(s.min_ground_cell_block, 248);
    assert_eq!(s.min_bed_block, i32::MAX);
    assert_eq!(
        ar.floor_block(Vec2::new(1150 / 32, 1100 / 32)),
        Some(248 - FLOOR_MARGIN_BLOCKS)
    );
}

#[test]
fn resolve_exact_ground_drops_every_engine_term() {
    let cell = AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT,
    };
    for sea_fill in [SeaFill::Auto, SeaFill::AuthoredOnly] {
        let r = ground_col(cell, sea_fill).resolve(engine(), dry());
        assert_eq!(r.alt, 251.5);
        assert_eq!(r.riverless_alt, 251.5);
        assert_eq!(r.warp_factor, 0.0);
        assert_eq!(r.cliff_offset, 0.0);
        assert_eq!(r.water_dist, Some(12.0));
        assert_eq!(r.water_level, match sea_fill {
            SeaFill::Auto => 139.01,
            SeaFill::AuthoredOnly => 251.5,
        });
    }
    // In a partial chunk a ground cell is still exact (only unauthored
    // columns keep the natural map).
    let mut natural = ground_col(cell, SeaFill::Auto);
    natural.natural = true;
    let r = apply(Some(natural), engine(), dry(), |l| l);
    assert_eq!(r.alt, 251.5);
    // Below sea level: Auto fills it with sea water, AuthoredOnly keeps it dry.
    let pit = AuthoredCell::Ground {
        block: 120,
        weight: GROUND_EXACT,
    };
    let auto = ground_col(pit, SeaFill::Auto).resolve(engine(), dry());
    assert!(auto.water_level > auto.alt + 18.0);
    let dry_pit = ground_col(pit, SeaFill::AuthoredOnly).resolve(engine(), dry());
    assert_eq!(dry_pit.water_level, dry_pit.alt);
    assert_eq!(dry_pit.alt, 120.5);
}

#[test]
fn authored_only_wet_and_bank_cells_have_no_sea_fill() {
    let lake = AuthoredCell::Wet {
        surface_block: 119,
        bed_block: 110,
    };
    // (In Auto such a lake is refused at load; a sea-level lake still gets
    // the sea's level.)
    let sea_lake = AuthoredCell::Wet {
        surface_block: 139,
        bed_block: 110,
    };
    let auto = ground_col(sea_lake, SeaFill::Auto).resolve(engine(), dry());
    assert_eq!(auto.water_level, 139.5);
    let own = ground_col(lake, SeaFill::AuthoredOnly).resolve(engine(), dry());
    assert_eq!(
        own.water_level, 119.5,
        "AuthoredOnly: the lake's own surface"
    );
    assert_eq!(own.alt, 110.5);
    let bank = AuthoredCell::Bank { bed_block: 100 };
    let own = ground_col(bank, SeaFill::AuthoredOnly).resolve(engine(), dry());
    assert_eq!(own.water_level, own.alt);
    let auto = ground_col(bank, SeaFill::Auto).resolve(engine(), dry());
    assert_eq!(auto.water_level, 139.01);
}

/// Both buried-ground paths (the exact column and the ring cell) use the
/// sampler's column top, `alt as i32`, which truncates toward zero: an
/// altitude in (-1, 0) has block 0 as its top, not -1.
#[test]
fn the_top_block_of_an_altitude_truncates_toward_zero() {
    use super::queries::top_block_of_levelled_alt as top;
    assert_eq!(top(-0.5), 0);
    assert_eq!(top(-1.5), -1);
    assert_eq!(top(-0.0), 0);
    assert_eq!(top(0.9), 0);
    assert_eq!(top(139.01), 139);
}

#[test]
fn top_block_alt_has_the_block_as_its_top_below_zero_too() {
    for block in [-1860, -5, -1, 0, 1, 139, 5140] {
        let alt = top_block_alt(block);
        // `block.rs`: solid when `z as i32 <= alt as i32`.
        assert_eq!(alt as i32, block, "truncation at {block}");
        assert_eq!(alt.floor() as i32, block, "floor at {block}");
    }
}

#[test]
fn ground_below_water_must_agree_with_the_water_layer() {
    let water = |shape| PaintOp::Water {
        shape,
        surface_cm: 23_868,
        bed_cm: 23_268,
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let base = |ground_cm: i32, bank_cm: i32| {
        region("both", (1024, 1024), (1536, 1216), 32, vec![
            water(rect(1100.0, 1100.0, 1200.0, 1110.0)),
            PaintOp::BankRing {
                width_m: 1,
                bed_cm: Some(bank_cm),
            },
            // Ground over the whole area: under the water, under the banks,
            // and beside them.
            PaintOp::Ground {
                shape: rect(1090.0, 1090.0, 1210.0, 1120.0),
                ground_cm,
                weight: GROUND_EXACT,
            },
        ])
    };
    // Ground disagrees with the bed.
    let e = load(&[build_region(&base(23_918, 23_918)).unwrap()]).unwrap_err();
    assert!(e.0.contains("under authored water"), "{e}");
    // Where only ground is painted it must match: paint the water layer
    // after the ground so the beds and banks override it exactly.
    let mut ok = base(23_918, 23_918);
    ok.ops.rotate_left(2); // ground first, then water + bank ring
    // The bank ring skips cells that already hold ground: the ground beside
    // the water is at 239.18 m, above the water's top block 238.
    let built = build_region(&ok).unwrap();
    assert_eq!(built.manifest.layers, vec![
        LayerKind::Water,
        LayerKind::Ground
    ]);
    // Water cells still carry their ground underneath: it is the plateau,
    // not the bed -> refused.
    let e = load(&[built]).unwrap_err();
    assert!(e.0.contains("under authored water"), "{e}");
    // Clearing the ground under the water makes the two layers consistent,
    // and the exact ground beside the water contains it.
    let mut ok = base(23_918, 23_918);
    ok.ops.rotate_left(2);
    ok.ops.push(PaintOp::ClearGround {
        shape: rect(1100.0, 1100.0, 1200.0, 1110.0),
    });
    let ar = load(&[build_region(&ok).unwrap()]).unwrap();
    assert!(ar.cell_at(Vec2::new(1150, 1105)).unwrap().is_wet());
    assert_eq!(
        ar.cell_at(Vec2::new(1150, 1110)).unwrap(),
        AuthoredCell::Ground {
            block: 239,
            weight: GROUND_EXACT
        }
    );
    // A bank with ground under it must agree too.
    let banked = region("banked", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Ground {
            shape: rect(1100.0, 1100.0, 1110.0, 1110.0),
            ground_cm: 24_000,
            weight: GROUND_EXACT,
        },
        PaintOp::Bank {
            shape: rect(1100.0, 1100.0, 1105.0, 1110.0),
            bed_cm: 24_100,
        },
    ]);
    let e = load(&[build_region(&banked).unwrap()]).unwrap_err();
    assert!(e.0.contains("under an authored bank"), "{e}");
}

#[test]
fn water_next_to_lower_ground_is_a_wall_and_refused() {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let spec = |ground_cm| {
        region("quay", (1024, 1024), (1536, 1216), 32, vec![
            PaintOp::Ground {
                shape: rect(1090.0, 1090.0, 1210.0, 1120.0),
                ground_cm,
                weight: GROUND_EXACT,
            },
            PaintOp::ClearGround {
                shape: rect(1100.0, 1100.0, 1200.0, 1110.0),
            },
            PaintOp::Water {
                shape: rect(1100.0, 1100.0, 1200.0, 1110.0),
                surface_cm: 23_868,
                bed_cm: 23_268,
            },
        ])
    };
    // A quay at 240 m holds water whose top block is 238.
    assert!(load(&[build_region(&spec(24_050)).unwrap()]).is_ok());
    // Ground at 237.5 m (block 237) next to water top 238: a wall.
    let e = load(&[build_region(&spec(23_750)).unwrap()]).unwrap_err();
    assert!(e.0.contains("would stand as a wall"), "{e}");
}

#[test]
fn ground_rules_accept_blend_weights_and_refuse_out_of_range_blocks() {
    let rect = Shape::Rect {
        x0: 1100.0,
        y0: 1100.0,
        x1: 1110.0,
        y1: 1110.0,
    };
    let spec = |ground_cm, weight| {
        region("g", (1024, 1024), (1536, 1216), 32, vec![PaintOp::Ground {
            shape: rect.clone(),
            ground_cm,
            weight,
        }])
    };
    // Every weight 1..=255 loads: below 255 a blend cell of the ring.
    for weight in [1, 128, 254, GROUND_EXACT] {
        let ar = load(&[build_region(&spec(24_000, weight)).unwrap()]).unwrap();
        assert_eq!(
            ar.cell_at(Vec2::new(1105, 1105)),
            Some(AuthoredCell::Ground { block: 240, weight })
        );
    }
    expect_error(
        vec![build_region(&spec(819_200, GROUND_EXACT)).unwrap()],
        "outside the blocks",
    );
    expect_error(
        vec![build_region(&spec(-409_601, GROUND_EXACT)).unwrap()],
        "outside the blocks",
    );
    // The extremes of the range load (an AuthoredOnly region: no sea rule).
    for cm in [-409_600, 819_199] {
        let mut s = spec(cm, GROUND_EXACT);
        s.sea_fill = SeaFill::AuthoredOnly;
        assert!(load(&[build_region(&s).unwrap()]).is_ok(), "{cm}");
    }
    // The 16 m box margin applies to ground cells too.
    let near_edge = region("edge", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1030.0,
                y0: 1100.0,
                x1: 1040.0,
                y1: 1110.0,
            },
            ground_cm: 24_000,
            weight: GROUND_EXACT,
        },
    ]);
    expect_error(
        vec![build_region(&near_edge).unwrap()],
        "at least 16 m inside",
    );
}

#[test]
fn layers_and_tile_listing_are_checked_per_layer() {
    let built = build_region(&plateau_spec()).unwrap();
    // A ground tile in a region that declares only water.
    let mut r = build_region(&plateau_spec()).unwrap();
    r.manifest.layers = vec![LayerKind::Water];
    expect_error(vec![r], "a layer the region does not declare");
    // Layer order and duplicates.
    let mut r = build_region(&plateau_spec()).unwrap();
    r.manifest.layers = vec![LayerKind::Ground, LayerKind::Water];
    expect_error(vec![r], "[Water], [Ground] or [Water, Ground]");
    let mut r = build_region(&plateau_spec()).unwrap();
    let dup = r.manifest.tiles[0].clone();
    r.manifest.tiles.push(dup);
    expect_error(vec![r], "Ground tile");
    // A water and a ground tile at the same index load (a two-layer region).
    let mut both = plateau_spec();
    both.ops.push(PaintOp::Water {
        shape: Shape::Rect {
            x0: 1350.0,
            y0: 1100.0,
            x1: 1360.0,
            y1: 1110.0,
        },
        surface_cm: 23_868,
        bed_cm: 23_268,
    });
    both.ops.push(PaintOp::BankRing {
        width_m: 1,
        bed_cm: Some(23_918),
    });
    let b = build_region(&both).unwrap();
    assert_eq!(b.manifest.layers, vec![LayerKind::Water, LayerKind::Ground]);
    let idx: Vec<_> = b.manifest.tiles.iter().map(|t| (t.tx, t.ty)).collect();
    assert!(idx.iter().filter(|t| **t == (1, 0)).count() == 2, "{idx:?}");
    assert!(load(&[b]).is_ok());
    // Corrupt ground tiles are refused like water tiles.
    let mut r = built;
    r.ground_tiles[0].1[30] ^= 0xFF;
    expect_error(vec![r], "sha256");
}

#[test]
fn ground_memory_is_charged_per_layer() {
    let ground_only = load(&[build_region(&plateau_spec()).unwrap()]).unwrap();
    // 2 x 1 grid; ground tiles, and the distance-to-exact-ground fields of
    // the grid tiles next to them (both, here), no distance-to-water fields.
    let n = build_region(&plateau_spec()).unwrap().ground_tiles.len();
    assert_eq!(ground_only.resident_bytes, n * (192 << 10) + 2 * (64 << 10));
    let water = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    let w = build_region(&river_spec()).unwrap().tiles.len();
    assert_eq!(water.resident_bytes, w * (256 << 10) + 2 * (64 << 10));
}

#[test]
fn sea_fill_auto_allows_ground_below_sea_only_in_the_sea() {
    let pit = |sea_fill| {
        let mut s = region("pit", (1024, 1024), (1536, 1216), 32, vec![
            PaintOp::Ground {
                shape: Shape::Rect {
                    x0: 1100.0,
                    y0: 1100.0,
                    x1: 1150.0,
                    y1: 1150.0,
                },
                ground_cm: 12_000,
                weight: GROUND_EXACT,
            },
        ]);
        s.sea_fill = sea_fill;
        load(&[build_region(&s).unwrap()]).unwrap()
    };
    let auto = pit(SeaFill::Auto);
    let e = auto.check_sea_fill("test", |_| Some(false)).unwrap_err();
    assert!(e.0.contains("sea_fill: AuthoredOnly"), "{e}");
    assert!(e.0.contains("lowest block 120"), "{e}");
    assert!(
        auto.check_sea_fill("test", |_| Some(true)).is_ok(),
        "a sea floor"
    );
    let own = pit(SeaFill::AuthoredOnly);
    assert!(
        own.check_sea_fill("test", |_| Some(false)).is_ok(),
        "a dry pit"
    );
    // Banks and wet surfaces below sea level: refused in Auto at load,
    // allowed in AuthoredOnly.
    let lake = |sea_fill| {
        let mut s = region("lake", (1024, 1024), (1536, 1216), 32, vec![
            PaintOp::Water {
                shape: Shape::Rect {
                    x0: 1100.0,
                    y0: 1100.0,
                    x1: 1150.0,
                    y1: 1150.0,
                },
                surface_cm: 12_050,
                bed_cm: 11_050,
            },
            PaintOp::BankRing {
                width_m: 1,
                bed_cm: Some(12_050),
            },
        ]);
        s.sea_fill = sea_fill;
        load(&[build_region(&s).unwrap()])
    };
    let e = lake(SeaFill::Auto).unwrap_err();
    assert!(e.0.contains("sea_fill: AuthoredOnly"), "{e}");
    let own = lake(SeaFill::AuthoredOnly).unwrap();
    assert_eq!(
        own.water_at(Vec2::new(1120, 1120)).unwrap().surface_block,
        120
    );
}

/// Identity gate 2 (digest part): a manifest whose new fields are at their
/// defaults serialises exactly as before them, so its digest does not move.
#[test]
fn new_manifest_fields_at_their_defaults_do_not_change_the_digest() {
    let m = Manifest {
        schema: 1,
        regions: vec![RegionManifest {
            id: "arena_stage1".into(),
            min: (22752, 24576),
            max: (23936, 25600),
            feather_m: 32,
            tile_size_m: 256,
            cell_size_m: 1,
            layers: vec![LayerKind::Water],
            tiles: vec![TileManifest {
                layer: LayerKind::Water,
                tx: 0,
                ty: 0,
                sha256: "ab".repeat(32),
            }],
            suppress_procedural_in_water: false,
            exclude_procedural_margin_m: 0,
            allow_partial: false,
            allow_partial_chunks: vec![],
            aquatic_ecology_profile: None,
            consistency: ConsistencyBudget::default(),
            sea_fill: SeaFill::Auto,
            sites_on_patch: vec![],
            site_levelling: true,
            max_exposed_void_columns: 0,
        }],
    };
    let text = ron::ser::to_string(&m).unwrap();
    // The Stage-1 engine's serialisation of the same manifest, byte for byte.
    let stage1 = "(schema:1,regions:[(id:\"arena_stage1\",min:(22752,24576),max:(23936,25600),\
                  feather_m:32,tile_size_m:256,cell_size_m:1,layers:[Water],tiles:[(layer:Water,\
                  tx:0,ty:0,sha256:\"\
                  abababababababababababababababababababababababababababababababab\")],\
                  suppress_procedural_in_water:false,exclude_procedural_margin_m:0,allow_partial:\
                  false,allow_partial_chunks:[],aquatic_ecology_profile:None,consistency:\
                  (max_authored_dry_table_wet_chunks:0,max_authored_wet_table_dry_chunks:None))])";
    assert_eq!(text, stage1);
    // A Stage-1 manifest text still parses to the same value.
    assert_eq!(ron::from_str::<Manifest>(stage1).unwrap(), m);
    // A non-default value is serialised (and so changes the digest).
    let mut own = m.clone();
    own.regions[0].sea_fill = SeaFill::AuthoredOnly;
    let t = ron::ser::to_string(&own).unwrap();
    assert!(t.contains("sea_fill:AuthoredOnly"), "{t}");
    assert_eq!(ron::from_str::<Manifest>(&t).unwrap(), own);
}

/// Pins the digest of one fixed region (see the digest contract on
/// [`RegionManifest`]): a field added to the manifest without
/// `skip_serializing_if` at its default, or any change to how a region
/// serialises, changes it -- and with it the digest of every deployed
/// region, so persistence would refuse to start and the rtsim water pass
/// would re-run. Update the constant only together with a release note.
#[test]
fn region_digest_is_pinned() {
    let rm = RegionManifest {
        id: "pinned".into(),
        min: (1024, 1024),
        max: (1536, 1280),
        feather_m: 32,
        tile_size_m: 256,
        cell_size_m: 1,
        layers: vec![LayerKind::Water, LayerKind::Ground],
        tiles: vec![
            TileManifest {
                layer: LayerKind::Water,
                tx: 0,
                ty: 0,
                sha256: "ab".repeat(32),
            },
            TileManifest {
                layer: LayerKind::Ground,
                tx: 1,
                ty: 0,
                sha256: "cd".repeat(32),
            },
        ],
        suppress_procedural_in_water: true,
        exclude_procedural_margin_m: 4,
        allow_partial: false,
        allow_partial_chunks: vec![(33, 33)],
        aquatic_ecology_profile: None,
        consistency: ConsistencyBudget::default(),
        sea_fill: SeaFill::Auto,
        sites_on_patch: vec![],
        site_levelling: true,
        max_exposed_void_columns: 0,
    };
    assert_eq!(
        region_digest(&rm).unwrap(),
        "7987cf0fedb2503b3b08d46895d3af63c8fb41db0dd318dfe9a0eb173e5d6203",
        "{}",
        ron::ser::to_string(&rm).unwrap()
    );
}

#[test]
fn the_resident_cap_has_a_default_a_warning_band_and_a_hard_ceiling() {
    const GIB: usize = 1 << 30;
    let ram = Some(15u64 << 30);
    // Default: 8 GiB, no warning.
    assert_eq!(resident_cap_from(None, ram).unwrap(), (8 * GIB, None));
    // Up to the default: no warning.
    assert_eq!(
        resident_cap_from(Some("2048"), ram).unwrap(),
        (2 * GIB, None)
    );
    assert_eq!(
        resident_cap_from(Some("8192"), ram).unwrap(),
        (8 * GIB, None)
    );
    // Between 8 and 10 GiB: allowed, with a warning that prints the budget.
    for mib in ["8193", "9216", "10240"] {
        let (cap, w) = resident_cap_from(Some(mib), ram).unwrap();
        assert_eq!(cap, mib.parse::<usize>().unwrap() << 20);
        let w = w.expect("warning");
        assert!(w.contains("recommended 8192 MiB"), "{w}");
        assert!(w.contains("total RAM 15360 MiB"), "{w}");
    }
    // Above 10 GiB, or not a number: refused.
    for v in ["10241", "16384", "lots", "-1"] {
        let e = resident_cap_from(Some(v), ram).unwrap_err();
        assert!(e.contains(RESIDENT_CAP_ENV), "{e}");
    }
    let e = resident_cap_from(Some("12288"), None).unwrap_err();
    assert!(e.contains("hard ceiling of 10240 MiB"), "{e}");
    assert!(e.contains("total RAM unknown"), "{e}");
}

#[test]
fn many_regions_use_the_index_and_answer_like_the_scan() {
    // 20 small regions spread over the map (> LINEAR_SCAN_MAX_REGIONS).
    let specs: Vec<_> = (0..20)
        .map(|k| {
            let x = 1024 + (k % 5) * 6144;
            let y = 1024 + (k / 5) * 7168 + (k % 3) * 96;
            build_region(&region(
                &format!("r{k}"),
                (x, y),
                (x + 288, y + 320),
                0,
                vec![PaintOp::Ground {
                    shape: Shape::Rect {
                        x0: (x + 40) as f32,
                        y0: (y + 40) as f32,
                        x1: (x + 60) as f32,
                        y1: (y + 60) as f32,
                    },
                    ground_cm: 24_000 + k * 100,
                    weight: GROUND_EXACT,
                }],
            ))
            .unwrap()
        })
        .collect();
    let ar = load(&specs).unwrap();
    assert!(ar.index.is_some());
    for (k, r) in ar.regions.iter().enumerate() {
        let inside = r.min + Vec2::new(50, 50);
        assert_eq!(
            ar.cell_at(inside),
            Some(AuthoredCell::Ground {
                block: 240 + k as i32,
                weight: GROUND_EXACT
            })
        );
        for p in [
            r.min,
            r.max - 1,
            r.min - 1,
            r.max,
            Vec2::new(r.max.x, r.min.y),
        ] {
            let scan = ar.regions.iter().position(|q| q.contains(p));
            let found = ar.region_at(p).map(|q| q.id.clone());
            assert_eq!(found, scan.map(|i| ar.regions[i].id.clone()), "{p:?}");
        }
    }
}

// --------------------------------------------------------------------------
// Stage 2b: the blend ring, seams
// --------------------------------------------------------------------------

#[test]
fn resolve_blend_ground_lerps_from_the_region_terrain_toward_the_block() {
    let blend = |weight| AuthoredCell::Ground { block: 251, weight };
    // Weight 255 is exact; weight w lerps from the column the region gives
    // without the ground layer (here the unauthored result) toward 251.5.
    let base = ground_col(AuthoredCell::None, SeaFill::Auto).resolve(engine(), dry());
    for weight in [1u8, 64, 128, 200, 254] {
        let w = weight as f32 / 255.0;
        let r = ground_col(blend(weight), SeaFill::Auto).resolve(engine(), dry());
        assert_eq!(r.alt, Lerp::lerp(base.alt, 251.5, w), "weight {weight}");
        assert_eq!(r.riverless_alt, Lerp::lerp(base.riverless_alt, 251.5, w));
        assert_eq!(r.warp_factor, Lerp::lerp(base.warp_factor, 0.0, w));
        assert_eq!(r.cliff_offset, base.cliff_offset * (1.0 - w));
        assert_eq!(r.water_dist, base.water_dist);
        assert_eq!(r.water_level, 139.01, "Auto: the sea's level, no sim water");
        let own = ground_col(blend(weight), SeaFill::AuthoredOnly).resolve(engine(), dry());
        assert_eq!(own.water_level, own.alt, "AuthoredOnly: no fill");
    }
    // The weight is monotone: a higher weight is closer to the block.
    let alts: Vec<f32> = (1..=255u8)
        .map(|w| {
            ground_col(blend(w), SeaFill::Auto)
                .resolve(engine(), dry())
                .alt
        })
        .collect();
    assert!(
        alts.windows(2).all(|p| p[0] <= p[1]),
        "monotone toward 251.5"
    );
    assert_eq!(*alts.last().unwrap(), 251.5);
    // In a partial chunk the blend starts from the natural engine column.
    let mut natural = ground_col(blend(128), SeaFill::Auto);
    natural.natural = true;
    let r = apply(Some(natural), engine(), dry(), |l| l);
    let w = 128.0 / 255.0;
    assert_eq!(r.alt, Lerp::lerp(engine().alt, 251.5, w));
    assert_eq!(r.cliff_offset, engine().cliff_offset * (1.0 - w));
    assert_eq!(r.water_dist, engine().water_dist);
    // The natural river level does not survive on authored land.
    assert_eq!(r.water_level, 139.01);
}

#[test]
fn snow_height_is_dropped_on_exact_and_faded_on_blend_cells() {
    let col = |cell| ground_col(cell, SeaFill::Auto);
    let exact = col(AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT,
    });
    assert_eq!(exact.ground_snow_height(1.2), 0.0);
    let half = col(AuthoredCell::Ground {
        block: 251,
        weight: 51,
    });
    assert_eq!(half.ground_snow_height(1.0), 1.0 - 51.0 / 255.0);
    for cell in [
        AuthoredCell::None,
        AuthoredCell::Bank { bed_block: 240 },
        AuthoredCell::Wet {
            surface_block: 240,
            bed_block: 230,
        },
    ] {
        assert_eq!(col(cell).ground_snow_height(1.2), 1.2, "{cell:?}");
    }
}

#[test]
fn ring_weights_fall_smoothly_from_254_to_nothing() {
    use writer::ring_weight;
    for blend in [8u32, 16, 32, 64] {
        let d = 3 * blend;
        assert_eq!(ring_weight(0, blend), None, "the footprint is exact");
        assert_eq!(ring_weight(d, blend), None, "the outer edge is engine");
        assert_eq!(ring_weight(d + 7, blend), None);
        let ws: Vec<u8> = (1..d).filter_map(|k| ring_weight(k, blend)).collect();
        assert!(ws.windows(2).all(|p| p[0] >= p[1]), "non-increasing");
        assert_eq!(ws[0], 254, "never exact");
        assert!(*ws.last().unwrap() >= 1);
        // Halfway: smoothstep(0.5) = 0.5 -> 127.5, rounded half up.
        assert_eq!(ring_weight(d / 2, blend), Some(128), "blend {blend}");
    }
    // Literal values pin the exporter's mirror (integer arithmetic).
    assert_eq!(ring_weight(3, 32), Some(254));
    assert_eq!(ring_weight(24, 32), Some(215));
    assert_eq!(ring_weight(72, 32), Some(40));
    assert_eq!(ring_weight(92, 32), Some(1));
    assert_eq!(ring_weight(95, 32), None, "rounds to 0");
}

#[test]
fn the_ground_ring_op_paints_blend_cells_around_exact_ground() {
    let spec = region("ring", (1024, 1024), (1536, 1280), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1200.0,
                y0: 1100.0,
                x1: 1300.0,
                y1: 1200.0,
            },
            ground_cm: 25_137,
            weight: GROUND_EXACT,
        },
        PaintOp::GroundRing {
            width_m: 32,
            ground_cm: None,
        },
    ]);
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let cell = |x, y| ar.cell_at(Vec2::new(x, y)).unwrap();
    assert_eq!(cell(1250, 1150), AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT
    });
    // 1 m out: chamfer 3 -> 254; 8 m out: chamfer 24 -> 215.
    assert_eq!(cell(1300, 1150), AuthoredCell::Ground {
        block: 251,
        weight: 254
    });
    assert_eq!(cell(1307, 1150), AuthoredCell::Ground {
        block: 251,
        weight: 215
    });
    assert_eq!(cell(1331, 1150), AuthoredCell::None, "beyond the ring");
    let w = |x| match cell(x, 1150) {
        AuthoredCell::Ground { weight, .. } => weight,
        _ => 0,
    };
    let profile: Vec<u8> = (1300..1340).map(w).collect();
    assert!(profile.windows(2).all(|p| p[0] >= p[1]), "{profile:?}");
    // A ring cell counts as a ground column of the chunk summary.
    let s = ar.chunk_summary(Vec2::new(1310 / 32, 1150 / 32)).unwrap();
    assert!(s.ground_columns > 0);
}

#[test]
fn blend_ground_never_holds_or_lies_under_water() {
    // Under a wet cell: refused (ground there must be exact and the bed).
    let under = region("u", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1200.0,
                y1: 1150.0,
            },
            ground_cm: 23_268,
            weight: 200,
        },
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1120.0,
                y0: 1120.0,
                x1: 1140.0,
                y1: 1130.0,
            },
            surface_cm: 23_868,
            bed_cm: 23_268,
        },
    ]);
    expect_error(vec![build_region(&under).unwrap()], "must be exact");
    // Beside a wet cell: a blend cell is never a wall that holds water.
    let beside = region("b", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1200.0,
                y1: 1150.0,
            },
            ground_cm: 24_000,
            weight: 200,
        },
        PaintOp::ClearGround {
            shape: Shape::Rect {
                x0: 1120.0,
                y0: 1120.0,
                x1: 1140.0,
                y1: 1130.0,
            },
        },
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1120.0,
                y0: 1120.0,
                x1: 1140.0,
                y1: 1130.0,
            },
            surface_cm: 23_868,
            bed_cm: 23_268,
        },
    ]);
    expect_error(vec![build_region(&beside).unwrap()], "or not exact");
}

#[test]
fn seam_pairs_are_ground_cells_meeting_the_natural_map() {
    let patch = |partial: bool, sea_fill: SeaFill, ground_cm: i32| {
        let mut s = region("seam", (1024, 1024), (1536, 1216), 32, vec![
            PaintOp::Ground {
                shape: Shape::Rect {
                    x0: 1100.0,
                    y0: 1100.0,
                    x1: 1103.0,
                    y1: 1103.0,
                },
                ground_cm,
                weight: GROUND_EXACT,
            },
        ]);
        s.allow_partial = partial;
        s.sea_fill = sea_fill;
        load(&[build_region(&s).unwrap()]).unwrap()
    };
    // An owned box above sea level: nothing meets the natural map.
    assert!(
        patch(false, SeaFill::Auto, 24_000)
            .ground_seam_pairs()
            .is_empty()
    );
    // A 3 x 3 patch in a partial chunk: its 12 outer edges.
    let pairs = patch(true, SeaFill::Auto, 24_000).ground_seam_pairs();
    assert_eq!(pairs.len(), 12, "{pairs:?}");
    assert!(pairs.iter().all(|(g, n)| {
        (g - n).map(i32::abs).sum() == 1
            && (1100..1103).contains(&g.x)
            && (1100..1103).contains(&g.y)
    }));
    // AuthoredOnly ground below the ocean's top block: every unauthored
    // neighbour, owned chunk or not; above it, none in an owned box.
    assert_eq!(
        patch(false, SeaFill::AuthoredOnly, 13_050)
            .ground_seam_pairs()
            .len(),
        12
    );
    assert!(
        patch(false, SeaFill::AuthoredOnly, 24_000)
            .ground_seam_pairs()
            .is_empty()
    );
}

#[test]
fn site_levelling_applies_on_ground_cells_only_when_the_region_levels() {
    let exact = AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT,
    };
    let with_pref = |pref: (f32, f32)| DryTerrain {
        site_prefer_alt: pref,
        ..dry()
    };
    // No site: the exact block, untouched.
    let r = ground_col(exact, SeaFill::Auto).resolve(engine(), dry());
    assert_eq!((r.alt, r.riverless_alt), (251.5, 251.5));
    // A prepared flat: the plot's altitude is the block (+0.1, the
    // sampler's hack): the top block does not move at any factor.
    for f in [0.25, 0.5, 1.0] {
        let r = ground_col(exact, SeaFill::Auto).resolve(engine(), with_pref((251.1, f)));
        assert_eq!(r.alt as i32, 251, "factor {f}");
    }
    // A plot 3 blocks lower levels its lot (houses level their lots).
    let r = ground_col(exact, SeaFill::Auto).resolve(engine(), with_pref((248.1, 1.0)));
    assert_eq!((r.alt, r.riverless_alt), (248.1, 248.1));
    // AuthoredOnly: the water level follows the levelled ground (no water).
    let r = ground_col(exact, SeaFill::AuthoredOnly).resolve(engine(), with_pref((248.1, 1.0)));
    assert_eq!(r.water_level, r.alt);
    // The region opts out: the patch wins.
    let mut col = ground_col(exact, SeaFill::Auto);
    col.settings.site_levelling = false;
    assert_eq!(col.resolve(engine(), with_pref((248.1, 1.0))).alt, 251.5);
    // Banks keep their behaviour (no levelling).
    let bank = ground_col(AuthoredCell::Bank { bed_block: 251 }, SeaFill::Auto)
        .resolve(engine(), with_pref((248.1, 1.0)));
    assert_eq!(bank.alt, 251.5);
    // Blend cells: levelled on top of the blend.
    let blend = AuthoredCell::Ground {
        block: 251,
        weight: 128,
    };
    let plain = ground_col(blend, SeaFill::Auto).resolve(engine(), dry());
    let levelled = ground_col(blend, SeaFill::Auto).resolve(engine(), with_pref((248.1, 0.5)));
    assert_eq!(levelled.alt, plain.alt + (248.1 - plain.alt) * 0.5);
}

#[test]
fn region_digests_follow_each_region_and_the_new_fields_are_digest_stable() {
    let spec = |id: &str, x: i32, cm: i32| {
        region(id, (x, 1024), (x + 256, 1280), 32, vec![PaintOp::Ground {
            shape: Shape::Rect {
                x0: (x + 64) as f32,
                y0: 1100.0,
                x1: (x + 70) as f32,
                y1: 1106.0,
            },
            ground_cm: cm,
            weight: GROUND_EXACT,
        }])
    };
    let a = load(&[
        build_region(&spec("a", 1024, 24_000)).unwrap(),
        build_region(&spec("b", 2048, 24_000)).unwrap(),
    ])
    .unwrap();
    let b = load(&[
        build_region(&spec("a", 1024, 24_000)).unwrap(),
        build_region(&spec("b", 2048, 24_100)).unwrap(),
    ])
    .unwrap();
    let (da, db) = (a.region_digests(), b.region_digests());
    assert_eq!(da["a"], db["a"], "an untouched region keeps its digest");
    assert_ne!(da["b"], db["b"], "an edited region changes its digest");
    assert_ne!(a.digest(), b.digest());
    // A listed site or a non-default setting changes the region digest; the
    // defaults serialise to nothing.
    let mut listed = spec("a", 1024, 24_000);
    listed.sites_on_patch = vec!["procedural:town:1,2".into()];
    let mut no_level = spec("a", 1024, 24_000);
    no_level.site_levelling = false;
    let mut voids = spec("a", 1024, 24_000);
    voids.max_exposed_void_columns = 3;
    for s in [listed, no_level, voids] {
        let r = load(&[build_region(&s).unwrap()]).unwrap();
        assert_ne!(r.region_digests()["a"], da["a"]);
    }
    let text =
        ron::ser::to_string(&build_region(&spec("a", 1024, 24_000)).unwrap().manifest).unwrap();
    for field in [
        "sites_on_patch",
        "site_levelling",
        "max_exposed_void_columns",
    ] {
        assert!(
            !text.contains(field),
            "{field} serialised at its default: {text}"
        );
    }
    let parsed: RegionManifest = ron::from_str(&text).unwrap();
    assert!(parsed.site_levelling && parsed.sites_on_patch.is_empty());
    assert_eq!(parsed.max_exposed_void_columns, 0);
}

#[test]
fn blend_cells_fade_into_the_natural_map_and_authored_only_chunks_are_lakes() {
    // A partial region: half of a chunk is an exact quay, the rest a ring
    // (half low weights) and unauthored natural columns.
    let mut s = region("fade", (1024, 1024), (1536, 1280), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1104.0,
                y0: 1088.0,
                x1: 1120.0,
                y1: 1120.0,
            },
            ground_cm: 14_150,
            weight: GROUND_EXACT,
        },
        PaintOp::GroundRing {
            width_m: 8,
            ground_cm: None,
        },
    ]);
    s.allow_partial = true;
    let r = load(&[build_region(&s).unwrap()]).unwrap();
    let sum = r.chunk_summary(Vec2::new(34, 34)).unwrap();
    assert!(sum.fading_blend_columns > 0);
    assert!(sum.fading_blend_columns < sum.ground_columns);
    assert!(!sum.authored_only);
    // The same in AuthoredOnly: blend cells are dry there (no fading).
    s.sea_fill = SeaFill::AuthoredOnly;
    let r = load(&[build_region(&s).unwrap()]).unwrap();
    let sum = r.chunk_summary(Vec2::new(34, 34)).unwrap();
    assert_eq!(sum.fading_blend_columns, 0);
    assert!(sum.authored_only);
}

/// The login spawn-fix and the ground queries on exact cells (no table
/// needed there): a feet position inside the ground of an exact cell is
/// lifted to one block above it; free space, water and unauthored columns
/// are left alone.
#[test]
fn buried_positions_are_lifted_to_the_authored_ground() {
    let s = region("lift", (1024, 1024), (1536, 1280), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1100.0,
                y0: 1100.0,
                x1: 1200.0,
                y1: 1200.0,
            },
            ground_cm: 25_050,
            weight: GROUND_EXACT,
        },
    ]);
    let mut sim = crate::sim::WorldSim::empty();
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&s).unwrap()]).unwrap()));
    let p = |z| Vec3::new(1150, 1150, z);
    // Buried 10 m and 90 m (a cave below or not: the search starts on top).
    assert_eq!(sim.buried_ground_lift(p(240), true), Some(251));
    assert_eq!(sim.buried_ground_lift(p(160), true), Some(251));
    // Feet in free space (a cave or an interior under the patch): unmoved.
    assert_eq!(sim.buried_ground_lift(p(240), false), None);
    // Already above the surface (ground dropped): the normal search.
    assert_eq!(sim.buried_ground_lift(p(260), true), None);
    // Outside the patch, outside every region: nothing.
    assert_eq!(
        sim.buried_ground_lift(Vec3::new(1300, 1150, 240), true),
        None
    );
    assert_eq!(sim.buried_ground_lift(Vec3::new(5, 5, 240), true), None);
    // The ground and surface queries on the exact cell, with no clamp.
    assert_eq!(sim.ground_alt_at(p(0).xy()), Some(250.5));
    assert_eq!(sim.surface_alt_at(p(0).xy()), 251.0);
    // A listed site's plot levelling lowers this column to 247.1: the
    // queries and the lift follow the levelled top; its neighbour does not
    // move.
    sim.set_site_levelling([(Vec2::new(1150, 1150), 247.1)]);
    assert_eq!(sim.ground_alt_at(p(0).xy()), Some(247.1));
    assert_eq!(sim.surface_alt_at(p(0).xy()), 248.0);
    assert_eq!(sim.buried_ground_lift(p(240), true), Some(248));
    assert_eq!(sim.ground_alt_at(Vec2::new(1151, 1150)), Some(250.5));
}

/// The layering rule of the ground queries (see the `Land` doc), per
/// directory of the `world` crate:
///
/// * `layer/` reads the chunk table (`Land::get_alt_approx`,
///   `get_gradient_approx`): an authored-aware read there would move spatially
///   extended features (the cave graph, caverns, authored voids) outside the
///   regions;
/// * `site/` reads the authored-aware ground (`Land::ground_alt_at`,
///   `ground_gradient_at`, `surface_alt_at`): plots stand on the authored
///   ground, and a table read would float or bury them on a patch;
/// * `civ/` and `sim/` place sites and build the sim on the natural table and
///   never read the authored ground layer.
///
/// Re-run after every upstream merge: a new call it reports gets the
/// accessor its directory needs, or an allow-list entry with its reason.
#[test]
fn ground_reads_follow_the_layering_rule() {
    const TABLE: &[&str] = &[
        ".get_alt_approx(",
        ".get_surface_alt_approx(",
        ".get_gradient_approx(",
    ];
    const AUTHORED: &[&str] = &[
        ".ground_alt_at(",
        ".ground_gradient_at(",
        ".surface_alt_at(",
    ];
    const GROUND_LAYER: &[&str] = &[".ground_alt_at(", ".ground_gradient_at("];
    // (directory, forbidden calls, allowed (file, trimmed line) with reason).
    type Rule = (
        &'static str,
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
    );
    const RULES: &[Rule] = &[
        ("src/layer", AUTHORED, &[]),
        ("src/site", TABLE, &[]),
        ("src/civ", GROUND_LAYER, &[]),
        ("src/sim", GROUND_LAYER, &[]),
    ];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut bad = Vec::new();
    for (dir, forbidden, allowed) in RULES {
        let mut files = Vec::new();
        walk(&root.join(dir), &mut files);
        assert!(!files.is_empty(), "{dir} holds no source file");
        for f in &files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            for (i, line) in std::fs::read_to_string(f).unwrap().lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                if forbidden.iter().any(|r| t.contains(r))
                    && !allowed.iter().any(|(af, al)| *af == name && t == *al)
                {
                    bad.push(format!("{dir}/{name}:{}: {t}", i + 1));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "ground reads that break the layering rule (see the `Land` doc): {bad:#?}"
    );
}

/// An authored void under a patch that comes within the tolerance of the
/// ground is exposed: a start-up error at the default allowance 0, accepted
/// when the region raises it.
#[test]
fn voids_exposed_by_a_patch_are_counted() {
    use super::post_civ::void_exposure;
    use crate::layer::{
        authored_voids::{AuthoredVoidsBuilder, DiscShape, ProceduralContact},
        traversal::AccommodationTier,
    };
    // Ground at block 250 over x 1100..1200; water (top 249, bed 243) at
    // x 1200..1210 held by a bank at 250 beyond it.
    let spec = |max_exposed: u32| {
        let mut s = region("voids", (1024, 1024), (1536, 1280), 32, vec![
            PaintOp::Ground {
                shape: Shape::Rect {
                    x0: 1100.0,
                    y0: 1100.0,
                    x1: 1200.0,
                    y1: 1200.0,
                },
                ground_cm: 25_050,
                weight: GROUND_EXACT,
            },
            PaintOp::Water {
                shape: Shape::Rect {
                    x0: 1200.0,
                    y0: 1100.0,
                    x1: 1210.0,
                    y1: 1200.0,
                },
                surface_cm: 24_950,
                bed_cm: 24_350,
            },
            PaintOp::Bank {
                shape: Shape::Rect {
                    x0: 1210.0,
                    y0: 1099.0,
                    x1: 1211.0,
                    y1: 1201.0,
                },
                bed_cm: 25_050,
            },
            PaintOp::Bank {
                shape: Shape::Rect {
                    x0: 1200.0,
                    y0: 1099.0,
                    x1: 1210.0,
                    y1: 1100.0,
                },
                bed_cm: 25_050,
            },
            PaintOp::Bank {
                shape: Shape::Rect {
                    x0: 1200.0,
                    y0: 1200.0,
                    x1: 1210.0,
                    y1: 1201.0,
                },
                bed_cm: 25_050,
            },
        ]);
        s.max_exposed_void_columns = max_exposed;
        s
    };
    let voids = |ceiling_z: i32| {
        let mut b = AuthoredVoidsBuilder::default();
        b.push_disc(
            DiscShape {
                centre: Vec2::new(1150, 1150),
                radius: 4.0,
                floor_z: 200,
                ceiling_z,
            },
            ProceduralContact::Seal,
            AccommodationTier::HandAuthored,
            "test_cave",
        );
        b.finish().unwrap()
    };
    let (mut world, _) = crate::World::empty();
    world
        .sim
        .set_authored_rasters_for_test(Some(load(&[build_region(&spec(0)).unwrap()]).unwrap()));
    // Ceiling 10 blocks under the ground: covered.
    let r = void_exposure(&world, Some(&voids(240)));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].exposed, 0);
    assert!(!r[0].is_error());
    // Ceiling 2 blocks under the ground: exposed on the disc's columns.
    let r = void_exposure(&world, Some(&voids(248)));
    let exposed = r[0].exposed;
    assert!(exposed > 0 && r[0].is_error(), "{r:?}");
    assert_eq!(r[0].features[0].0, "test_cave");
    // The region allows the opening.
    world.sim.set_authored_rasters_for_test(Some(
        load(&[build_region(&spec(exposed as u32)).unwrap()]).unwrap(),
    ));
    let r = void_exposure(&world, Some(&voids(248)));
    assert!(!r[0].is_error(), "{r:?}");
}

/// The map/LOD presentation of a column: the rendered column altitude on
/// ground cells only (exact cells, across the patch's edge), the original
/// table path on water, bank, unpainted cells and outside every region, and
/// ground below sea level clamps to sea level in the packed altitude.
#[test]
fn map_presentation_reads_the_rendered_column_only_on_ground_cells() {
    use crate::{
        CONFIG,
        sim::{pack_map_alt, sample_wpos, sample_wpos_presented},
    };
    use common::terrain::map::MapConfig;

    let ground = region("present", (2048, 1024), (2560, 1280), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 2100.0,
                y0: 1100.0,
                x1: 2200.0,
                y1: 1200.0,
            },
            ground_cm: 25_050,
            weight: GROUND_EXACT,
        },
    ]);
    let mut sim = crate::sim::WorldSim::empty();
    sim.set_authored_rasters_for_test(Some(
        load(&[
            build_region(&river_spec()).unwrap(),
            build_region(&ground).unwrap(),
        ])
        .unwrap(),
    ));
    let config = MapConfig::orthographic(
        sim.map_size_lg(),
        CONFIG.sea_level..=CONFIG.sea_level + 1000.0,
    );
    let rendered = 300.0;
    let presented =
        |wpos: Vec2<i32>, alt: f32| sample_wpos_presented(&config, &sim, wpos, || Some(alt));
    let table = |wpos: Vec2<i32>| sample_wpos(&config, &sim, wpos);
    let cell = |wpos: Vec2<i32>| sim.authored_rasters.as_ref().unwrap().cell_at(wpos);
    let as_map = |alt: f32| (alt - CONFIG.sea_level) / 1000.0;

    // Across the patch's west and east edges: the rendered column exactly
    // where the cell is ground, the table path everywhere else.
    let (mut on, mut off) = (0, 0);
    for x in 2060..2240 {
        let p = Vec2::new(x, 1150);
        if matches!(cell(p), Some(AuthoredCell::Ground { .. })) {
            on += 1;
            assert_eq!(presented(p, rendered), as_map(rendered), "at {p:?}");
        } else {
            off += 1;
            assert_eq!(
                presented(p, rendered).to_bits(),
                table(p).to_bits(),
                "at {p:?}"
            );
        }
    }
    assert!(
        on >= 100 && off > 0,
        "the line crosses the patch ({on} on, {off} off)"
    );

    // Water, bank, an unpainted cell of a region and a column outside every
    // region: the original path, the rendered value is never read.
    for (p, expect) in [
        (Vec2::new(1200, 1110), "water"),
        (Vec2::new(1200, 1121), "bank"),
        (Vec2::new(1200, 1180), "none"),
        (Vec2::new(5000, 5000), "outside"),
    ] {
        let kind = match cell(p) {
            Some(AuthoredCell::Wet { .. }) => "water",
            Some(AuthoredCell::Bank { .. }) => "bank",
            Some(AuthoredCell::None) => "none",
            None => "outside",
            Some(AuthoredCell::Ground { .. }) => "ground",
        };
        assert_eq!(kind, expect, "fixture at {p:?}");
        let got = sample_wpos_presented(&config, &sim, p, || panic!("read at {p:?}"));
        assert_eq!(got.to_bits(), table(p).to_bits(), "at {p:?}");
    }

    // No rendered column (not a sample column): the table path.
    let p = Vec2::new(2150, 1150);
    assert_eq!(
        sample_wpos_presented(&config, &sim, p, || None).to_bits(),
        table(p).to_bits()
    );
    // Dry ground below sea level: negative here, sea level once packed.
    assert!(presented(p, CONFIG.sea_level - 30.0) < 0.0);
    assert_eq!(pack_map_alt(presented(p, CONFIG.sea_level - 30.0)), 0);
    assert_eq!(
        pack_map_alt(presented(p, CONFIG.sea_level + 1000.0)),
        8191 << 3
    );
}

/// The world map colour of authored dry ground: a chunk of a
/// `sea_fill: AuthoredOnly` region that is dry ground below sea level (a pit
/// at -10 m, a depression at -1500 m) is coloured as ground even where the
/// chunk table calls it sea or lies below sea level, while an authored lake
/// below sea level in the same region, a sea floor of an `Auto` region and
/// columns outside every region keep exactly the table's colour (water).
#[test]
fn map_colour_of_authored_dry_ground_below_sea_level_is_not_water() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{RiverKind, WorldSim, sample_pos_with},
    };
    use common::terrain::{MapSizeLg, map::MapConfig, vec2_as_uniform_idx};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    let lake = rect(2176.0, 1152.0, 2240.0, 1216.0);
    let mut own = region("dry_below_sea", (2048, 1024), (2304, 1280), 0, vec![
        ground(rect(2080.0, 1056.0, 2272.0, 1248.0), 25_050),
        // A dry pit at -10 m and a dry depression at -1500 m.
        ground(rect(2080.0, 1056.0, 2144.0, 1120.0), 13_050),
        ground(rect(2176.0, 1056.0, 2240.0, 1120.0), -135_950),
        // A lake whose surface is 20 m below sea level.
        PaintOp::ClearGround {
            shape: lake.clone(),
        },
        PaintOp::Water {
            shape: lake,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    // A sea floor 10 m below sea level in an `Auto` region.
    let auto = region("sea_floor", (1024, 1024), (1280, 1280), 0, vec![
        ground(rect(1056.0, 1056.0, 1248.0, 1248.0), 13_050),
        // A patch in a chunk whose other columns keep the natural sea.
        ground(rect(1252.0, 1120.0, 1262.0, 1152.0), 13_050),
    ]);
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // The frozen table: sea everywhere, 10 m below sea level.
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    // The pit's chunk is no table water, only below sea level.
    let pit = vec2_as_uniform_idx(sim.map_size_lg(), Vec2::new(2096, 1072) / CHUNK);
    sim.chunks[pit].river.river_kind = None;
    sim.set_authored_rasters_for_test(Some(
        load(&[build_region(&own).unwrap(), build_region(&auto).unwrap()]).unwrap(),
    ));
    let config = MapConfig::orthographic(
        sim.map_size_lg(),
        CONFIG.sea_level..=CONFIG.sea_level + 1000.0,
    );
    let index = IndexOwned::new(Index::new(0));
    let rgb = |chunk: Vec2<i32>, presented: bool| {
        sample_pos_with(&config, &sim, index.as_index_ref(), None, chunk, presented).rgb
    };
    let is_water_colour = |c: Rgb<u8>| c.r == 0 && c.b > c.g / 2 && c.b >= 0x52;
    let chunk_of = |x: i32, y: i32| Vec2::new(x, y) / CHUNK;

    // Dry ground below sea level: water in the table's colour, ground once
    // presented, with (depression) or without (pit) the table's sea.
    for (wpos, what) in [
        ((2096, 1072), "pit -10 m"),
        ((2192, 1072), "depression -1500 m"),
    ] {
        let c = chunk_of(wpos.0, wpos.1);
        assert!(sim.chunk_is_authored_dry(c), "{what} is dry");
        assert!(is_water_colour(rgb(c, false)), "{what}: table colour");
        assert!(
            !is_water_colour(rgb(c, true)),
            "{what}: presented {:?}",
            rgb(c, true)
        );
    }

    // Water stays water, byte for byte: the lake below sea level, the `Auto`
    // sea floor, and a chunk outside every region.
    for (wpos, what) in [
        ((2192, 1168), "lake -20 m"),
        ((1104, 1104), "Auto sea floor"),
        ((3000, 3000), "outside"),
    ] {
        let c = chunk_of(wpos.0, wpos.1);
        assert!(!sim.chunk_is_authored_dry(c), "{what} is not dry ground");
        assert!(is_water_colour(rgb(c, true)), "{what}: water colour");
        assert_eq!(rgb(c, true), rgb(c, false), "{what}");
    }
}

/// The map colour of an authored lake whose surface lies below sea level:
/// the chunk is water-coloured with the depth shade of the authored water
/// altitude, whether the frozen table calls the chunk sea or dry land, and
/// the presented and table colours agree.
#[test]
fn map_colour_of_authored_lake_below_sea_level_is_water() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{RiverKind, WorldSim, sample_pos_with, water_shade},
    };
    use common::terrain::{MapSizeLg, map::MapConfig, vec2_as_uniform_idx};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    // Two lakes, both with the surface 20 m below sea level and a 10 m depth.
    let lake_sea = rect(2176.0, 1152.0, 2240.0, 1216.0);
    let lake_land = rect(2176.0, 1056.0, 2240.0, 1120.0);
    let mut own = region("lake_below_sea", (2048, 1024), (2304, 1280), 0, vec![
        ground(rect(2080.0, 1040.0, 2272.0, 1248.0), 25_050),
        PaintOp::ClearGround {
            shape: lake_sea.clone(),
        },
        PaintOp::Water {
            shape: lake_sea,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
        PaintOp::ClearGround {
            shape: lake_land.clone(),
        },
        PaintOp::Water {
            shape: lake_land,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // The frozen table: deep sea (100 m) everywhere, except the chunks of
    // the second lake, which are dry land 30 m above sea level.
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 100.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    for (x, y) in [(2192, 1072), (2224, 1072), (2192, 1104), (2224, 1104)] {
        let idx = vec2_as_uniform_idx(sim.map_size_lg(), Vec2::new(x, y) / CHUNK);
        sim.chunks[idx].alt = CONFIG.sea_level + 30.0;
        sim.chunks[idx].water_alt = CONFIG.sea_level + 30.0;
        sim.chunks[idx].river.river_kind = None;
    }
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    let config = MapConfig::orthographic(
        sim.map_size_lg(),
        CONFIG.sea_level..=CONFIG.sea_level + 1000.0,
    );
    let index = IndexOwned::new(Index::new(0));
    let rgb = |wpos: (i32, i32), presented: bool| {
        let c = Vec2::new(wpos.0, wpos.1) / CHUNK;
        sample_pos_with(&config, &sim, index.as_index_ref(), None, c, presented).rgb
    };
    for (wpos, what) in [
        ((2192, 1168), "lake over a table sea chunk"),
        ((2192, 1072), "lake over a table land chunk"),
    ] {
        let c = Vec2::new(wpos.0, wpos.1) / CHUNK;
        assert!(sim.chunk_wet_authored(&config, c), "{what}: wet");
    }
    // Over the table sea the table already draws water, so the presented and
    // table colours are the same bytes (the table's 100 m sea shade).
    assert_eq!(rgb((2192, 1168), true), Rgb::from(water_shade(100.0)));
    assert_eq!(
        rgb((2192, 1168), true),
        rgb((2192, 1168), false),
        "lake over a table sea chunk: presented == table"
    );
    // Over land the authored depth (10 m bed to surface) picks the bucket,
    // not the ~1 m of natural altitude under the surface.
    assert_eq!(rgb((2192, 1072), true), Rgb::from(water_shade(10.0)));
    assert_ne!(rgb((2192, 1072), true), rgb((2192, 1072), false));
}

/// A lake below sea level over a chunk the frozen table calls land, with
/// column samples whose altitude is itself below sea level (the real map path
/// always has them): the guard keys on the table chunk altitude, so the chunk
/// is shaded by the authored depth bucket of `chunk_water_depth`, not by the
/// table's water altitude over the column altitude.
#[test]
fn map_colour_of_authored_lake_below_sea_over_table_land_with_samples() {
    use crate::{
        CONFIG,
        column::ColumnGen,
        index::{Index, IndexOwned},
        sim::{WorldSim, sample_pos_with, water_shade},
        util::Sampler,
    };
    use common::terrain::{MapSizeLg, map::MapConfig, vec2_as_uniform_idx};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let lake = rect(2176.0, 1056.0, 2240.0, 1120.0);
    let mut own = region(
        "lake_below_sea_samples",
        (2048, 1024),
        (2304, 1280),
        0,
        vec![
            PaintOp::Ground {
                shape: rect(2080.0, 1040.0, 2272.0, 1248.0),
                ground_cm: 25_050,
                weight: GROUND_EXACT,
            },
            PaintOp::ClearGround {
                shape: lake.clone(),
            },
            PaintOp::Water {
                shape: lake,
                surface_cm: 12_050,
                bed_cm: 11_050,
            },
        ],
    );
    own.sea_fill = SeaFill::AuthoredOnly;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // The frozen table: land 30 m above sea level everywhere.
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level + 30.0;
        c.water_alt = CONFIG.sea_level + 30.0;
        c.river.river_kind = None;
    }
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    let config = MapConfig::orthographic(
        sim.map_size_lg(),
        CONFIG.sea_level..=CONFIG.sea_level + 1000.0,
    );
    let index = IndexOwned::new(Index::new(0));
    let index = index.as_index_ref();
    let pos = Vec2::new(2192, 1072) / CHUNK;
    assert!(sim.chunk_wet_authored(&config, pos), "the lake is wet");

    // Column samples whose altitude lies below sea level over the table land.
    let mut samples = vec![None; sim.map_size_lg().chunks_len()];
    let mut col = ColumnGen::new(&sim)
        .get((pos * CHUNK, index, None))
        .expect("column on the map");
    col.alt = CONFIG.sea_level - 20.0;
    samples[vec2_as_uniform_idx(sim.map_size_lg(), pos)] = Some(col);

    let presented = sample_pos_with(&config, &sim, index, Some(&samples), pos, true).rgb;
    let depth = sim.chunk_water_depth(pos).expect("authored lake depth");
    assert!(
        (water_shade(depth as f64) != water_shade(50.0)),
        "the authored depth {depth} m and the table's water-over-column 50 m differ in bucket"
    );
    assert_eq!(
        presented,
        Rgb::from(water_shade(depth.max(0.0) as f64)),
        "below-sea lake over table land is shaded by the authored depth ({depth} m)"
    );
}

/// The client tells water from land by a zero red channel
/// (`r == 0 && (g > 8 || b > 8)`), so no land chunk of the map may be painted
/// with red zero, authored dry ground included, while real water (the table's
/// sea, an authored lake) keeps red zero. The map is sampled unshaded with no
/// column samples, so the land base colour is the altitude-green default,
/// whose red channel is zero.
#[test]
fn land_on_the_map_never_has_a_zero_red_channel() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{RiverKind, WorldSim, sample_pos_with},
    };
    use common::terrain::{MapSizeLg, map::MapConfig, vec2_as_uniform_idx};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    let lake = rect(2176.0, 1152.0, 2240.0, 1216.0);
    let mut own = region("zero_red", (2048, 1024), (2304, 1280), 0, vec![
        // High dry land, a dry pit at -10 m and a dry depression at -1500 m.
        ground(rect(2080.0, 1056.0, 2272.0, 1248.0), 75_050),
        ground(rect(2080.0, 1056.0, 2144.0, 1120.0), 13_050),
        ground(rect(2176.0, 1056.0, 2240.0, 1120.0), -135_950),
        PaintOp::ClearGround {
            shape: lake.clone(),
        },
        PaintOp::Water {
            shape: lake,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // A table of sea with a patch of high table land (no river) among it.
    for (i, c) in sim.chunks.iter_mut().enumerate() {
        if i % 5 == 0 {
            c.alt = CONFIG.sea_level + 400.0;
            c.water_alt = CONFIG.sea_level;
            c.river.river_kind = None;
        } else {
            c.alt = CONFIG.sea_level - 10.0;
            c.water_alt = CONFIG.sea_level;
            c.river.river_kind = Some(RiverKind::Ocean);
        }
    }
    let pit = vec2_as_uniform_idx(sim.map_size_lg(), Vec2::new(2096, 1072) / CHUNK);
    sim.chunks[pit].river.river_kind = None;
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    let mut config = MapConfig::orthographic(
        sim.map_size_lg(),
        CONFIG.sea_level..=CONFIG.sea_level + 1000.0,
    );
    config.is_shaded = false;
    let index = IndexOwned::new(Index::new(0));
    let client_water = |c: Rgb<u8>| c.r == 0 && (c.g > 8 || c.b > 8);

    let (mut land, mut water, mut lake_water) = (0, 0, 0);
    for y in 0..128 {
        for x in 0..128 {
            let c = Vec2::new(x, y);
            let idx = vec2_as_uniform_idx(sim.map_size_lg(), c);
            for presented in [false, true] {
                let rgb =
                    sample_pos_with(&config, &sim, index.as_index_ref(), None, c, presented).rgb;
                // The table paints the pit (below sea level, no river) as water.
                let table_water = sim.chunks[idx].river.river_kind.is_some() || idx == pit;
                let dry = presented && sim.chunk_is_authored_dry(c);
                // An authored lake over table land is water once presented.
                let lake = presented
                    && !table_water
                    && !dry
                    && sim.chunk_water(c).is_some_and(|w| w.wet());
                let is_water = (table_water && !dry) || lake;
                if is_water {
                    water += 1;
                    lake_water += usize::from(lake);
                    assert!(
                        client_water(rgb),
                        "{c:?} (presented {presented}) water lost its zero red: {rgb:?}"
                    );
                } else {
                    land += 1;
                    assert_ne!(rgb.r, 0, "{c:?} (presented {presented}) land has red zero");
                    assert!(
                        !client_water(rgb),
                        "{c:?} (presented {presented}) reads as water"
                    );
                }
            }
        }
    }
    assert!(
        land > 1000 && water > 1000 && lake_water > 0,
        "land {land}, water {water}, lake {lake_water}"
    );
}

/// The dry rule is per chunk and follows the majority of its columns: a
/// river or sea chunk a dry `sea_fill: AuthoredOnly` patch covers for more
/// than half is coloured as ground, for less than half it keeps exactly the
/// table's water colour and river link; a mostly dry chunk the patch does not
/// cover entirely keeps its river link (the river still runs through it),
/// a fully covered one drops it; at the region's edge, where the patch can
/// cover at most half of the first chunk, that chunk stays water and the
/// chunk outside the region is untouched.
#[test]
fn map_colour_of_partly_authored_dry_chunks_follows_the_majority() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{RiverKind, WorldSim, sample_pos_with},
    };
    use common::terrain::{MapSizeLg, map::MapConfig, vec2_as_uniform_idx};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let dry = |shape| PaintOp::Ground {
        shape,
        ground_cm: 13_050,
        weight: GROUND_EXACT,
    };
    let mut own = region("straddle", (2560, 1024), (2816, 1280), 0, vec![
        // River chunks: 26 of 32 columns, 12 of 32, all 32.
        dry(rect(2624.0, 1056.0, 2650.0, 1088.0)),
        dry(rect(2688.0, 1056.0, 2700.0, 1088.0)),
        dry(rect(2752.0, 1056.0, 2784.0, 1088.0)),
        // Sea chunks: 26 of 32 columns, 12 of 32.
        dry(rect(2624.0, 1120.0, 2650.0, 1152.0)),
        dry(rect(2688.0, 1120.0, 2700.0, 1152.0)),
        // From the region's 16 m margin into the next chunk.
        dry(rect(2576.0, 1184.0, 2624.0, 1216.0)),
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    // Unauthored columns keep the natural map (the table's water), so a
    // chunk the patch covers partly straddles patch and natural water.
    own.allow_partial = true;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    let size = sim.map_size_lg();
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    // A river along y = 1056..1088 flowing east, one chunk to the next.
    let chunk_of = |x: i32, y: i32| Vec2::new(x, y) / CHUNK;
    for x in (2560..2816).step_by(CHUNK as usize) {
        let c = &mut sim.chunks[vec2_as_uniform_idx(size, chunk_of(x, 1056))];
        c.river.river_kind = Some(RiverKind::River {
            cross_section: Vec2::new(12.0, 2.0),
        });
        c.downhill = Some(Vec2::new(x + CHUNK, 1056));
    }
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    let config = MapConfig::orthographic(size, CONFIG.sea_level..=CONFIG.sea_level + 1000.0);
    let index = IndexOwned::new(Index::new(0));
    let sample = |chunk: Vec2<i32>, presented: bool| {
        sample_pos_with(&config, &sim, index.as_index_ref(), None, chunk, presented)
    };
    let is_water_colour = |c: Rgb<u8>| c.r == 0 && c.b > c.g / 2 && c.b >= 0x52;
    let links = |chunk: Vec2<i32>, presented: bool| {
        sample(chunk, presented)
            .connections
            .map_or(0, |c| c.iter().flatten().count())
    };
    let same = |chunk: Vec2<i32>, what: &str| {
        let (new, old) = (sample(chunk, true), sample(chunk, false));
        assert_eq!(new.rgb, old.rgb, "{what}: colour");
        assert_eq!(new.alt.to_bits(), old.alt.to_bits(), "{what}: alt");
        assert_eq!(links(chunk, true), links(chunk, false), "{what}: links");
    };

    // River chunks (coloured as land with or without the patch: the link
    // is what shows the river).
    let (most, few, all) = (
        chunk_of(2624, 1056),
        chunk_of(2688, 1056),
        chunk_of(2752, 1056),
    );
    assert!(sim.chunk_is_authored_dry(most));
    assert_eq!(
        links(most, true),
        1,
        "mostly dry river chunk keeps its link"
    );
    assert!(!sim.chunk_is_authored_dry(few));
    same(few, "river chunk 12/32 dry");
    assert_eq!(links(few, true), 1);
    assert!(sim.chunk_is_authored_dry(all));
    assert_eq!(links(all, false), 1);
    assert_eq!(links(all, true), 0, "fully dry river chunk drops its link");
    // Upstream of the patch, untouched.
    same(chunk_of(2592, 1056), "upstream river chunk");

    // Sea chunks.
    let (most, few) = (chunk_of(2624, 1120), chunk_of(2688, 1120));
    assert!(sim.chunk_is_authored_dry(most));
    assert!(is_water_colour(sample(most, false).rgb));
    assert!(
        !is_water_colour(sample(most, true).rgb),
        "sea chunk 26/32 dry"
    );
    assert!(!sim.chunk_is_authored_dry(few));
    assert!(is_water_colour(sample(few, true).rgb));
    same(few, "sea chunk 12/32 dry");

    // The region's edge: the first chunk holds 16 of 32 dry columns (a tie
    // is water), the next is all dry, the one outside is untouched.
    let (edge, next, outside) = (
        chunk_of(2560, 1184),
        chunk_of(2592, 1184),
        chunk_of(2528, 1184),
    );
    assert!(!sim.chunk_is_authored_dry(edge));
    assert!(is_water_colour(sample(edge, true).rgb));
    same(edge, "edge chunk 16/32 dry");
    assert!(sim.chunk_is_authored_dry(next));
    assert!(
        !is_water_colour(sample(next, true).rgb),
        "chunk next to the edge"
    );
    same(outside, "chunk outside the region");
}

#[test]
fn biome_label_stays_off_authored_dry_ground() {
    use crate::{
        CONFIG,
        civ::{PoiKind, PointOfInterest},
        sim::{RiverKind, WorldSim},
    };
    use common::{
        store::Store,
        terrain::{MapSizeLg, map::MapConfig},
    };

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    // A dry pit carved 10 m below sea level into a natural sea.
    let mut own = region("dry_below_sea", (2048, 1024), (2304, 1280), 0, vec![
        ground(rect(2080.0, 1056.0, 2144.0, 1120.0), 13_050),
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    let auto = region("sea_floor", (2048, 1024), (2304, 1280), 0, vec![ground(
        rect(2080.0, 1056.0, 2144.0, 1120.0),
        13_050,
    )]);

    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    let mut pois = Store::<PointOfInterest>::default();
    let sea = pois.insert(PointOfInterest {
        name: "Sea".to_string(),
        kind: PoiKind::Biome(1),
        loc: Vec2::zero(),
    });
    let size = sim.map_size_lg();
    let config = MapConfig::orthographic(size, CONFIG.sea_level..=CONFIG.sea_level + 1000.0);
    let pit = Vec2::new(2096, 1072) / CHUNK;
    let tag = |sim: &mut WorldSim, keep: &dyn Fn(Vec2<i32>) -> bool| {
        for (posi, c) in sim.chunks.iter_mut().enumerate() {
            let pos = common::terrain::uniform_idx_as_vec2(size, posi);
            c.poi = keep(pos).then_some(sea);
        }
    };
    let everywhere = |_: Vec2<i32>| true;

    // Procedural (no rasters), and only an Auto region: every label stays put.
    tag(&mut sim, &everywhere);
    let labels = |sim: &WorldSim, loc| sim.biome_label_chunk(&config, sea, loc);
    sim.set_authored_rasters_for_test(None);
    assert_eq!(labels(&sim, pit), Some(pit), "no rasters");
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&auto).unwrap()]).unwrap()));
    assert!(!sim.chunk_is_authored_dry(pit));
    assert_eq!(labels(&sim, pit), Some(pit), "only an Auto region");

    // With the authored dry region, the label named inside the pit moves to
    // the nearest chunk of the same biome that is not dry.
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    assert!(sim.chunk_is_authored_dry(pit), "the pit is dry");
    let moved = labels(&sim, pit).expect("a wet chunk of the biome exists");
    assert!(
        !sim.chunk_is_authored_dry(moved),
        "the label left the dry pit"
    );
    assert_ne!(moved, pit);
    for pos in (0..128).flat_map(|x| (0..128).map(move |y| Vec2::new(x, y))) {
        if !sim.chunk_is_authored_dry(pos) {
            assert!(
                pit.distance_squared(moved) <= pit.distance_squared(pos),
                "{pos:?} is not closer than {moved:?}"
            );
        }
    }
    // A label already on wet ground is untouched.
    let wet = Vec2::new(1, 1);
    assert_eq!(labels(&sim, wet), Some(wet));

    // A biome made only of dry chunks has nowhere to put its label.
    let only_dry = |p: Vec2<i32>| sim.chunk_is_authored_dry(p);
    let dry_set: Vec<bool> = (0..128 * 128)
        .map(|posi| only_dry(common::terrain::uniform_idx_as_vec2(size, posi)))
        .collect();
    for (posi, c) in sim.chunks.iter_mut().enumerate() {
        c.poi = dry_set[posi].then_some(sea);
    }
    assert_eq!(labels(&sim, pit), None, "dropped");
}

/// The shared presentation helper on a tiny synthetic raster: authored dry
/// ground of a `sea_fill: AuthoredOnly` region (fully or mostly covered
/// chunks) is presented dry, the region's lake below sea level, an `Auto`
/// region's sea floor, a minority-dry chunk, a chunk outside every region and
/// the basement map present the table unchanged; the helper's map sample and
/// altitude without column samples equal the table-path functions there.
#[test]
fn chunk_presentation_on_a_synthetic_raster() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{ChunkPresentation, RiverKind, WorldSim, sample_pos, sample_wpos},
    };
    use common::terrain::{MapSizeLg, map::MapConfig};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    let lake = rect(2176.0, 1152.0, 2240.0, 1216.0);
    let mut own = region("dry_below_sea", (2048, 1024), (2304, 1280), 0, vec![
        ground(rect(2080.0, 1056.0, 2272.0, 1248.0), 25_050),
        // A dry pit at -10 m.
        ground(rect(2080.0, 1056.0, 2144.0, 1120.0), 13_050),
        // A lake whose surface is 20 m below sea level.
        PaintOp::ClearGround {
            shape: lake.clone(),
        },
        PaintOp::Water {
            shape: lake,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    // Dry patches covering 26 and 12 of a sea chunk's 32 columns.
    let mut partial = region("partial", (2560, 1024), (2816, 1280), 0, vec![
        ground(rect(2624.0, 1120.0, 2650.0, 1152.0), 13_050),
        ground(rect(2688.0, 1120.0, 2700.0, 1152.0), 13_050),
        // Exactly half and one column over half: `chunk_water` calls a chunk
        // wet when water covers at least half of it, so 16 dry columns of 32
        // keep the table and 17 do not.
        ground(rect(2720.0, 1120.0, 2736.0, 1152.0), 13_050),
        ground(rect(2752.0, 1120.0, 2769.0, 1152.0), 13_050),
    ]);
    partial.sea_fill = SeaFill::AuthoredOnly;
    partial.allow_partial = true;
    // A sea floor 10 m below sea level in an `Auto` region.
    let auto = region("sea_floor", (1024, 1024), (1280, 1280), 0, vec![ground(
        rect(1056.0, 1056.0, 1248.0, 1248.0),
        13_050,
    )]);
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // The frozen table: sea everywhere, 10 m below sea level.
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    sim.set_authored_rasters_for_test(Some(
        load(&[
            build_region(&own).unwrap(),
            build_region(&partial).unwrap(),
            build_region(&auto).unwrap(),
        ])
        .unwrap(),
    ));
    let size = sim.map_size_lg();
    let config = MapConfig::orthographic(size, CONFIG.sea_level..=CONFIG.sea_level + 1000.0);
    let basement = MapConfig {
        is_basement: true,
        ..config
    };
    let index = IndexOwned::new(Index::new(0));
    let chunk_of = |x: i32, y: i32| Vec2::new(x, y) / CHUNK;
    let dry = ChunkPresentation {
        authored_dry: true,
        authored_fully_dry: false,
    };
    let fully_dry = ChunkPresentation {
        authored_dry: true,
        authored_fully_dry: true,
    };

    for (wpos, what, expected) in [
        ((2096, 1072), "pit -10 m, fully covered", fully_dry),
        ((2624, 1120), "sea chunk 26/32 dry", dry),
        (
            (2688, 1120),
            "sea chunk 12/32 dry",
            ChunkPresentation::TABLE,
        ),
        (
            (2720, 1120),
            "sea chunk 16/32 dry: a tie is wet",
            ChunkPresentation::TABLE,
        ),
        ((2752, 1120), "sea chunk 17/32 dry", dry),
        (
            (1252, 1120),
            "Auto chunk, natural sea in the unauthored columns",
            ChunkPresentation::TABLE,
        ),
        ((2192, 1168), "lake -20 m", ChunkPresentation::TABLE),
        ((1104, 1104), "Auto sea floor", ChunkPresentation::TABLE),
        (
            (3000, 3000),
            "outside every region",
            ChunkPresentation::TABLE,
        ),
    ] {
        let c = chunk_of(wpos.0, wpos.1);
        let presentation = sim.chunk_presentation(&config, c);
        assert_eq!(presentation, expected, "{what}");
        assert_eq!(presentation.authored_dry, sim.chunk_is_authored_dry(c));
        assert_eq!(
            sim.chunk_presentation(&basement, c),
            ChunkPresentation::TABLE,
            "{what}: basement"
        );

        // On table chunks the presented map sample is exactly `sample_pos`.
        let new = sim.presented_map_sample(&config, index.as_index_ref(), None, c);
        if expected == ChunkPresentation::TABLE {
            let table = sample_pos(&config, &sim, index.as_index_ref(), None, c);
            assert_eq!(new.rgb, table.rgb, "{what}: table colour");
            assert_eq!(new.alt.to_bits(), table.alt.to_bits(), "{what}: table alt");
        }

        // Without column samples the presented altitude is the table's.
        for dx in [0, 1, 17] {
            let w = c * CHUNK + Vec2::new(dx, 0);
            assert_eq!(
                sim.presented_wpos(&config, w, None).to_bits(),
                sample_wpos(&config, &sim, w).to_bits(),
                "{what}: altitude at {w:?}"
            );
        }
    }

    // What the colour, relief and connection decisions read.
    let river = Some(RiverKind::River {
        cross_section: Vec2::new(12.0, 2.0),
    });
    let table = ChunkPresentation::TABLE;
    assert_eq!(table.water_kind(river), river);
    assert_eq!(table.connection_kind(river), river);
    assert_eq!(table.water_depth(3.0), 3.0);
    assert_eq!(table.colour_alt(-0.25, 0.0), -0.25);
    assert!(table.shows_water(true));
    assert_eq!(dry.water_kind(river), None);
    assert_eq!(
        dry.connection_kind(river),
        river,
        "mostly dry keeps the link"
    );
    assert_eq!(dry.water_depth(3.0), 0.0);
    assert_eq!(dry.colour_alt(-0.25, 0.0), 0.0);
    assert_eq!(dry.colour_alt(0.5, 0.0), 0.5);
    assert!(!dry.shows_water(true));
    assert!(!table.shows_water(false));
    assert_eq!(fully_dry.connection_kind(river), None);

    // `table_alt` takes the caller's altitude: dry ground keeps it, a table
    // chunk is raised to the water plane.
    assert_eq!(dry.table_alt(5.0, 9.0), 5.0);
    assert_eq!(fully_dry.table_alt(5.0, 9.0), 5.0);
    assert_eq!(table.table_alt(5.0, 9.0), 9.0);
    assert_eq!(table.table_alt(12.0, 9.0), 12.0);
}

/// A chunk whose 1024 columns are all authored but 504 of which are water is
/// mostly dry, yet not "fully dry": it keeps the table's river link. (The
/// authored-column count alone, which includes the wet columns, would call it
/// fully dry.)
#[test]
fn a_fully_authored_chunk_with_wet_columns_is_not_fully_dry() {
    use crate::sim::WorldSim;
    use common::terrain::{MapSizeLg, map::MapConfig};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    // The chunk is x 2080..2112, y 1056..1088; the lake takes 28 x 18 of it.
    let mut own = region("mostly_dry", (2048, 1024), (2304, 1280), 0, vec![
        PaintOp::Ground {
            shape: rect(2080.0, 1056.0, 2272.0, 1248.0),
            ground_cm: 25_050,
            weight: GROUND_EXACT,
        },
        PaintOp::ClearGround {
            shape: rect(2082.0, 1058.0, 2110.0, 1076.0),
        },
        PaintOp::Water {
            shape: rect(2082.0, 1058.0, 2110.0, 1076.0),
            surface_cm: 24_068,
            bed_cm: 23_468,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    let config = MapConfig::orthographic(sim.map_size_lg(), 0.0..=1000.0);
    let chunk = Vec2::new(2080, 1056) / CHUNK;
    let summary = sim
        .authored_rasters
        .as_ref()
        .and_then(|r| r.chunk_summary(chunk))
        .expect("the chunk is authored");
    assert_eq!(summary.authored_columns, 1024);
    assert_eq!(summary.wet_columns, 504);
    let presentation = sim.chunk_presentation(&config, chunk);
    assert!(
        presentation.authored_dry,
        "504 of 1024 columns wet is mostly dry"
    );
    assert!(
        !presentation.authored_fully_dry,
        "a chunk with wet columns keeps its river link"
    );
}

#[test]
fn a_levelled_altitude_below_zero_truncates_like_the_sampler() {
    // The sampler's column top is `alt as i32` (towards zero), so the exact
    // ground top for -3.5 is block -3, not -4.
    assert_eq!(super::queries::top_block_of_levelled_alt(-3.5), -3);
    assert_eq!(super::queries::top_block_of_levelled_alt(3.5), 3);
    assert_eq!(super::queries::top_block_of_levelled_alt(-0.2), 0);
}

#[test]
fn airship_route_map_presents_authored_dry_ground() {
    use crate::{
        CONFIG,
        index::{Index, IndexOwned},
        sim::{ChunkPresentation, RiverKind, WorldSim, sample_pos, sample_wpos},
    };
    use common::terrain::{MapSizeLg, map::MapConfig};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let mut own = region("dry_pit", (2048, 1024), (2304, 1280), 0, vec![
        PaintOp::Ground {
            shape: rect(2080.0, 1056.0, 2272.0, 1248.0),
            ground_cm: 25_050,
            weight: GROUND_EXACT,
        },
        // A dry pit at -10 m.
        PaintOp::Ground {
            shape: rect(2080.0, 1056.0, 2144.0, 1120.0),
            ground_cm: 13_050,
            weight: GROUND_EXACT,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    // The frozen table: sea everywhere, 10 m below sea level.
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    let config = MapConfig::orthographic(sim.map_size_lg(), 0.0..=1000.0);
    let index = IndexOwned::new(Index::new(0));
    let chunk_of = |x: i32, y: i32| Vec2::new(x, y) / CHUNK;
    let pit = chunk_of(2096, 1072);
    let outside = chunk_of(3000, 3000);
    let pit_wpos = pit * CHUNK;
    let outside_wpos = outside * CHUNK;

    // The airship map is exactly the table-only one at `c`/`w`.
    let check_same_at = |sim: &WorldSim, c: Vec2<i32>, w: Vec2<i32>, what: &str| {
        let new = sim.presented_map_sample(&config, index.as_index_ref(), None, c);
        let old = sample_pos(&config, sim, index.as_index_ref(), None, c);
        assert_eq!(new.rgb, old.rgb, "{what}: colour");
        assert_eq!(new.alt.to_bits(), old.alt.to_bits(), "{what}: alt");
        assert_eq!(
            new.connections.is_some(),
            old.connections.is_some(),
            "{what}: link"
        );
        assert_eq!(
            sim.presented_table_wpos(&config, w).to_bits(),
            sample_wpos(&config, sim, w).to_bits(),
            "{what}: relief"
        );
    };
    // No ground layer: identical everywhere.
    let check_identical = |sim: &WorldSim, what: &str| {
        for (c, w) in [(pit, pit_wpos), (outside, outside_wpos)] {
            check_same_at(sim, c, w, what);
        }
    };
    check_identical(&sim, "no ground layer");
    // Only an `Auto` region: nothing is authored dry.
    let auto = region("sea_floor", (2048, 1024), (2304, 1280), 0, vec![
        PaintOp::Ground {
            shape: rect(2080.0, 1056.0, 2272.0, 1248.0),
            ground_cm: 13_050,
            weight: GROUND_EXACT,
        },
    ]);
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&auto).unwrap()]).unwrap()));
    check_identical(&sim, "only an Auto region");

    // With the dry region: the fully dry pit draws no river link and its
    // relief is its own altitude, not the table water plane.
    sim.set_authored_rasters_for_test(Some(load(&[build_region(&own).unwrap()]).unwrap()));
    assert!(sim.chunk_presentation(&config, pit).authored_fully_dry);
    let table = sample_pos(&config, &sim, index.as_index_ref(), None, pit);
    assert!(table.connections.is_some(), "the table draws a link");
    let new = sim.presented_map_sample(&config, index.as_index_ref(), None, pit);
    assert!(new.connections.is_none(), "no link on a fully dry chunk");
    let relief = sim.presented_table_wpos(&config, pit_wpos);
    let water_plane = sample_wpos(&config, &sim, pit_wpos);
    assert_eq!(
        relief.to_bits(),
        ((CONFIG.sea_level - 10.0) / config.gain).to_bits()
    );
    assert!(relief < water_plane, "not the table water plane");
    // Outside every region nothing changes.
    check_same_at(&sim, outside, outside_wpos, "outside every region");

    // The basement map presents the table unchanged, even over the dry pit.
    let basement = MapConfig {
        is_basement: true,
        ..config
    };
    assert_eq!(
        sim.chunk_presentation(&basement, pit),
        ChunkPresentation::TABLE
    );
    assert_eq!(
        sim.presented_table_wpos(&basement, pit_wpos).to_bits(),
        sample_wpos(&basement, &sim, pit_wpos).to_bits()
    );
    let base_new = sim.presented_map_sample(&basement, index.as_index_ref(), None, pit);
    let base_old = sample_pos(&basement, &sim, index.as_index_ref(), None, pit);
    assert_eq!(base_new.rgb, base_old.rgb);
    assert_eq!(
        base_new.connections.is_some(),
        base_old.connections.is_some()
    );

    // A chunk that is mostly (26 of 32 columns) but not fully dry keeps the
    // table's river link.
    let mut partial = region("partial", (2560, 1024), (2816, 1280), 0, vec![
        PaintOp::Ground {
            shape: rect(2624.0, 1120.0, 2650.0, 1152.0),
            ground_cm: 13_050,
            weight: GROUND_EXACT,
        },
    ]);
    partial.sea_fill = SeaFill::AuthoredOnly;
    partial.allow_partial = true;
    sim.set_authored_rasters_for_test(Some(
        load(&[build_region(&own).unwrap(), build_region(&partial).unwrap()]).unwrap(),
    ));
    let mostly = chunk_of(2624, 1120);
    let presentation = sim.chunk_presentation(&config, mostly);
    assert!(presentation.authored_dry && !presentation.authored_fully_dry);
    let table = sample_pos(&config, &sim, index.as_index_ref(), None, mostly);
    let new = sim.presented_map_sample(&config, index.as_index_ref(), None, mostly);
    assert!(table.connections.is_some(), "the table draws a link");
    assert!(
        new.connections.is_some(),
        "a mostly dry chunk keeps its link"
    );
}

/// The two flags the terrain probe's sim table writes per chunk: authored dry
/// ground (a pit), authored water (a lake) and a table chunk (outside every
/// region, and an `Auto` sea floor); no rasters at all gives 0 and 0.
#[test]
fn authored_map_columns_on_a_synthetic_raster() {
    use crate::{
        CONFIG,
        sim::{RiverKind, WorldSim},
    };
    use common::terrain::{MapSizeLg, map::MapConfig};

    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |shape, ground_cm| PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    };
    let lake = rect(2176.0, 1152.0, 2240.0, 1216.0);
    let mut own = region("dry_below_sea", (2048, 1024), (2304, 1280), 0, vec![
        ground(rect(2080.0, 1056.0, 2272.0, 1248.0), 25_050),
        ground(rect(2080.0, 1056.0, 2144.0, 1120.0), 13_050),
        PaintOp::ClearGround {
            shape: lake.clone(),
        },
        PaintOp::Water {
            shape: lake,
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    own.sea_fill = SeaFill::AuthoredOnly;
    // Chunks that straddle authored and natural columns: a sliver of lake in a
    // natural sea chunk, and a few water columns beside mostly dry ground.
    let mut partial = region("partial_wet", (2560, 1024), (2816, 1280), 0, vec![
        PaintOp::Water {
            shape: rect(2624.0, 1120.0, 2628.0, 1152.0),
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
        ground(rect(2688.0, 1120.0, 2716.0, 1152.0), 13_050),
        PaintOp::Water {
            shape: rect(2716.0, 1120.0, 2720.0, 1152.0),
            surface_cm: 12_050,
            bed_cm: 11_050,
        },
    ]);
    partial.sea_fill = SeaFill::AuthoredOnly;
    partial.allow_partial = true;
    let auto = region("sea_floor", (1024, 1024), (1280, 1280), 0, vec![ground(
        rect(1056.0, 1056.0, 1248.0, 1248.0),
        13_050,
    )]);
    let mut sim = WorldSim::empty_with_size_for_test(MapSizeLg::new(Vec2::new(7, 7)).unwrap());
    for c in sim.chunks.iter_mut() {
        c.alt = CONFIG.sea_level - 10.0;
        c.water_alt = CONFIG.sea_level;
        c.river.river_kind = Some(RiverKind::Ocean);
    }
    let config = MapConfig::orthographic(sim.map_size_lg(), 0.0..=1.0);
    let cols = |sim: &WorldSim, wpos: (i32, i32)| {
        let c = Vec2::new(wpos.0, wpos.1) / CHUNK;
        (
            sim.chunk_wet_authored(&config, c),
            sim.chunk_presentation(&config, c).authored_dry,
        )
    };
    let probes = [
        ((2096, 1072), "dry pit"),
        ((2192, 1168), "wet lake"),
        ((3000, 3000), "outside every region"),
        ((1104, 1104), "Auto sea floor"),
    ];

    sim.set_authored_rasters_for_test(None);
    for (wpos, what) in probes {
        assert_eq!(cols(&sim, wpos), (false, false), "{what}: no rasters");
    }

    sim.set_authored_rasters_for_test(Some(
        load(&[
            build_region(&own).unwrap(),
            build_region(&partial).unwrap(),
            build_region(&auto).unwrap(),
        ])
        .unwrap(),
    ));
    assert_eq!(cols(&sim, (2096, 1072)), (false, true), "dry pit");
    assert!(
        cols(&sim, (2624, 1120)).0,
        "natural sea plus a sliver of authored water is wet_authored"
    );
    assert!(
        !cols(&sim, (2688, 1120)).0,
        "authored water in a mostly dry chunk that chunk_water calls dry"
    );
    assert_eq!(cols(&sim, (2192, 1168)), (true, false), "wet lake");
    assert_eq!(cols(&sim, (3000, 3000)), (false, false), "outside");
    assert_eq!(cols(&sim, (1104, 1104)), (false, false), "Auto sea floor");
    // The basement map presents neither column.
    let basement = MapConfig {
        is_basement: true,
        ..config
    };
    assert!(!sim.chunk_wet_authored(&basement, Vec2::new(2192, 1168) / CHUNK));
    assert!(
        !sim.chunk_presentation(&basement, Vec2::new(2096, 1072) / CHUNK)
            .authored_dry
    );
}

/// An unauthored column of an owned chunk beside a pit whose engine altitude
/// (the chunk table's interpolation, feathered toward the dry terrain) is below
/// sea level: `Auto` lets the sea fill it; in `AuthoredOnly` the region's only
/// water is authored, so it is dry at any altitude.
#[test]
fn unauthored_columns_below_sea_level_stay_dry_in_authored_only_regions() {
    let low = EngineColumn {
        alt: 131.0,
        water_level: 139.01,
        ..engine()
    };
    // A feathered edge: weight 0.3 keeps most of the low engine altitude.
    let low_dry = DryTerrain {
        riverless_alt: 120.0,
        riverless_alt_delta: 0.0,
        warp: 0.0,
        ..dry()
    };
    let auto = ground_col(AuthoredCell::None, SeaFill::Auto).resolve(low, low_dry);
    let own = ground_col(AuthoredCell::None, SeaFill::AuthoredOnly).resolve(low, low_dry);
    assert!(
        own.alt < 139.0,
        "the column really is below the sea: {}",
        own.alt
    );
    assert_eq!(auto.water_level, 139.01, "Auto: the sea fills it");
    assert_eq!(own.water_level, own.alt, "AuthoredOnly: dry, no fill");
    assert_eq!(auto.alt, own.alt, "only the water level differs");
    // Above the sea nothing changes: the sea's level stays, as before.
    let high = ground_col(AuthoredCell::None, SeaFill::AuthoredOnly).resolve(engine(), dry());
    let high_auto = ground_col(AuthoredCell::None, SeaFill::Auto).resolve(engine(), dry());
    assert_eq!(high, high_auto);
    assert_eq!(high.water_level, 139.01);
    // A natural (partial-chunk) column is the engine's own, in both.
    let mut natural = ground_col(AuthoredCell::None, SeaFill::AuthoredOnly);
    natural.natural = true;
    assert_eq!(natural.resolve(low, low_dry), low);
}

#[test]
fn ground_warp_weight_fades_smoothly_from_exact_ground() {
    let fade = GROUND_WARP_FADE_M as f32;
    assert_eq!(ground_warp_weight(0.0), 0.0);
    // Next to an exact cell the relief is all but gone.
    assert!(ground_warp_weight(1.0) < 0.002);
    assert_eq!(ground_warp_weight(fade / 2.0), 0.5);
    assert_eq!(ground_warp_weight(fade), 1.0);
    assert_eq!(ground_warp_weight(fade + 0.5), 1.0);
    assert_eq!(ground_warp_weight(10_000.0), 1.0);
    // Monotonic and continuous over the whole fade, in chamfer thirds (the
    // distance field's resolution) and finer.
    let mut prev = ground_warp_weight(0.0);
    for i in 1..=(GROUND_WARP_FADE_M * 30 + 30) {
        let w = ground_warp_weight(i as f32 / 30.0);
        assert!((0.0..=1.0).contains(&w));
        assert!(w >= prev, "not monotonic at {}", i as f32 / 30.0);
        // Smoothstep's steepest slope is 1.5 / fade per metre.
        assert!(w - prev <= 1.5 / fade / 30.0 + 1e-6, "jump at {i}");
        prev = w;
    }
}

#[test]
fn dry_relief_fades_towards_exact_ground_and_nowhere_else() {
    let col = |cell, weight, ground_dist| AuthoredColumn {
        settings: SETTINGS,
        weight,
        cell,
        water_dist: None,
        ground_dist,
        natural: false,
    };
    let none = AuthoredCell::None;
    // No exact ground in reach: the relief is the engine's, bit for bit.
    let far = col(none, 1.0, None).resolve(engine(), dry());
    assert_eq!(far.alt, 245.0 + 3.0 + 1.5);
    assert_eq!(far.warp_factor, 1.0);
    // At the fade distance the weight is exactly 1: no seam where the field
    // ends.
    let edge = col(none, 1.0, Some(GROUND_WARP_FADE_M as f32)).resolve(engine(), dry());
    assert_eq!(edge, far);
    // On an exact cell's edge: the spline altitude and the delta, no warp
    // (the delta does not scale with the plateau's slope, so it stays).
    let near = col(none, 1.0, Some(0.0)).resolve(engine(), dry());
    assert_eq!(near.alt, 245.0 + 3.0);
    assert_eq!(near.warp_factor, 0.0);
    // Half way: half the warp.
    let mid = col(none, 1.0, Some(GROUND_WARP_FADE_M as f32 / 2.0)).resolve(engine(), dry());
    assert_eq!(mid.alt, 245.0 + 3.0 + 0.75);
    assert_eq!(mid.warp_factor, 0.5);
    // A blend cell next to the patch keeps none of the warp's dip.
    let ring = AuthoredCell::Ground {
        block: 250,
        weight: 128,
    };
    let w = 128.0 / GROUND_EXACT as f32;
    let r = col(ring, 1.0, Some(1.0)).resolve(engine(), dry());
    let relief = 3.0 + 1.5 * ground_warp_weight(1.0);
    let expected = Lerp::lerp(245.0 + relief, top_block_alt(250), w);
    assert!((r.alt - expected).abs() < 1e-4, "{} vs {expected}", r.alt);
    assert!((r.alt - Lerp::lerp(248.0, top_block_alt(250), w)).abs() < 0.01);
    // Water fade unchanged, and the two multiply.
    let wet_near = AuthoredColumn {
        water_dist: Some(32.0),
        ..col(none, 1.0, None)
    }
    .resolve(engine(), dry());
    assert_eq!(wet_near.alt, 245.0 + 1.5 + 0.75);
    let both = AuthoredColumn {
        water_dist: Some(32.0),
        ..col(none, 1.0, Some(GROUND_WARP_FADE_M as f32 / 2.0))
    }
    .resolve(engine(), dry());
    assert_eq!(both.warp_factor, 0.25);
    // Delta halved by the water fade, warp quartered by both.
    assert_eq!(both.alt, 245.0 + 1.5 + 0.375);
    // A partial chunk's natural column is never touched.
    let natural = AuthoredColumn {
        natural: true,
        ..col(none, 1.0, Some(0.0))
    };
    assert_eq!(apply(Some(natural), engine(), dry(), |l| l), engine());
}

#[test]
fn ground_distance_is_to_the_nearest_exact_ground_cell() {
    let ar = load(&[build_region(&plateau_spec()).unwrap()]).unwrap();
    let gd = |x, y| ar.column(Vec2::new(x, y)).unwrap().ground_dist;
    // The plateau's west edge is x = 1100; 1099 is unauthored.
    assert_eq!(gd(1099, 1100), Some(1.0));
    assert_eq!(gd(1090, 1100), Some(10.0));
    // An exact cell skips the lookup (`resolve` ignores it there); the
    // field itself holds 0 on it.
    assert_eq!(gd(1100, 1100), None);
    let region = ar.region_at(Vec2::new(1100, 1100)).unwrap();
    assert_eq!(region.ground_dist(Vec2::new(1100, 1100)), Some(0.0));
    // North edge y = 1149: 1200 is 51 m away.
    assert_eq!(gd(1200, 1200), Some(51.0));
    // 1450 is 150 m east of the plateau: beyond the fade.
    assert_eq!(gd(1450, 1100), None);
    // A region without a ground layer has no ground distance anywhere, so
    // its terrain is exactly what it was.
    let river = load(&[build_region(&river_spec()).unwrap()]).unwrap();
    for (x, y) in [(1050, 1050), (1100, 1095), (1300, 1150), (1500, 1200)] {
        assert_eq!(river.column(Vec2::new(x, y)).unwrap().ground_dist, None);
    }
}

/// Every blend weight and every distance of the ring (1..=8 m from the
/// patch): the relief never leaves the envelope between the fully faded and
/// the unfaded blend, never dips as the weight or the distance grows, and
/// moves less than a decimetre per metre.
#[test]
fn ring_relief_profile_is_dip_free_for_every_weight_and_distance() {
    let ground = top_block_alt(250);
    let alt = |weight: u8, d: f32| {
        AuthoredColumn {
            settings: SETTINGS,
            weight: 1.0,
            cell: AuthoredCell::Ground { block: 250, weight },
            water_dist: None,
            ground_dist: Some(d),
            natural: false,
        }
        .resolve(engine(), dry())
        .alt
    };
    // The fixture's full relief: delta 3 plus warp 1.5 over the 245 m
    // spline; only the warp fades, so the faded floor keeps the delta.
    let (base, full) = (245.0 + 3.0, 245.0 + 4.5);
    assert!(ground > full);
    let mut prev_row: Option<Vec<f32>> = None;
    for d in 1..=8 {
        let d = d as f32;
        let relief = (full - base) * ground_warp_weight(d);
        let mut prev = f32::MIN;
        let mut row = Vec::new();
        for weight in 1..GROUND_EXACT {
            let w = weight as f32 / GROUND_EXACT as f32;
            let a = alt(weight, d);
            assert!((a - Lerp::lerp(base + relief, ground, w)).abs() < 1e-4);
            let (lo, hi) = (Lerp::lerp(base, ground, w), Lerp::lerp(full, ground, w));
            assert!(a >= lo - 1e-4 && a <= hi + 1e-4, "w {weight} d {d}: {a}");
            assert!(a >= prev, "dip with the weight at w {weight} d {d}");
            prev = a;
            row.push(a);
        }
        if let Some(p) = &prev_row {
            for (weight, (a, b)) in p.iter().zip(&row).enumerate() {
                assert!(b >= a, "dip with the distance at w {} d {d}", weight + 1);
                assert!(b - a < 0.1, "step of {} m at w {} d {d}", b - a, weight + 1);
            }
        }
        prev_row = Some(row);
    }
}

/// A ground box of 256 m tiles whose sources sit in another tile than the
/// column: the distance crosses the tile border, straight and diagonally,
/// also when the column's own tile lists no ground tile at all.
#[test]
fn ground_distance_crosses_tile_borders() {
    let spec = region("borders", (1024, 1024), (1536, 1536), 32, vec![
        PaintOp::Ground {
            shape: Shape::Rect {
                x0: 1300.0,
                y0: 1300.0,
                x1: 1340.0,
                y1: 1340.0,
            },
            ground_cm: 25_137,
            weight: GROUND_EXACT,
        },
    ]);
    let built = build_region(&spec).unwrap();
    // Only the tile (1, 1) holds ground; the 2 x 2 grid's other tiles are
    // not listed.
    assert_eq!(built.ground_tiles.len(), 1);
    let ar = load(&[built]).unwrap();
    let gd = |x, y| ar.column(Vec2::new(x, y)).unwrap().ground_dist;
    // Tile borders at x = 1280 and y = 1280.
    assert_eq!(gd(1279, 1320), Some(21.0));
    assert_eq!(gd(1320, 1279), Some(21.0));
    assert_eq!(gd(1280, 1320), Some(20.0));
    // Diagonal over the corner of four tiles: 21 chamfer diagonals (4/3 m
    // each).
    assert_eq!(gd(1279, 1279), Some(28.0));
    assert_eq!(gd(1270, 1270), Some(40.0));
    // Far side of the border, same field.
    assert_eq!(gd(1299, 1320), Some(1.0));
    // Beyond the fade distance in the other tile: nothing.
    assert_eq!(gd(1236, 1320), Some(64.0));
    assert_eq!(gd(1235, 1320), None);
}

/// Water over ground is water: the ground cells under authored water are not
/// sources of the ground distance.
#[test]
fn water_cells_over_ground_are_not_ground_sources() {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let spec = region("wet_ground", (1024, 1024), (1536, 1216), 32, vec![
        // Ground under the lake that agrees with its bed, then the lake.
        PaintOp::Ground {
            shape: rect(1100.0, 1100.0, 1200.0, 1110.0),
            ground_cm: 23_268,
            weight: GROUND_EXACT,
        },
        PaintOp::Water {
            shape: rect(1100.0, 1100.0, 1200.0, 1110.0),
            surface_cm: 23_868,
            bed_cm: 23_268,
        },
        PaintOp::BankRing {
            width_m: 1,
            bed_cm: Some(23_918),
        },
        // A real patch of exact ground, 50 m east of the lake.
        PaintOp::Ground {
            shape: rect(1250.0, 1100.0, 1260.0, 1110.0),
            ground_cm: 24_050,
            weight: GROUND_EXACT,
        },
    ]);
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let region = ar.region_at(Vec2::new(1150, 1105)).unwrap();
    // The lake's own cells hold ground in the ground layer ...
    let g = region.ground[0]
        .as_ref()
        .and_then(|g| g.get(cell_index(1150 - 1024, 1105 - 1024)));
    assert_eq!(g, Some((232, GROUND_EXACT)));
    // ... but are water, and are not sources: the lake's east edge (x 1199)
    // is 51 m from the patch, not 0, and the unauthored column beyond its
    // bank is 49 m from the patch, not 2.
    assert!(ar.cell_at(Vec2::new(1199, 1105)).unwrap().is_wet());
    assert_eq!(region.ground_dist(Vec2::new(1199, 1105)), Some(51.0));
    assert_eq!(region.ground_dist(Vec2::new(1150, 1105)), None);
    assert_eq!(
        ar.column(Vec2::new(1201, 1105)).unwrap().cell,
        AuthoredCell::None
    );
    assert_eq!(
        ar.column(Vec2::new(1201, 1105)).unwrap().ground_dist,
        Some(49.0)
    );
}

/// Exact ground in a partial chunk is not a source: next to the chunk
/// border the owned unauthored column keeps the engine's full relief, like
/// its natural neighbour, instead of a step the size of the relief.
#[test]
fn ground_in_a_partial_chunk_does_not_fade_the_owned_columns_beside_it() {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let ground = |x0, x1| PaintOp::Ground {
        shape: rect(x0, 1100.0, x1, 1110.0),
        ground_cm: 25_137,
        weight: GROUND_EXACT,
    };
    // Chunk (34, 34) is wpos 1088..1120: partial, holds a patch ending 9 m
    // before its east border. A second patch in the owned chunk (36, 34)
    // (wpos 1152..1184) keeps the fade.
    let mut spec = region("partial", (1024, 1024), (1536, 1216), 32, vec![
        ground(1100.0, 1111.0),
        ground(1160.0, 1170.0),
    ]);
    spec.allow_partial_chunks = vec![(34, 34)];
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let col = |x| ar.column(Vec2::new(x, 1105)).unwrap();
    // The border: x = 1119 is the partial chunk's natural column, x = 1120
    // the first owned one. Both are unauthored and 9 / 10 m from the patch.
    assert!(col(1119).natural && !col(1120).natural);
    assert_eq!(col(1119).cell, AuthoredCell::None);
    assert_eq!(col(1120).cell, AuthoredCell::None);
    assert_eq!(col(1119).ground_dist, None);
    assert_eq!(col(1120).ground_dist, Some(40.0));
    let owned = col(1120).resolve(engine(), dry());
    // (the nearest source for 1120 is the owned chunk's patch, 40 m away; the
    // partial chunk's patch, 10 m away, is ignored)
    assert!(owned.warp_factor > 0.5 && owned.warp_factor < 1.0);
    // Without the owned chunk's patch nothing fades next to the border: the
    // owned column is the engine's relief bit for bit, as without ground.
    let mut spec = region("partial_only", (1024, 1024), (1536, 1216), 32, vec![
        ground(1100.0, 1111.0),
    ]);
    spec.allow_partial_chunks = vec![(34, 34)];
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let col = |x| ar.column(Vec2::new(x, 1105)).unwrap();
    assert_eq!(col(1120).ground_dist, None);
    assert_eq!(col(1119).ground_dist, None);
    let owned = col(1120).resolve(engine(), dry());
    let far = AuthoredColumn {
        settings: SETTINGS,
        weight: 1.0,
        cell: AuthoredCell::None,
        water_dist: None,
        ground_dist: None,
        natural: false,
    }
    .resolve(engine(), dry());
    assert_eq!(owned.alt, far.alt);
    assert_eq!(owned.warp_factor, far.warp_factor);
    // The patch itself is still exact in its partial chunk.
    assert_eq!(col(1105).cell, AuthoredCell::Ground {
        block: 251,
        weight: GROUND_EXACT
    });
}

/// A straightforward reference for the distance-to-water field: one
/// chamfer 3-4 pass pair over the whole region, no tiles, no apron.
fn reference_water_field(region: &Region) -> Vec<Option<f32>> {
    let size = region.max - region.min;
    let (w, h) = (size.x, size.y);
    let at = |x: i32, y: i32| (y * w + x) as usize;
    let mut d = vec![u16::MAX; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            if region.cell(region.min + Vec2::new(x, y)).is_wet() {
                d[at(x, y)] = 0;
            }
        }
    }
    let relax = |d: &mut Vec<u16>, x: i32, y: i32, nx: i32, ny: i32, k: u16| {
        if nx >= 0 && ny >= 0 && nx < w && ny < h {
            let v = d[at(nx, ny)].saturating_add(k);
            if v < d[at(x, y)] {
                d[at(x, y)] = v;
            }
        }
    };
    for y in 0..h {
        for x in 0..w {
            relax(&mut d, x, y, x - 1, y, 3);
            relax(&mut d, x, y, x, y - 1, 3);
            relax(&mut d, x, y, x - 1, y - 1, 4);
            relax(&mut d, x, y, x + 1, y - 1, 4);
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            relax(&mut d, x, y, x + 1, y, 3);
            relax(&mut d, x, y, x, y + 1, 3);
            relax(&mut d, x, y, x + 1, y + 1, 4);
            relax(&mut d, x, y, x - 1, y + 1, 4);
        }
    }
    d.iter()
        .map(|&v| (v <= (DIST_CAP_M * 3) as u16).then(|| v as f32 / 3.0))
        .collect()
}

/// The distance-to-water field, built through the generalised field builder,
/// is the plain chamfer distance everywhere in the region, tile borders
/// included (rivers and a pond that straddle them, ground tiles present).
#[test]
fn water_distance_field_is_the_plain_chamfer_distance() {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect { x0, y0, x1, y1 };
    let mut spec = river_spec();
    spec.ops.insert(0, PaintOp::Water {
        shape: rect(1270.0, 1150.0, 1292.0, 1170.0),
        surface_cm: 23_868,
        bed_cm: 23_268,
    });
    // Ground in the same region must not leak into the water field.
    spec.ops.push(PaintOp::Ground {
        shape: rect(1400.0, 1150.0, 1450.0, 1180.0),
        ground_cm: 25_137,
        weight: GROUND_EXACT,
    });
    let ar = load(&[build_region(&spec).unwrap()]).unwrap();
    let region = ar.region_at(Vec2::new(1100, 1100)).unwrap();
    let reference = reference_water_field(region);
    let size = region.max - region.min;
    let mut some = 0;
    for y in 0..size.y {
        for x in 0..size.x {
            let wpos = region.min + Vec2::new(x, y);
            let got = region.water_dist(wpos);
            assert_eq!(got, reference[(y * size.x + x) as usize], "{wpos:?}");
            some += got.is_some() as usize;
        }
    }
    assert!(some > 10_000);
}
