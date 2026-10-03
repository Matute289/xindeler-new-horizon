//! Edge-case battery for the engine side of the probe. Ids `EC-*` refer to the
//! precision-tooling corner-case catalogue.
//!
//! * Pure tests (classification, box parsing, authored-point extraction from
//!   the committed RON text files) run in the normal `cargo test`.
//! * Tests that generate the real Cromatolis world are `#[ignore = "heavy:
//!   .."]`: `VELOREN_ASSETS=<engine>/assets cargo test -p
//!   xindeler-terrain-probe -- --ignored` (needs the LFS map + world assets; ~3
//!   s of world generation per test). They only read synthetic/empty areas: the
//!   research arena around (23264, 25312), map corners, and a wilderness box
//!   that the test itself proves empty of sites and authored points.
//! * A test that exposes a bug carries its catalogue id (`BUG-P*`) in its doc
//!   comment; all of them pass since the fixes (a still-open bug would be
//!   `#[ignore = "BUG-.."]`).

use std::{path::PathBuf, sync::OnceLock};

use common::{
    terrain::{Block, BlockKind, SpriteKind},
    vol::WriteVol,
};
use vek::{Rgb, Vec2, Vec3};

use super::*;
use crate::format::{NO_Z, class, flag};

fn repo_assets() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets") }

// ------------------------------------------------------------- pure

/// EC-D30 (BUG-P9, fixed): lava is recorded as liquid (README: "2 liquid
/// (water and lava)"). `BlockKind::Lava` is neither fluid nor solid in the
/// engine, so `classify` matches it explicitly; without that, lava channels
/// and lakes would read as holes.
#[test]
fn ec_d30_lava_is_liquid() {
    let lava = Block::new(BlockKind::Lava, Rgb::new(255, 80, 0));
    assert_eq!(classify(&lava, false), class::LIQUID);
    assert_eq!(classify(&lava, true), class::LIQUID);
    assert!(is_lava(&lava));
    assert!(!is_lava(&Block::water(SpriteKind::Empty)));
}

/// EC-D30b: a column holding lava carries `flag::LAVA` (and `flag::LIQUID`),
/// a water column carries only `LIQUID`, so verifiers can tell them apart
/// without a layout change; a NaN-altitude column carries `RIM_NO_SAMPLE`.
#[test]
fn ec_d30b_lava_and_rim_flags() {
    use common::terrain::TerrainChunkMeta;
    let mut ch = TerrainChunk::new(
        10,
        Block::new(BlockKind::Rock, Rgb::zero()),
        Block::empty(),
        TerrainChunkMeta::void(),
    );
    let lava = Block::new(BlockKind::Lava, Rgb::new(255, 80, 0));
    for z in 11..14 {
        ch.set(Vec3::new(1, 1, z), lava).unwrap();
        ch.set(Vec3::new(2, 1, z), Block::water(SpriteKind::Empty))
            .unwrap();
    }
    let flags_at = |x: i32, cap: Option<i32>| {
        let (runs, extra) = column_runs_ex(&ch, Vec2::new(x, 1), 0, 30, false, cap);
        format::summarize(0, &runs).flags | extra
    };
    let l = flags_at(1, Some(10));
    assert_eq!(l & (flag::LAVA | flag::LIQUID), flag::LAVA | flag::LIQUID);
    let w = flags_at(2, Some(10));
    assert_eq!(w & (flag::LAVA | flag::LIQUID), flag::LIQUID);
    assert_eq!(
        flags_at(3, Some(10)) & (flag::LAVA | flag::RIM_NO_SAMPLE),
        0
    );
    assert_ne!(flags_at(3, None) & flag::RIM_NO_SAMPLE, 0);
}

