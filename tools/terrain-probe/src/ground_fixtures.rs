//! Golden accept/refuse fixtures of the authored ground layer (Stage 2), the
//! contract shared with the open-world exporter: each directory under
//! `tests/fixtures/authored_ground/` holds a manifest, its tiles and
//! `expect.json` (the loader's verdict, a substring of its message, and the
//! sha256 of every tile's *decompressed* payload, which pins the plane
//! encoding independently of the zstd build). The exporter's tests read the
//! same bytes, so its mirror of the loader cannot drift.
//!
//! Regenerate after a deliberate format or rule change:
//! `TPROBE_BLESS_FIXTURES=1 cargo test -p xindeler-terrain-probe
//! ground_fixtures`.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use world::authored_raster::{
    SeaFill,
    format::{GROUND_EXACT, LayerKind, sha256_hex},
    writer::{self, BuiltRegion, PaintOp, RegionSpec, Shape},
};

const STEM: &str = "cromatolis_v0";
const MIN: (i32, i32) = (2048, 2048);
const MAX: (i32, i32) = (2560, 2560);
const PLATEAU: i32 = 24_000;

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape { Shape::Rect { x0, y0, x1, y1 } }

fn ground(shape: Shape, ground_cm: i32) -> PaintOp {
    PaintOp::Ground {
        shape,
        ground_cm,
        weight: GROUND_EXACT,
    }
}

fn region(ops: Vec<PaintOp>) -> RegionSpec { RegionSpec::new("fixture", MIN, MAX, 32, ops) }

/// A plateau with a channel of water held by exact ground (a quay).
fn quay(ground_cm: i32) -> Vec<PaintOp> {
    vec![
        ground(rect(2100.0, 2100.0, 2500.0, 2500.0), ground_cm),
        PaintOp::ClearGround {
            shape: rect(2200.0, 2290.0, 2400.0, 2310.0),
        },
        PaintOp::Water {
            shape: rect(2200.0, 2290.0, 2400.0, 2310.0),
            surface_cm: 23_850,
            bed_cm: 23_250,
        },
    ]
}

struct Fixture {
    name: &'static str,
    /// `None` = accepted; `Some(text)` = refused with a message containing it.
    refuse: Option<&'static str>,
    build: fn() -> Vec<BuiltRegion>,
}

