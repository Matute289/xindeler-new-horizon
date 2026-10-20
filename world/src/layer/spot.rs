use crate::{
    Canvas,
    sim::{SimChunk, WorldSim},
    util::{Sampler, UnitChooser, seed_expan},
};
use common::{
    generation::{EntityInfo, EntitySpawn},
    spot::{RON_SPOT_PROPERTIES, Spot, SpotCondition},
    terrain::{BiomeKind, BlockKind, Structure, TerrainChunkSize, structure::StructureBlock},
    vol::{ReadVol, RectVolSize},
};
use rand::{prelude::*, seq::IndexedRandom};
use rand_chacha::ChaChaRng;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
use vek::*;

pub trait SpotGenerate {
    fn generate(world: &mut WorldSim);

    fn generate_spots(
        spot: Spot,
        world: &mut WorldSim,
        freq: f32,
        valid: impl FnMut(f32, &SimChunk) -> bool,
        spawn: bool,
    );
}

impl SpotGenerate for Spot {
    fn generate(world: &mut WorldSim) {
        use BiomeKind::*;
        // Trees/spawn: false => *No* trees around the spot
        // Themed Spots -> Act as an introduction to themes of sites
        for s in RON_SPOT_PROPERTIES.0.iter() {
            Self::generate_spots(
                Spot::RonFile(s),
                world,
                s.freq,
                |g, c| is_valid(&s.condition, g, c),
                s.spawn,
            );
        }
        Self::generate_spots(
            Spot::WitchHouse,
            world,
            1.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(
                        c.get_biome(),
                        Grassland | Forest | Taiga | Snowland | Jungle
                    )
            },
            false,
        );
        Self::generate_spots(
            Spot::Igloo,
            world,
            2.0,
            |g, c| {
                g < 0.5
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Snowland)
            },
            false,
        );
        Self::generate_spots(
            Spot::SaurokAltar,
            world,
            1.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Jungle | Forest)
            },
            false,
        );
        Self::generate_spots(
            Spot::SaurokTotem,
            world,
            1.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Jungle | Forest)
            },
            false,
        );
        Self::generate_spots(
            Spot::JungleOutpost,
            world,
            1.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Jungle | Forest)
            },
            false,
        );
        Self::generate_spots(
            Spot::JungleTemple,
            world,
            0.5,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Jungle | Forest)
            },
            false,
        );
        Self::generate_spots(
            Spot::MyrmidonTemple,
            world,
            1.0,
            |g, c| {
                g < 0.1
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Desert | Jungle)
            },
            false,
        );
        Self::generate_spots(
            Spot::GnarlingTotem,
            world,
            1.5,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest | Grassland)
            },
            false,
        );
        Self::generate_spots(
            Spot::FallenTree,
            world,
            1.5,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest | Grassland)
            },
            false,
        );
        // Random World Objects -> Themed to their Biome and the NPCs that regularly
        // spawn there
        Self::generate_spots(
            Spot::LionRock,
            world,
            1.5,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Savannah)
            },
            false,
        );
        Self::generate_spots(
            Spot::WolfBurrow,
            world,
            1.5,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest | Grassland)
            },
            false,
        );
        Self::generate_spots(
            Spot::TreeStumpForest,
            world,
            20.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Jungle | Forest)
            },
            true,
        );
        Self::generate_spots(
            Spot::DesertBones,
            world,
            6.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Desert)
            },
            false,
        );
        Self::generate_spots(
            Spot::Arch,
            world,
            2.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Desert)
            },
            false,
        );
        Self::generate_spots(
            Spot::AirshipCrash,
            world,
            0.7,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && !matches!(c.get_biome(), Mountain | Void | Ocean)
            },
            false,
        );
        Self::generate_spots(
            Spot::FruitTree,
            world,
            20.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest)
            },
            true,
        );
        Self::generate_spots(
            Spot::GnomeSpring,
            world,
            1.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest)
            },
            false,
        );
        Self::generate_spots(
            Spot::Shipwreck,
            world,
            1.0,
            |g, c| {
                g < 0.25 && c.is_underwater() && c.sites.is_empty() && c.water_alt > c.alt + 30.0
            },
            true,
        );
        Self::generate_spots(
            Spot::Shipwreck2,
            world,
            1.0,
            |g, c| {
                g < 0.25 && c.is_underwater() && c.sites.is_empty() && c.water_alt > c.alt + 30.0
            },
            true,
        );
        // Small Grave
        Self::generate_spots(
            Spot::GraveSmall,
            world,
            2.0,
            |g, c| {
                g < 0.25
                    && !c.near_cliffs()
                    && !c.river.near_water()
                    && !c.path.0.is_way()
                    && c.sites.is_empty()
                    && matches!(c.get_biome(), Forest | Taiga | Jungle | Grassland)
            },
            false,
        );

        // Missing:
        /*
        Bandit Camp
        Hunter Camp
        TowerRuinForest
        TowerRuinDesert
        WellOfLight
        Merchant Outpost -> Near a road!
        *Quirky:*
        TreeHouse (Forest)
        EnchantedRock (Forest, Jungle)
        */
    }

    fn generate_spots(
        // What kind of spot are we generating?
        spot: Spot,
        world: &mut WorldSim,
        // How often should this spot appear (per square km, on average)?
        freq: f32,
        // What tests should we perform to see whether we can spawn the spot here? The two
        // parameters are the gradient of the terrain and the [`SimChunk`] of the candidate
        // location.
        mut valid: impl FnMut(f32, &SimChunk) -> bool,
        // Should we allow trees and other trivial structures to spawn close to the spot?
        spawn: bool,
    ) {
        // A region that disables the procedural spot layer gets no spot
        // *markers* either, not just no rendered structures. `SimChunk::spot`
        // is read on its own by rtsim's `get_nearest_spot` quest targeting and
        // by the `/spot` admin lookup, both of which already have correct
        // "no such spot" paths -- leaving a marker behind for a structure
        // `apply_spots_to` will never stamp would aim a quest map-marker at
        // empty ground.
        if world
            .authored_procedural_layers()
            .is_some_and(|layers| !layers.spots)
        {
            return;
        }

        let world_size = world.get_size();
        for _ in
            0..(world_size.product() as f32 * TerrainChunkSize::RECT_SIZE.product() as f32 * freq
                / 1000.0f32.powi(2))
            .ceil() as u64
        {
            let pos = world_size.map(|e| (world.rng.random_range(0..e) & !0b11) as i32);
            // XINDELER: the predicates see the chunk through the authored
            // water (`WorldSim::chunk_view`); outside every region it is the
            // chunk itself.
            let is_valid = world
                .get_gradient_approx(pos)
                .zip(world.chunk_view(pos))
                .is_some_and(|(grad, view)| valid(grad, &view));
            if let Some(chunk) = world.get_mut(pos).filter(|_| is_valid) {
                chunk.spot = Some(spot);
                // `spawn: false` zeroes `tree_density`/`spawn_rate` outright,
                // assuming they are seed-derived values nothing else cares
                // about. In an authored region they are hand-painted content,
                // so this silently discards authoring work -- the same bug
                // class as the `generate_cliffs` vegetation crush. Authored
                // data wins; the spot itself is still placed.
                //
                // Be aware of the real trade-off this makes, rather than
                // reading it as a pure win: the clearing also serves as
                // structure clearance (don't grow a tree through the witch
                // house), so an authored region that *enables* the spot layer
                // accepts trees crowding its spot structures. The two can't
                // both hold as written. If that combination is ever wanted,
                // the answer is to clamp `tree_density` locally instead of
                // choosing between zeroing it and leaving it untouched.
                // Inert while the region disables spots (early return above).
                if !spawn && chunk.authored_region_id.is_none() {
                    chunk.tree_density = 0.0;
                    chunk.spawn_rate = 0.0;
                }
            }
        }
    }
}

