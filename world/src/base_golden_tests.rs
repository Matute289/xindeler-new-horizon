//! Guards that the procedural world is unchanged by the authored water and
//! ground layers, the stable per-site seeds and the land red clamp, on a
//! generated 2^7 x 2^7 procedural world (seed 0) that needs no authored
//! assets.
//!
//! The older "byte-identical" guards (`authored_raster::
//! presentation_real_world_tests`) compare `get_map_with(.., true)` with
//! `(.., false)` through the *same* new code, which tests a switch rather
//! than equality with the code before. Two kinds of test live here.
//!
//! **Structural tests (run everywhere, including CI on Linux).** They compare
//! the new code with itself and never hash absolute values, because the noise
//! and erosion use f32 `powf`/`exp`/`sin` whose last bits differ between
//! platform maths libraries (Apple libm vs glibc): the same code makes a
//! different map on macOS and on Linux.
//!
//! * the map with the land red clamp on equals the map with it off except for
//!   land pixels whose red channel went from 0 to 1 (same green, blue and
//!   alpha; no water pixel changes; altitude untouched);
//! * `authored_ground` true and false give identical maps when no authored
//!   layer exists;
//! * generating the world twice in one process gives a bit-identical map.
//!
//! **Local-only digests (`#[ignore]`).** Digests recorded at the engine commit
//! `65deeb2a20` (before those changes) on **macOS arm64** pin the new code to
//! the old output byte for byte, with one deliberate exception: the digest of
//! chunk (20, 20) was re-blessed on purpose, because the tree layer no longer
//! writes tree voxels that would be buried in natural terrain (a chunk with
//! trees against a cliff or hillside changes; the map digests and the other
//! five chunks are untouched). They cannot hold on another platform, so they
//! are ignored by default. Run them on a Mac with the `.vox` assets (LFS):
//!
//! ```text
//! VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-world --lib \
//!     base_golden_tests -- --ignored
//! ```
//!
//! To re-bless after an intended change to procedural generation, run on the
//! same platform:
//!
//! ```text
//! VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-world --lib \
//!     base_golden_tests::print_current_digests -- --ignored --nocapture
//! ```
//!
//! paste the printed constants below and say so in the commit. The chunk
//! digests need the real `.vox` structures (LFS); without them that test says
//! so and skips.
//!
//! The base was built with the same test-only seeding of the dynamic and
//! structure-choice RNGs that this crate has (`with_deterministic_dynamic_rng`)
//! ported onto it.

use crate::{
    World,
    sim::{FileOpts, GenOpts, WorldOpts},
};
use common::{
    terrain::{Block, BlockKind, TerrainChunkSize},
    vol::{ReadVol, RectVolSize},
};
use common_net::msg::WorldMapMsg;
use vek::*;

fn sha256(bytes: &[u8]) -> String { crate::authored_raster::format::sha256_hex(bytes) }

// ---- shared with the recorder that ran on the base commit ----

/// The procedural world every golden is recorded on: 2^7 x 2^7 chunks, seed 0,
/// no authored layers at all (so the authored-raster code is idle).
fn procedural_world() -> (World, crate::index::IndexOwned) {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    World::generate(
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
    )
}

