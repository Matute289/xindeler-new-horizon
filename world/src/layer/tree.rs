use crate::{
    Canvas, CanvasInfo, ColumnSample,
    all::*,
    block::block_from_structure,
    column::ColumnGen,
    layer::cave::tunnel_bounds_at,
    util::{RandomPerm, Sampler, UnitChooser, gen_cache::StructureGenCache},
};
use common::{
    assets::AssetHandle,
    calendar::{Calendar, CalendarEvent},
    terrain::{
        Block, BlockKind, SpriteCfg, SpriteKind,
        structure::{Structure, StructureBlock, StructuresGroup},
    },
    vol::ReadVol,
};
use lazy_static::lazy_static;
use rand::prelude::*;
use std::{f32, ops::Range};
use vek::*;

lazy_static! {
    static ref OAK_STUMPS: AssetHandle<StructuresGroup> = Structure::load_group("trees.oak_stumps");
    static ref PALMS: AssetHandle<StructuresGroup> = Structure::load_group("trees.palms");
    static ref FRUIT_TREES: AssetHandle<StructuresGroup> =
        Structure::load_group("trees.fruit_trees");
    static ref BIRCHES: AssetHandle<StructuresGroup> = Structure::load_group("trees.birch");
    static ref SWAMP_TREES: AssetHandle<StructuresGroup> =
        Structure::load_group("trees.swamp_trees");
}

static MODEL_RAND: RandomPerm = RandomPerm::new(0xDB21C052);
static UNIT_CHOOSER: UnitChooser = UnitChooser::new(0x700F4EC7);
static QUIRKY_RAND: RandomPerm = RandomPerm::new(0xA634460F);

// Ensure that it's valid to place a tree here
pub fn tree_valid_at(
    wpos: Vec2<i32>,
    col: &ColumnSample,
    info: Option<CanvasInfo<'_>>,
    seed: u32,
) -> bool {
    if col.alt < col.water_level
        // XINDELER: no tree rooted in (or, per region, near) authored water.
        || col.authored_procedural_suppressed()
        || col.spawn_rate < 0.9
        || col.water_dist.map(|d| d < 8.0).unwrap_or(false)
        || col.path.map(|(d, _, _, _)| d < 12.0).unwrap_or(false)
        // Keep clear of a near-surface procedural tunnel -- but only when that
        // layer actually runs. `Tunnel`s are hash-derived on demand, so in a
        // world that skips `apply_caves_to` this would carve treeless corridors
        // across the surface with no cave underneath them, crushing authored
        // `tree_density` for nothing.
        || info.is_some_and(|info| {
            info.index().features.caves
                && info.chunks().authored_procedural_caves_enabled()
                && tunnel_bounds_at(wpos, &info, &info.land())
                    .any(|(_, z_range, _, _, _, _)| z_range.contains(&(col.alt as i32 - 2)))
        })
    {
        return false;
    }

    if ((seed.wrapping_mul(13)) & 0xFF) as f32 / 256.0 > col.tree_density {
        return false;
    }

    true
}