#[derive(Default)]
struct SpotConfig<'a> {
    // The manifest containing a list of possible base structures for the spot (one will be
    // chosen)
    base_structures: Option<&'a str>,
    // The maximum distance from the centre of the spot that entities will spawn
    entity_radius: f32,
    // The entities that should be spawned in the spot, from closest to furthest
    // (count_range, spec)
    // count_range = number of entities, chosen randomly within this range (not inclusive!)
    // spec = Manifest spec for the entity kind
    entities: &'a [(Range<i32>, &'a str)],
}
fn spot_config(spot: &Spot) -> SpotConfig<'_> {
    match spot {
        // Themed Spots
        Spot::DwarvenGrave => SpotConfig {
            base_structures: Some("spots_grasslands.dwarven_grave"),
            entity_radius: 60.0,
            entities: &[(6..12, "common.entity.spot.dwarf_grave_robber")],
        },
        Spot::SaurokAltar => SpotConfig {
            base_structures: Some("spots.jungle.saurok-altar"),
            entity_radius: 12.0,
            entities: &[
                (0..3, "common.entity.wild.aggressive.occult_saurok"),
                (0..3, "common.entity.wild.aggressive.sly_saurok"),
                (0..3, "common.entity.wild.aggressive.mighty_saurok"),
            ],
        },
        Spot::SaurokTotem => SpotConfig {
            base_structures: Some("spots.jungle.saurok_totem"),
            entity_radius: 20.0,
            entities: &[
                (0..3, "common.entity.wild.aggressive.occult_saurok"),
                (0..3, "common.entity.wild.aggressive.sly_saurok"),
                (0..3, "common.entity.wild.aggressive.mighty_saurok"),
            ],
        },
        Spot::JungleOutpost => SpotConfig {
            base_structures: Some("spots.jungle.outpost"),
            entity_radius: 40.0,
            entities: &[(6..12, "common.entity.spot.grim_salvager")],
        },
        Spot::JungleTemple => SpotConfig {
            base_structures: Some("spots.jungle.temple_small"),
            entity_radius: 40.0,
            entities: &[
                (2..8, "common.entity.wild.aggressive.occult_saurok"),
                (2..8, "common.entity.wild.aggressive.sly_saurok"),
                (2..8, "common.entity.wild.aggressive.mighty_saurok"),
            ],
        },
        Spot::MyrmidonTemple => SpotConfig {
            base_structures: Some("spots.myrmidon-temple"),
            entity_radius: 10.0,
            entities: &[
                (3..5, "common.entity.dungeon.myrmidon.hoplite"),
                (3..5, "common.entity.dungeon.myrmidon.strategian"),
                (2..3, "common.entity.dungeon.myrmidon.marksman"),
            ],
        },
        Spot::WitchHouse => SpotConfig {
            base_structures: Some("spots_general.witch_hut"),
            entity_radius: 1.0,
            entities: &[
                (1..2, "common.entity.spot.witch_dark"),
                (0..4, "common.entity.wild.peaceful.cat"),
                (0..3, "common.entity.wild.peaceful.frog"),
            ],
        },
        Spot::Igloo => SpotConfig {
            base_structures: Some("spots_general.igloo"),
            entity_radius: 2.0,
            entities: &[
                (3..5, "common.entity.dungeon.adlet.hunter"),
                (3..5, "common.entity.dungeon.adlet.icepicker"),
                (2..3, "common.entity.dungeon.adlet.tracker"),
            ],
        },
        Spot::GnarlingTotem => SpotConfig {
            base_structures: Some("site_structures.gnarling.totem"),
            entity_radius: 30.0,
            entities: &[
                (3..5, "common.entity.dungeon.gnarling.mugger"),
                (3..5, "common.entity.dungeon.gnarling.stalker"),
                (3..5, "common.entity.dungeon.gnarling.logger"),
                (1..3, "common.entity.wild.aggressive.deadwood"),
                (1..2, "common.entity.dungeon.gnarling.woodgolem"),
            ],
        },
        Spot::FallenTree => SpotConfig {
            base_structures: Some("spots_grasslands.fallen_tree"),
            entity_radius: 64.0,
            entities: &[
                (1..2, "common.entity.dungeon.gnarling.mandragora"),
                (2..6, "common.entity.wild.aggressive.deadwood"),
                (0..2, "common.entity.wild.aggressive.mossdrake"),
            ],
        },
        // Random World Objects
        Spot::LionRock => SpotConfig {
            base_structures: Some("spots_savannah.lion_rock"),
            entity_radius: 30.0,
            entities: &[
                (5..10, "common.entity.spot.female_lion"),
                (1..2, "common.entity.wild.aggressive.male_lion"),
            ],
        },
        Spot::WolfBurrow => SpotConfig {
            base_structures: Some("spots_savannah.wolf_burrow"),
            entity_radius: 10.0,
            entities: &[(5..8, "common.entity.wild.aggressive.wolf")],
        },
        Spot::TreeStumpForest => SpotConfig {
            base_structures: Some("trees.oak_stumps"),
            entity_radius: 30.0,
            entities: &[(0..2, "common.entity.wild.aggressive.deadwood")],
        },
        Spot::DesertBones => SpotConfig {
            base_structures: Some("spots.bones"),
            entity_radius: 40.0,
            entities: &[(4..9, "common.entity.wild.aggressive.hyena")],
        },
        Spot::Arch => SpotConfig {
            base_structures: Some("spots.arch"),
            entity_radius: 50.0,
            entities: &[],
        },
        Spot::AirshipCrash => SpotConfig {
            base_structures: Some("trees.airship_crash"),
            entity_radius: 20.0,
            entities: &[(4..9, "common.entity.spot.grim_salvager")],
        },
        Spot::FruitTree => SpotConfig {
            base_structures: Some("trees.fruit_trees"),
            entity_radius: 2.0,
            entities: &[(0..2, "common.entity.spot.bear")],
        },
        Spot::GnomeSpring => SpotConfig {
            base_structures: Some("spots.gnome_spring"),
            entity_radius: 40.0,
            entities: &[(7..10, "common.entity.spot.gnome.spear")],
        },
        Spot::Shipwreck => SpotConfig {
            base_structures: Some("spots.water.shipwreck"),
            entity_radius: 2.0,
            entities: &[(0..2, "common.entity.wild.peaceful.clownfish")],
        },
        Spot::Shipwreck2 => SpotConfig {
            base_structures: Some("spots.water.shipwreck2"),
            entity_radius: 20.0,
            entities: &[(0..3, "common.entity.wild.peaceful.clownfish")],
        },
        Spot::GraveSmall => SpotConfig {
            base_structures: Some("spots.grave_small"),
            entity_radius: 2.0,
            entities: &[],
        },
        Spot::RonFile(properties) => SpotConfig {
            base_structures: Some(&properties.base_structures),
            entity_radius: 1.0,
            entities: &[],
        },
    }
}

