//! The columns a placed spot's prefab can clear of natural terrain.
//!
//! Tooling only (the `tools` feature, and tests): the game never asks which
//! columns a spot digs, it just blits the prefab. The external `spots/v1`
//! consumer needs that set to tell a spot's dent from a defect on authored
//! ground.

use super::{SpotDraw, spot_config, spot_draw};
use common::{
    spot::Spot,
    terrain::{Structure, structure::StructureBlock},
    vol::ReadVol,
};
use std::{collections::BTreeMap, ops::Range};
use vek::*;

/// Per-axis offsets from its chunk centre that a spot is blitted at: the engine
/// blits a spot into the canvas of its own chunk and of the eight around it
/// (`Canvas::nearby_spots`), i.e. offsets from -48 (centre - 16 - 32) to 47
/// (centre + 16 + 32 - 1) on each axis. Asymmetric because a chunk is 32 wide
/// and the centre sits at its 16th block.
pub(super) const SPOT_REACH: Range<i32> = -48..48;

/// Whether `block` can turn a block of natural terrain into air or liquid when
/// the prefab is blitted (`block_from_structure` / `Canvas::blit_structure`).
///
/// The rule is inverted on purpose: only a prefab voxel that leaves the
/// terrain alone (`None`) or replaces it with a solid block does NOT clear it;
/// everything else does. In particular every sprite voxel is built on
/// `block.into_vacant()`, so a sprite at ground level turns the solid terrain
/// block into air plus the sprite and digs the column; `EntitySpawner` writes
/// air; `MaybeChest`, `Liana`, `Keyhole*` and `Sign` are air plus a sprite;
/// `Water`, `GreenSludge` and fluid `Filled` blocks are liquid or air. Voxels
/// that only sometimes produce a block (`Fruit`, `Coconut`, `MaybeChest`) and a
/// `Choice` with any clearing option count as clearing (conservative).
///
/// No wildcard arm: a new `StructureBlock` variant has to be classified here.
pub(super) fn block_clears_terrain(block: &StructureBlock) -> bool {
    use StructureBlock as S;
    match block {
        // Left alone.
        S::None => false,
        // Solid blocks written over the terrain block.
        S::Grass
        | S::Normal(_)
        | S::Log
        | S::BirchWood
        | S::RedwoodWood
        | S::TemperateLeaves
        | S::PineLeaves
        | S::FrostpineLeaves
        | S::PalmLeavesInner
        | S::PalmLeavesOuter
        | S::Acacia
        | S::Mangrove
        | S::Chestnut
        | S::Baobab
        | S::MapleLeaves
        | S::CherryLeaves
        | S::AutumnLeaves => false,
        S::Filled(kind, _) => !kind.is_filled(),
        // Air, liquid, or air with a sprite on it.
        S::Hollow
        | S::Water
        | S::GreenSludge
        | S::Liana
        | S::Fruit
        | S::Coconut
        | S::MaybeChest
        | S::Sprite(_)
        | S::SpriteWithCfg(..)
        | S::EntitySpawner(..)
        | S::Keyhole(_)
        | S::BoneKeyhole(_)
        | S::GlassKeyhole(_)
        | S::KeyholeBars(_)
        | S::HaniwaKeyhole(_)
        | S::TerracottaKeyhole(_)
        | S::SahaginKeyhole(_)
        | S::VampireKeyhole(_)
        | S::MyrmidonKeyhole(_)
        | S::MinotaurKeyhole(_)
        | S::Sign(..) => true,
        // Resolved per position by the blitter.
        S::Choice(table) => table.iter().any(|(_, b)| block_clears_terrain(b)),
    }
}

/// One world column that a spot's prefab can clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HollowColumn {
    /// World column.
    pub wpos: Vec2<i32>,
    /// Clearing voxels of the prefab in this column, as heights relative to the
    /// spot origin (`0` = the origin block, `-1` = the block below), ascending.
    pub hollow_z: Vec<i32>,
}

impl HollowColumn {
    /// Blocks removed from the top of a column whose top natural block sits at
    /// `top_rel` (relative to the spot origin): the length of the clearing run
    /// that starts at `top_rel` and goes down. `0` when the top block itself is
    /// not cleared (a cavity under a roof does not dent the top).
    pub fn dig_blocks(&self, top_rel: i32) -> i32 {
        let mut n = 0;
        while self.hollow_z.binary_search(&(top_rel - n)).is_ok() {
            n += 1;
        }
        n
    }
}

