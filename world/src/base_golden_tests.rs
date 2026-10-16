//! Golden digests of the procedural world, recorded at the engine commit
//! `65deeb2a20` (before the authored water and ground layers, the stable
//! per-site seeds and the land red clamp), against the same world generated
//! by the current code.
//!
//! The older "byte-identical" guards (`authored_raster::
//! presentation_real_world_tests`) compare `get_map_with(.., true)` with
//! `(.., false)` through the *same* new code, which tests a switch rather
//! than equality with the code before. These tests compare against digests
//! recorded from that earlier code, on a generated 2^7 x 2^7 procedural
//! world (seed 0) that needs no authored assets, so they run in the normal
//! test pass:
//!
//! * the map with the land red clamp off is exactly the base map (colour and
//!   altitude planes);
//! * the map with the clamp on differs from it only by land pixels whose red
//!   channel went from 0 to 1;
//! * generated chunks (terrain, water, ground and where every structure or
//!   sprite stands) are exactly the base chunks. Chunk generation loads the
//!   `.vox` structures, which live in LFS; without them (the CI checkout) the
//!   chunk test says so and skips, so it only guards where the assets exist.
//!
//! The base was built with the same test-only seeding of the dynamic and
//! structure-choice RNGs that this crate has (`with_deterministic_dynamic_rng`)
//! ported onto it; the recorder is the `shared` block below run on the base.
//! If an intended change to procedural generation lands, re-record by running
//! the helpers below on the new code and say so in the commit.

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

// ---- the goldens, recorded at 65deeb2a20 ----

const BASE_MAP_RGBA_SHA256: &str =
    "43ae18d2b3a3f63fbc86779a54c9c4ff148e9fa59f5a49670e5e7257a49fdf9c";
const BASE_MAP_ALT_SHA256: &str =
    "0ccf81c195ab03967dc687676b6bea59be044ae1dad9e9e7cfab259fa6ce3672";
const BASE_CHUNK_SHA256: [((i32, i32), &str); 6] = [
    (
        (20, 20),
        "8ee5af6b5392c3eea4e4a5b88570a944a9a9d1b010d6bb2808dfafd6af26aa2c",
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