pub fn apply_spots_to(canvas: &mut Canvas, _dynamic_rng: &mut impl Rng) {
    let nearby_spots = canvas.nearby_spots().collect::<Vec<_>>();

    for (spot_wpos2d, spot, seed) in nearby_spots.iter().copied() {
        let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(seed));

        let units = UnitChooser::new(seed).get(seed).into();

        let spot_config = spot_config(&spot);
        // Blit base structure
        if let Some(base_structures) = spot_config.base_structures {
            let structures = Structure::load_group(base_structures).read();
            let structure = structures.choose(&mut rng).unwrap();
            let origin = spot_wpos2d.with_z(
                canvas
                    .col_or_gen(spot_wpos2d)
                    .map(|c| c.alt as i32)
                    .unwrap_or(0),
            );
            canvas.blit_structure(origin, structure, seed, units, true);
        }

        // Spawn entities
        const PHI: f32 = 1.618;
        for (spawn_count, spec) in spot_config.entities {
            let spawn_count = rng.random_range(spawn_count.clone()).max(0);

            let dir_offset = rng.random::<f32>();
            for i in 0..spawn_count {
                let dir = Vec2::new(
                    ((dir_offset + i as f32 * PHI) * std::f32::consts::TAU).sin(),
                    ((dir_offset + i as f32 * PHI) * std::f32::consts::TAU).cos(),
                );
                let dist = i as f32 / spawn_count as f32 * spot_config.entity_radius;
                let wpos2d = spot_wpos2d + (dir * dist).map(|e| e.round() as i32);

                let alt = canvas.col_or_gen(wpos2d).map(|c| c.alt as i32).unwrap_or(0);

                if let Some(wpos) = canvas
                    .area()
                    .contains_point(wpos2d)
                    .then(|| canvas.find_spawn_pos(wpos2d.with_z(alt)))
                    .flatten()
                {
                    canvas.spawn(EntitySpawn::Entity(Box::new(
                        EntityInfo::at(wpos.map(|e| e as f32) + Vec3::new(0.5, 0.5, 0.0))
                            .with_asset_expect(spec, &mut rng, None),
                    )));
                }
            }
        }
    }
}