/// EC-D31 (BUG-P10, fixed): every natural rock/soil kind is GROUND, including
/// `GlowingWeakRock` (cave rock), which used to become STRUCTURE. With a
/// surface cap the same natural-vs-structure rule applies: above the cap it
/// is STRUCTURE like any other natural kind.
#[test]
fn ec_d31_all_natural_rock_is_ground() {
    let b = Block::new(BlockKind::GlowingWeakRock, Rgb::new(10, 10, 10));
    assert_eq!(classify(&b, false), class::GROUND);
    assert_eq!(classify_at(&b, 5, Some(10), false), class::GROUND);
    assert_eq!(classify_at(&b, 11, Some(10), false), class::STRUCTURE);
}

/// EC-D32: classification table for the cases the verifiers depend on.
#[test]
fn ec_d32_classification_table() {
    let c = Rgb::new(1, 2, 3);
    let ground = [
        BlockKind::Rock,
        BlockKind::WeakRock,
        BlockKind::GlowingRock,
        BlockKind::Grass,
        BlockKind::Snow,
        BlockKind::Earth,
        BlockKind::Sand,
        BlockKind::Ice,
    ];
    for k in ground {
        assert_eq!(classify(&Block::new(k, c), false), class::GROUND, "{k:?}");
    }
    // Trees and site blocks are STRUCTURE (verifiers must not call tree
    // canopies "floating terrain").
    for k in [
        BlockKind::Wood,
        BlockKind::Leaves,
        BlockKind::ArtLeaves,
        BlockKind::Misc,
        BlockKind::ArtSnow,
    ] {
        assert_eq!(
            classify(&Block::new(k, c), false),
            class::STRUCTURE,
            "{k:?}"
        );
    }
    assert_eq!(classify(&Block::empty(), false), class::AIR);
    // Water carrying a sprite (seagrass) is still liquid.
    let wet = Block::water(SpriteKind::Seagrass);
    assert_eq!(classify(&wet, false), class::LIQUID);
    assert_eq!(classify(&wet, true), class::LIQUID);
    // An air block with a sprite: AIR by default, SPRITE with --keep-sprites.
    let tuft = Block::air(SpriteKind::LongGrass);
    assert_eq!(classify(&tuft, false), class::AIR);
    assert_eq!(classify(&tuft, true), class::SPRITE);
}

/// EC-A10: box parsing edge cases.
#[test]
fn ec_a10_box_parse_edges() {
    assert!(Box2::parse("0,0,1,1").is_ok(), "1x1 box");
    assert!(Box2::parse(" 1 , 2 , 3 , 4 ").is_ok(), "spaces");
    assert!(Box2::parse("0,0,0,5").is_err(), "zero width");
    assert!(Box2::parse("5,0,4,5").is_err(), "inverted");
    assert!(Box2::parse("0,0,1.5,2").is_err(), "fractional");
    assert!(Box2::parse("0,0,1,2,3").is_err(), "5 values");
    assert!(Box2::parse("").is_err());
    assert!(Box2::parse("0,0,99999999999,1").is_err(), "i32 overflow");
    // Negative boxes parse (the dump command then refuses them).
    let b = Box2::parse("-64,-64,-1,-1").unwrap();
    let (c0, c1) = b.chunk_range();
    assert_eq!((c0, c1), (Vec2::new(-2, -2), Vec2::new(-1, -1)));
    // Chunk boundaries: a box ending exactly on a chunk edge does not touch
    // the next chunk; one block more does.
    assert_eq!(
        Box2::parse("0,0,32,32").unwrap().chunk_range().1,
        Vec2::new(0, 0)
    );
    assert_eq!(
        Box2::parse("0,0,33,32").unwrap().chunk_range().1,
        Vec2::new(1, 0)
    );
    assert_eq!(
        Box2::parse("31,31,33,33").unwrap().chunk_range(),
        (Vec2::new(0, 0), Vec2::new(1, 1))
    );
}