fn built(spec: RegionSpec) -> Vec<BuiltRegion> { vec![writer::build_region(&spec).unwrap()] }

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "accept_plateau_trench_ramp",
            refuse: None,
            build: || {
                built(region(vec![
                    ground(rect(2100.0, 2100.0, 2500.0, 2500.0), PLATEAU),
                    ground(rect(2200.0, 2100.0, 2201.0, 2500.0), PLATEAU - 600),
                    PaintOp::GroundPlane {
                        shape: rect(2300.0, 2100.0, 2400.0, 2500.0),
                        origin: (2300.0, 2100.0),
                        origin_cm: PLATEAU,
                        cm_per_m: (25.0, 3.0),
                    },
                ]))
            },
        },
        Fixture {
            name: "accept_water_and_ground_quay",
            refuse: None,
            build: || built(region(quay(24_050))),
        },
        Fixture {
            name: "accept_authored_only_pit_and_lake",
            refuse: None,
            build: || {
                let mut s = region(vec![
                    ground(rect(2100.0, 2100.0, 2500.0, 2500.0), PLATEAU),
                    ground(rect(2150.0, 2150.0, 2250.0, 2250.0), 13_050),
                    PaintOp::ClearGround {
                        shape: rect(2300.0, 2300.0, 2400.0, 2400.0),
                    },
                    PaintOp::Water {
                        shape: rect(2300.0, 2300.0, 2400.0, 2400.0),
                        surface_cm: 12_050,
                        bed_cm: 11_050,
                    },
                ]);
                s.sea_fill = SeaFill::AuthoredOnly;
                built(s)
            },
        },
        Fixture {
            name: "accept_block_range_extremes",
            refuse: None,
            build: || {
                let mut s = region(vec![
                    ground(rect(2100.0, 2100.0, 2110.0, 2110.0), -409_600),
                    ground(rect(2200.0, 2200.0, 2210.0, 2210.0), 819_199),
                ]);
                s.sea_fill = SeaFill::AuthoredOnly;
                built(s)
            },
        },
        Fixture {
            name: "refuse_blend_weight",
            refuse: Some("blend weights"),
            build: || {
                built(region(vec![PaintOp::Ground {
                    shape: rect(2100.0, 2100.0, 2110.0, 2110.0),
                    ground_cm: PLATEAU,
                    weight: 128,
                }]))
            },
        },
        Fixture {
            name: "refuse_block_out_of_range",
            refuse: Some("outside the blocks"),
            build: || {
                built(region(vec![ground(
                    rect(2100.0, 2100.0, 2110.0, 2110.0),
                    819_200,
                )]))
            },
        },
        Fixture {
            name: "refuse_ground_under_water_disagrees",
            refuse: Some("under authored water"),
            build: || {
                built(region(vec![
                    ground(rect(2100.0, 2100.0, 2500.0, 2500.0), 24_050),
                    PaintOp::Water {
                        shape: rect(2200.0, 2290.0, 2400.0, 2310.0),
                        surface_cm: 23_850,
                        bed_cm: 23_250,
                    },
                ]))
            },
        },
        Fixture {
            name: "refuse_ground_under_bank_disagrees",
            refuse: Some("under an authored bank"),
            build: || {
                built(region(vec![
                    ground(rect(2100.0, 2100.0, 2200.0, 2200.0), 24_000),
                    PaintOp::Bank {
                        shape: rect(2100.0, 2100.0, 2150.0, 2200.0),
                        bed_cm: 24_100,
                    },
                ]))
            },
        },
        Fixture {
            name: "refuse_water_wall_against_lower_ground",
            refuse: Some("would stand as a wall"),
            build: || built(region(quay(23_750))),
        },
        Fixture {
            name: "refuse_auto_lake_below_sea_level",
            refuse: Some("a lake below sea level needs sea_fill: AuthoredOnly"),
            build: || {
                built(region(vec![
                    ground(rect(2100.0, 2100.0, 2500.0, 2500.0), PLATEAU),
                    PaintOp::ClearGround {
                        shape: rect(2300.0, 2300.0, 2400.0, 2400.0),
                    },
                    PaintOp::Water {
                        shape: rect(2300.0, 2300.0, 2400.0, 2400.0),
                        surface_cm: 12_050,
                        bed_cm: 11_050,
                    },
                ]))
            },
        },
        Fixture {
            name: "refuse_auto_bank_below_sea_level",
            refuse: Some("dry ground below sea level needs sea_fill: AuthoredOnly"),
            build: || {
                built(region(vec![PaintOp::Bank {
                    shape: rect(2100.0, 2100.0, 2110.0, 2110.0),
                    bed_cm: 13_000,
                }]))
            },
        },
        Fixture {
            name: "refuse_ground_within_the_box_margin",
            refuse: Some("at least 16 m inside"),
            build: || {
                built(region(vec![ground(
                    rect(2050.0, 2100.0, 2060.0, 2110.0),
                    PLATEAU,
                )]))
            },
        },
        Fixture {
            name: "refuse_tile_of_an_undeclared_layer",
            refuse: Some("a layer the region does not declare"),
            build: || {
                let mut b = built(region(vec![ground(
                    rect(2100.0, 2100.0, 2110.0, 2110.0),
                    PLATEAU,
                )]));
                b[0].manifest.layers = vec![LayerKind::Water];
                b
            },
        },
        Fixture {
            name: "refuse_layers_out_of_order",
            refuse: Some("[Water], [Ground] or [Water, Ground]"),
            build: || {
                let mut b = built(region(quay(24_050)));
                b[0].manifest.layers = vec![LayerKind::Ground, LayerKind::Water];
                b
            },
        },
        Fixture {
            name: "refuse_ground_tile_listed_twice",
            refuse: Some("listed twice"),
            build: || {
                let mut b = built(region(vec![ground(
                    rect(2100.0, 2100.0, 2110.0, 2110.0),
                    PLATEAU,
                )]));
                let dup = b[0].manifest.tiles[0].clone();
                b[0].manifest.tiles.push(dup);
                b
            },
        },
        Fixture {
            name: "refuse_non_canonical_ground_tile",
            refuse: Some("weight is 0"),
            build: || {
                let mut b = built(region(vec![ground(
                    rect(2100.0, 2100.0, 2110.0, 2110.0),
                    PLATEAU,
                )]));
                // Cell (0, 0) of tile (0, 0) is not authored (weight 0) but
                // stores a non-zero difference: rewrite the payload by hand.
                let tile = &mut b[0].ground_tiles[0].1;
                let mut raw = payload(tile);
                raw[0] = 7;
                let comp = zstd::bulk::compress(&raw, 19).unwrap();
                tile.truncate(24);
                tile.extend_from_slice(&(comp.len() as u32).to_le_bytes());
                tile.extend_from_slice(&comp);
                b[0].manifest.tiles[0].sha256 = sha256_hex(tile);
                b
            },
        },
    ]
}