/// World position (the centre of the chunk) and placement seed of the spot
/// that chunk `pos` may hold. This is the single definition used by
/// `Canvas::nearby_spots` and by [`spot_footprint`], so both always agree.
pub fn chunk_spot_placement(pos: Vec2<i32>) -> (Vec2<i32>, u32) {
    let wpos = pos.map2(TerrainChunkSize::RECT_SIZE, |e, sz| {
        e * sz as i32 + sz as i32 / 2
    });
    // TODO: Very dumb, not this.
    let seed = pos.x as u32 | (pos.y as u32).wrapping_shl(16);
    (wpos, seed ^ 0xA801D82E)
}

/// How far (in blocks, per axis) from its chunk centre a spot can change
/// terrain: the engine blits a spot into the canvas of its own chunk and of
/// the eight around it (`Canvas::nearby_spots`), i.e. centre +- (32 + 16).
pub const SPOT_REACH: i32 = 48;

/// The `Spot` kinds' short human label (used for names in tooling output).
pub fn spot_label(spot: &Spot) -> String {
    match spot {
        Spot::RonFile(p) => p
            .base_structures
            .rsplit('.')
            .next()
            .unwrap_or("ron_spot")
            .to_string(),
        other => format!("{other:?}"),
    }
}

/// Whether a prefab voxel can clear natural terrain when blitted (turns the
/// block it lands on into plain air): `StructureBlock::Hollow`, and
/// `StructureBlock::Filled` with `BlockKind::Air` (the way e.g. the small grave
/// prefab digs its pit; it places the very same air block). `None` leaves the
/// terrain untouched, so it is not hollow. A `Choice` resolves per position, so
/// it counts as hollow if any of its options can be (conservative). Sprite
/// voxels are not counted: they stand on the surface.
pub fn block_is_hollow(block: &StructureBlock) -> bool {
    match block {
        StructureBlock::Hollow => true,
        StructureBlock::Filled(BlockKind::Air, _) => true,
        StructureBlock::Choice(table) => table.iter().any(|(_, b)| block_is_hollow(b)),
        _ => false,
    }
}

/// One world column that a spot's prefab can hollow out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HollowColumn {
    /// World column.
    pub wpos: Vec2<i32>,
    /// Hollow voxels of the prefab in this column, as heights relative to the
    /// spot origin (`0` = the origin block, `-1` = the block below), ascending.
    pub hollow_z: Vec<i32>,
}

impl HollowColumn {
    /// Blocks removed from the top of a column whose top natural block sits at
    /// `top_rel` (relative to the spot origin): the length of the hollow run
    /// that starts at `top_rel` and goes down. `0` when the top block itself is
    /// not hollow (a cavity under a roof does not dent the top).
    pub fn dig_blocks(&self, top_rel: i32) -> i32 {
        let mut n = 0;
        while self.hollow_z.binary_search(&(top_rel - n)).is_ok() {
            n += 1;
        }
        n
    }