/// EC-A11 (BUG-P6, fixed): `near_box` uses i64, so boxes and margins near the
/// i32 limits neither panic (debug) nor wrap and mis-filter (release).
#[test]
fn ec_a11_near_box_extreme_inputs() {
    let b = Box2 {
        x0: i32::MIN + 5,
        y0: 0,
        x1: i32::MIN + 10,
        y1: 10,
    };
    assert!(!near_box(0, 0, b, 600), "near_box on an extreme box");
    assert!(near_box(i32::MIN + 8, 5, b, 600));
    let top = Box2 {
        x0: i32::MAX - 10,
        y0: i32::MAX - 10,
        x1: i32::MAX,
        y1: i32::MAX,
    };
    assert!(near_box(i32::MAX, i32::MAX, top, i32::MAX));
    assert!(!near_box(0, 0, top, 600));
}

/// EC-D33 (BUG-P7, fixed): the committed fortifications file (walls and gates
/// in 2048x1536 source pixels) reaches the emptiness guard, converted through
/// its own `source_map` like the engine does, so a box crossed by an authored
/// wall is no longer called empty.
#[test]
fn ec_d33_fortifications_are_seen_by_the_emptiness_guard() {
    let mut all = Vec::new();
    all_authored_points(&repo_assets(), Vec2::broadcast(32768), &mut all);
    assert!(
        !all.is_empty(),
        "the committed RON files have normalised points"
    );
    assert!(
        all.iter()
            .any(|r| r.source == "cromatolis_v0_fortifications.ron"),
        "no fortification point/segment reached the guard"
    );
    // The Northwall (start (956, 8), end (1141, 8) in source pixels):
    // x = px / 2047 * 32768, y = (1 - py / 1535) * 32768.
    let (x, y) = (956.0 / 2047.0 * 32768.0, (1.0 - 8.0 / 1535.0) * 32768.0);
    let b = Box2 {
        x0: x as i32 + 10,
        y0: y as i32 - 10,
        x1: x as i32 + 30,
        y1: y as i32 + 10,
    };
    let near = authored_near(&repo_assets(), Vec2::broadcast(32768), b, 50).unwrap();
    assert!(
        near.iter()
            .any(|r| r.id.as_deref() == Some("site.northwall_stone")),
        "{near:?}"
    );
}

/// EC-D34 (BUG-P7, fixed): a linear authored feature (route/wall/bridge) whose
/// vertices are far from the box but whose segment crosses it is reported.
#[test]
fn ec_d34_segment_crossing_the_box_is_reported() {
    let dir = std::env::temp_dir().join(format!("tprobe-edge-assets-{}", std::process::id()));
    let map = dir.join("world/map");
    std::fs::create_dir_all(&map).unwrap();
    // Vertices at x=0.6 and x=0.8 of the map (19.6 km and 26.2 km), same y:
    // the straight segment crosses the box below at x 23000..23100.
    let y = 1.0 - 25300.0 / 32768.0;
    std::fs::write(
        map.join("cromatolis_v0_routes.ron"),
        format!(
            "(coordinate_space: \"normalized_map_xy_top_left_origin\", routes: [(id: \
             \"route.synthetic\", points: [(x: 0.6, y: {y}), (x: 0.8, y: {y})])])"
        ),
    )
    .unwrap();
    let b = Box2 {
        x0: 23000,
        y0: 25250,
        x1: 23100,
        y1: 25350,
    };
    let found = authored_points(&dir, Vec2::broadcast(32768), b, 50);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!found.is_empty(), "segment through the box not reported");
}

/// EC-D34b (BUG-P7, the measured case): wilderness chunk (653, 998) is NOT
/// empty at a 600 m margin, because a committed route polyline passes about
/// 312 m from it although its vertices are kilometres away. The committed RON
/// files are read directly (no world generation).
#[test]
fn ec_d34b_measured_chunk_is_not_empty() {
    let b = Box2 {
        x0: 653 * 32,
        y0: 998 * 32,
        x1: 654 * 32,
        y1: 999 * 32,
    };
    let size = Vec2::broadcast(32768);
    let near = authored_near(&repo_assets(), size, b, 600).unwrap();
    assert!(
        near.iter().any(|r| r.source == "cromatolis_v0_routes.ron"),
        "route segment within 600 m not reported: {near:?}"
    );
    // The vertex-only test (what authored_points did before) finds nothing.
    let mut verts = Vec::new();
    all_authored_points(&repo_assets(), size, &mut verts);
    assert!(
        !verts.iter().any(|r| near_box(r.wx, r.wy, b, 600)),
        "a vertex is near: the case no longer proves segment awareness"
    );
}