/// The map's colour plane, every chunk's `u32` as little-endian bytes.
fn rgba_bytes(msg: &WorldMapMsg) -> Vec<u8> {
    msg.rgba
        .raw()
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

/// The map's altitude plane, every chunk's `u32` as little-endian bytes.
fn alt_bytes(msg: &WorldMapMsg) -> Vec<u8> {
    msg.alt.raw().iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A chunk, block by block: the z range, then one class per block. Air 0,
/// natural ground 1, any other solid or sprite 2, liquids 0x100 + kind. Which
/// kind of decoration a structure block gets still partly comes from
/// unseeded RNGs, so decoration is compared by presence, not kind.
fn chunk_bytes(world: &World, index: crate::IndexRef, cpos: Vec2<i32>) -> Vec<u8> {
    let mut seed = [0u8; 32];
    seed[..4].copy_from_slice(&cpos.x.to_le_bytes());
    seed[4..8].copy_from_slice(&cpos.y.to_le_bytes());
    let (chunk, _) = crate::with_deterministic_dynamic_rng(seed, || {
        world
            .generate_chunk(index, cpos, None, || false, None, None)
            .unwrap()
    });
    let class = |b: &Block| -> u32 {
        if b.is_liquid() || b.kind() == BlockKind::Lava {
            0x100 + b.kind() as u32
        } else if matches!(
            b.kind(),
            BlockKind::Rock
                | BlockKind::WeakRock
                | BlockKind::Earth
                | BlockKind::Grass
                | BlockKind::Sand
                | BlockKind::Snow
        ) {
            1
        } else if b.is_filled()
            || b.get_sprite()
                .is_some_and(|s| s != common::terrain::SpriteKind::Empty)
        {
            2
        } else {
            0
        }
    };
    let sz = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    let mut out = Vec::new();
    out.extend(chunk.get_min_z().to_le_bytes());
    out.extend(chunk.get_max_z().to_le_bytes());
    for z in chunk.get_min_z()..chunk.get_max_z() {
        for y in 0..sz.y {
            for x in 0..sz.x {
                let b = chunk
                    .get(Vec3::new(x, y, z))
                    .copied()
                    .unwrap_or_else(|_| Block::empty());
                out.extend(class(&b).to_le_bytes());
            }
        }
    }
    out
}

// ---- the local-only goldens, recorded at 65deeb2a20 on macOS arm64 ----

const BASE_MAP_RGBA_SHA256: &str =
    "43ae18d2b3a3f63fbc86779a54c9c4ff148e9fa59f5a49670e5e7257a49fdf9c";
const BASE_MAP_ALT_SHA256: &str =
    "0ccf81c195ab03967dc687676b6bea59be044ae1dad9e9e7cfab259fa6ce3672";
const BASE_CHUNK_SHA256: [((i32, i32), &str); 6] = [
    (
        (20, 20),
        "4389a7f63f446348474c9cdb1fe777e9cddfc429ed3e4f6b3a7dc77268194463",
    ),
    (
        (64, 64),
        "946f699079c624f8112dba743f82061f850bbf5d3f839e729bea23229d20d491",
    ),
    (
        (100, 30),
        "24c3a553c7ec3ad279a93e559630ac7f55dc85c4a26946e4fce2cc7e0825197f",
    ),
    (
        (10, 100),
        "e30db48cdd0322deb673bfaf66ef14b9b48205e93ffa13db6885cd33d48759fe",
    ),
    (
        (64, 30),
        "6dd94715adaaa2b1e006971fcddc2c0f65b3bddbd6ce0daab2e065a3cc5d6e55",
    ),
    (
        (30, 64),
        "ca6a7bae8563d57647ae9e262f8b1b4378fb59b6acb4ebe7626770cf8999a479",
    ),
];
/// The colour plane with the land red clamp on (current code): the base
/// plane plus the one intended difference.
const CLAMPED_MAP_RGBA_SHA256: &str =
    "de4fc5bfa75996e4f367f46d694d694aa7acf2a40d7351f8c33185e714d4b51e";

#[test]
#[ignore = "digest recorded on macOS arm64; run locally with --ignored"]
fn the_procedural_map_without_the_red_clamp_is_the_base_map() {
    let (world, index) = procedural_world();
    assert!(world.sim().authored_rasters.is_none());
    for authored_ground in [true, false] {
        let old = world
            .sim()
            .get_map_opts(index.as_index_ref(), None, authored_ground, false);
        assert_eq!(
            sha256(&rgba_bytes(&old)),
            BASE_MAP_RGBA_SHA256,
            "colour plane (authored_ground {authored_ground}) differs from the base map"
        );
        assert_eq!(
            sha256(&alt_bytes(&old)),
            BASE_MAP_ALT_SHA256,
            "altitude plane (authored_ground {authored_ground}) differs from the base map"
        );
    }
}

#[test]
#[ignore = "digest recorded on macOS arm64; run locally with --ignored"]
fn the_red_clamp_only_raises_land_red_from_zero_to_one() {
    let (world, index) = procedural_world();
    let old = world
        .sim()
        .get_map_opts(index.as_index_ref(), None, true, false);
    let new = world.sim().get_map(index.as_index_ref(), None);
    // The clamp never moves altitude, and the clamped map is the one the
    // game serves (recorded, so a later change to it is deliberate).
    assert_eq!(alt_bytes(&new), alt_bytes(&old));
    assert_eq!(sha256(&rgba_bytes(&new)), CLAMPED_MAP_RGBA_SHA256);
    let mut changed = 0usize;
    for (n, o) in new.rgba.raw().iter().zip(old.rgba.raw()) {
        if n != o {
            changed += 1;
            let ([nr, ng, nb, na], [or, og, ob, oa]) = (n.to_le_bytes(), o.to_le_bytes());
            assert!(
                or == 0 && nr == 1 && (ng, nb, na) == (og, ob, oa),
                "a pixel changed beyond red 0 -> 1: {o:#010x} -> {n:#010x}"
            );
        }
    }
    println!("{changed} pixels raised red 0 -> 1");
}

#[test]
#[ignore = "digest recorded on macOS arm64; run locally with --ignored"]
fn procedural_chunks_match_the_base() {
    let vox = common::assets::ASSETS_PATH.join("world/structure/natural/ribcage-small.vox");
    if !std::fs::read(&vox).is_ok_and(|bytes| bytes.starts_with(b"VOX ")) {
        eprintln!(
            "skipped: {} is not a real .vox file (no LFS assets), and chunk generation loads \
             structures",
            vox.display()
        );
        return;
    }
    let (world, index) = procedural_world();
    for ((x, y), want) in BASE_CHUNK_SHA256 {
        let got = sha256(&chunk_bytes(&world, index.as_index_ref(), Vec2::new(x, y)));
        assert_eq!(got, want, "chunk ({x}, {y}) differs from the base chunk");
    }
}

/// Prints the current digests in the shape of the constants above, for
/// re-blessing (see the file header). Local only.
#[test]
#[ignore = "re-bless helper; run locally with --ignored --nocapture"]
fn print_current_digests() {
    let (world, index) = procedural_world();
    let old = world
        .sim()
        .get_map_opts(index.as_index_ref(), None, true, false);
    let new = world.sim().get_map(index.as_index_ref(), None);
    println!("BASE_MAP_RGBA_SHA256 = {}", sha256(&rgba_bytes(&old)));
    println!("BASE_MAP_ALT_SHA256 = {}", sha256(&alt_bytes(&old)));
    println!("CLAMPED_MAP_RGBA_SHA256 = {}", sha256(&rgba_bytes(&new)));
    for (x, y) in BASE_CHUNK_SHA256.map(|(c, _)| c) {
        println!(
            "chunk ({x}, {y}) = {}",
            sha256(&chunk_bytes(&world, index.as_index_ref(), Vec2::new(x, y)))
        );
    }
}

// ---- platform-independent structural tests (run in CI) ----

#[test]
fn the_red_clamp_changes_only_land_red_from_zero_to_one() {
    let (world, index) = procedural_world();
    assert!(world.sim().authored_rasters.is_none());
    let off = world
        .sim()
        .get_map_opts(index.as_index_ref(), None, true, false);
    let on = world.sim().get_map(index.as_index_ref(), None);
    assert_eq!(off.rgba.size(), on.rgba.size());
    assert_eq!(alt_bytes(&on), alt_bytes(&off), "the clamp moved altitude");
    let mut changed = 0usize;
    for ((pos, &n), &o) in on.rgba.iter().zip(off.rgba.raw()) {
        if n != o {
            changed += 1;
            let ([nr, ng, nb, na], [or, og, ob, oa]) = (n.to_le_bytes(), o.to_le_bytes());
            assert!(
                or == 0 && nr == 1 && (ng, nb, na) == (og, ob, oa),
                "a pixel changed beyond red 0 -> 1: {o:#010x} -> {n:#010x}"
            );
            assert!(
                world.sim().get(pos).is_some_and(|c| !c.is_underwater()),
                "a water pixel changed at {pos}"
            );
        }
    }
    println!("{changed} pixels raised red 0 -> 1");
}

#[test]
fn authored_ground_on_or_off_gives_the_same_map_without_authored_layers() {
    let (world, index) = procedural_world();
    assert!(world.sim().authored_rasters.is_none());
    let with = world
        .sim()
        .get_map_opts(index.as_index_ref(), None, true, false);
    let without = world
        .sim()
        .get_map_opts(index.as_index_ref(), None, false, false);
    assert!(
        rgba_bytes(&with) == rgba_bytes(&without),
        "colour plane differs"
    );
    assert!(
        alt_bytes(&with) == alt_bytes(&without),
        "altitude plane differs"
    );
}

#[test]
fn the_map_is_bit_identical_when_generated_twice_in_one_process() {
    let (world_a, index_a) = procedural_world();
    let (world_b, index_b) = procedural_world();
    let a = world_a.sim().get_map(index_a.as_index_ref(), None);
    let b = world_b.sim().get_map(index_b.as_index_ref(), None);
    assert!(rgba_bytes(&a) == rgba_bytes(&b), "colour plane differs");
    assert!(alt_bytes(&a) == alt_bytes(&b), "altitude plane differs");
}