    /// Deepest [`Self::dig_blocks`] for a top natural block anywhere in
    /// `top_lo..=top_hi` (the altitude range of the terrain over the footprint,
    /// relative to the spot origin).
    pub fn dig_blocks_in(&self, top_lo: i32, top_hi: i32) -> i32 {
        (top_lo..=top_hi)
            .map(|t| self.dig_blocks(t))
            .max()
            .unwrap_or(0)
    }
}

/// The columns a placed spot can hollow out, in world space.
///
/// Computed from the HOLLOW voxels only (never from the model's rectangle).
/// Built by [`spot_footprint`] (real placement) or [`hollow_footprint`] (any
/// prefab volume, used by the asset-free tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotFootprint {
    /// Spot origin column (the chunk centre for a real placement).
    pub origin: Vec2<i32>,
    /// Hollow columns, clipped to the spot's reach, sorted by `(y, x)`.
    pub columns: Vec<HollowColumn>,
}

impl SpotFootprint {
    /// Deepest hollow below the spot origin on flat ground: the most blocks a
    /// single column loses when its top natural block is the origin block.
    pub fn deepest_hollow(&self) -> i32 { self.max_dig_blocks(0, 0) }

    /// The `max_dig_blocks` of the spot for terrain whose top natural block
    /// over the footprint lies within `top_lo..=top_hi` relative to the origin
    /// block (the blit is anchored at the altitude of one point, so on a slope
    /// the other columns' tops sit higher or lower than the origin: this is the
    /// slope spread). The caller, which can sample the terrain, supplies the
    /// window; `(0, 0)` is flat ground.
    pub fn max_dig_blocks(&self, top_lo: i32, top_hi: i32) -> i32 {
        self.columns
            .iter()
            .map(|c| c.dig_blocks_in(top_lo, top_hi))
            .max()
            .unwrap_or(0)
    }

    /// Disjoint half-open boxes `[x0, y0, x1, y1]` covering exactly the hollow
    /// columns, each side at most `max_side`.
    pub fn boxes(&self, max_side: i32) -> Vec<[i32; 4]> {
        column_boxes(self.columns.iter().map(|c| c.wpos), max_side)
    }
}

/// Partition a set of world columns into disjoint half-open boxes
/// `[x0, y0, x1, y1]` that cover exactly those columns (no column outside the
/// set), with every side at most `max_side` (greedy: grow along x, then along
/// y). Deterministic.
pub fn column_boxes(cols: impl IntoIterator<Item = Vec2<i32>>, max_side: i32) -> Vec<[i32; 4]> {
    let max_side = max_side.max(1);
    // Keyed (y, x) so the iteration order is row by row.
    let free: BTreeSet<(i32, i32)> = cols.into_iter().map(|c| (c.y, c.x)).collect();
    let mut used: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut out = Vec::new();
    for &(y, x) in &free {
        if used.contains(&(y, x)) {
            continue;
        }
        let ok = |y: i32, x: i32, used: &BTreeSet<(i32, i32)>| {
            free.contains(&(y, x)) && !used.contains(&(y, x))
        };
        let mut x1 = x + 1;
        while x1 - x < max_side && ok(y, x1, &used) {
            x1 += 1;
        }
        let mut y1 = y + 1;
        while y1 - y < max_side && (x..x1).all(|xx| ok(y1, xx, &used)) {
            y1 += 1;
        }
        for yy in y..y1 {
            for xx in x..x1 {
                used.insert((yy, xx));
            }
        }
        out.push([x, y, x1, y1]);
    }
    out
}