pub fn apply_trees_to(
    canvas: &mut Canvas,
    dynamic_rng: &mut impl Rng,
    calendar: Option<&Calendar>,
) {
    // TODO: Get rid of this
    #[expect(clippy::large_enum_variant)]
    enum TreeModel {
        Structure(Structure),
        Procedural(ProceduralTree),
    }

    struct Tree {
        pos: Vec3<i32>,
        model: TreeModel,
        leaf_block: StructureBlock,
        seed: u32,
        units: (Vec2<i32>, Vec2<i32>),
        lights: bool,
    }

    let info = canvas.info();
    let mut tree_cache = StructureGenCache::new(info.chunks().gen_ctx.structure_gen.clone());

    let mut column_terrain = ColumnTerrain::default();

    canvas.foreach_col(|canvas, wpos2d, col| {
        if info
            .chunks()
            .has_additional_tree_candidate_fields_near(wpos2d)
        {
            tree_cache
                .use_fields_for_next_get(wpos2d, info.chunks().tree_candidate_fields_near(wpos2d));
        }
        let trees = tree_cache.get(wpos2d, |wpos, seed| {
            let scale = 1.0;
            let inhabited = false;

            // Reads active regional terrain overrides (see
            // `world/src/canvas.rs`'s `CanvasInfo::overrides` doc comment)
            // so a tree isn't placed at the un-cratered altitude -- floating
            // over a bowl a crater/debris override just carved into this
            // chunk.
            let column_gen = match info.overrides() {
                Some(overrides) => ColumnGen::with_overrides(info.chunks(), overrides),
                None => ColumnGen::new(info.chunks()),
            };
            let col = column_gen.get((wpos, info.index(), calendar))?;

            // A governing `BiomeProfile` override with a nonempty `forest`
            // list bypasses the normal proclivity-based lottery entirely and
            // picks from its own weighted list instead -- same
            // `Lottery`/`choose_seeded` mechanism and seed, just a different
            // candidate set. Otherwise, uses `info.chunk()` (the chunk-level,
            // once-per-`generate_chunk` climate-override-patched `SimChunk`
            // -- see `world/src/lib.rs::generate_chunk`) rather than
            // `info.chunks().make_forest_lottery(wpos)`, which would re-fetch
            // the RAW, un-overridden `SimChunk` at `wpos` and silently bypass
            // any active regional terrain override's effect on which trees
            // spawn here.
            let forest_kind = match col
                .governing_biome_profile
                .filter(|rp| !rp.profile.forest.is_empty())
            {
                Some(rp) => *common::lottery::Lottery::from(
                    rp.profile
                        .forest
                        .iter()
                        .map(|(fk, w)| (*w, Some(*fk)))
                        .collect::<Vec<_>>(),
                )
                .choose_seeded(seed)
                .as_ref()?,
                None => {
                    *crate::sim::make_forest_lottery_for_env(wpos, info.chunk().get_environment())
                        .choose_seeded(seed)
                        .as_ref()?
                },
            };

            let crowding = col.tree_density;

            if !tree_valid_at(wpos, &col, Some(info), seed) {
                return None;
            }

            Some(Tree {
                pos: Vec3::new(wpos.x, wpos.y, col.alt as i32),
                model: 'model: {
                    let models: AssetHandle<_> = match forest_kind {
                        ForestKind::Oak if QUIRKY_RAND.chance(seed + 1, 1.0 / 16.0) => *OAK_STUMPS,
                        ForestKind::Oak if QUIRKY_RAND.chance(seed + 2, 1.0 / 20.0) => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::apple(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Palm => *PALMS,
                        ForestKind::Acacia => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::acacia(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Baobab => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::baobab(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Oak => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::oak(&mut RandomPerm::new(seed), scale, crowding),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Dead => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::dead(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Chestnut => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::chestnut(&mut RandomPerm::new(seed), scale, crowding),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Pine => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::pine(&mut RandomPerm::new(seed), scale, calendar),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Cedar => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::cedar(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Redwood => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::redwood(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Birch => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::birch(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Frostpine => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::frostpine(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },

                        ForestKind::Mangrove => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::jungle(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Swamp => *SWAMP_TREES,
                        ForestKind::Giant => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::giant(&mut RandomPerm::new(seed), scale, inhabited),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Mapletree => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::oak(&mut RandomPerm::new(seed), scale, crowding),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::Cherry => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::cherry(&mut RandomPerm::new(seed), scale),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                        ForestKind::AutumnTree => {
                            break 'model TreeModel::Procedural(ProceduralTree::generate(
                                TreeConfig::oak(&mut RandomPerm::new(seed), scale, crowding),
                                &mut RandomPerm::new(seed),
                            ));
                        },
                    };

                    let models = models.read();
                    TreeModel::Structure(
                        models
                            [(MODEL_RAND.get(seed.wrapping_mul(17)) / 13) as usize % models.len()]
                        .clone(),
                    )
                },
                leaf_block: forest_kind.leaf_block(),
                seed,
                units: UNIT_CHOOSER.get(seed),
                lights: inhabited,
            })
        });

        let tree_bounds = |tree: &Tree| match &tree.model {
            TreeModel::Structure(s) => s.get_bounds(),
            TreeModel::Procedural(t) => t.get_bounds().map(|e| e as i32),
        };
        let column_in_tree = |tree: &Tree, bounds: Aabb<i32>| {
            let rpos2d = (wpos2d - tree.pos.xy())
                .map2(Vec2::new(tree.units.0, tree.units.1), |p, unit| unit * p)
                .sum();
            Aabr::from(bounds).contains_point(rpos2d)
        };

        // Record which blocks of this column are natural terrain *before any
        // tree touches it*, so that whether a voxel is buried does not depend
        // on the order of the trees or on what an earlier tree wrote.
        column_terrain.fill_union(
            trees.iter().filter_map(|tree| {
                let bounds = tree_bounds(tree);
                column_in_tree(tree, bounds)
                    .then(|| (tree.pos.z + bounds.min.z, tree.pos.z + bounds.max.z))
            }),
            |z| {
                canvas
                    .get(Vec3::new(wpos2d.x, wpos2d.y, z))
                    .kind()
                    .is_terrain()
            },
        );

        for tree in trees {
            let bounds = tree_bounds(tree);
            if !column_in_tree(tree, bounds) {
                // Skip this column
                continue;
            }

            let hanging_sprites = match &tree.model {
                TreeModel::Structure(_) => [(0.0004, SpriteKind::Beehive)].as_ref(),
                TreeModel::Procedural(t) => t.config.hanging_sprites,
            };

            write_tree_column(
                canvas,
                dynamic_rng,
                &column_terrain,
                wpos2d,
                tree.pos.z + bounds.min.z..tree.pos.z + bounds.max.z,
                tree.lights,
                col.snow_cover,
                hanging_sprites,
                |wpos| {
                    let model_pos = Vec3::from(
                        (wpos - tree.pos)
                            .xy()
                            .map2(Vec2::new(tree.units.0, tree.units.1), |rpos, unit| {
                                unit * rpos
                            })
                            .sum(),
                    ) + Vec3::unit_z() * (wpos.z - tree.pos.z);
                    let sblock;
                    let Some(block) = (match &tree.model {
                        TreeModel::Structure(s) => s.get(model_pos).ok(),
                        TreeModel::Procedural(t) => Some(
                            match t.is_branch_or_leaves_at(model_pos.map(|e| e as f32 + 0.5)) {
                                (_, _, true, _) => {
                                    sblock = StructureBlock::Filled(
                                        BlockKind::Wood,
                                        Rgb::new(150, 98, 41),
                                    );
                                    &sblock
                                },
                                (_, _, _, true) => &StructureBlock::None,
                                (true, _, _, _) => &t.config.trunk_block,
                                (_, true, _, _) => &tree.leaf_block,
                                _ => &StructureBlock::None,
                            },
                        ),
                    }) else {
                        return TreeVoxel::End;
                    };
                    match block_from_structure(
                        info.index(),
                        block,
                        wpos,
                        tree.pos.xy(),
                        tree.seed,
                        col,
                        || Block::air(SpriteKind::Empty),
                        calendar,
                        &Vec2::new(tree.units.0, tree.units.1),
                    ) {
                        Some((block, sprite_cfg, _entity_path)) => {
                            TreeVoxel::Fill(block, sprite_cfg)
                        },
                        None => TreeVoxel::Empty,
                    }
                },
            );
        }
    });
}

/// What a tree model holds at one voxel of a column.
enum TreeVoxel {
    /// The model ends here: stop writing this column.
    End,
    /// Nothing at this voxel.
    Empty,
    Fill(Block, Option<SpriteCfg>),
}

/// Writes one tree's voxels into the column at `xy`, top-down over the world
/// heights `zs`. `terrain` is the column as it was before any tree touched it.
fn write_tree_column(
    sink: &mut impl ColumnSink,
    dynamic_rng: &mut impl Rng,
    terrain: &ColumnTerrain,
    xy: Vec2<i32>,
    zs: Range<i32>,
    lights: bool,
    snow_cover: bool,
    hanging_sprites: &[(f32, SpriteKind)],
    mut voxel_at: impl FnMut(Vec3<i32>) -> TreeVoxel,
) {
    let mut writer = ColumnWriter::new();
    for z in zs.rev() {
        let wpos = Vec3::new(xy.x, xy.y, z);
        match voxel_at(wpos) {
            TreeVoxel::End => break,
            TreeVoxel::Empty => writer.gap(sink, dynamic_rng, wpos, hanging_sprites),
            TreeVoxel::Fill(block, sprite_cfg) => writer.voxel(
                sink,
                dynamic_rng,
                wpos,
                block,
                sprite_cfg,
                terrain.is_closed(z),
                lights,
                snow_cover,
            ),
        }
    }
}

/// The writes one tree column makes. [`Canvas`] implements it for the layer;
/// the tests use a plain vector.
trait ColumnSink {
    fn set(&mut self, wpos: Vec3<i32>, block: Block);
    fn set_sprite_cfg(&mut self, wpos: Vec3<i32>, sprite_cfg: SpriteCfg);
    fn add_sprite(&mut self, wpos: Vec3<i32>, sprite: SpriteKind);
}

impl ColumnSink for Canvas<'_> {
    fn set(&mut self, wpos: Vec3<i32>, block: Block) { Canvas::set(self, wpos, block) }

    fn set_sprite_cfg(&mut self, wpos: Vec3<i32>, sprite_cfg: SpriteCfg) {
        Canvas::set_sprite_cfg(self, wpos, sprite_cfg)
    }

    fn add_sprite(&mut self, wpos: Vec3<i32>, sprite: SpriteKind) {
        self.map_resource(wpos, |block| block.with_sprite(sprite));
    }
}

/// Which blocks of one world column were natural terrain
/// ([`BlockKind::is_terrain`]) before any tree touched it, as a bitset over
/// `z`. One value is kept for the whole layer and refilled per column, so it
/// allocates only until it has grown to the tallest tree seen.
#[derive(Default)]
struct ColumnTerrain {
    bottom: i32,
    bits: Vec<u64>,
}

impl ColumnTerrain {
    /// Records `is_terrain(z)` for `bottom..=top` and clears everything else.
    fn fill(&mut self, bottom: i32, top: i32, mut is_terrain: impl FnMut(i32) -> bool) {
        let len = (top - bottom + 1).max(0) as usize;
        self.bottom = bottom;
        self.bits.clear();
        self.bits.resize(len.div_ceil(64), 0);
        for i in 0..len {
            if is_terrain(bottom + i as i32) {
                self.bits[i / 64] |= 1 << (i % 64);
            }
        }
    }

    /// Records the terrain over the union of the trees' write spans
    /// ([`union_range`]); does nothing when no tree reaches the column.
    fn fill_union(
        &mut self,
        spans: impl IntoIterator<Item = (i32, i32)>,
        is_terrain: impl FnMut(i32) -> bool,
    ) {
        if let Some((bottom, top)) = union_range(spans) {
            self.fill(bottom, top, is_terrain);
        }
    }

    fn is_terrain(&self, z: i32) -> bool {
        let i = z.wrapping_sub(self.bottom);
        i >= 0
            && self
                .bits
                .get(i as usize / 64)
                .is_some_and(|word| word >> (i as usize % 64) & 1 == 1)
    }

    /// Whether the block at `z` was natural terrain and so was the block
    /// directly above it: it has no open face upward. See
    /// [`tree_voxel_is_buried`].
    fn is_closed(&self, z: i32) -> bool { self.is_terrain(z) && self.is_terrain(z + 1) }
}

/// The smallest `(bottom, top)` containing every `(lo, hi)` span, where a tree
/// writes `lo..hi` (exclusive) top-down. `top` is the span's own exclusive
/// end, deliberately kept as an inclusive bound of the result: the topmost
/// voxel written at `hi - 1` asks whether the block at `hi` above it was
/// terrain, so that block has to be in the snapshot too.
fn union_range(spans: impl IntoIterator<Item = (i32, i32)>) -> Option<(i32, i32)> {
    spans.into_iter().fold(None, |acc, (lo, hi)| {
        Some(acc.map_or((lo, hi), |(l, h)| (l.min(lo), h.max(hi))))
    })
}

/// The per-column state of one tree's top-down write, and the one place that
/// decides what a tree voxel does to the column.
struct ColumnWriter {
    is_top: bool,
    is_leaf_top: bool,
    last_block: Block,
    /// Whether `last_block` was dropped as buried rather than written.
    last_dropped: bool,
}

impl ColumnWriter {
    fn new() -> Self {
        Self {
            is_top: true,
            is_leaf_top: true,
            last_block: Block::empty(),
            last_dropped: false,
        }
    }

    /// A voxel the tree model fills with `block`. `buried` is
    /// [`ColumnTerrain::is_closed`] at `wpos`.
    fn voxel(
        &mut self,
        sink: &mut impl ColumnSink,
        dynamic_rng: &mut impl Rng,
        wpos: Vec3<i32>,
        block: Block,
        sprite_cfg: Option<SpriteCfg>,
        buried: bool,
        lights: bool,
        snow_cover: bool,
    ) {
        let lantern_spot = lights && self.last_block.is_air() && block.kind() == BlockKind::Wood;
        if tree_voxel_is_buried(buried, block) {
            // Nothing is written: no block, no snow cap, and no hanging
            // sprite under it. The dynamic RNG is drawn exactly as if it were
            // written, so the rest of the chunk (lanterns, hanging sprites,
            // later layers) stays where it was.
            if lantern_spot {
                let _ = dynamic_rng.random_range(0..256);
            }
            self.last_dropped = true;
        } else {
            // Add lights to the tree
            if lantern_spot && dynamic_rng.random_range(0..256) == 0 {
                sink.set(wpos + Vec3::unit_z(), Block::air(SpriteKind::Lantern));
                // Add a snow covering to the block above under certain
                // circumstances
            } else if snow_cover
                && ((block.kind() == BlockKind::Leaves && self.is_leaf_top)
                    || (self.is_top && block.is_filled()))
            {
                sink.set(
                    wpos + Vec3::unit_z(),
                    Block::new(BlockKind::Snow, Rgb::new(210, 210, 255)),
                );
            }
            sink.set(wpos, block);
            if let Some(sprite_cfg) = sprite_cfg {
                sink.set_sprite_cfg(wpos, sprite_cfg);
            }
            self.last_dropped = false;
        }
        self.is_leaf_top = false;
        self.is_top = false;
        self.last_block = block;
    }

    /// A voxel the tree model leaves empty: hanging sprites under a filled
    /// block above.
    fn gap(
        &mut self,
        sink: &mut impl ColumnSink,
        dynamic_rng: &mut impl Rng,
        wpos: Vec3<i32>,
        hanging_sprites: &[(f32, SpriteKind)],
    ) {
        if self.last_block.is_filled() {
            for (chance, sprite) in hanging_sprites {
                if dynamic_rng.random_bool(*chance as f64) && !self.last_dropped {
                    sink.add_sprite(wpos, *sprite);
                }
            }
        }

        self.is_leaf_top = true;
        self.last_block = Block::empty();
        self.last_dropped = false;
    }
}

/// Whether a tree voxel must be dropped because it would be buried in natural
/// terrain.
///
/// `buried` is [`ColumnTerrain::is_closed`]: the target block is natural
/// terrain ([`BlockKind::is_terrain`]) and so was the block directly above it,
/// both as they were before any tree touched the column (not what an earlier
/// tree wrote there, so the result does not depend on tree order). Such a
/// voxel has no open face upward: a crown, branch or trunk part reaching into
/// a neighbouring hillside, cliff or pit wall, or the buried part of a root.
/// It only costs memory and turns up as stray wood and leaves when a cave or a
/// dig later opens the rock. The surface voxel of that terrain (open above) is
/// still replaced, so a crown still rests on and pokes into the ground
/// surface, a trunk foot still replaces the ground block it stands on, and
/// roots still show where they break the surface.
///
/// A pure air output (a `Hollow` in a tree model) is never dropped, so any
/// dig a tree model asks for stays exactly as it was. Everything else (wood,
/// leaves, fruit or liana sprites) is dropped when buried.
///
/// The rule reads only the column being written and is the same for every
/// world, authored or procedural.
#[inline]
fn tree_voxel_is_buried(buried: bool, block: Block) -> bool {
    buried
        && (block.kind() != BlockKind::Air
            || block
                .get_sprite()
                .is_some_and(|sprite| sprite != SpriteKind::Empty))
}

/// A type that specifies the generation properties of a tree.
#[derive(Clone)]
pub struct TreeConfig {
    /// Length of trunk, also scales other branches.
    pub trunk_len: f32,
    /// Radius of trunk, also scales other branches.
    pub trunk_radius: f32,
    /// The scale that child branch lengths should be compared to their parents.
    pub branch_child_len: f32,
    /// The scale that child branch radii should be compared to their parents.
    pub branch_child_radius: f32,
    /// Whether the child of a branch has its radius lerped to its parent.
    pub branch_child_radius_lerp: bool,
    /// The range of radii that leaf-emitting branches might have.
    pub leaf_radius: Range<f32>,
    /// An additional leaf radius that may be scaled with proportion along the
    /// parent and `branch_len_bias`.
    pub leaf_radius_scaled: f32,
    /// 0 - 1 (0 = chaotic, 1 = straight).
    pub straightness: f32,
    /// Maximum number of branch layers (not including trunk).
    pub max_depth: usize,
    /// The number of branches that form from each branch.
    pub splits: Range<f32>,
    /// The range of proportions along a branch at which a split into another
    /// branch might occur. This value is clamped between 0 and 1, but a
    /// wider range may bias the results towards branch ends.
    pub split_range: Range<f32>,
    /// The bias applied to the length of branches based on the proportion along
    /// their parent that they eminate from. -1.0 = negative bias (branches
    /// at ends are longer, branches at the start are shorter) 0.0 = no bias
    /// (branches do not change their length with regard to parent branch
    /// proportion) 1.0 = positive bias (branches at ends are shorter,
    /// branches at the start are longer)
    pub branch_len_bias: f32,
    /// The scale of leaves in the vertical plane. Less than 1.0 implies a
    /// flattening of the leaves.
    pub leaf_vertical_scale: f32,
    /// How evenly spaced (vs random) sub-branches are along their parent.
    pub proportionality: f32,
    /// Whether the tree is inhabited (adds various features and effects)
    pub inhabited: bool,
    pub hanging_sprites: &'static [(f32, SpriteKind)],
    /// The colour of branches and the trunk.
    pub trunk_block: StructureBlock,
}

impl TreeConfig {
    pub fn oak(rng: &mut impl Rng, scale: f32, crowding: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 11.0 * scale,
            trunk_radius: 1.5 * scale,
            branch_child_len: 0.75,
            branch_child_radius: 0.75,
            branch_child_radius_lerp: true,
            leaf_radius: 2.0 * log_scale..2.5 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.3 + crowding * 0.3,
            max_depth: 4,
            splits: 4.25..6.25,
            split_range: 0.75..1.5,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 1.0,
            proportionality: 0.0,
            inhabited: false,
            hanging_sprites: &[(0.0002, SpriteKind::Apple), (0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(90, 45, 15)),
        }
    }

    pub fn dead(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);

        Self {
            trunk_len: 9.0 * scale,
            trunk_radius: 2.0 * scale,
            branch_child_len: 0.9,
            branch_child_radius: 0.7,
            branch_child_radius_lerp: true,
            leaf_radius: 0.0..0.1,
            leaf_radius_scaled: 0.0,
            straightness: 0.35,
            max_depth: 3,
            splits: 2.25..3.25,
            split_range: 0.75..1.5,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 1.0,
            proportionality: 0.0,
            inhabited: false,
            hanging_sprites: &[(0.0002, SpriteKind::Apple), (0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(55, 34, 32)),
        }
    }

    pub fn apple(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 3.0 * scale,
            trunk_radius: 1.5 * scale,
            branch_child_len: 0.9,
            branch_child_radius: 0.9,
            branch_child_radius_lerp: true,
            leaf_radius: 2.0 * log_scale..3.0 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.4,
            max_depth: 6,
            splits: 1.0..3.0,
            split_range: 0.5..2.0,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.7,
            proportionality: 0.0,
            inhabited: false,
            hanging_sprites: &[(0.03, SpriteKind::Apple), (0.007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(90, 45, 15)),
        }
    }

    pub fn frostpine(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 36.0 * scale,
            trunk_radius: 2.3 * scale,
            branch_child_len: 0.25 / scale,
            branch_child_radius: 0.0,
            branch_child_radius_lerp: false,
            leaf_radius: 1.3..2.2,
            leaf_radius_scaled: 0.4 * log_scale,
            straightness: 0.3,
            max_depth: 1,
            splits: 34.0 * scale..35.0 * scale,
            split_range: 0.1..1.2,
            branch_len_bias: 0.75,
            leaf_vertical_scale: 0.6,
            proportionality: 1.0,
            inhabited: false,
            hanging_sprites: &[(0.0001, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(79, 102, 105)),
        }
    }

    pub fn jungle(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>() * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 44.0 * scale,
            trunk_radius: 2.25 * scale,
            branch_child_len: 0.35,
            branch_child_radius: 0.5,
            branch_child_radius_lerp: true,
            leaf_radius: 10.0 * log_scale..11.5 * log_scale,
            leaf_radius_scaled: -8.0 * log_scale,
            straightness: 0.2,
            max_depth: 2,
            splits: 7.5..8.5,
            split_range: 0.2..1.25,
            branch_len_bias: 0.5,
            leaf_vertical_scale: 0.35,
            proportionality: 0.8,
            inhabited: false,
            hanging_sprites: &[(0.00007, SpriteKind::Beehive), (0.015, SpriteKind::Liana)],

            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(69, 62, 43)),
        }
    }

    pub fn baobab(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.5 + rng.random::<f32>().powi(4) * 1.0);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 24.0 * scale,
            trunk_radius: 7.0 * scale,
            branch_child_len: 0.55,
            branch_child_radius: 0.3,
            branch_child_radius_lerp: true,
            leaf_radius: 2.5 * log_scale..3.0 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.5,
            max_depth: 4,
            splits: 3.0..3.5,
            split_range: 0.95..1.0,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.2,
            proportionality: 1.0,
            inhabited: false,
            hanging_sprites: &[(0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(125, 60, 6)),
        }
    }

    pub fn cedar(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 45.0 * scale,
            trunk_radius: 1.3 * scale,
            branch_child_len: 0.4,
            branch_child_radius: 0.6,
            branch_child_radius_lerp: true,
            leaf_radius: 2.0 * log_scale..2.5 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.3,
            max_depth: 2,
            splits: 16.0..18.0,
            split_range: 0.2..1.2,
            branch_len_bias: 0.7,
            leaf_vertical_scale: 0.3,
            proportionality: 0.7,
            inhabited: false,
            hanging_sprites: &[(0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(110, 68, 65)),
        }
    }

    pub fn birch(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 24.0 * scale,
            trunk_radius: 1.2 * scale,
            branch_child_len: 0.4,
            branch_child_radius: 0.75,
            branch_child_radius_lerp: true,
            leaf_radius: 4.0 * log_scale..5.0 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.6,
            max_depth: 4,
            splits: 1.75..2.5,
            split_range: 0.6..1.2,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.5,
            proportionality: 0.0,
            inhabited: false,
            hanging_sprites: &[(0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::BirchWood,
        }
    }

    pub fn acacia(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.9 + rng.random::<f32>().powi(4) * 0.75);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 7.5 * scale,
            trunk_radius: 1.5 * scale,
            branch_child_len: 0.75,
            branch_child_radius: 0.75,
            branch_child_radius_lerp: true,
            leaf_radius: 4.5 * log_scale..5.5 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.4,
            max_depth: 5,
            splits: 1.75..2.25,
            split_range: 1.0..1.25,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.2,
            proportionality: 1.0,
            inhabited: false,
            hanging_sprites: &[(0.00005, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(150, 95, 65)),
        }
    }

    pub fn chestnut(rng: &mut impl Rng, scale: f32, crowding: f32) -> Self {
        let scale = scale * (0.85 + rng.random::<f32>().powi(4) * 0.3);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 13.0 * scale,
            trunk_radius: 1.65 * scale,
            branch_child_len: 0.75,
            branch_child_radius: 0.6,
            branch_child_radius_lerp: true,
            leaf_radius: 1.5 * log_scale..2.0 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.25 + crowding * 0.2,
            max_depth: 5,
            splits: 3.5..4.25,
            split_range: 0.5..1.25,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.65,
            proportionality: 0.5,
            inhabited: false,
            hanging_sprites: &[(0.00007, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(110, 42, 28)),
        }
    }

    pub fn pine(rng: &mut impl Rng, scale: f32, calendar: Option<&Calendar>) -> Self {
        let scale = scale * (1.0 + rng.random::<f32>().powi(4) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 32.0 * scale,
            trunk_radius: 1.25 * scale,
            branch_child_len: 0.3 / scale,
            branch_child_radius: 0.0,
            branch_child_radius_lerp: false,
            leaf_radius: 1.9..2.1,
            leaf_radius_scaled: 1.5 * log_scale,
            straightness: -0.25,
            max_depth: 1,
            splits: 34.0 * scale..35.0 * scale,
            split_range: 0.2..1.2,
            branch_len_bias: 0.75,
            leaf_vertical_scale: 0.3,
            proportionality: 1.0,
            inhabited: false,
            hanging_sprites: if calendar.is_some_and(|c| c.is_event(CalendarEvent::Christmas)) {
                &[(0.0001, SpriteKind::Beehive), (0.01, SpriteKind::Orb)]
            } else {
                &[(0.0001, SpriteKind::Beehive)]
            },
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(90, 35, 15)),
        }
    }

    pub fn redwood(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (1.0 + rng.random::<f32>().powi(4) * 0.5);

        Self {
            trunk_len: 80.0 * scale,
            trunk_radius: 2.75 * scale,
            branch_child_len: 0.25,
            branch_child_radius: 0.3,
            branch_child_radius_lerp: false,
            leaf_radius: 1.3..1.5,
            leaf_radius_scaled: 0.0,
            straightness: -0.3,
            max_depth: 2,
            splits: 45.0 * scale..50.0 * scale,
            split_range: 0.4..1.2,
            branch_len_bias: 0.75,
            leaf_vertical_scale: 0.6,
            proportionality: 1.0,
            inhabited: false,
            hanging_sprites: &[(0.001, SpriteKind::Beehive)],
            trunk_block: StructureBlock::RedwoodWood,
        }
    }

    pub fn giant(_rng: &mut impl Rng, scale: f32, inhabited: bool) -> Self {
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 11.0 * scale,
            trunk_radius: 6.0 * scale,
            branch_child_len: 0.9,
            branch_child_radius: 0.75,
            branch_child_radius_lerp: true,
            leaf_radius: 2.5 * scale..3.75 * scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.36,
            max_depth: (7.0 + log_scale) as usize,
            splits: 1.5..2.5,
            split_range: 1.0..1.1,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 0.6,
            proportionality: 0.0,
            inhabited,
            hanging_sprites: &[(0.00025, SpriteKind::Apple), (0.00025, SpriteKind::Beehive)],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(110, 68, 22)),
        }
    }

    pub fn cherry(rng: &mut impl Rng, scale: f32) -> Self {
        let scale = scale * (0.8 + rng.random::<f32>().powi(2) * 0.5);
        let log_scale = 1.0 + scale.log2().max(0.0);

        Self {
            trunk_len: 7.0 * scale,
            trunk_radius: 1.27 * scale,
            branch_child_len: 0.9,
            branch_child_radius: 0.70,
            branch_child_radius_lerp: true,
            leaf_radius: 2.5 * log_scale..3.0 * log_scale,
            leaf_radius_scaled: 0.0,
            straightness: 0.55,
            max_depth: 4,
            splits: 2.0..3.0,
            split_range: 0.75..1.3,
            branch_len_bias: 0.0,
            leaf_vertical_scale: 1.0,
            proportionality: 0.0,
            inhabited: false,
            hanging_sprites: &[],
            trunk_block: StructureBlock::Filled(BlockKind::Wood, Rgb::new(69, 37, 17)),
        }
    }
}

