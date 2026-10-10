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
            natural: false,
        }
        .resolve(engine(), dry());
        assert_eq!(bank.alt, 239.5);
        assert_eq!(bank.water_level, 139.01);
        assert_eq!(bank.riverless_alt, 239.5);
    }
}

#[test]
fn resolve_none_blends_engine_into_dry_terrain() {
    let col = |weight, water_dist| AuthoredColumn {
        settings: SETTINGS,
        weight,
        cell: AuthoredCell::None,
        water_dist,
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
        natural: false,
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
    // 2 x 1 grid; ground tiles only, no distance fields.
    let n = build_region(&plateau_spec()).unwrap().ground_tiles.len();
    assert_eq!(ground_only.resident_bytes, n * (192 << 10));
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
    // Banks keep the Stage-1 behaviour (no levelling).
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
    let text = ron::ser::to_string(&build_region(&spec("a", 1024, 24_000)).unwrap().manifest)
        .unwrap();
    for field in ["sites_on_patch", "site_levelling", "max_exposed_void_columns"] {
        assert!(!text.contains(field), "{field} serialised at its default: {text}");
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
    let s = region("lift", (1024, 1024), (1536, 1280), 32, vec![PaintOp::Ground {
        shape: Shape::Rect {
            x0: 1100.0,
            y0: 1100.0,
            x1: 1200.0,
            y1: 1200.0,
        },
        ground_cm: 25_050,
        weight: GROUND_EXACT,
    }]);
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
}

/// Everything under `world/src/layer/` reads the chunk table through `Land`
/// (`*_table` accessors): a routed (authored-aware) altitude or gradient
/// call there would move spatially extended features (the cave graph,
/// caverns, authored voids) outside the regions. A new call must use a table
/// accessor or be added to the allow-list below with its reason.
#[test]
fn layers_read_the_chunk_table() {
    // (file, trimmed line): `WorldSim` receivers, which are the table.
    const ALLOWED: &[(&str, &str)] = &[
        // `world` is the `WorldSim`: `get_gradient_approx` is the table.
        ("spot.rs", ".get_gradient_approx(pos)"),
    ];
    const ROUTED: &[&str] = &[
        ".get_alt_approx(",
        ".get_surface_alt_approx(",
        ".get_gradient_approx(",
        ".surface_alt_at(",
        ".ground_alt_at(",
        ".ground_gradient_at(",
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
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/layer"),
        &mut files,
    );
    let mut bad = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        for (i, line) in std::fs::read_to_string(f).unwrap().lines().enumerate() {
            let t = line.trim();
            if ROUTED.iter().any(|r| t.contains(r))
                && !ALLOWED.iter().any(|(af, al)| *af == name && t == *al)
            {
                bad.push(format!("{name}:{}: {t}", i + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "authored-aware ground reads under world/src/layer (use the *_table accessors): {bad:#?}"
    );
}

/// B-K1 / §9.6: an authored void under a patch that comes within the
/// tolerance of the ground is exposed (a start-up error at the default
/// tolerance 0, allowed when the region raises it); a declared mouth is
/// exempt; a declared wet mouth must not hold more water than it declares.
#[test]
fn voids_exposed_by_a_patch_are_counted_and_mouths_are_exempt() {
    use super::post_civ::{VoidMouth, void_exposure};
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
    world.sim.set_authored_rasters_for_test(Some(load(&[build_region(&spec(0)).unwrap()]).unwrap()));
    // Ceiling 10 blocks under the ground: covered.
    let r = void_exposure(&world, Some(&voids(240)), &[]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].exposed, 0);
    assert!(!r[0].is_error());
    // Ceiling 2 blocks under the ground: exposed on the disc's columns.
    let r = void_exposure(&world, Some(&voids(248)), &[]);
    let exposed = r[0].exposed;
    assert!(exposed > 0 && r[0].is_error(), "{r:?}");
    assert_eq!(r[0].features[0].0, "test_cave");
    // A declared mouth is exempt.
    let mouth = VoidMouth {
        column: r[0].features[0].2,
        max_water_depth_blocks: None,
    };
    let r = void_exposure(&world, Some(&voids(248)), &[mouth]);
    assert_eq!(r[0].exposed, exposed - 1);
    // A wet mouth on the water: 6 blocks of water against 2 declared.
    let wet = VoidMouth {
        column: Vec2::new(1205, 1150),
        max_water_depth_blocks: Some(2),
    };
    let r = void_exposure(&world, Some(&voids(240)), &[wet]);
    assert_eq!(r[0].flooded_mouths, vec![(Vec2::new(1205, 1150), 6)]);
    assert!(r[0].is_error());
    let r = void_exposure(&world, Some(&voids(240)), &[VoidMouth {
        max_water_depth_blocks: Some(6),
        ..wet
    }]);
    assert!(!r[0].is_error());
    // The region allows the opening.
    world.sim.set_authored_rasters_for_test(Some(
        load(&[build_region(&spec(exposed as u32)).unwrap()]).unwrap(),
    ));
    let r = void_exposure(&world, Some(&voids(248)), &[]);
    assert!(!r[0].is_error(), "{r:?}");
}