/// Hollow footprint of any prefab volume `vol` (voxels in `bounds`), blitted at
/// world column `origin` with the rotation/mirroring `units` exactly as
/// `Canvas::blit_structure` does (`rpos = units.x * r.x + units.y * r.y` with
/// `r = wpos - origin`), clipped to `origin +- reach`. Pure: no assets, no RNG.
pub fn hollow_footprint<V: ReadVol<Vox = StructureBlock>>(
    vol: &V,
    bounds: Aabb<i32>,
    origin: Vec2<i32>,
    units: Vec2<Vec2<i32>>,
    reach: i32,
) -> SpotFootprint {
    // `units` is a signed permutation matrix, so its inverse is its transpose:
    // r.x = units.x . s, r.y = units.y . s for the prefab column s.
    let mut cols: BTreeMap<(i32, i32), Vec<i32>> = BTreeMap::new();
    for sy in bounds.min.y..bounds.max.y {
        for sx in bounds.min.x..bounds.max.x {
            let s = Vec2::new(sx, sy);
            let wpos = origin + Vec2::new(units.x.dot(s), units.y.dot(s));
            if (wpos - origin).map(i32::abs).reduce_max() >= reach {
                continue;
            }
            let zs = (bounds.min.z..bounds.max.z)
                .filter(|&z| vol.get(s.with_z(z)).map(block_is_hollow).unwrap_or(false))
                .collect::<Vec<_>>();
            if !zs.is_empty() {
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

/// The hollow footprint of the spot `apply_spots_to` blits at `spot_wpos2d`
/// with placement `seed` (as returned by [`chunk_spot_placement`] /
/// `Canvas::nearby_spots`). It repeats `apply_spots_to`'s draw order
/// exactly: a `ChaChaRng` from `seed_expan::rng_state(seed)`, `units` from
/// `UnitChooser::new(seed).get(seed)`, then one `choose` over the spot's
/// structure group. `None` when the spot has no base structure or the group is
/// empty. The caller adds the terrain-dependent part with
/// [`SpotFootprint::max_dig_blocks`] (it needs the altitude of the columns).
pub fn spot_footprint(spot_wpos2d: Vec2<i32>, spot: &Spot, seed: u32) -> Option<SpotFootprint> {
    let base_structures = spot_config(spot).base_structures?;
    let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(seed));
    let units = UnitChooser::new(seed).get(seed).into();
    let structures = Structure::load_group(base_structures).read();
    let structure = structures.choose(&mut rng)?;
    Some(hollow_footprint(
        structure,
        structure.get_bounds(),
        spot_wpos2d,
        units,
        SPOT_REACH,
    ))
}

pub fn is_valid(condition: &SpotCondition, g: f32, c: &SimChunk) -> bool {
    c.sites.is_empty()
        && match condition {
            SpotCondition::MaxGradient(value) => g < *value,
            SpotCondition::Biome(biomes) => biomes.contains(&c.get_biome()),
            SpotCondition::NearCliffs => c.near_cliffs(),
            SpotCondition::NearRiver => c.river.near_water(),
            SpotCondition::IsWay => c.path.0.is_way(),
            SpotCondition::IsUnderwater => c.is_underwater(),
            SpotCondition::Typical => {
                !c.near_cliffs() && !c.river.near_water() && !c.path.0.is_way()
            },
            SpotCondition::MinWaterDepth(depth) => {
                is_valid(&SpotCondition::IsUnderwater, g, c) && c.water_alt > c.alt + depth
            },
            SpotCondition::Not(condition) => !is_valid(condition, g, c),
            SpotCondition::All(conditions) => conditions.iter().all(|cond| is_valid(cond, g, c)),
            SpotCondition::Any(conditions) => conditions.iter().any(|cond| is_valid(cond, g, c)),
        }
}

#[test]
#[ignore = "needs LFS .vox assets"]
fn test_spot_configs() {
    let all_spots = [
        Spot::DwarvenGrave,
        Spot::SaurokAltar,
        Spot::MyrmidonTemple,
        Spot::GnarlingTotem,
        Spot::WitchHouse,
        Spot::GnomeSpring,
        Spot::WolfBurrow,
        Spot::Igloo,
        Spot::LionRock,
        Spot::TreeStumpForest,
        Spot::DesertBones,
        Spot::Arch,
        Spot::AirshipCrash,
        Spot::FruitTree,
        Spot::Shipwreck,
        Spot::Shipwreck2,
        Spot::FallenTree,
        Spot::GraveSmall,
        Spot::JungleTemple,
        Spot::SaurokTotem,
        Spot::JungleOutpost,
    ];

    for spot in all_spots
        .into_iter()
        .chain(RON_SPOT_PROPERTIES.0.iter().map(Spot::RonFile))
    {
        let config = spot_config(&spot);

        if let Some(base_structures) = config.base_structures {
            let _structures = Structure::load_group(base_structures).read();
        }
        // Entities are tested in another test so don't really need to test it
        // here.
    }
}

/// Full-world smoke test for the spawn work (the "no monsters outside
/// settlements" bug report): confirms that enabling `spots: true` for
/// Cromatolis in `assets/world/map/cromatolis_v0_procedural_layers.ron`
/// actually places spot markers on the real generated map, that none of them
/// land on a chunk claimed by a registered `Site` (settlements, landmarks,
/// bridges and fortifications are all registered as `Site`s with a real
/// footprint radius -- see `civ::Civs::establish_site`'s "Place sites in world"
/// pass -- so `is_valid`'s `c.sites.is_empty()` check is real collision
/// protection, not just a procedural-world nicety), and that a healthy share of
/// the placed spots are creature-cluster variants (a non-empty `entities` list
/// in `spot_config`), not just structure-only decoration.
///
/// Requires the real Cromatolis LFS assets pulled locally, same precedent as
/// `cromatolis_cave_features.rs`'s and `cromatolis_interior.rs`'s own
/// ignored full-world tests.
/// `cargo test -p xindeler-world layer::spot:: -- --ignored`
#[cfg(test)]
mod cromatolis_real_world {
    use super::*;

    fn generate_cromatolis_world() -> (crate::World, crate::IndexOwned) {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        )
    }

    #[test]
    #[ignore]
    fn cromatolis_spots_enabled_places_collision_free_creature_clusters_against_real_lfs_assets() {
        let (world, _index) = generate_cromatolis_world();
        let sim = world.sim();

        assert!(
            sim.authored_procedural_layers()
                .is_some_and(|layers| layers.spots),
            "this test measures the real effect of `spots: true` -- if \
             `cromatolis_v0_procedural_layers.ron` has `spots: false` again, this assertion is \
             the tripwire that catches it, since every count below would silently go back to zero \
             for an unrelated reason"
        );

        let dims = sim.map_size_lg().chunks().map(i32::from);
        let mut land_chunks = 0usize;
        let mut spot_chunks = 0usize;
        let mut creature_cluster_chunks = 0usize;
        let mut collisions_with_registered_sites = 0usize;

        for y in 0..dims.y {
            for x in 0..dims.x {
                let Some(chunk) = sim.get(Vec2::new(x, y)) else {
                    continue;
                };
                if chunk.is_underwater() {
                    continue;
                }
                land_chunks += 1;
                let Some(spot) = chunk.spot else { continue };
                spot_chunks += 1;
                if !chunk.sites.is_empty() {
                    collisions_with_registered_sites += 1;
                }
                if !spot_config(&spot).entities.is_empty() {
                    creature_cluster_chunks += 1;
                }
            }
        }

        assert_eq!(
            collisions_with_registered_sites, 0,
            "found {collisions_with_registered_sites} spot(s) placed on a chunk already claimed \
             by a registered Site (settlement/landmark/bridge/fortification) -- the \
             `c.sites.is_empty()` guard in `is_valid` should make this structurally impossible"
        );

        // Measured on the real map: land=681560, spot=7630 (~1.12% of
        // land), creature_cluster=7075 (~92.7% of spots). Generous ranges
        // around the measured baseline: tight enough to catch a regression
        // back to `spots: false` (which would zero both), loose enough to
        // tolerate a future freq-tuning pass.
        let spot_fraction = spot_chunks as f64 / land_chunks as f64;
        assert!(
            (0.003..0.05).contains(&spot_fraction),
            "expected spot coverage in 0.3%..5% of land chunks, got {spot_fraction:.6} \
             ({spot_chunks}/{land_chunks})"
        );
        let creature_cluster_fraction = creature_cluster_chunks as f64 / spot_chunks as f64;
        assert!(
            (0.5..1.0).contains(&creature_cluster_fraction),
            "expected the majority of placed spots to carry a real entity list (a monster \
             cluster, e.g. WolfBurrow/LionRock/DesertBones), got {creature_cluster_fraction:.6} \
             ({creature_cluster_chunks}/{spot_chunks})"
        );
    }

    /// End-to-end companion to the marker-count test above: finds a real
    /// creature-cluster spot on the map and actually generates that chunk
    /// (`World::generate_chunk`, the same call the server makes when a
    /// player approaches), confirming the monster entities described by
    /// `spot_config` are really queued in the chunk's `ChunkSupplement` --
    /// not just that a marker byte got set on `SimChunk`.
    /// `cargo test -p xindeler-world layer::spot:: -- --ignored`
    #[test]
    #[ignore]
    fn cromatolis_spot_chunk_generation_spawns_real_entities_against_real_lfs_assets() {
        let (world, index) = generate_cromatolis_world();
        let sim = world.sim();
        let dims = sim.map_size_lg().chunks().map(i32::from);

        // Require at least one entity range with a strictly-positive lower
        // bound, so the random spawn count for that entry can't roll to
        // zero (e.g. WolfBurrow's `5..8`, not a `0..3` that could legally
        // spawn nothing this seed) -- otherwise this test would be flaky by
        // construction. Also stay comfortably inside the map (not chunk 0
        // or the outermost ring), since the canvas/area clipping near the
        // world edge is a separate concern from what this test checks.
        let chunk_pos = (16..dims.y - 16)
            .flat_map(|y| (16..dims.x - 16).map(move |x| Vec2::new(x, y)))
            .find(|&pos| {
                sim.get(pos).is_some_and(|chunk| {
                    chunk.spot.is_some_and(|spot| {
                        spot_config(&spot)
                            .entities
                            .iter()
                            .any(|(range, _)| range.start > 0)
                    })
                })
            })
            .expect(
                "expected at least one creature-cluster spot with a guaranteed non-zero spawn \
                 count on the real Cromatolis map with spots enabled",
            );

        let index_ref = index.as_index_ref();
        let (_chunk, supplement) = world
            .generate_chunk(index_ref, chunk_pos, None, || false, None, None)
            .expect("chunk generation must not fail for a real, in-bounds Cromatolis chunk");

        assert!(
            !supplement.entity_spawns.is_empty(),
            "chunk {chunk_pos:?} has a creature-cluster spot but generating it produced no entity \
             spawns"
        );
    }
}

/// Asset-free tests of the hollow footprint (a synthetic prefab, no `.vox`).
#[cfg(test)]
mod footprint_tests {
    use super::*;
    use common::vol::BaseVol;
    use std::collections::HashMap;

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
        // Solid prefab blocks and a non-hollow choice do not clear terrain.
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

    #[test]
    fn hollow_none_and_solid_are_told_apart() {
        assert!(block_is_hollow(&StructureBlock::Hollow));
        assert!(!block_is_hollow(&StructureBlock::None));
        assert!(block_is_hollow(&StructureBlock::Filled(
            BlockKind::Air,
            Rgb::zero()
        )));
        assert!(!block_is_hollow(&StructureBlock::Filled(
            BlockKind::Rock,
            Rgb::zero()
        )));
        assert!(!block_is_hollow(&StructureBlock::Normal(Rgb::zero())));
        assert!(block_is_hollow(&StructureBlock::Choice(vec![
            (1.0, StructureBlock::None),
            (
                1.0,
                StructureBlock::Choice(vec![(1.0, StructureBlock::Hollow)])
            ),
        ])));
        assert!(!block_is_hollow(&StructureBlock::Choice(vec![(
            1.0,
            StructureBlock::None
        )])));
    }

    #[test]
    fn identity_footprint_is_only_the_hollow_columns() {
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
        assert_eq!(f.deepest_hollow(), 2);
        assert_eq!(f.max_dig_blocks(0, 0), 2);
        assert_eq!(f.max_dig_blocks(-2, 1), 2);
        assert_eq!(f.max_dig_blocks(1, 3), 1);
    }

    #[test]
    fn rotations_and_mirrors_match_the_blitter() {
        // Independent check against the forward transform `blit_structure`
        // applies: a world column is hollow iff its prefab column is.
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
                    let hollow = (b.min.z..b.max.z).any(|z| {
                        prefab
                            .get(s.with_z(z))
                            .map(block_is_hollow)
                            .unwrap_or(false)
                    });
                    if hollow {
                        expect.insert((wx, wy));
                    }
                }
            }
            assert_eq!(cols(&f), expect, "units {units:?}");
            // Same depths whatever the orientation.
            assert_eq!(f.deepest_hollow(), 2, "units {units:?}");
        }
        // The rotations really differ (the prefab is not symmetric).
        let a = hollow_footprint(&prefab, bounds(), origin, all_units()[0], SPOT_REACH);
        let b = hollow_footprint(&prefab, bounds(), origin, all_units()[4], SPOT_REACH);
        assert_ne!(cols(&a), cols(&b));
    }

    #[test]
    fn columns_beyond_the_spot_reach_are_dropped() {
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let f = hollow_footprint(&prefab(), bounds(), Vec2::zero(), id, 3);
        // |dx| or |dy| >= 3 is out of reach: (3, 0) and (-3, 2) are dropped.
        assert_eq!(cols(&f), [(0, 0), (2, -1)].into_iter().collect());
    }

    #[test]
    fn boxes_cover_exactly_the_columns_and_are_disjoint_and_small() {
        let id = Vec2::new(Vec2::new(1, 0), Vec2::new(0, 1));
        let f = hollow_footprint(&prefab(), bounds(), Vec2::new(7, 9), id, SPOT_REACH);
        let boxes = f.boxes(48);
        let mut covered = BTreeSet::new();
        for b in &boxes {
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    assert!(covered.insert((x, y)), "overlap at {x},{y}");
                }
            }
        }
        assert_eq!(covered, cols(&f));
        // A long strip is cut at the side cap, still exact.
        let strip: Vec<_> = (0..130)
            .flat_map(|x| (0..3).map(move |y| Vec2::new(x, y)))
            .collect();
        let boxes = column_boxes(strip.iter().copied(), 48);
        let mut covered = BTreeSet::new();
        for b in &boxes {
            assert!(b[2] - b[0] <= 48 && b[3] - b[1] <= 48, "{b:?}");
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    assert!(covered.insert((x, y)));
                }
            }
        }
        assert_eq!(covered.len(), 130 * 3);
        // An L shape is not boxed as its bounding rectangle.
        let l = [Vec2::new(0, 0), Vec2::new(1, 0), Vec2::new(0, 1)];
        let boxes = column_boxes(l, 48);
        assert_eq!(
            boxes
                .iter()
                .map(|b| (b[2] - b[0]) * (b[3] - b[1]))
                .sum::<i32>(),
            3
        );
    }

    #[test]
    fn chunk_placement_is_the_chunk_centre_with_the_xor_seed() {
        let (w, seed) = chunk_spot_placement(Vec2::new(3, 5));
        assert_eq!(w, Vec2::new(3 * 32 + 16, 5 * 32 + 16));
        assert_eq!(seed, (3u32 | (5u32 << 16)) ^ 0xA801D82E);
    }
}