/// The decompressed payload of a tile (header skipped).
fn payload(tile: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    zstd::stream::read::Decoder::new(&tile[28..])
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    raw
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authored_ground")
}

/// The files of a fixture, name -> bytes, plus its `expect.json`.
fn render(f: &Fixture) -> BTreeMap<String, Vec<u8>> {
    let regions = (f.build)();
    let mut files = BTreeMap::new();
    let manifest = ron::ser::to_string_pretty(
        &writer::manifest(&regions),
        ron::ser::PrettyConfig::default(),
    )
    .unwrap();
    files.insert(
        format!("{STEM}_authored_rasters.ron"),
        manifest.into_bytes(),
    );
    let mut payloads = BTreeMap::new();
    for r in &regions {
        for (layer, list) in [
            (LayerKind::Water, &r.tiles),
            (LayerKind::Ground, &r.ground_tiles),
        ] {
            for ((tx, ty), bytes) in list {
                let name = format!(
                    "{STEM}_ar_{}_{}_{tx}_{ty}.bin",
                    r.manifest.id,
                    layer.asset_name()
                );
                payloads.insert(name.clone(), sha256_hex(&payload(bytes)));
                files.insert(name, bytes.clone());
            }
        }
    }
    let expect = serde_json::json!({
        "verdict": if f.refuse.is_some() { "refuse" } else { "accept" },
        "message": f.refuse,
        "payload_sha256": payloads,
    });
    files.insert(
        "expect.json".into(),
        (serde_json::to_string_pretty(&expect).unwrap() + "\n").into_bytes(),
    );
    files
}

#[test]
fn ground_fixtures_are_current_and_the_loader_agrees() {
    let bless = std::env::var_os("TPROBE_BLESS_FIXTURES").is_some();
    let root = fixture_root();
    let mut names = Vec::new();
    for f in fixtures() {
        let dir = root.join(f.name);
        let files = render(&f);
        if bless {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, bytes) in &files {
                std::fs::write(dir.join(name), bytes).unwrap();
            }
        }
        // The committed bytes are exactly what the engine's writer produces
        // (a zstd upgrade that re-encodes differently shows up here: re-bless).
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e} (bless the fixtures)", dir.display()))
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        assert_eq!(
            on_disk,
            files.keys().cloned().collect::<Vec<_>>(),
            "{}",
            f.name
        );
        for (name, bytes) in &files {
            assert_eq!(
                &std::fs::read(dir.join(name)).unwrap(),
                bytes,
                "{}/{name} differs from the writer's output (re-bless?)",
                f.name
            );
        }
        // And the loader's verdict is the expected one.
        let verdict = crate::load_manifest_dir(&dir, STEM, 32768).unwrap();
        match (f.refuse, verdict) {
            (None, Ok(_)) => {},
            (None, Err(e)) => panic!("{}: refused: {e}", f.name),
            (Some(want), Ok(_)) => panic!("{}: accepted, expected {want:?}", f.name),
            (Some(want), Err(e)) => assert!(e.contains(want), "{}: {want:?} not in {e}", f.name),
        }
        names.push(f.name);
    }
    // No stale fixture directory.
    let mut dirs: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    names.sort();
    assert_eq!(dirs, names);
}