/// EC-D34c: exact segment-vs-grown-box geometry (Liang-Barsky), including
/// segments that only graze the margin, degenerate segments and axis-aligned
/// ones.
#[test]
fn ec_d34c_segment_near_box_geometry() {
    let b = Box2 {
        x0: 100,
        y0: 100,
        x1: 200,
        y1: 200,
    };
    // Crosses the box.
    assert!(segment_near_box((0.0, 150.0), (1000.0, 150.0), b, 0));
    // Parallel, 50 m below: only within a margin >= 50.
    assert!(!segment_near_box((0.0, 50.0), (1000.0, 50.0), b, 40));
    assert!(segment_near_box((0.0, 50.0), (1000.0, 50.0), b, 50));
    // Diagonal x + y = 500: its closest point to the box is (250, 250), 50 m
    // from the corner along each axis.
    assert!(!segment_near_box((400.0, 100.0), (100.0, 400.0), b, 40));
    assert!(segment_near_box((400.0, 100.0), (100.0, 400.0), b, 60));
    // Degenerate (a point) and ending before the box.
    assert!(segment_near_box((150.0, 150.0), (150.0, 150.0), b, 0));
    assert!(!segment_near_box((0.0, 0.0), (50.0, 0.0), b, 10));
    // closest_on_segment clamps to the ends.
    assert_eq!(
        closest_on_segment((0.0, 0.0), (10.0, 0.0), (20.0, 5.0)),
        (10.0, 0.0)
    );
    assert_eq!(
        closest_on_segment((0.0, 0.0), (10.0, 0.0), (4.0, 5.0)),
        (4.0, 0.0)
    );
}

/// EC-D34d: a RON file declaring a coordinate space the probe cannot convert
/// is an error for the strict guard (and a stderr warning for the lenient
/// dump), never a silent skip.
#[test]
fn ec_d34d_unknown_coordinate_space_is_refused() {
    let dir = std::env::temp_dir().join(format!("tprobe-edge-assets3-{}", std::process::id()));
    let map = dir.join("world/map");
    std::fs::create_dir_all(&map).unwrap();
    std::fs::write(
        map.join("cromatolis_v0_future.ron"),
        "(coordinate_space: \"brand_new_space\", things: [(id: \"a\", x: 0.5, y: 0.5)])",
    )
    .unwrap();
    let b = Box2 {
        x0: 0,
        y0: 0,
        x1: 10,
        y1: 10,
    };
    let strict = authored_near(&dir, Vec2::broadcast(32768), b, 600);
    let _ = std::fs::remove_dir_all(&dir);
    let err = strict.expect_err("unknown space must not be skipped silently");
    assert!(err.contains("brand_new_space"), "{err}");
}