// TODO: Rename this to `Tree` when the name conflict is gone
pub struct ProceduralTree {
    branches: Vec<Branch>,
    trunk_idx: usize,
    config: TreeConfig,
    roots: Vec<Root>,
    root_aabb: Aabb<f32>,
}

impl ProceduralTree {
    /// Generate a new tree using the given configuration and seed.
    pub fn generate(config: TreeConfig, rng: &mut impl Rng) -> Self {
        let mut this = Self {
            branches: Vec::new(),
            trunk_idx: 0, // Gets replaced later
            config: config.clone(),
            roots: Vec::new(),
            root_aabb: Aabb::new_empty(Vec3::zero()),
        };

        // Make the roots visible a little
        let trunk_origin = Vec3::unit_z() * (config.trunk_radius * 0.25 + 3.0);

        // Add the tree trunk (and sub-branches) recursively
        let (trunk_idx, _) = this.add_branch(
            &config,
            // Our trunk starts at the origin...
            trunk_origin,
            // ...and has a roughly upward direction
            Vec3::new(
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
                10.0,
            )
            .normalized(),
            config.trunk_len,
            config.trunk_radius,
            0,
            None,
            1.0,
            rng,
        );
        this.trunk_idx = trunk_idx;

        // Add roots
        let mut root_aabb = Aabb::new_empty(Vec3::zero());
        for _ in 0..4 {
            let dir = Vec3::new(
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
                -1.0,
            )
            .normalized();
            let len = config.trunk_len * 0.75;
            let radius = config.trunk_radius;
            let mut aabb = Aabb {
                min: trunk_origin,
                max: trunk_origin + dir * len,
            }
            .made_valid();
            aabb.min -= radius;
            aabb.max += radius;

            root_aabb.expand_to_contain(aabb);

            this.roots.push(Root {
                line: LineSegment3 {
                    start: trunk_origin,
                    end: trunk_origin + dir * 10.0,
                },
                radius,
            });
        }

        this.root_aabb = root_aabb;

        this
    }

