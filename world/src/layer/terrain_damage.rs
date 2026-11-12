//! The Regional Terrain Event Engine's debris layer.
//!
//! For a chunk touched by an active [`DamageShape::Debris`] override, scatters
//! felled trees (blitting the existing `spots_grasslands.fallen_tree`
//! structure -- itself wrapping
//! `assets/world/structure/natural/fallen_tree.vox`, the same structure a
//! grassland "spot" already uses) and simple rubble blocks, both clearing in a
//! fixed, seeded order as the override heals (`heal_progress` toward `1.0`) --
//! no per-block stored state needed, since the SAME seed always makes the SAME
//! chance roll against a shrinking probability.
//!
//! Wired into `world/src/lib.rs::generate_chunk` right after
//! `apply_trees_to` and before `apply_scatter_to`.

use crate::{
    Canvas,
    util::{
        RandomField, Sampler, StructureGen2d, UnitChooser, gen_cache::StructureGenCache, seed_expan,
    },
};
use common::{
    assets::AssetHandle,
    terrain::{
        Block, BlockKind, DamageOverride, DamageShape, RegionalTerrainOverride, Structure,
        StructuresGroup, TerrainChunkSize, TerrainOverrides,
    },
    vol::RectVolSize,
};
use lazy_static::lazy_static;
use rand::{prelude::*, seq::IndexedRandom};
use rand_chacha::ChaChaRng;
use vek::*;

lazy_static! {
    static ref FALLEN_TREE: AssetHandle<StructuresGroup> =
        Structure::load_group("spots_grasslands.fallen_tree");
}

/// The highest-priority active `Damage` override whose region has any effect
/// (even a sliver of falloff) at `wpos`, if any. A small local lookup
/// (mirrors `TerrainOverrides::governing_damage_override`'s shape, but
/// doesn't need the fully radial-blended `DamageEffectsAt` this layer
/// doesn't use) -- this layer only needs the RAW override (its `shapes`,
/// `heal_progress`) to decide placement, not the column-level
/// depth/rim/scorch/vegetation blend `ColumnGen::get` computes.
fn governing_damage_at(
    overrides: &TerrainOverrides,
    wpos: Vec2<i32>,
) -> Option<(&RegionalTerrainOverride, &DamageOverride)> {
    overrides
        .active
        .iter()
        .filter(|o| o.region.blend_factor(wpos) > 0.0)
        .filter_map(|o| o.damage().map(|damage| (o, damage)))
        .max_by_key(|(o, _)| o.priority)
}

fn debris_shape(damage: &DamageOverride) -> Option<(f32, f32)> {
    damage.shapes.iter().find_map(|shape| match shape {
        DamageShape::Debris {
            rubble_density,
            felled_tree_chance,
        } => Some((*rubble_density, *felled_tree_chance)),
        _ => None,
    })
}

#[derive(Clone)]
struct FelledTree {
    wpos: Vec3<i32>,
    seed: u32,
    units: Vec2<Vec2<i32>>,
}

pub fn apply_terrain_damage_to(canvas: &mut Canvas, _dynamic_rng: &mut impl Rng) {
    let info = canvas.info();
    let Some(overrides) = info.overrides() else {
        return;
    };
    // Cheap prefilter: skip every bit of work below for the overwhelming
    // majority of chunks, which have no active `Debris` override touching
    // them at all.
    if !overrides.active.iter().any(|o| {
        o.damage()
            .is_some_and(|damage| debris_shape(damage).is_some())
    }) {
        return;
    }

    let chunk_center_wpos2d = info.wpos() + TerrainChunkSize::RECT_SIZE.map(|e| e as i32 / 2);

    // Felled trees: the same `StructureGenCache`-spaced candidate idiom
    // `world/src/layer/rock.rs`/`tree.rs` already use. Queried ONCE at this
    // chunk's center -- `StructureGenCache::get` memoizes by candidate
    // position, so this already returns every unique nearby candidate
    // (typically the surrounding 3x3 `StructureGen2d` cells) without
    // needing a per-column call like tree/rock placement does (those need
    // a per-column call to render each column's own slice of the
    // structure; this layer instead blits the whole structure at once via
    // `Canvas::blit_structure`, below).
    let mut tree_cache =
        StructureGenCache::new(StructureGen2d::new(info.index().seed ^ 0x07ED_11EE, 48, 20));
    let felled_trees: Vec<FelledTree> = tree_cache
        .get(chunk_center_wpos2d, |wpos, seed| {
            let (_, damage) = governing_damage_at(overrides, wpos)?;
            let (_, felled_tree_chance) = debris_shape(damage)?;
            let remaining = 1.0 - damage.heal_progress.clamp(0.0, 1.0);
            if !RandomField::new(seed).chance(wpos.with_z(0), felled_tree_chance * remaining) {
                return None;
            }
            let col = info.col_or_gen(wpos)?;
            if col.alt < col.water_level {
                // Don't fell trees into open water.
                return None;
            }
            Some(FelledTree {
                wpos: wpos.with_z(col.alt as i32),
                seed,
                units: UnitChooser::new(seed).get(seed).into(),
            })
        })
        .into_iter()
        .cloned()
        .collect();

    for tree in felled_trees {
        let structures = FALLEN_TREE.read();
        let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(tree.seed));
        let Some(structure) = structures.choose(&mut rng) else {
            continue;
        };
        canvas.blit_structure(tree.wpos, structure, tree.seed, tree.units, true);
    }

    // Rubble: a simple per-column scatter, no structure -- scaled by
    // `(1 - heal_progress)` exactly like the felled-tree chance above, so
    // rubble clears in the same fixed, stable order as healing progresses.
    canvas.foreach_col(|canvas, wpos2d, col| {
        let Some((_, damage)) = governing_damage_at(overrides, wpos2d) else {
            return;
        };
        let Some((rubble_density, _)) = debris_shape(damage) else {
            return;
        };
        let remaining = 1.0 - damage.heal_progress.clamp(0.0, 1.0);
        let wpos = wpos2d.with_z(col.alt as i32);
        if col.alt >= col.water_level
            && RandomField::new(info.index().seed ^ 0x5CADD1E5)
                .chance(wpos, rubble_density * remaining)
        {
            canvas.set(
                wpos,
                Block::new(
                    BlockKind::WeakRock,
                    col.stone_col.map(|c| c.saturating_sub(20)),
                ),
            );
        }
    });
}