/// EC-D35: normalised points on the map border map to the world border and
/// rounding is half away from zero (documented here so the Python side
/// matches).
#[test]
fn ec_d35_normalised_point_mapping_at_the_border() {
    let dir = std::env::temp_dir().join(format!("tprobe-edge-assets2-{}", std::process::id()));
    let map = dir.join("world/map");
    std::fs::create_dir_all(&map).unwrap();
    std::fs::write(
        map.join("cromatolis_v0_sites.ron"),
        "(coordinate_space: \"normalized_map_xy_top_left_origin\", sites: [(id: \"a\", x: 0.0, y: \
         0.0), (id: \"b\", x: 1.0, y: 1.0), (id: \"c\", x: 1.5, y: 0.5), (id: \"d\", x: \
         0.0000152587890625, y: 0.5)])",
    )
    .unwrap();
    let mut all = Vec::new();
    all_authored_points(&dir, Vec2::broadcast(32768), &mut all);
    let _ = std::fs::remove_dir_all(&dir);
    let get = |id: &str| {
        all.iter()
            .find(|r| r.id.as_deref() == Some(id))
            .map(|r| (r.wx, r.wy))
    };
    assert_eq!(get("a"), Some((0, 32768)), "top-left = north-west corner");
    assert_eq!(
        get("b"),
        Some((32768, 0)),
        "bottom-right = south-east corner"
    );
    assert_eq!(get("c"), None, "x outside [0,1] is dropped silently");
    assert_eq!(
        get("d"),
        Some((1, 16384)),
        "0.5 m rounds half away from zero"
    );
}

/// Every consecutive vertex pair of the `points: [...]` polylines in the
/// normalised authored RON files, in world metres (test helper: a segment-aware
/// emptiness check, see BUG-P7).
fn authored_segments(root: &Path, size: Vec2<i32>) -> Vec<((f64, f64), (f64, f64))> {
    fn walk(v: &ron::Value, size: Vec2<i32>, out: &mut Vec<((f64, f64), (f64, f64))>) {
        match v {
            ron::Value::Map(m) => {
                if let Some(ron::Value::Seq(pts)) = field(m, "points") {
                    let xy: Vec<(f64, f64)> = pts
                        .iter()
                        .filter_map(|p| match p {
                            ron::Value::Map(pm) => Some((
                                field(pm, "x").and_then(num)? * f64::from(size.x),
                                (1.0 - field(pm, "y").and_then(num)?) * f64::from(size.y),
                            )),
                            _ => None,
                        })
                        .collect();
                    out.extend(xy.windows(2).map(|w| (w[0], w[1])));
                }
                for (_, c) in m.iter() {
                    walk(c, size, out);
                }
            },
            ron::Value::Seq(items) => items.iter().for_each(|c| walk(c, size, out)),
            ron::Value::Option(Some(c)) => walk(c, size, out),
            _ => {},
        }
    }
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("world/map")) else {
        return out;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !(n.starts_with("cromatolis_v0_") && n.ends_with(".ron")) {
            continue;
        }
        if let Some(v) = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|t| ron::from_str::<ron::Value>(&t).ok())
            && coordinate_space_is_normalized(&v)
        {
            walk(&v, size, &mut out);
        }
    }
    out
}

/// Distance from segment `a`-`e` to the box (0 when they meet), sampled finely
/// enough (1 m) for a 600 m clearance test.
fn segment_box_distance(a: (f64, f64), e: (f64, f64), b: Box2) -> f64 {
    let len = (e.0 - a.0).hypot(e.1 - a.1);
    let n = (len.ceil() as usize).max(1);
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let (x, y) = (a.0 + (e.0 - a.0) * t, a.1 + (e.1 - a.1) * t);
            let dx = (f64::from(b.x0) - x).max(0.0).max(x - f64::from(b.x1));
            let dy = (f64::from(b.y0) - y).max(0.0).max(y - f64::from(b.y1));
            dx.hypot(dy)
        })
        .fold(f64::INFINITY, f64::min)
}

// ------------------------------------------------------------- heavy

fn probe() -> &'static Probe {
    static P: OnceLock<Probe> = OnceLock::new();
    P.get_or_init(|| {
        let p = load(0, None).expect("world generation");
        assert!(
            p.assets_root.join("world/map/cromatolis_v0.bin").exists(),
            "VELOREN_ASSETS is not a Cromatolis asset root"
        );
        p
    })
}

fn opts(b: Box2, zmin: Option<i32>, zmax: Option<i32>) -> DumpOpts {
    DumpOpts {
        bx: b,
        zmin,
        zmax,
        site_margin: 600,
        keep_sprites: false,
        force: false,
    }
}