    // Recursively add a branch (with sub-branches) to the tree's branch graph,
    // returning the index and AABB of the branch. This AABB gets propagated
    // down to the parent and is used later during sampling to cull the branches to
    // be sampled.
    fn add_branch(
        &mut self,
        config: &TreeConfig,
        start: Vec3<f32>,
        dir: Vec3<f32>,
        branch_len: f32,
        branch_radius: f32,
        depth: usize,
        sibling_idx: Option<usize>,
        proportion: f32,
        rng: &mut impl Rng,
    ) -> (usize, Aabb<f32>) {
        let end = start + dir * branch_len;
        let line = LineSegment3 { start, end };
        let wood_radius = branch_radius;
        let leaf_radius = if depth == config.max_depth {
            rng.random_range(config.leaf_radius.clone())
                + config.leaf_radius_scaled
                    * Lerp::lerp(1.0, 1.0 - proportion, config.branch_len_bias.abs())
        } else {
            0.0
        };

        let has_stairs = config.inhabited
            && depth < config.max_depth
            && branch_radius > 6.5
            && start.xy().distance(end.xy()) < (start.z - end.z).abs() * 1.5;
        let bark_radius = if has_stairs { 5.0 } else { 0.0 } + wood_radius * 0.25;

        // The AABB that covers this branch, along with wood and leaves that eminate
        // from it
        let mut aabb = Aabb {
            min: Vec3::partial_min(start, end) - (wood_radius + bark_radius).max(leaf_radius),
            max: Vec3::partial_max(start, end) + (wood_radius + bark_radius).max(leaf_radius),
        };

        let mut child_idx = None;
        // Don't add child branches if we're already enough layers into the tree
        if depth < config.max_depth {
            let x_axis = dir
                .cross(Vec3::<f32>::zero().map(|_| rng.random_range(-1.0..1.0)))
                .normalized();
            let y_axis = dir.cross(x_axis).normalized();
            let screw_shift = rng.random_range(0.0..f32::consts::TAU);

            let splits = rng.random_range(config.splits.clone()).round() as usize;
            for i in 0..splits {
                let proportion = i as f32 / (splits - 1) as f32;
                let dist = Lerp::lerp(
                    rng.random_range(0.0..1.0),
                    proportion,
                    config.proportionality,
                );

                const PHI: f32 = 0.618;
                const RAD_PER_BRANCH: f32 = f32::consts::TAU * PHI;
                let screw = (screw_shift + i as f32 * RAD_PER_BRANCH).sin() * x_axis
                    + (screw_shift + i as f32 * RAD_PER_BRANCH).cos() * y_axis;

                // Choose a point close to the branch to act as the target direction for the
                // branch to grow in let split_factor =
                // rng.random_range(config.split_range.start,
                // config.split_range.end).clamped(0.0, 1.0);
                let split_factor =
                    Lerp::lerp(config.split_range.start, config.split_range.end, dist);
                let tgt = Lerp::lerp_unclamped(start, end, split_factor)
                    + Lerp::lerp(
                        Vec3::<f32>::zero().map(|_| rng.random_range(-1.0..1.0)),
                        screw,
                        config.proportionality,
                    );
                // Start the branch at the closest point to the target
                let branch_start = line.projected_point(tgt);
                // Now, interpolate between the target direction and the parent branch's
                // direction to find a direction
                let branch_dir =
                    Lerp::lerp_unclamped(tgt - branch_start, dir, config.straightness).normalized();

                let (branch_idx, branch_aabb) = self.add_branch(
                    config,
                    branch_start,
                    branch_dir,
                    branch_len
                        * config.branch_child_len
                        * (1.0
                            - (split_factor - 0.5)
                                * 2.0
                                * config.branch_len_bias.clamped(-1.0, 1.0)),
                    branch_radius * config.branch_child_radius,
                    depth + 1,
                    child_idx,
                    proportion,
                    rng,
                );
                child_idx = Some(branch_idx);
                // Parent branches AABBs include the AABBs of child branches to allow for
                // culling during sampling
                aabb.expand_to_contain(branch_aabb);
            }
        }

        let idx = self.branches.len(); // Compute the index that this branch is going to have
        self.branches.push(Branch {
            line,
            wood_radius,
            leaf_radius,
            leaf_vertical_scale: config.leaf_vertical_scale,
            aabb,
            sibling_idx,
            child_idx,
            has_stairs,
        });

        (idx, aabb)
    }