/// The columns a placed spot can clear, in world space.
///
/// Computed from the clearing voxels only (never from the model's rectangle).
/// Built by [`spot_footprint`] (real placement) or [`hollow_footprint`] (any
/// prefab volume, used by the asset-free tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotFootprint {
    /// Spot origin column (the chunk centre for a real placement).
    pub origin: Vec2<i32>,
    /// Cleared columns, clipped to the spot's reach, sorted by `(y, x)`.
    pub columns: Vec<HollowColumn>,
}

/// Cleared footprint of any prefab volume `vol` (voxels in `bounds`), blitted
/// at world column `origin` with the rotation/mirroring `units` exactly as
/// `Canvas::blit_structure` does (`rpos = units.x * r.x + units.y * r.y` with
/// `r = wpos - origin`), keeping only the columns whose offset from `origin`
/// lies in `reach` on both axes. Pure: no assets, no RNG.
pub(super) fn hollow_footprint<V: ReadVol<Vox = StructureBlock>>(
    vol: &V,
    bounds: Aabb<i32>,
    origin: Vec2<i32>,
    units: Vec2<Vec2<i32>>,
    reach: Range<i32>,
) -> SpotFootprint {
    // `units` is a signed permutation matrix, so its inverse is its transpose:
    // r.x = units.x . s, r.y = units.y . s for the prefab column s.
    let mut cols: BTreeMap<(i32, i32), Vec<i32>> = BTreeMap::new();
    for sy in bounds.min.y..bounds.max.y {
        for sx in bounds.min.x..bounds.max.x {
            let s = Vec2::new(sx, sy);
            let r = Vec2::new(units.x.dot(s), units.y.dot(s));
            if !reach.contains(&r.x) || !reach.contains(&r.y) {
                continue;
            }
            let zs = (bounds.min.z..bounds.max.z)
                .filter(|&z| {
                    vol.get(s.with_z(z))
                        .map(block_clears_terrain)
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>();
            if !zs.is_empty() {
                let wpos = origin + r;
                cols.insert((wpos.y, wpos.x), zs);
            }
        }
    }
    SpotFootprint {
        origin,
        columns: cols
            .into_iter()
            .map(|((y, x), hollow_z)| HollowColumn {
                wpos: Vec2::new(x, y),
                hollow_z,
            })
            .collect(),
    }
}

/// The footprint of the spot `apply_spots_to` blits at `spot_wpos2d` with
/// placement `seed` (as returned by [`super::chunk_spot_placement`]). The
/// structure and rotation come from the same `spot_draw` the blitter uses.
/// `None` when the spot has no base structure or its group is empty. The caller
/// adds the terrain-dependent part with [`HollowColumn::dig_blocks`] (it needs
/// the altitude of each column).
pub fn spot_footprint(spot_wpos2d: Vec2<i32>, spot: &Spot, seed: u32) -> Option<SpotFootprint> {
    let group = spot_config(spot).base_structures.map(Structure::load_group);
    let group = group.as_ref().map(|group| group.read());
    let SpotDraw {
        units, structure, ..
    } = spot_draw(seed, group.as_ref().map(|group| -> &[Structure] { group }));
    let structure = structure?;
    Some(hollow_footprint(
        structure,
        structure.get_bounds(),
        spot_wpos2d,
        units,
        SPOT_REACH,
    ))
}

/// Asset-free tests of the footprint (a synthetic prefab, no `.vox`).
#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        terrain::{Block, BlockKind, SpriteCfg, SpriteKind, StructureSprite},
        vol::BaseVol,
    };
    use std::collections::{BTreeSet, HashMap};

    #[derive(Debug)]
    struct TestPrefab(HashMap<Vec3<i32>, StructureBlock>);

    impl BaseVol for TestPrefab {
        type Error = ();
        type Vox = StructureBlock;
    }

    impl ReadVol for TestPrefab {
        fn get(&self, pos: Vec3<i32>) -> Result<&StructureBlock, ()> {
            Ok(self.0.get(&pos).unwrap_or(&StructureBlock::None))
        }
    }

    fn bounds() -> Aabb<i32> {
        // Deliberately not square and not centred, so a transposition or a
        // mirror cannot cancel out.
        Aabb {
            min: Vec3::new(-3, -2, -4),
            max: Vec3::new(4, 3, 2),
        }
    }

    fn prefab() -> TestPrefab {
        let solid = || StructureBlock::Normal(Rgb::new(10, 20, 30));
        let mut m = HashMap::new();
        // Two hollow blocks from the origin block down.
        m.insert(Vec3::new(0, 0, 0), StructureBlock::Hollow);
        m.insert(Vec3::new(0, 0, -1), StructureBlock::Hollow);
        // A cavity that does not reach the origin block.
        m.insert(Vec3::new(2, -1, -2), StructureBlock::Hollow);
        m.insert(Vec3::new(2, -1, -3), StructureBlock::Hollow);
        // A single hollow block above the origin.
        m.insert(Vec3::new(-3, 2, 1), StructureBlock::Hollow);
        // A choice that can resolve to hollow.
        m.insert(
            Vec3::new(3, 0, 0),
            StructureBlock::Choice(vec![
                (1.0, StructureBlock::None),
                (1.0, StructureBlock::Hollow),
            ]),
        );
        // Solid prefab blocks and a non-clearing choice do not clear terrain.
        m.insert(Vec3::new(1, 1, 0), solid());
        m.insert(Vec3::new(1, 1, -1), solid());
        m.insert(
            Vec3::new(-2, -2, 0),
            StructureBlock::Choice(vec![(1.0, StructureBlock::None), (1.0, solid())]),
        );
        TestPrefab(m)
    }

    /// The eight rotation/mirror choices `UnitChooser` can return.
    fn all_units() -> Vec<Vec2<Vec2<i32>>> {
        let mut out = Vec::new();
        for swap in [false, true] {
            for sx in [-1, 1] {
                for sy in [-1, 1] {
                    let (a, b) = if swap {
                        (Vec2::new(0, sx), Vec2::new(sy, 0))
                    } else {
                        (Vec2::new(sx, 0), Vec2::new(0, sy))
                    };
                    out.push(Vec2::new(a, b));
                }
            }
        }
        out
    }

    fn cols(f: &SpotFootprint) -> BTreeSet<(i32, i32)> {
        f.columns.iter().map(|c| (c.wpos.x, c.wpos.y)).collect()
    }

    /// Deepest `dig_blocks` of any column for a top natural block anywhere in
    /// `top_lo..=top_hi` relative to the origin.
    fn max_dig(f: &SpotFootprint, top_lo: i32, top_hi: i32) -> i32 {
        f.columns
            .iter()
            .flat_map(|c| (top_lo..=top_hi).map(|t| c.dig_blocks(t)))
            .max()
            .unwrap_or(0)
    }

    fn sprite() -> StructureSprite {
        StructureSprite::from_block(&Block::air(SpriteKind::Chest))
            .expect("chest is a prefab sprite")
    }

    /// Every `StructureBlock` variant against what `block_from_structure` does
    /// to the terrain block it lands on.
    #[test]
    fn clearing_rule_covers_every_variant() {
        use StructureBlock as S;
        let rgb = || Rgb::new(1, 2, 3);
        let consumes = || "common.items.test_key".to_string();
        let leaves = [
            S::TemperateLeaves,
            S::PineLeaves,
            S::FrostpineLeaves,
            S::PalmLeavesInner,
            S::PalmLeavesOuter,
            S::Acacia,
            S::Mangrove,
            S::Chestnut,
            S::Baobab,
            S::MapleLeaves,
            S::CherryLeaves,
            S::AutumnLeaves,
        ];
        // Left alone or replaced by a solid block: the terrain block stays
        // (or is overwritten by something solid).
        for (name, b) in [
            ("None", S::None),
            ("Grass", S::Grass),
            ("Normal", S::Normal(rgb())),
            ("Log", S::Log),
            ("BirchWood", S::BirchWood),
            ("RedwoodWood", S::RedwoodWood),
            ("Filled(Rock)", S::Filled(BlockKind::Rock, rgb())),
            ("Filled(Earth)", S::Filled(BlockKind::Earth, rgb())),
            ("Filled(Lava)", S::Filled(BlockKind::Lava, rgb())),
            ("Filled(Wood)", S::Filled(BlockKind::Wood, rgb())),
        ]
        .into_iter()
        .chain(leaves.map(|b| ("leaves", b)))
        {
            assert!(!block_clears_terrain(&b), "{name} must not clear terrain");
        }
        // Air, liquid, or air with a sprite on it.
        for (name, b) in [
            ("Hollow", S::Hollow),
            ("Filled(Air)", S::Filled(BlockKind::Air, rgb())),
            ("Filled(Water)", S::Filled(BlockKind::Water, rgb())),
            ("Water", S::Water),
            ("GreenSludge", S::GreenSludge),
            ("Liana", S::Liana),
            ("Fruit", S::Fruit),
            ("Coconut", S::Coconut),
            ("MaybeChest", S::MaybeChest),
            ("Sprite", S::Sprite(sprite())),
            (
                "SpriteWithCfg",
                S::SpriteWithCfg(sprite(), SpriteCfg::default()),
            ),
            ("EntitySpawner", S::EntitySpawner("x.y".to_string(), 0.5)),
            ("Keyhole", S::Keyhole(consumes())),
            ("BoneKeyhole", S::BoneKeyhole(consumes())),
            ("GlassKeyhole", S::GlassKeyhole(consumes())),
            ("KeyholeBars", S::KeyholeBars(consumes())),
            ("HaniwaKeyhole", S::HaniwaKeyhole(consumes())),
            ("TerracottaKeyhole", S::TerracottaKeyhole(consumes())),
            ("SahaginKeyhole", S::SahaginKeyhole(consumes())),
            ("VampireKeyhole", S::VampireKeyhole(consumes())),
            ("MyrmidonKeyhole", S::MyrmidonKeyhole(consumes())),
            ("MinotaurKeyhole", S::MinotaurKeyhole(consumes())),
            (
                "Sign",
                S::Sign(common::comp::Content::Plain("x".to_string()), 0),
            ),
        ] {
            assert!(block_clears_terrain(&b), "{name} must clear terrain");
        }
        // Choice: clearing if any option can be.
        assert!(block_clears_terrain(&S::Choice(vec![
            (1.0, S::None),
            (1.0, S::Choice(vec![(1.0, S::Sprite(sprite()))])),
        ])));
        assert!(!block_clears_terrain(&S::Choice(vec![
            (1.0, S::None),
            (1.0, S::Normal(rgb())),
        ])));
        assert!(!block_clears_terrain(&S::Choice(vec![])));
    }

    #[test]
    fn identity_footprint_is_only_the_clearing_columns() {
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let f = hollow_footprint(&prefab(), bounds(), Vec2::new(100, 200), id, SPOT_REACH);
        let expect: BTreeSet<_> = [(0, 0), (2, -1), (-3, 2), (3, 0)]
            .iter()
            .map(|&(x, y)| (100 + x, 200 + y))
            .collect();
        assert_eq!(cols(&f), expect);
        // The solid prefab columns and the empty ones are not in it.
        assert!(!cols(&f).contains(&(101, 201)));
        assert!(!cols(&f).contains(&(98, 198)));
        // Sorted by (y, x), as documented.
        let keys: Vec<_> = f.columns.iter().map(|c| (c.wpos.y, c.wpos.x)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn dig_depth_counts_the_run_down_from_the_top_block() {
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let f = hollow_footprint(&prefab(), bounds(), Vec2::zero(), id, SPOT_REACH);
        let col = |x, y| {
            f.columns
                .iter()
                .find(|c| c.wpos == Vec2::new(x, y))
                .unwrap()
        };
        // Origin block and the one below: two blocks off a flat top.
        assert_eq!(col(0, 0).dig_blocks(0), 2);
        // A top one block lower than the origin loses only the lower hollow.
        assert_eq!(col(0, 0).dig_blocks(-1), 1);
        // A top above the prefab is not touched.
        assert_eq!(col(0, 0).dig_blocks(1), 0);
        // The cavity does not dent a flat top, but does dent a top two lower.
        assert_eq!(col(2, -1).dig_blocks(0), 0);
        assert_eq!(col(2, -1).dig_blocks(-2), 2);
        // The block above the origin only matters on a rise of one.
        assert_eq!(col(-3, 2).dig_blocks(0), 0);
        assert_eq!(col(-3, 2).dig_blocks(1), 1);
        // Footprint level: flat ground vs a slope window.
        assert_eq!(max_dig(&f, 0, 0), 2);
        assert_eq!(max_dig(&f, -2, 1), 2);
        assert_eq!(max_dig(&f, 1, 3), 1);
    }

    #[test]
    fn rotations_and_mirrors_match_the_blitter() {
        // Independent check against the forward transform `blit_structure`
        // applies: a world column is cleared iff its prefab column is.
        let prefab = prefab();
        let origin = Vec2::new(40, -17);
        for units in all_units() {
            let f = hollow_footprint(&prefab, bounds(), origin, units, SPOT_REACH);
            let mut expect = BTreeSet::new();
            for wy in origin.y - 20..origin.y + 20 {
                for wx in origin.x - 20..origin.x + 20 {
                    let r = Vec2::new(wx, wy) - origin;
                    let s = units.x * r.x + units.y * r.y;
                    let b = bounds();
                    let clears = (b.min.z..b.max.z).any(|z| {
                        prefab
                            .get(s.with_z(z))
                            .map(block_clears_terrain)
                            .unwrap_or(false)
                    });
                    if clears {
                        expect.insert((wx, wy));
                    }
                }
            }
            assert_eq!(cols(&f), expect, "units {units:?}");
            // Same depths whatever the orientation.
            assert_eq!(max_dig(&f, 0, 0), 2, "units {units:?}");
        }
        // The rotations really differ (the prefab is not symmetric).
        let a = hollow_footprint(&prefab, bounds(), origin, all_units()[0], SPOT_REACH);
        let b = hollow_footprint(&prefab, bounds(), origin, all_units()[4], SPOT_REACH);
        assert_ne!(cols(&a), cols(&b));
    }

    #[test]
    fn the_reach_is_the_blitted_range_minus_48_to_47() {
        // The engine blits offsets -48..=47 (own chunk plus the eight around
        // it); a column at +48 is never written, one at -48 is.
        assert_eq!(SPOT_REACH, -48..48);
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let mut m = HashMap::new();
        for x in [-49, -48, 47, 48] {
            m.insert(Vec3::new(x, 0, 0), StructureBlock::Hollow);
        }
        let f = hollow_footprint(
            &TestPrefab(m),
            Aabb {
                min: Vec3::new(-60, -2, -1),
                max: Vec3::new(60, 2, 1),
            },
            Vec2::zero(),
            id,
            SPOT_REACH,
        );
        assert_eq!(cols(&f), [(-48, 0), (47, 0)].into_iter().collect());
    }

    #[test]
    fn columns_beyond_the_given_reach_are_dropped() {
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let f = hollow_footprint(&prefab(), bounds(), Vec2::zero(), id, -3..3);
        // The range is half-open and asymmetric: x = -3 is in, x = 3 is out, so
        // (3, 0) is dropped while (-3, 2) stays.
        assert_eq!(cols(&f), [(0, 0), (2, -1), (-3, 2)].into_iter().collect());
    }
}

/// The footprint against the real blitter. Needs the committed prefab assets
/// (`VELOREN_ASSETS`, with the `.vox` files pulled), so it is `#[ignore]`d like
/// the other real-asset tests: `cargo test -p xindeler-world --lib
/// layer::spot::footprint -- --ignored`.
#[cfg(test)]
mod blit_tests {
    use super::*;
    use crate::{
        Canvas,
        block::ZCache,
        canvas::CanvasInfo,
        column::ColumnGen,
        util::{Grid, Sampler},
    };
    use common::{
        terrain::{Block, BlockKind, SpriteKind, TerrainChunk, TerrainChunkMeta},
        vol::WriteVol,
    };
    use strum::IntoEnumIterator;

    const GROUND_TOP: i32 = 40;

    /// Blit `structure` at `origin` over flat solid ground (top block at
    /// `GROUND_TOP`) into the nine chunks around chunk (0, 0), the way the
    /// engine does for the spot of that chunk (`Canvas::nearby_spots`), and
    /// return, per world column, how many blocks were taken off the top of the
    /// ground (consecutive non-solid blocks from `GROUND_TOP` down).
    fn blit_dug_columns(
        world: &crate::World,
        index_ref: crate::index::IndexRef,
        structure: &Structure,
        origin: Vec2<i32>,
        seed: u32,
        units: Vec2<Vec2<i32>>,
    ) -> BTreeMap<(i32, i32), i32> {
        let sim = world.sim();
        // Any real column will do: the blit only reads its surface colour and
        // snow flag. (The empty world cannot sample columns.)
        let middle = sim.get_size().map(|e| e as i32) / 2;
        let sample = ColumnGen::new(sim)
            .get((middle * 32 + 16, index_ref, None))
            .expect("a column in the middle of the map");
        let grid = Grid::populate_from(Vec2::broadcast(32), |_| {
            Some(ZCache {
                sample: sample.clone(),
                calendar: None,
            })
        });
        let sim_chunk = sim.get(middle).unwrap();
        let stone = Block::new(BlockKind::Rock, Rgb::zero());
        let air = Block::air(SpriteKind::Empty);

        let mut dug = BTreeMap::new();
        for cy in -1..=1 {
            for cx in -1..=1 {
                let chunk_pos = Vec2::new(cx, cy);
                let wpos = chunk_pos * 32;
                let mut chunk = TerrainChunk::new(0, stone, air, TerrainChunkMeta::void());
                for y in 0..32 {
                    for x in 0..32 {
                        for z in 0..=GROUND_TOP {
                            chunk.set(Vec3::new(x, y, z), stone).unwrap();
                        }
                    }
                }
                let mut canvas = Canvas {
                    info: CanvasInfo {
                        chunk_pos,
                        wpos,
                        column_grid: &grid,
                        column_grid_border: 0,
                        chunks: sim,
                        index: index_ref,
                        chunk: sim_chunk,
                        calendar: None,
                        overrides: None,
                    },
                    chunk: &mut chunk,
                    entity_spawns: Vec::new(),
                    rtsim_resource_blocks: Vec::new(),
                };
                canvas.blit_structure(origin.with_z(GROUND_TOP), structure, seed, units, false);
                for y in 0..32 {
                    for x in 0..32 {
                        let mut n = 0;
                        while !canvas
                            .get((wpos + Vec2::new(x, y)).with_z(GROUND_TOP - n))
                            .is_filled()
                        {
                            n += 1;
                        }
                        if n > 0 {
                            dug.insert((wpos.y + y, wpos.x + x), n);
                        }
                    }
                }
            }
        }
        dug
    }

    #[test]
    #[ignore]
    fn blitted_spots_dig_exactly_the_columns_of_their_footprint() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let origin = Vec2::new(16, 16);
        let mut checked = 0;
        let mut dug_total = 0;
        for spot in Spot::iter() {
            let Some(base) = spot_config(&spot).base_structures else {
                continue;
            };
            // Several placement seeds: they pick different rotations and, for
            // groups with more than one prefab, different prefabs.
            for i in 0..8 {
                let seed = crate::layer::spot::chunk_spot_placement(Vec2::new(i, 3)).1;
                let group = Structure::load_group(base);
                let group = group.read();
                let draw = spot_draw(seed, Some(&group));
                let structure = draw.structure.unwrap();
                let blitted =
                    blit_dug_columns(&world, index_ref, structure, origin, seed, draw.units);
                let fp = spot_footprint(origin, &spot, seed).unwrap();
                let predicted: BTreeMap<(i32, i32), i32> = fp
                    .columns
                    .iter()
                    .filter_map(|c| {
                        let n = c.dig_blocks(0);
                        (n > 0).then_some(((c.wpos.y, c.wpos.x), n))
                    })
                    .collect();
                assert_eq!(blitted, predicted, "{spot:?} seed {seed:#x}");
                checked += 1;
                dug_total += predicted.len();
            }
        }
        assert!(checked > 0);
        assert!(
            dug_total > 0,
            "no spot dug anything: the comparison proved nothing"
        );
    }
}