fn bx(x0: i32, y0: i32, x1: i32, y1: i32) -> Box2 { Box2 { x0, y0, x1, y1 } }

/// The research arena (spec section 4.1: >= 2.15 km from any authored point).
const ARENA: (i32, i32) = (23264, 25312);

/// EC-D40: the research arena is empty (sites and authored points within
/// 600 m), so the heavy tests below may use it.
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d40_arena_is_empty() {
    let p = probe();
    let b = bx(ARENA.0 - 200, ARENA.1 - 200, ARENA.0 + 200, ARENA.1 + 200);
    assert!(world_sites(&p.index.as_index_ref(), b, 600).is_empty());
    assert!(authored_points(&p.assets_root, world_size(p.world.sim()), b, 600).is_empty());
}

/// EC-D41: boxes leaving the world or straddling its edge are refused; boxes
/// touching the edge from inside dump fine, but the column sampler returns
/// nothing (NaN floats) in a band along the world border: verifiers must
/// treat NaN `alt`/`water_level` as "no data", not as a value.
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d41_world_edge_boxes() {
    let p = probe();
    let s = world_size(p.world.sim());
    for b in [
        bx(-1, 0, 5, 5),
        bx(0, -1, 5, 5),
        bx(s.x - 5, 0, s.x + 1, 5),
        bx(0, s.y - 5, 5, s.y + 1),
    ] {
        assert!(dump(p, &opts(b, Some(0), Some(300)), &|_, _| {}).is_err());
    }
    let w = 96;
    for (b, name) in [
        (bx(0, 0, w, w), "SW corner"),
        (bx(s.x - w, s.y - w, s.x, s.y), "NE corner"),
        (bx(0, s.y - w, w, s.y), "NW corner"),
        (bx(s.x - w, 0, s.x, w), "SE corner"),
    ] {
        let (d, st) = dump(p, &opts(b, Some(-600), Some(400)), &|_, _| {})
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(st.columns, (w * w) as usize);
        d.validate().unwrap();
        let mut nan = 0;
        let mut band = 0;
        let mut nan_no_ground = 0;
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                let i = d.col_index(x, y).unwrap();
                // Every column the sampler returned nothing for is flagged,
                // and only those.
                assert_eq!(
                    d.alt[i].is_nan(),
                    d.flags[i] & flag::RIM_NO_SAMPLE != 0,
                    "{name}: ({x},{y}) RIM_NO_SAMPLE flag disagrees with a NaN alt"
                );
                if d.alt[i].is_nan() {
                    nan += 1;
                    nan_no_ground += usize::from(d.ground_top[i] == NO_Z);
                    let edge = x.min(y).min(s.x - 1 - x).min(s.y - 1 - y);
                    band = band.max(edge + 1);
                }
            }
        }
        let wet = d.flags.iter().filter(|&&f| f & flag::LIQUID != 0).count();
        let surf: Vec<i16> = d.water_top.iter().copied().filter(|&z| z != NO_Z).collect();
        eprintln!(
            "{name}: NaN alt {nan}/{} ({nan_no_ground} of them without ground in z -600..400), \
             NaN band {band} m from the border, wet {wet}, water_top {:?}..{:?}, ground_top \
             {:?}..{:?}",
            w * w,
            surf.iter().min(),
            surf.iter().max(),
            d.ground_top.iter().min(),
            d.ground_top.iter().max()
        );
        assert!(band <= 64, "{name}: NaN columns {band} m inside the world");
        assert_eq!(d.header.stats["rim_no_sample_columns"], nan as u64);
    }
}