    /// Get the bounding box that covers the tree (all branches and leaves)
    pub fn get_bounds(&self) -> Aabb<f32> {
        self.branches[self.trunk_idx].aabb.union(self.root_aabb)
    }

    // Recursively search for branches or leaves by walking the tree's branch graph.
    fn walk_inner(
        &self,
        descend: &mut impl FnMut(&Branch, &Branch) -> bool,
        parent: &Branch,
        branch_idx: usize,
    ) {
        let branch = &self.branches[branch_idx];
        // Always probe the sibling branch, since it's not a child of the current
        // branch.
        let _branch_or_leaves = branch
            .sibling_idx
            .map(|idx| self.walk_inner(descend, parent, idx));

        // Only continue probing this sub-graph of the tree if the branch maches a
        // criteria (usually that it falls within the region we care about
        // sampling)
        if descend(branch, parent) {
            // Probe the children of this branch
            let _children = branch
                .child_idx
                .map(|idx| self.walk_inner(descend, branch, idx));
        }
    }

    /// Recursively walk the tree's branches, calling the current closure with
    /// the branch and its parent. If the closure returns `false`, recursion
    /// into the child branches is skipped.
    pub fn walk<F: FnMut(&Branch, &Branch) -> bool>(&self, mut f: F) {
        self.walk_inner(&mut f, &self.branches[self.trunk_idx], self.trunk_idx);
    }

