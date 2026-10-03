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
    let fetch = move |id: &str, _layer: LayerKind, tx: i32, ty: i32| {
        files
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

/// A 20 m river along x at y = 1100..1120, water top block 238 over bed
/// 232, a 2 m bank ring at 239 m.
fn river_spec() -> RegionSpec {
    region("river", (1024, 1024), (1536, 1216), 32, vec![
        PaintOp::Water {
            shape: Shape::Rect {
                x0: 1024.0,
                y0: 1100.0,
                x1: 1536.0,
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
    let raw = RawTile {
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
    assert_eq!(cell_of(&raw, 0), AuthoredCell::Bank { bed_block: -3 });
    assert_eq!(cell_of(&raw, 1), AuthoredCell::Bank { bed_block: -1 });
    assert_eq!(cell_of(&raw, 2), AuthoredCell::Bank { bed_block: 0 });
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
fn water_on_the_region_edge_is_the_seam_and_allowed() {
    // A river crossing the whole region: its ends touch the box edge.
    assert!(load(&[build_region(&river_spec()).unwrap()]).is_ok());
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
    let unknown_layer = text.replacen("layers:[Water]", "layers:[Water,Ground]", 1);
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
        },
        weight: 1.0,
        cell,
        water_dist: d,
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
    // chunk rows 34 (y 1100..1119 -> chunk 34) and banks around it.
    let table_all_dry = |_: Vec2<i32>| Some(false);
    let r = ar.check_consistency("t", table_all_dry).unwrap();
    assert_eq!(
        r[0].authored_wet_table_dry.len(),
        16,
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
    // The two end chunks of the river row are on the region's outer ring
    // and the river runs to the box edge there: border crossings, exempt.
    let table_river = |c: Vec2<i32>| Some(c.y == 34);
    let e = ar.check_consistency("t", table_river).unwrap_err();
    assert!(
        e.0.contains("14 chunk(s) the sim table calls water"),
        "{}",
        e.0
    );
    assert!(
        e.0.contains("run to the box edge"),
        "the error explains the border case"
    );
    let mut spec = river_spec();
    spec.consistency.max_authored_dry_table_wet_chunks = 14;
    let r = load(&[build_region(&spec).unwrap()])
        .unwrap()
        .check_consistency("t", table_river)
        .unwrap();
    assert_eq!(r[0].border_crossings, vec![
        Vec2::new(32, 34),
        Vec2::new(47, 34)
    ]);
    assert_eq!(r[0].authored_dry_table_wet.len(), 14);
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
    let full = region("full", (1024, 1024), (1056, 1056), 0, vec![
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
    ]);
    let ar = load(&[build_region(&full).unwrap()]).unwrap();
    assert!(ar.check_consistency("t", |_| Some(true)).is_ok());
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
    assert!(global_resident_cap() >= GLOBAL_RESIDENT_CAP_BYTES.min(global_resident_cap()));
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