/// EC-D42: the open ocean at the map corners sits at sea level: every
/// liquid column's surface (`water_top + 1`) is 140 m.
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d42_ocean_surface_is_sea_level() {
    let p = probe();
    let (d, _) = dump(
        p,
        &opts(bx(0, 0, 64, 64), Some(-200), Some(400)),
        &|_, _| {},
    )
    .unwrap();
    let wet: Vec<i16> = d.water_top.iter().copied().filter(|&z| z != NO_Z).collect();
    assert!(!wet.is_empty(), "the SW corner is expected to be ocean");
    assert!(
        wet.iter().all(|&z| z + 1 == 140),
        "surfaces {:?}..{:?}",
        wet.iter().min(),
        wet.iter().max()
    );
}

/// EC-D43: a sub-box with odd, non-chunk-aligned edges reads exactly the same
/// columns as a larger box (chunk/tile boundary independence).
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d43_sub_box_equals_slice_of_large_box() {
    let p = probe();
    let big = bx(ARENA.0 - 70, ARENA.1 - 70, ARENA.0 + 70, ARENA.1 + 70);
    let (a, _) = dump(p, &opts(big, Some(200), Some(300)), &|_, _| {}).unwrap();
    let mut rng = crate::format::edge_tests::Rng::new(43);
    for _ in 0..6 {
        let x0 = big.x0 + rng.below(100) as i32;
        let y0 = big.y0 + rng.below(100) as i32;
        let small = bx(
            x0,
            y0,
            x0 + 1 + rng.below(39) as i32,
            y0 + 1 + rng.below(39) as i32,
        );
        let (s, _) = dump(p, &opts(small, Some(200), Some(300)), &|_, _| {}).unwrap();
        let (oa, os) = (a.run_offsets(), s.run_offsets());
        for y in small.y0..small.y1 {
            for x in small.x0..small.x1 {
                let (i, j) = (a.col_index(x, y).unwrap(), s.col_index(x, y).unwrap());
                assert_eq!(a.alt[i].to_bits(), s.alt[j].to_bits(), "alt at {x},{y}");
                assert_eq!(a.water_level[i].to_bits(), s.water_level[j].to_bits());
                assert_eq!(a.ground_top[i], s.ground_top[j]);
                assert_eq!(a.flags[i], s.flags[j]);
                assert!(a.runs_at(&oa, i).eq(s.runs_at(&os, j)), "runs at {x},{y}");
            }
        }
    }
}

/// EC-D44: dumps are identical for different worker-thread counts (the world
/// is loaded from a file; no thread-order dependence may leak in).
#[test]
#[ignore = "heavy: generates the real world twice"]
fn ec_d44_thread_count_independence() {
    let b = bx(ARENA.0 - 48, ARENA.1 - 48, ARENA.0 + 48, ARENA.1 + 48);
    let one = load(0, Some(1)).unwrap();
    let (a, _) = dump(&one, &opts(b, Some(200), Some(300)), &|_, _| {}).unwrap();
    drop(one);
    let many = load(0, Some(8)).unwrap();
    let (m, _) = dump(&many, &opts(b, Some(200), Some(300)), &|_, _| {}).unwrap();
    assert!(crate::format::edge_tests::bytes(&a) == crate::format::edge_tests::bytes(&m));
}

/// EC-D45: `--zmax` below the ground clips every column (flag set, ground at
/// zmax-1); `--zmin` above the ground leaves no ground and NO flag (the
/// clipped-bottom gap of EC-D27, on real data).
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d45_z_range_cuts() {
    let p = probe();
    let b = bx(ARENA.0, ARENA.1, ARENA.0 + 32, ARENA.1 + 32);
    let (low, _) = dump(p, &opts(b, Some(100), Some(200)), &|_, _| {}).unwrap();
    assert!(low.flags.iter().all(|&f| f & flag::CLIPPED_TOP != 0));
    assert!(low.ground_top.iter().all(|&z| z == 199));
    assert_eq!(low.header.stats["clipped_top_columns"], 32 * 32);
    let (high, _) = dump(p, &opts(b, Some(400), Some(450)), &|_, _| {}).unwrap();
    assert!(high.ground_top.iter().all(|&z| z == NO_Z));
    assert!(high.flags.iter().all(|&f| f == 0));
}