    /// Determine whether there are either branches or leaves at the given
    /// position in the tree.
    #[inline(always)]
    pub fn is_branch_or_leaves_at(&self, pos: Vec3<f32>) -> (bool, bool, bool, bool) {
        let mut flags = Vec4::broadcast(false);
        self.walk(|branch, parent| {
            if branch.aabb.contains_point(pos) {
                flags |=
                    Vec4::<bool>::from(branch.is_branch_or_leaves_at(&self.config, pos, parent).0);
                true
            } else {
                false
            }
        });

        let (log, leaf, platform, air) = flags.into_tuple();

        let root = if self.root_aabb.contains_point(pos) {
            self.roots.iter().any(|root| {
                let p = root.line.projected_point(pos);
                let d2 = p.distance_squared(pos);
                d2 < root.radius.powi(2)
            })
        } else {
            false
        };
        (
            (log || root), /* & !air */
            leaf & !air,
            platform & !air,
            air,
        )
    }
}

// Branches are arranged in a graph shape. Each branch points to both its first
// child (if any) and also to the next branch in the list of child branches
// associated with the parent. This means that the entire tree is laid out in a
// walkable graph where each branch refers only to two other branches. As a
// result, walking the tree is simply a case of performing double recursion.
pub struct Branch {
    line: LineSegment3<f32>,
    wood_radius: f32,
    leaf_radius: f32,
    leaf_vertical_scale: f32,
    aabb: Aabb<f32>,

    sibling_idx: Option<usize>,
    child_idx: Option<usize>,

    has_stairs: bool,
}

impl Branch {
    /// Determine whether there are either branches or leaves at the given
    /// position in the branch.
    /// (branch, leaves, stairs, forced_air)
    pub fn is_branch_or_leaves_at(
        &self,
        config: &TreeConfig,
        pos: Vec3<f32>,
        parent: &Branch,
    ) -> ((bool, bool, bool, bool), f32) {
        // fn finvsqrt(x: f32) -> f32 {
        //     let y = f32::from_bits(0x5f375a86 - (x.to_bits() >> 1));
        //     y * (1.5 - ( x * 0.5 * y * y ))
        // }

        fn length_factor(line: LineSegment3<f32>, p: Vec3<f32>) -> f32 {
            let len_sq = line.start.distance_squared(line.end);
            if len_sq < 0.001 {
                0.0
            } else {
                (p - line.start).dot(line.end - line.start) / len_sq
            }
        }

        // fn smooth(a: f32, b: f32, k: f32) -> f32 {
        //     // let h = (0.5 + 0.5 * (b - a) / k).clamped(0.0, 1.0);
        //     // Lerp::lerp(b, a, h) - k * h * (1.0 - h)

        //     let h = (k-(a-b).abs()).max(0.0);
        //     a.min(b) - h * h * 0.25 / k
        // }

        let p = self.line.projected_point(pos);
        let d2 = p.distance_squared(pos);

        let length_factor = length_factor(self.line, pos);
        let wood_radius = if config.branch_child_radius_lerp {
            Lerp::lerp(parent.wood_radius, self.wood_radius, length_factor)
        } else {
            self.wood_radius
        };

        let mask = if d2 < wood_radius.powi(2) {
            (true, false, false, false) // Wood
        } else if {
            let diff = (p - pos) / Vec3::new(1.0, 1.0, self.leaf_vertical_scale);
            diff.magnitude_squared() < self.leaf_radius.powi(2)
        } {
            (false, true, false, false) // Leaves
        } else {
            let stair_width = 5.0;
            let stair_thickness = 2.0;
            let stair_space = 5.0;
            if self.has_stairs {
                let (platform, air) = if pos.z >= self.line.start.z.min(self.line.end.z) - 1.0
                    && pos.z
                        <= self.line.start.z.max(self.line.end.z) + stair_thickness + stair_space
                    && d2 < (wood_radius + stair_width).powi(2)
                {
                    let rpos = pos.xy() - p;
                    let stretch = 32.0;
                    let stair_section = (rpos.x.atan2(rpos.y) / (f32::consts::PI * 2.0) * stretch
                        + pos.z)
                        .rem_euclid(stretch);
                    (
                        stair_section < stair_thickness,
                        stair_section >= stair_thickness
                            && stair_section < stair_thickness + stair_space,
                    ) // Stairs
                } else {
                    (false, false)
                };

                let platform = platform
                    || (self.has_stairs
                        && self.wood_radius > 4.0
                        && !air
                        && d2 < (wood_radius + 10.0).powi(2)
                        && pos.z % 48.0 < stair_thickness);

                (false, false, platform, air)
            } else {
                (false, false, false, false)
            }
        };

        (mask, d2)
    }

    /// This returns an AABB of both the branch and all of the children of that
    /// branch
    pub fn get_aabb(&self) -> Aabb<f32> { self.aabb }

    pub fn get_line(&self) -> LineSegment3<f32> { self.line }

    pub fn get_wood_radius(&self) -> f32 { self.wood_radius }

    pub fn get_leaf_radius(&self) -> f32 { self.leaf_radius }
}

struct Root {
    line: LineSegment3<f32>,
    radius: f32,
}