/// EC-D46: z ranges outside the i16 span are accepted by `dump` and then
/// wrap in the i16 summary fields (see EC-D25).
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d46_z_range_outside_i16_is_refused() {
    let p = probe();
    let b = bx(ARENA.0, ARENA.1, ARENA.0 + 8, ARENA.1 + 8);
    assert!(dump(p, &opts(b, Some(-40_000), Some(-39_990)), &|_, _| {}).is_err());
}

/// EC-D47: `--keep-sprites` only turns AIR blocks into SPRITE; terrain,
/// water and structure blocks are unchanged.
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d47_keep_sprites_only_changes_air() {
    let p = probe();
    let b = bx(ARENA.0 - 64, ARENA.1 - 64, ARENA.0 + 64, ARENA.1 + 64);
    let (a, _) = dump(p, &opts(b, Some(200), Some(300)), &|_, _| {}).unwrap();
    let mut o = opts(b, Some(200), Some(300));
    o.keep_sprites = true;
    let (s, _) = dump(p, &o, &|_, _| {}).unwrap();
    let (oa, os) = (a.run_offsets(), s.run_offsets());
    let mut sprites = 0u64;
    for i in 0..a.header.columns() {
        for z in 200..300 {
            let (ca, cs) = (
                a.class_at(&oa, i, z).unwrap(),
                s.class_at(&os, i, z).unwrap(),
            );
            if ca != cs {
                assert_eq!((ca, cs), (class::AIR, class::SPRITE), "column {i} z {z}");
                sprites += 1;
            }
        }
    }
    eprintln!("sprite cells in the arena box: {sprites}");
}

/// EC-D48: the automatic z range is computed from the chunks the box touches
/// only, but a column's altitude interpolates towards its east/north
/// neighbour chunk. Next to a steep neighbour the default range clips the
/// surface (the README promises "highest alt/water + 96" covers it).
#[test]
#[ignore = "heavy: generates the real world"]
fn ec_d48_auto_z_range_covers_the_surface() {
    let p = probe();
    let sim = p.world.sim();
    let s = sim.get_size().map(|e| e as i32);
    let ir = p.index.as_index_ref();
    // Steepest eastward/northward chunk steps in the interior of the map.
    let mut cand: Vec<(f32, Vec2<i32>)> = Vec::new();
    for cy in 4..s.y - 4 {
        for cx in 4..s.x - 4 {
            let c = Vec2::new(cx, cy);
            let a = sim.get(c).unwrap().alt;
            let up = sim
                .get(c + Vec2::unit_x())
                .unwrap()
                .alt
                .max(sim.get(c + Vec2::unit_y()).unwrap().alt);
            cand.push((up - a, c));
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    let segments = authored_segments(&p.assets_root, world_size(sim));
    assert!(!segments.is_empty(), "no authored polylines found");
    let mut tested = 0;
    for (step, c) in cand.into_iter().take(400) {
        let b = bx(c.x * 32, c.y * 32, c.x * 32 + 32, c.y * 32 + 32);
        // Only wilderness: nothing authored or generated within 600 m, and no
        // authored polyline segment either (the vertex-only guard is BUG-P7).
        if !world_sites(&ir, b, 600).is_empty()
            || !authored_points(&p.assets_root, world_size(sim), b, 600).is_empty()
            || segments
                .iter()
                .any(|&(a, e)| segment_box_distance(a, e, b) < 600.0)
        {
            continue;
        }
        let (d, _) = dump(p, &opts(b, None, None), &|_, _| {}).unwrap();
        eprintln!(
            "chunk {c} step {step:.0} m: z {}..{}, clipped {}",
            d.header.zmin, d.header.zmax, d.header.stats["clipped_top_columns"]
        );
        assert_eq!(d.header.stats["clipped_top_columns"], 0, "chunk {c}");
        tested += 1;
        if tested == 3 {
            break;
        }
    }
    assert!(tested > 0, "no empty steep chunk found");
}