#[cfg(test)]
mod buried_voxel_tests {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};

    fn air() -> Block { Block::air(SpriteKind::Empty) }
    fn rock() -> Block { Block::new(BlockKind::Rock, Rgb::new(100, 100, 100)) }
    fn grass() -> Block { Block::new(BlockKind::Grass, Rgb::new(40, 160, 40)) }
    fn leaves() -> Block { Block::new(BlockKind::Leaves, Rgb::new(30, 120, 30)) }
    fn wood() -> Block { Block::new(BlockKind::Wood, Rgb::new(60, 30, 0)) }

    /// A column of blocks, listed top-down: index `i` is height `len - 1 - i`.
    /// Index 0 is the block just above anything a tree writes.
    struct Column(Vec<Block>);

    impl Column {
        fn z(&self, i: usize) -> i32 { (self.0.len() - 1 - i) as i32 }

        fn index(&self, wpos: Vec3<i32>) -> Option<usize> {
            let i = self.0.len() as i32 - 1 - wpos.z;
            (0..self.0.len() as i32).contains(&i).then_some(i as usize)
        }
    }

    impl ColumnSink for Column {
        fn set(&mut self, wpos: Vec3<i32>, block: Block) {
            if let Some(i) = self.index(wpos) {
                self.0[i] = block;
            }
        }

        fn set_sprite_cfg(&mut self, _: Vec3<i32>, _: SpriteCfg) {}

        fn add_sprite(&mut self, wpos: Vec3<i32>, sprite: SpriteKind) {
            if let Some(i) = self.index(wpos) {
                self.0[i] = self.0[i].with_sprite(sprite);
            }
        }
    }

    /// One tree in the column: its voxels top-down from index `first`
    /// (`None` = the model leaves the voxel empty).
    struct TreeSpec {
        first: usize,
        voxels: Vec<Option<Block>>,
        lights: bool,
        hanging: &'static [(f32, SpriteKind)],
        /// The voxel index (into `voxels`) at which the model ends the column.
        end_at: Option<usize>,
    }

    impl TreeSpec {
        fn new(first: usize, voxels: Vec<Option<Block>>) -> Self {
            Self {
                first,
                voxels,
                lights: false,
                hanging: &[],
                end_at: None,
            }
        }
    }

    /// Runs the layer's per-column logic (a snapshot over the union of the
    /// trees' spans, then every tree in order through `write_tree_column`) on
    /// a plain column.
    fn run(before: &[Block], trees: &[TreeSpec], rng: &mut StdRng) -> Vec<Block> {
        run_with_snow(before, trees, rng, false)
    }

    fn span(column: &Column, tree: &TreeSpec) -> (i32, i32) {
        let hi = column.z(tree.first) + 1;
        (hi - tree.voxels.len() as i32, hi)
    }

    fn run_with_snow(
        before: &[Block],
        trees: &[TreeSpec],
        rng: &mut StdRng,
        snow_cover: bool,
    ) -> Vec<Block> {
        let mut column = Column(before.to_vec());
        let mut terrain = ColumnTerrain::default();
        terrain.fill_union(trees.iter().map(|tree| span(&column, tree)), |z| {
            column
                .index(Vec3::new(0, 0, z))
                .is_some_and(|i| before[i].kind().is_terrain())
        });
        for tree in trees {
            let (lo, hi) = span(&column, tree);
            write_tree_column(
                &mut column,
                rng,
                &terrain,
                Vec2::zero(),
                lo..hi,
                tree.lights,
                snow_cover,
                tree.hanging,
                |wpos| {
                    let k = (hi - 1 - wpos.z) as usize;
                    if tree.end_at == Some(k) {
                        return TreeVoxel::End;
                    }
                    match tree.voxels[k] {
                        Some(block) => TreeVoxel::Fill(block, None),
                        None => TreeVoxel::Empty,
                    }
                },
            );
        }
        column.0
    }

    fn run_plain(before: &[Block], trees: &[TreeSpec]) -> Vec<Block> {
        run(before, trees, &mut StdRng::seed_from_u64(1))
    }

    #[test]
    fn a_crown_voxel_buried_in_rock_is_not_written() {
        // A pit wall column: solid rock from far above down past the crown.
        let before = vec![rock(); 5];
        let after = run_plain(&before, &[TreeSpec::new(1, vec![Some(leaves()); 4])]);
        assert_eq!(after, before);
        // Wood and a liana sprite are buried just the same.
        let after = run_plain(&before, &[TreeSpec::new(1, vec![Some(wood()); 4])]);
        assert_eq!(after, before);
        assert!(tree_voxel_is_buried(
            true,
            Block::air(SpriteKind::Empty).with_sprite(SpriteKind::Liana)
        ));
    }

    #[test]
    fn a_crown_voxel_in_air_or_water_is_still_written() {
        let after = run_plain(&[air(); 4], &[TreeSpec::new(1, vec![Some(leaves()); 3])]);
        assert_eq!(after, [air(), leaves(), leaves(), leaves()]);
        // Air under a rock overhang is open space: written.
        let after = run_plain(&[rock(), air(), air()], &[TreeSpec::new(1, vec![
            Some(
                leaves()
            );
            2
        ])]);
        assert_eq!(after, [rock(), leaves(), leaves()]);
        // Water is not natural terrain, so it is never "closed".
        let water = Block::water(SpriteKind::Empty);
        let after = run_plain(&[rock(), water, water, rock()], &[TreeSpec::new(
            1,
            vec![Some(leaves()); 2],
        )]);
        assert_eq!(after, [rock(), leaves(), leaves(), rock()]);
    }

    #[test]
    fn the_open_ground_surface_is_replaced_but_not_the_rock_under_it() {
        // A hillside column: air above, grass surface, rock below.
        let before = [air(), air(), grass(), rock(), rock()];
        let after = run_plain(&before, &[TreeSpec::new(1, vec![Some(leaves()); 4])]);
        assert_eq!(after, [air(), leaves(), leaves(), rock(), rock()]);
    }

    #[test]
    fn the_trunk_foot_is_written_and_buried_roots_are_not() {
        let before = [air(), grass(), rock(), rock()];
        let after = run_plain(&before, &[TreeSpec::new(1, vec![Some(wood()); 3])]);
        assert_eq!(after, [air(), wood(), rock(), rock()]);
        // A root reaching into an air pocket is open space: written, and so
        // is the pocket's floor block, but not the rock under that floor.
        let before = [rock(), air(), rock(), rock()];
        let after = run_plain(&before, &[TreeSpec::new(1, vec![Some(wood()); 3])]);
        assert_eq!(after, [rock(), wood(), wood(), rock()]);
    }

    #[test]
    fn a_hollow_in_a_tree_model_still_digs() {
        // A pure air output is a dig and is never dropped, buried or not.
        let after = run_plain(&[rock(), rock()], &[TreeSpec::new(1, vec![Some(air())])]);
        assert_eq!(after, [rock(), air()]);
    }

    #[test]
    fn overlapping_trees_give_the_same_column_in_either_order() {
        // A hillside: air, grass surface, rock below. One tree puts wood on
        // the open surface voxel; the other only reaches into the rock under
        // it. Neither may write into the rock, whichever runs first (an
        // earlier tree's wood over the surface must not "open" the rock for
        // the later one).
        let before = [air(), air(), grass(), rock(), rock(), rock()];
        let surface = TreeSpec::new(2, vec![Some(wood())]);
        let under = TreeSpec::new(3, vec![Some(leaves()); 3]);
        let a = run_plain(&before, &[surface, under]);
        let surface = TreeSpec::new(2, vec![Some(wood())]);
        let under = TreeSpec::new(3, vec![Some(leaves()); 3]);
        let b = run_plain(&before, &[under, surface]);
        assert_eq!(a, [air(), air(), wood(), rock(), rock(), rock()]);
        assert_eq!(a, b);
    }

    #[test]
    fn the_snapshot_covers_exactly_its_range() {
        let mut t = ColumnTerrain::default();
        t.fill(-3, 70, |z| z % 2 == 0);
        for z in -3..=70 {
            assert_eq!(t.is_terrain(z), z % 2 == 0, "z {z}");
        }
        assert!(!t.is_terrain(-4) && !t.is_terrain(71) && !t.is_terrain(1000));
        // A refill forgets the previous column.
        t.fill(10, 12, |z| z == 11);
        assert!(!t.is_terrain(-2) && !t.is_terrain(10) && t.is_terrain(11) && !t.is_terrain(12));
        assert!(!t.is_closed(10) && !t.is_closed(11));
    }

    /// The write loop as it was before the buried rule: a literal copy of the
    /// state machine, used as a reference for the random draws.
    fn base_reference(
        before: &[Block],
        tree: &TreeSpec,
        rng: &mut StdRng,
        snow_cover: bool,
    ) -> Vec<Block> {
        let mut column = Column(before.to_vec());
        let hi = column.z(tree.first) + 1;
        let mut last_block = Block::empty();
        let mut is_top = true;
        let mut is_leaf_top = true;
        for (k, voxel) in tree.voxels.iter().enumerate() {
            let wpos = Vec3::new(0, 0, hi - 1 - k as i32);
            match voxel {
                Some(block) => {
                    if tree.lights
                        && last_block.is_air()
                        && block.kind() == BlockKind::Wood
                        && rng.random_range(0..256) == 0
                    {
                        column.set(wpos + Vec3::unit_z(), Block::air(SpriteKind::Lantern));
                    } else if snow_cover
                        && ((block.kind() == BlockKind::Leaves && is_leaf_top)
                            || (is_top && block.is_filled()))
                    {
                        column.set(
                            wpos + Vec3::unit_z(),
                            Block::new(BlockKind::Snow, Rgb::new(210, 210, 255)),
                        );
                    }
                    column.set(wpos, *block);
                    is_leaf_top = false;
                    is_top = false;
                    last_block = *block;
                },
                None => {
                    if last_block.is_filled() {
                        for (chance, sprite) in tree.hanging {
                            if rng.random_bool(*chance as f64) {
                                column.add_sprite(wpos, *sprite);
                            }
                        }
                    }
                    is_leaf_top = true;
                    last_block = Block::empty();
                },
            }
        }
        column.0
    }

    fn lit_tree() -> TreeSpec {
        let mut voxels = Vec::new();
        for k in 0..40 {
            voxels.push(match k % 4 {
                0 | 1 => Some(wood()),
                2 => Some(leaves()),
                _ => None,
            });
        }
        TreeSpec {
            first: 1,
            voxels,
            lights: true,
            hanging: &[(0.5, SpriteKind::Beehive), (0.3, SpriteKind::Apple)],
            end_at: None,
        }
    }

    #[test]
    fn an_unburied_tree_draws_and_writes_exactly_as_the_base_did() {
        for seed in 0..64 {
            let before = vec![air(); 41];
            let mut a = StdRng::seed_from_u64(seed);
            let mut b = StdRng::seed_from_u64(seed);
            let new = run(&before, &[lit_tree()], &mut a);
            let old = base_reference(&before, &lit_tree(), &mut b, false);
            assert_eq!(new, old, "seed {seed}");
            assert_eq!(a.random::<u64>(), b.random::<u64>(), "seed {seed}");
        }
    }

    #[test]
    fn a_buried_tree_writes_nothing_but_consumes_the_same_draws() {
        let mut lanterns_in_air = 0;
        for seed in 0..64 {
            let mut a = StdRng::seed_from_u64(seed);
            let mut b = StdRng::seed_from_u64(seed);
            let in_rock = run(&[rock(); 41], &[lit_tree()], &mut a);
            let in_air = run(&[air(); 41], &[lit_tree()], &mut b);
            assert_eq!(in_rock, [rock(); 41], "seed {seed}");
            assert_eq!(a.random::<u64>(), b.random::<u64>(), "seed {seed}");
            lanterns_in_air += usize::from(
                in_air
                    .iter()
                    .any(|b| b.get_sprite() == Some(SpriteKind::Lantern)),
            );
        }
        // The draws were really exercised on the written side.
        assert!(lanterns_in_air > 0);
    }

    #[test]
    fn the_union_range_spans_every_tree_and_keeps_the_top_inclusive() {
        assert_eq!(union_range([]), None);
        assert_eq!(union_range([(5, 8)]), Some((5, 8)));
        // Non-adjacent spans: the gap between them is inside the range.
        assert_eq!(union_range([(0, 2), (5, 8), (3, 4)]), Some((0, 8)));
        let mut t = ColumnTerrain::default();
        t.fill_union([(5, 8), (0, 2)], |_| true);
        assert!((0..=8).all(|z| t.is_terrain(z)));
        assert!(!t.is_terrain(-1) && !t.is_terrain(9));
        // No tree reaches the column: nothing is recorded.
        let mut empty = ColumnTerrain::default();
        empty.fill_union([], |_| true);
        assert!(!empty.is_terrain(0));
    }

    #[test]
    fn the_snapshot_reaches_the_block_above_the_tallest_trees_top_voxel() {
        // z 9..=0, top-down. A tall tree writes z 7..=5 and a short,
        // non-adjacent one z 1..=0; rock fills z 8 down to 0, so every voxel
        // of both is closed and nothing may be written. The topmost voxel
        // (z 7) is only closed if the rock at z 8, exactly the union's `hi`,
        // is in the snapshot.
        let mut before = vec![rock(); 10];
        before[0] = air();
        let tall = TreeSpec::new(2, vec![Some(wood()); 3]);
        let short = TreeSpec::new(8, vec![Some(leaves()); 2]);
        let after = run_plain(&before, &[tall, short]);
        assert_eq!(after, before);

        // With air at z 8 the top voxel is the open surface: it is written,
        // and the voxels under it are still closed.
        before[1] = air();
        let tall = TreeSpec::new(2, vec![Some(wood()); 3]);
        let short = TreeSpec::new(8, vec![Some(leaves()); 2]);
        let after = run_plain(&before, &[tall, short]);
        let mut expected = before.clone();
        expected[2] = wood();
        assert_eq!(after, expected);
    }

    #[test]
    fn a_model_ending_on_its_first_voxel_writes_and_draws_nothing() {
        let before = [air(), air(), air(), rock(), rock(), rock()];
        let ends_first = || TreeSpec {
            end_at: Some(0),
            ..lit_tree()
        };
        let short = || TreeSpec::new(4, vec![Some(leaves()); 2]);
        let mut a = StdRng::seed_from_u64(7);
        let mut b = StdRng::seed_from_u64(7);
        let with = run(&before, &[ends_first(), short()], &mut a);
        let without = run(&before, &[short()], &mut b);
        // The ended tree is still part of the snapshot's span, but writes
        // nothing and draws nothing; the other tree is buried as usual.
        assert_eq!(with, without);
        assert_eq!(with, before);
        assert_eq!(a.random::<u64>(), b.random::<u64>());
    }

    #[test]
    fn snow_matches_the_base_on_unburied_parts_and_never_follows_a_dropped_voxel() {
        // A leaf crown over a rock surface with a buried voxel under it.
        // z 6..=0 top-down: air x3, rock surface at z 3, rock at z 2, an air
        // pocket at z 1, rock at z 0.
        let before = [air(), air(), air(), rock(), rock(), air(), rock()];
        let crown = || TreeSpec {
            first: 1,
            voxels: vec![
                Some(leaves()),
                Some(leaves()),
                Some(leaves()),
                Some(leaves()),
                Some(wood()),
            ],
            lights: true,
            hanging: &[(0.5, SpriteKind::Beehive)],
            end_at: None,
        };
        for seed in 0..64 {
            let mut a = StdRng::seed_from_u64(seed);
            let mut b = StdRng::seed_from_u64(seed);
            let new = run_with_snow(&before, &[crown()], &mut a, true);
            let old = base_reference(&before, &crown(), &mut b, true);
            // Everything down to and including the open rock surface (z 3) is
            // as in the base, including the snow cap over the crown top.
            assert_eq!(new[..4], old[..4], "seed {seed}");
            assert_eq!(new[0].kind(), BlockKind::Snow, "seed {seed}");
            // The leaf at z 2 is buried and stays rock; the wood in the pocket
            // is open and is written, as in the base.
            assert_eq!(new[4], rock(), "seed {seed}");
            assert_eq!(new[5], old[5], "seed {seed}");
            assert_eq!(a.random::<u64>(), b.random::<u64>(), "seed {seed}");
        }

        // A crown whose first voxels are buried: the first written voxel is
        // not the top of the tree, so it gets no snow cap, and the dropped
        // voxels place none.
        let before = [rock(), rock(), rock(), air(), air(), rock()];
        let buried_top = || TreeSpec::new(1, vec![Some(leaves()); 4]);
        let new = run_with_snow(
            &before,
            &[buried_top()],
            &mut StdRng::seed_from_u64(3),
            true,
        );
        let old = base_reference(&before, &buried_top(), &mut StdRng::seed_from_u64(3), true);
        assert!(old.iter().any(|b| b.kind() == BlockKind::Snow));
        assert_eq!(new, [rock(), rock(), rock(), leaves(), leaves(), rock()]);
    }
}
