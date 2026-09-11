//! Authored Cromatolis "Aerial Citadel" -- a floating island ~5000 m above
//! sea level: an ~800 m irregular island (an SDF composition of 4
//! elliptical lobes), a 200 m snow-capped mountain steeper on its
//! city-facing side, a castle embedded in the mountain base, a 25 m
//! crenellated contour wall following the usable rim, and 24 perimeter
//! towers (upper watch decks + lower under-island lookouts) with frozen
//! spiral-stair/hatch geometry.
//!
//! ## Architecture
//!
//! Every tunable geometric constant (the anchor position, altitude, max
//! radius, mountain/castle centers, wall/tower heights, dome radii, and the
//! 24 authored tower-anchor positions) is a field of [`AerialCitadelConfig`],
//! loaded once per world from the schema-versioned
//! `assets/world/map/cromatolis_v0_aerial_features.ron` asset and cached on
//! [`crate::index::Index`] -- the same shape `cromatolis_interior.rs` and
//! `cromatolis_cave_features.rs` (sibling modules) already established for
//! authored Cromatolis geometry.
//!
//! Like those siblings, this only ever activates for chunks with
//! `SimChunk::authored_cromatolis_v0` set.
//!
//! ## Per-column geometry, not per-voxel
//!
//! The single-position `castle_voxel_at`/`wall_voxel_at`/`voxel_at` methods
//! (kept for tests, and for any other one-off caller) are thin wrappers over
//! a two-phase design: [`AerialCitadelConfig::build_column`] resolves every
//! column-invariant quantity exactly once for a given `(x, y)` --
//! `surface_z`'s trig/`powf` chain, each candidate tower's altitude/axis
//! math, the castle's local-axis/corner-tower lookup -- into a
//! [`ColumnContext`]; the actual per-`z` sampling
//! (`ColumnContext::voxel_at`) then only ever does cheap integer range
//! checks against that precomputed data. The real entry point,
//! [`apply_cromatolis_local_aerial_features_to`], builds one `ColumnContext`
//! per column and reuses it across the citadel's full ~380-block vertical
//! extent, mirroring how `cromatolis_interior.rs`'s `carve_level_room` and
//! `cromatolis_cave_features.rs`'s `carve_hub`/`carve_branch` compute their
//! per-column invariants once before ever touching `z`.
//!
//! `build_column` also prunes the 24 authored towers down to the handful
//! (typically 0-2, given a mean spacing of ~93 m against each tower's ~19 m
//! reach) that could plausibly affect the column at all, via
//! [`AerialCitadelConfig::tower_reach_radius_squared`] -- the same
//! bounding-radius pre-filter `cromatolis_interior.rs`'s
//! `level_touches_chunk`/`connection_touches_chunk` and
//! `cromatolis_cave_features.rs`'s `hub_touches_chunk`/`branch_touches_chunk`
//! already establish as this codebase's convention for exactly this shape
//! of authored-geometry layer, applied here per column rather than per chunk
//! since the citadel data itself (24 small fixed anchors) is cheap enough to
//! scan for reachability -- only the *trig* per reachable tower needs
//! hoisting out of the per-`z` loop.
//!
//! ## Scope
//!
//! This module intentionally covers *only* the Aerial Citadel: the
//! floating island, its mountain, its castle, its contour wall, and its 24
//! perimeter towers. A separate, unrelated "sky island" smoke-test feature
//! (a much smaller proof-of-concept for high-altitude authored voxels, at a
//! different altitude) is not part of the Aerial Citadel and is not ported
//! here.
//!
//! There is no terrain-level cannon geometry here: every tower cannon is a
//! physical entity, so the terrain volume at each cannon emplacement is
//! simply left clear (air).
//!
//! No NPC/entity/loot placement happens here -- physical terrain only. The
//! `pub` anchor accessors below exist so other code (e.g. entity
//! placement) can resolve tower positions without duplicating this
//! module's geometry.
//!
//! ## Naming
//!
//! This repo has a pre-existing, unrelated `world/src/site/plot/citadel.rs`
//! and `SiteKind::Citadel`. Every identifier here uses `aerial_citadel`
//! (never a bare `citadel`) to avoid any confusion with that feature.

use crate::{Canvas, config::CONFIG};
use common::{
    assets::{AssetExt, BoxedError, FileAsset, load_ron},
    terrain::{Block, BlockKind, SpriteKind},
};
use serde::Deserialize;
use std::borrow::Cow;
use tracing::warn;
use vek::*;

const AERIAL_CITADEL_ASSET: &str = "world.map.cromatolis_v0_aerial_features";
const EXPECTED_SCHEMA: &str = "xindeler_open_world.aerial_citadel.v1";
const EXPECTED_COORDINATE_SPACE: &str = "citadel_relative_meters";

const EMPTY_AIR: Block = Block::empty();

// ---------------------------------------------------------------------
// RON data model.
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct AerialCitadelConfig {
    schema: String,
    coordinate_space: String,
    center: (i32, i32),
    altitude_above_sea_m: i32,
    max_radius_m: i32,
    mountain_center: (i32, i32),
    mountain_height_m: i32,
    snowline_above_deck_m: i32,
    castle_center: (i32, i32),
    castle_floor_above_deck_m: i32,
    castle_wall_height_m: i32,
    wall_height_m: i32,
    wall_tower_radius_m: i32,
    wall_tower_height_extra_m: i32,
    tower_stair_width_m: i32,
    pilot_dome_radius_m: i32,
    pilot_dome_height_m: i32,
    lower_pilot_tower_height_m: i32,
    lower_tower_stair_phase: i32,
    wall_towers: Vec<(i32, i32)>,
}

impl FileAsset for AerialCitadelConfig {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CromatolisSkyCitadelVoxel {
    Air,
    Grass,
    Earth,
    Rock,
    Snow,
    Stone,
    Wood,
    Lantern,
}

impl AerialCitadelConfig {
    fn validate(&self) -> Result<(), String> {
        if self.schema != EXPECTED_SCHEMA {
            return Err(format!(
                "expected schema {EXPECTED_SCHEMA}, got {}",
                self.schema
            ));
        }
        if self.coordinate_space != EXPECTED_COORDINATE_SPACE {
            return Err(format!(
                "expected coordinate space {EXPECTED_COORDINATE_SPACE}, got {}",
                self.coordinate_space
            ));
        }
        if self.wall_towers.is_empty() {
            return Err("wall_towers must not be empty".to_string());
        }
        Ok(())
    }

    fn center(&self) -> Vec2<i32> { Vec2::new(self.center.0, self.center.1) }

    fn mountain_center(&self) -> Vec2<i32> {
        Vec2::new(self.mountain_center.0, self.mountain_center.1)
    }

    fn castle_center(&self) -> Vec2<i32> { Vec2::new(self.castle_center.0, self.castle_center.1) }

    fn wall_towers(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.wall_towers.iter().map(|&(x, y)| Vec2::new(x, y))
    }

    fn wall_tower(&self, tower_index: usize) -> Option<Vec2<i32>> {
        self.wall_towers
            .get(tower_index)
            .map(|&(x, y)| Vec2::new(x, y))
    }

    fn wall_tower_height(&self) -> i32 { self.wall_height_m + self.wall_tower_height_extra_m }

    /// The spiral reaches the level directly beneath the watch deck. The
    /// hatch is cut above its final turn, so the player never has to
    /// reverse direction into a separate landing just to leave the
    /// stairwell.
    fn wall_tower_final_stair_level(&self) -> i32 { self.wall_tower_height() - 2 }

    fn deck_z(&self, sea_level: i32) -> i32 { sea_level + self.altitude_above_sea_m }

    fn footprint(&self, relative: Vec2<i32>) -> f32 {
        let elliptical_field = |origin: Vec2<i32>, radius: Vec2<f32>| {
            let offset = relative - origin;
            1.0 - (offset.x as f32 / radius.x).powi(2) - (offset.y as f32 / radius.y).powi(2)
        };

        elliptical_field(Vec2::zero(), Vec2::new(360.0, 330.0))
            .max(elliptical_field(
                Vec2::new(-190, 40),
                Vec2::new(250.0, 190.0),
            ))
            .max(elliptical_field(
                Vec2::new(190, -90),
                Vec2::new(220.0, 210.0),
            ))
            .max(elliptical_field(
                Vec2::new(30, 190),
                Vec2::new(250.0, 170.0),
            ))
    }

    /// Returns coordinates measured relative to the local city-facing axis
    /// of the mountain. Positive `forward` points from the mountain toward
    /// the main city shelf; that is the deliberately steeper face carrying
    /// the castle facade.
    fn local_axes(&self, relative: Vec2<i32>, origin: Vec2<i32>) -> (f32, f32) {
        let offset = relative - origin;
        const SQRT_FIVE: f32 = 2.236_068;
        (
            (-2 * offset.x + offset.y) as f32 / SQRT_FIVE,
            (-offset.x - 2 * offset.y) as f32 / SQRT_FIVE,
        )
    }

    #[cfg(test)]
    fn local_offset(&self, forward: i32, side: i32) -> Vec2<i32> {
        const SQRT_FIVE: f32 = 2.236_068;
        Vec2::new(
            ((-2 * forward - side) as f32 / SQRT_FIVE).round() as i32,
            ((forward - 2 * side) as f32 / SQRT_FIVE).round() as i32,
        )
    }

    fn mountain_height(&self, relative: Vec2<i32>, edge_drop: i32) -> i32 {
        let (forward, side) = self.local_axes(relative, self.mountain_center());
        let forward_radius = if forward >= 0.0 { 115.0 } else { 205.0 };
        let main_field =
            (1.0 - (forward / forward_radius).powi(2) - (side / 165.0).powi(2)).clamped(0.0, 1.0);

        // A lower shoulder breaks the circular cone silhouette without
        // creating a second summit. It gives the mountain a rockier flank
        // above the city.
        let shoulder_origin = self.mountain_center() + Vec2::new(55, 72);
        let (shoulder_forward, shoulder_side) = self.local_axes(relative, shoulder_origin);
        let shoulder_field =
            (1.0 - (shoulder_forward / 120.0).powi(2) - (shoulder_side / 95.0).powi(2))
                .clamped(0.0, 1.0);

        // A higher exponent narrows the upper mass, producing a true summit
        // rather than a broad, flattened highland.
        let main_height = main_field.powf(2.35) * (self.mountain_height_m + edge_drop) as f32;
        let shoulder_height = shoulder_field.powf(1.8) * 95.0;
        main_height
            .max(shoulder_height)
            .round()
            .min((self.mountain_height_m + edge_drop) as f32) as i32
    }

    fn surface_z(&self, relative: Vec2<i32>, deck: i32) -> Option<i32> {
        let footprint = self.footprint(relative);
        if footprint <= 0.0 {
            return None;
        }

        let edge_drop = ((1.0 - footprint.clamped(0.0, 1.0)) * 20.0).ceil() as i32;
        // Compensating the deck edge drop keeps the 200 m summit exact even
        // though its base stands on the uneven floating-island shelf.
        let mountain_height = self.mountain_height(relative, edge_drop);

        Some(deck - edge_drop + mountain_height)
    }

    fn island_thickness(&self, relative: Vec2<i32>) -> i32 {
        20 + (self.footprint(relative).clamped(0.0, 1.0) * 140.0).round() as i32
    }

    /// Conservative (never under-counts) squared bound on how far a tower's
    /// effects -- its own footprint radius, the pilot dome, or the
    /// extended-cannon muzzle emplacement (`forward` 1..=18, `lateral`
    /// <=4) -- can reach horizontally from its centre. Used to prune the 24
    /// authored towers down to the handful that could plausibly affect a
    /// given column before doing any of the comparatively expensive
    /// per-tower altitude/axis work.
    fn tower_reach_radius_squared(&self) -> i32 {
        let cannon_reach = ((18 * 18 + 4 * 4) as f32).sqrt();
        let reach =
            (self.wall_tower_radius_m.max(self.pilot_dome_radius_m) as f32).max(cannon_reach) + 1.0;
        (reach.ceil() as i32).pow(2)
    }

    /// Resolves every column-invariant quantity the castle's voxel decision
    /// depends on, once, regardless of `z`. Returns `None` if the column
    /// has no castle structure at all (outside every core/gatehouse/corner
    /// box) or lies outside the island footprint entirely.
    fn build_castle_context(
        &self,
        relative: Vec2<i32>,
        deck: i32,
        terrain_surface: Option<i32>,
    ) -> Option<CastleColumnContext> {
        let (forward, side) = self.local_axes(relative, self.castle_center());
        let forward = forward.round() as i32;
        let side = side.round() as i32;
        let floor = deck + self.castle_floor_above_deck_m;
        let core_top = floor + self.castle_wall_height_m + 8;
        let gatehouse_top = floor + self.castle_wall_height_m;
        let in_core = (-22..=14).contains(&forward) && side.abs() <= 24;
        let in_gatehouse = (15..=29).contains(&forward) && side.abs() <= 13;
        // Four hand-fit corner towers embedded in the keep. Kept as a
        // code-level literal (unlike the 24 external perimeter towers,
        // which live in the RON asset): these coordinates are tightly
        // coupled to the core/gatehouse box math immediately around them,
        // not standalone authored anchors consumed anywhere else.
        let towers: [(i32, i32, i32, i32); 4] = [
            (17, -25, 10, floor + 42),
            (17, 25, 10, floor + 42),
            (-17, -23, 10, floor + 50),
            (-17, 23, 10, floor + 50),
        ];
        let tower_match =
            towers
                .into_iter()
                .find_map(|(tower_forward, tower_side, radius, top)| {
                    let delta_forward = forward - tower_forward;
                    let delta_side = side - tower_side;
                    (delta_forward.pow(2) + delta_side.pow(2) <= radius.pow(2)).then_some((
                        delta_forward,
                        delta_side,
                        radius,
                        top,
                    ))
                });
        let top = tower_match
            .map(|(_, _, _, top)| top)
            .or(in_core.then_some(core_top))
            .or(in_gatehouse.then_some(gatehouse_top))?;

        let terrain_surface = terrain_surface?;
        let foundation_base = terrain_surface.min(floor);
        let gate_open = in_gatehouse && forward >= 27 && side.abs() <= 5;
        let battlement_pattern = (forward + side).rem_euclid(6) < 3;
        let tower_result = tower_match.map(|(delta_forward, delta_side, radius, _)| {
            let tower_wall = delta_forward.pow(2) + delta_side.pow(2) >= (radius - 2).pow(2);
            if tower_wall {
                CromatolisSkyCitadelVoxel::Stone
            } else {
                CromatolisSkyCitadelVoxel::Air
            }
        });
        let core_wall = in_core && (forward <= -19 || forward >= 11 || side.abs() >= 21);
        let gatehouse_wall = in_gatehouse && (forward >= 27 || side.abs() >= 10);

        Some(CastleColumnContext {
            top,
            floor,
            foundation_base,
            gate_open,
            battlement_pattern,
            tower_result,
            core_or_gatehouse_wall: core_wall || gatehouse_wall,
        })
    }

    /// Resolves every column-invariant quantity one tower's voxel decision
    /// depends on, once, regardless of `z`. See [`TowerBuildResult`] for
    /// the three possible outcomes.
    fn build_tower_context(
        &self,
        tower_center: Vec2<i32>,
        relative: Vec2<i32>,
        deck: i32,
        terrain_surface: Option<i32>,
        reach_squared: i32,
    ) -> TowerBuildResult {
        let offset = relative - tower_center;
        let distance_squared = offset.magnitude_squared();
        if distance_squared > reach_squared {
            return TowerBuildResult::NotReachable;
        }

        let radius_squared = self.wall_tower_radius_m.pow(2);
        let tower_surface = self
            .surface_z(tower_center, deck)
            .expect("perimeter tower centers must remain inside the island footprint");
        let top = tower_surface + self.wall_tower_height();
        let roof_top = top + self.pilot_dome_height_m;
        let inward = tower_inward_axis(tower_center);
        let outward = -inward;
        let side = Vec2::new(-outward.y, outward.x);
        let forward = offset.x * outward.x + offset.y * outward.y;
        let lateral = offset.x * side.x + offset.y * side.y;

        if distance_squared > radius_squared {
            return TowerBuildResult::Reachable(TowerColumnContext {
                tower_surface,
                top,
                roof_top,
                forward,
                lateral,
                offset,
                inside: None,
            });
        }

        let Some(terrain_surface) = terrain_surface else {
            // Inside this tower's own footprint radius, but the general
            // island surface is undefined here (footprint <= 0). Mirrors
            // the source's `terrain_surface?` early return, which
            // propagates out of the whole per-position wall lookup rather
            // than falling back to another tower or the contour band.
            return TowerBuildResult::ForceNone;
        };
        let foundation_base = terrain_surface.min(tower_surface);
        let entrance_depth = offset.x * inward.x + offset.y * inward.y;
        let entrance_width = offset.x * inward.y - offset.y * inward.x;
        let outer_ring = distance_squared >= (self.wall_tower_radius_m - 3).pow(2);
        let inner_roof = distance_squared <= (self.wall_tower_radius_m - 3).pow(2);
        let on_outer_edge = distance_squared >= (self.wall_tower_radius_m - 1).pow(2);
        let support = matches!((offset.x, offset.y), (-7, -7) | (-7, 7) | (7, -7) | (7, 7));
        let roof_hatch_is_at_offset = self.tower_hatch_is_at(offset);
        let is_hatch_transition_step = offset == self.tower_hatch_transition_step();

        let underside = tower_surface - self.island_thickness(tower_center);
        let lower_floor = underside - self.lower_pilot_tower_height_m;
        let lower_hatch = self.lower_tower_hatch_center();
        let lower_entry_floor = self
            .surface_z(tower_center + lower_hatch, deck)
            .expect("the lower hatch must remain inside the island footprint")
            .max(tower_surface);
        let hatch_is_at_offset = self.lower_tower_hatch_is_at(offset);
        let landing_is_wall = distance_squared >= (self.wall_tower_radius_m - 5).pow(2);
        let central_platform = offset.x.abs() <= 5 && offset.y.abs() <= 5;
        let east_west_beam = offset.y.abs() <= 0 && (6..=10).contains(&offset.x.abs());
        let north_south_beam = offset.x.abs() <= 0 && (6..=10).contains(&offset.y.abs());

        TowerBuildResult::Reachable(TowerColumnContext {
            tower_surface,
            top,
            roof_top,
            forward,
            lateral,
            offset,
            inside: Some(TowerInsideContext {
                foundation_base,
                entrance_depth,
                entrance_width,
                terrain_surface,
                outer_ring,
                inner_roof,
                on_outer_edge,
                support,
                roof_hatch_is_at_offset,
                is_hatch_transition_step,
                lower: Some(TowerLowerColumnContext {
                    lower_floor,
                    lower_entry_floor,
                    hatch_is_at_offset,
                    landing_is_wall,
                    platform_or_beam: central_platform || east_west_beam || north_south_beam,
                }),
            }),
        })
    }

    /// Resolves every column-invariant quantity for the whole citadel at
    /// `wpos2d`, once. This is the seam that lets
    /// [`apply_cromatolis_local_aerial_features_to`] pay for `surface_z`'s
    /// trig, each candidate tower's altitude/axis math, and the castle's
    /// local-axis/corner-tower lookup exactly once per column, then reuse
    /// the result across the citadel's full vertical extent instead of
    /// recomputing all of it at every `z`.
    fn build_column(&self, wpos2d: Vec2<i32>, sea_level: i32) -> ColumnContext {
        let deck = self.deck_z(sea_level);
        let relative = wpos2d - self.center();
        let terrain_surface = self.surface_z(relative, deck);
        let thickness = self.island_thickness(relative);
        let is_snow =
            terrain_surface.is_some_and(|surface| surface >= deck + self.snowline_above_deck_m);

        let castle = self.build_castle_context(relative, deck, terrain_surface);

        let reach_squared = self.tower_reach_radius_squared();
        let mut towers = Vec::new();
        let mut wall_force_none = false;
        for tower_center in self.wall_towers() {
            match self.build_tower_context(
                tower_center,
                relative,
                deck,
                terrain_surface,
                reach_squared,
            ) {
                TowerBuildResult::NotReachable => {},
                TowerBuildResult::ForceNone => wall_force_none = true,
                TowerBuildResult::Reachable(context) => towers.push(context),
            }
        }

        // A narrow contour band follows the authored asymmetric footprint
        // instead of approximating it with a circular fence. Only reached
        // (in `ColumnContext::wall_at`) when the column falls inside no
        // tower's own radius at all.
        let contour = terrain_surface.and_then(|surface| {
            let footprint = self.footprint(relative);
            if !(0.055..=0.105).contains(&footprint) {
                return None;
            }
            Some(ContourColumnContext {
                top: surface + self.wall_height_m,
                terrain_surface: surface,
                is_gap_pattern: (relative.x + relative.y).rem_euclid(5) >= 3,
            })
        });

        ColumnContext {
            terrain_surface,
            thickness,
            is_snow,
            castle,
            wall_force_none,
            towers,
            contour,
        }
    }

    /// Test-only single-position wrapper over [`Self::build_column`] +
    /// [`ColumnContext::castle_at`]; production code (the real entry
    /// point) builds one `ColumnContext` per column and calls
    /// `ColumnContext::voxel_at` directly for every `z`, rather than
    /// rebuilding the whole column per position.
    #[cfg(test)]
    fn castle_voxel_at(
        &self,
        wpos: Vec3<i32>,
        sea_level: i32,
    ) -> Option<CromatolisSkyCitadelVoxel> {
        self.build_column(wpos.xy(), sea_level).castle_at(wpos.z)
    }

    fn tower_stair_is_at(&self, offset: Vec2<i32>, step: i32) -> bool {
        // Three horizontal blocks per vertical step produce a gentle climb.
        // The 40 m tower therefore gets more than two turns of stair while
        // each tread remains pressed against the interior wall.
        (0..3).any(|tread| {
            let outer = tower_spiral_step(step * 3 + tread);
            let inward = spiral_inward(outer);
            // Six blocks of radial width make the established upper route
            // forgiving to traverse, while keeping its existing spiral
            // path.
            (0..self.tower_stair_width_m).any(|inset| offset == outer + inward * inset)
        })
    }

    fn lower_tower_stair_is_at(&self, offset: Vec2<i32>, descent: i32) -> bool {
        // Deliberately identical to the accepted upper-tower stair profile:
        // three successive ring positions per rise, each six blocks wide
        // toward the centre. This forms a continuous stone spiral rather
        // than isolated posts beneath the hatch.
        (0..3).any(|tread| {
            let outer = tower_spiral_step(self.lower_tower_stair_phase + descent * 3 + tread);
            let inward = spiral_inward(outer);
            (0..self.tower_stair_width_m).any(|inset| offset == outer + inward * inset)
        })
    }

    fn lower_tower_hatch_center(&self) -> Vec2<i32> {
        let outer = tower_spiral_step(self.lower_tower_stair_phase + 1);
        let inward = spiral_inward(outer);
        // Keep the descending hatch well inside the circular chamber, away
        // from the exterior ground-level entrance. It belongs to the tower
        // interior, opposite the existing upward route.
        outer + inward * 2
    }

    fn lower_tower_hatch_is_at(&self, offset: Vec2<i32>) -> bool {
        let delta = offset - self.lower_tower_hatch_center();
        // The descent needs more than a square shaft: the opening follows
        // the first three physical spiral levels, so a player can take a
        // couple of real steps before the floor closes above them. This
        // keeps the hatch away from the exterior entrance while removing
        // the old gap between the top floor and the first safe tread.
        (delta.x.abs() <= 2 && delta.y.abs() <= 2)
            || (0..3).any(|descent| self.lower_tower_stair_is_at(offset, descent))
    }

    fn lower_tower_lantern_is_at(&self, offset: Vec2<i32>, descent: i32) -> bool {
        let outer = tower_spiral_step(self.lower_tower_stair_phase + descent * 3 + 1);
        let inward = spiral_inward(outer);
        offset == outer + inward * self.tower_stair_width_m
    }

    fn tower_hatch_center(&self) -> Vec2<i32> {
        // Keep the opening one tread-width inside the final turn. The last
        // spiral tread still runs below its inner edge, but the 3x3
        // opening stays clear of the outer wall and does not make the
        // player reverse into a hairpin.
        let final_middle = tower_spiral_step(self.wall_tower_final_stair_level() * 3 + 1);
        let inward = spiral_inward(final_middle);
        // The compact exit is shifted one additional metre toward the
        // island exterior without changing the already-walkable spiral
        // itself.
        final_middle + inward * 3 + Vec2::new(1, -1)
    }

    fn tower_hatch_transition_step(&self) -> Vec2<i32> {
        let final_middle = tower_spiral_step(self.wall_tower_final_stair_level() * 3 + 1);
        let inward = spiral_inward(final_middle);

        // One final tread bridges the spiral's last level to the roof
        // opening.
        final_middle + inward * 3
    }

    fn tower_hatch_is_at(&self, offset: Vec2<i32>) -> bool {
        // A compact 3x3 shaft covers the final three-wide tread and still
        // leaves a solid watch deck around it.
        let center = self.tower_hatch_center();
        let delta = offset - center;
        delta.x.abs() <= 1 && delta.y.abs() <= 1
    }

    fn tower_lantern_is_at(&self, offset: Vec2<i32>, stair_level: i32) -> bool {
        let stair = tower_spiral_step((stair_level - 3) * 3 + 1);
        let inward = spiral_inward(stair);
        offset == stair + inward * self.tower_stair_width_m
    }

    /// Test-only single-position wrapper; see [`Self::castle_voxel_at`].
    #[cfg(test)]
    fn wall_voxel_at(&self, wpos: Vec3<i32>, sea_level: i32) -> Option<CromatolisSkyCitadelVoxel> {
        self.build_column(wpos.xy(), sea_level)
            .wall_at(self, wpos.z)
    }

    /// Test-only single-position wrapper; see [`Self::castle_voxel_at`].
    #[cfg(test)]
    fn voxel_at(&self, wpos: Vec3<i32>, sea_level: i32) -> Option<CromatolisSkyCitadelVoxel> {
        self.build_column(wpos.xy(), sea_level)
            .voxel_at(self, wpos.z)
    }
}

// ---------------------------------------------------------------------
// Per-column resolved geometry (see the module doc's "Per-column geometry,
// not per-voxel" section). Every field here is column-invariant -- none of
// it depends on `z` -- so it is computed once by `AerialCitadelConfig::
// build_column` and reused for every `z` sampled at that column.
// ---------------------------------------------------------------------

struct ColumnContext {
    terrain_surface: Option<i32>,
    thickness: i32,
    is_snow: bool,
    castle: Option<CastleColumnContext>,
    /// Mirrors the source's `terrain_surface?` early return from inside
    /// the wall-tower lookup: set when the column falls inside some
    /// tower's own radius but the general island surface is undefined
    /// there. When set, the wall lookup returns `None` for every `z`,
    /// never falling back to another tower or the contour band.
    wall_force_none: bool,
    /// The (typically 0-2) towers whose reach could plausibly affect this
    /// column, in ascending tower-index order -- see
    /// [`AerialCitadelConfig::tower_reach_radius_squared`].
    towers: Vec<TowerColumnContext>,
    contour: Option<ContourColumnContext>,
}

struct CastleColumnContext {
    top: i32,
    floor: i32,
    foundation_base: i32,
    gate_open: bool,
    battlement_pattern: bool,
    tower_result: Option<CromatolisSkyCitadelVoxel>,
    core_or_gatehouse_wall: bool,
}

struct ContourColumnContext {
    top: i32,
    terrain_surface: i32,
    is_gap_pattern: bool,
}

/// Outcome of resolving one tower's column-invariant context.
enum TowerBuildResult {
    /// Outside even this tower's generous reach bound -- no effect on this
    /// column at any `z`.
    NotReachable,
    /// See [`ColumnContext::wall_force_none`].
    ForceNone,
    Reachable(TowerColumnContext),
}

struct TowerColumnContext {
    tower_surface: i32,
    top: i32,
    roof_top: i32,
    forward: i32,
    lateral: i32,
    offset: Vec2<i32>,
    /// `Some` only when the column falls inside this tower's own radius
    /// (`distance_squared <= wall_tower_radius_m^2`) -- in that case this
    /// tower is the sole authority for every `z` at this column.
    inside: Option<TowerInsideContext>,
}

struct TowerInsideContext {
    foundation_base: i32,
    entrance_depth: i32,
    entrance_width: i32,
    terrain_surface: i32,
    /// `distance_squared >= (wall_tower_radius_m - 3)^2`.
    outer_ring: bool,
    /// `distance_squared <= (wall_tower_radius_m - 3)^2`. Not simply
    /// `!outer_ring`: the boundary value satisfies both, matching the
    /// source's independent `>=`/`<=` checks there (the roof's `Wood` cap
    /// takes priority over its `Stone` fallback exactly on that boundary).
    inner_roof: bool,
    on_outer_edge: bool,
    support: bool,
    roof_hatch_is_at_offset: bool,
    is_hatch_transition_step: bool,
    lower: Option<TowerLowerColumnContext>,
}

struct TowerLowerColumnContext {
    lower_floor: i32,
    lower_entry_floor: i32,
    hatch_is_at_offset: bool,
    landing_is_wall: bool,
    platform_or_beam: bool,
}

impl ColumnContext {
    fn castle_at(&self, z: i32) -> Option<CromatolisSkyCitadelVoxel> {
        let c = self.castle.as_ref()?;
        if !(c.foundation_base..=c.top).contains(&z) {
            return None;
        }

        // The entire footprint is carried down to the floating terrain.
        // This prevents a castle floor from hanging in open air on an
        // uneven shelf.
        if z <= c.floor {
            return Some(CromatolisSkyCitadelVoxel::Stone);
        }

        if c.gate_open && z <= c.floor + 10 {
            return Some(CromatolisSkyCitadelVoxel::Air);
        }

        if z == c.top - 1 || (z == c.top && c.battlement_pattern) {
            return Some(CromatolisSkyCitadelVoxel::Stone);
        }

        if let Some(result) = c.tower_result {
            return Some(result);
        }

        Some(if c.core_or_gatehouse_wall {
            CromatolisSkyCitadelVoxel::Stone
        } else {
            CromatolisSkyCitadelVoxel::Air
        })
    }

    fn wall_at(&self, config: &AerialCitadelConfig, z: i32) -> Option<CromatolisSkyCitadelVoxel> {
        if self.wall_force_none {
            return None;
        }

        for tower in &self.towers {
            if let Some(result) = tower.resolve(config, z) {
                return Some(result);
            }
            if tower.inside.is_some() {
                // This tower unconditionally owns every `z` at this column
                // once it falls inside its footprint radius: `resolve`
                // already returned `Some` for the z-ranges it recognizes,
                // so reaching here means `z` is outside this tower's
                // vertical band, which must surface as "no wall data" for
                // the whole column -- never falling through to another
                // (unreachable, by construction) tower or the contour band.
                return None;
            }
        }

        let contour = self.contour.as_ref()?;
        if !(contour.terrain_surface..=contour.top).contains(&z) {
            return None;
        }
        Some(if z == contour.top && contour.is_gap_pattern {
            CromatolisSkyCitadelVoxel::Air
        } else {
            CromatolisSkyCitadelVoxel::Stone
        })
    }

    fn island_at(&self, z: i32) -> Option<CromatolisSkyCitadelVoxel> {
        let surface = self.terrain_surface?;
        let bottom = surface - self.thickness;
        if !(bottom..=surface).contains(&z) {
            return None;
        }

        if z == surface {
            Some(if self.is_snow {
                CromatolisSkyCitadelVoxel::Snow
            } else {
                CromatolisSkyCitadelVoxel::Grass
            })
        } else if z >= surface - 4 {
            Some(CromatolisSkyCitadelVoxel::Earth)
        } else {
            Some(CromatolisSkyCitadelVoxel::Rock)
        }
    }

    fn voxel_at(&self, config: &AerialCitadelConfig, z: i32) -> Option<CromatolisSkyCitadelVoxel> {
        self.castle_at(z)
            .or_else(|| self.wall_at(config, z))
            .or_else(|| self.island_at(z))
    }
}

impl TowerColumnContext {
    fn resolve(&self, config: &AerialCitadelConfig, z: i32) -> Option<CromatolisSkyCitadelVoxel> {
        let in_pilot_dome = (self.top + 1..=self.top + config.pilot_dome_height_m).contains(&z)
            && self.offset.magnitude_squared() <= config.pilot_dome_radius_m.pow(2);

        let Some(inside) = &self.inside else {
            // The field itself is a client-transparent mesh attached to
            // the pilot cannon; clear the old voxel shell here (voxel
            // sprites have no alpha channel and would make it opaque).
            // Every cannon is likewise a physical entity, so its terrain
            // emplacement volume is simply left clear.
            let is_extended_cannon = (1..=18).contains(&self.forward)
                && self.lateral.abs() <= 4
                && (self.top + 1..=self.top + 5).contains(&z);
            return if in_pilot_dome || is_extended_cannon {
                Some(CromatolisSkyCitadelVoxel::Air)
            } else {
                None
            };
        };

        if let Some(lower) = &inside.lower {
            // Clear every local floor layer above the canonical hatch
            // level. Otherwise an adjacent slope block can form an
            // invisible stone lid over part of the elongated opening.
            if lower.hatch_is_at_offset
                && (lower.lower_entry_floor..=inside.terrain_surface.max(self.tower_surface))
                    .contains(&z)
            {
                return Some(CromatolisSkyCitadelVoxel::Air);
            }
            // This is an independent descending interior, cut only below
            // the accepted upper tower floor. It exits into a hanging
            // tower; the upper spiral and hatch above are never changed.
            if (lower.lower_floor..self.tower_surface).contains(&z) {
                if z == lower.lower_floor {
                    // A solid annular landing receives the final tread.
                    // The open centre remains reserved for the visual
                    // energy floor and the view into the precipice.
                    return Some(if lower.landing_is_wall {
                        CromatolisSkyCitadelVoxel::Stone
                    } else {
                        CromatolisSkyCitadelVoxel::Air
                    });
                }
                if z == lower.lower_floor + 1 && lower.platform_or_beam {
                    // The inverted cannon is mounted from a centred stone
                    // deck rather than floating over the lookout.
                    return Some(CromatolisSkyCitadelVoxel::Stone);
                }
                if inside.outer_ring {
                    return Some(CromatolisSkyCitadelVoxel::Stone);
                }
                let descent = lower.lower_entry_floor - 1 - z;
                if descent >= 0 && config.lower_tower_stair_is_at(self.offset, descent) {
                    return Some(CromatolisSkyCitadelVoxel::Stone);
                }
                if descent.rem_euclid(4) == 0
                    && config.lower_tower_lantern_is_at(self.offset, descent)
                {
                    return Some(CromatolisSkyCitadelVoxel::Lantern);
                }
                return Some(CromatolisSkyCitadelVoxel::Air);
            }
            // The hatch may lie on a locally higher slope than the tower
            // centre. Preserve the upper tower's solid floor at its datum,
            // but continue the first treads through that short slope gap.
            if (self.tower_surface + 1..lower.lower_entry_floor).contains(&z) {
                let descent = lower.lower_entry_floor - 1 - z;
                if config.lower_tower_stair_is_at(self.offset, descent) {
                    return Some(CromatolisSkyCitadelVoxel::Stone);
                }
                if descent.rem_euclid(4) == 0
                    && config.lower_tower_lantern_is_at(self.offset, descent)
                {
                    return Some(CromatolisSkyCitadelVoxel::Lantern);
                }
            }
        }

        if !(inside.foundation_base..=self.roof_top).contains(&z) {
            return None;
        }

        let entrance = inside.entrance_depth >= config.wall_tower_radius_m - 3
            && inside.entrance_width.abs() <= 3
            && z > inside.terrain_surface
            && z <= self.tower_surface + 7;
        if entrance {
            return Some(CromatolisSkyCitadelVoxel::Air);
        }

        if z <= self.tower_surface {
            return Some(CromatolisSkyCitadelVoxel::Stone);
        }

        // Test the hatch before the now-wide final stair tread. The stair
        // continues into the shaft below, but must not refill the
        // established roof opening itself.
        let hatch_shaft = inside.roof_hatch_is_at_offset && (self.top..=self.top + 2).contains(&z);
        if hatch_shaft {
            return Some(CromatolisSkyCitadelVoxel::Air);
        }

        let stair_level = z - self.tower_surface - 1;
        let highest_stair_level = config.wall_tower_final_stair_level();
        if (0..=highest_stair_level).contains(&stair_level)
            && (config.tower_stair_is_at(self.offset, stair_level)
                || (stair_level == highest_stair_level && inside.is_hatch_transition_step))
        {
            return Some(CromatolisSkyCitadelVoxel::Stone);
        }

        if (5..=highest_stair_level).contains(&stair_level)
            && stair_level.rem_euclid(8) == 0
            && config.tower_lantern_is_at(self.offset, stair_level)
        {
            return Some(CromatolisSkyCitadelVoxel::Lantern);
        }

        if z == self.top {
            return Some(if inside.on_outer_edge {
                CromatolisSkyCitadelVoxel::Wood
            } else {
                CromatolisSkyCitadelVoxel::Stone
            });
        }

        // The pilot tower's old posts and solid wooden roof are replaced
        // by a client-rendered, hollow transparent field. The deck and the
        // stair/hatch below this height have already returned above and
        // remain unchanged.
        if in_pilot_dome {
            return Some(CromatolisSkyCitadelVoxel::Air);
        }

        if (self.top + 1..self.roof_top).contains(&z) {
            // The watch level deliberately has broad, unobstructed
            // openings: just four roof posts above a low perimeter rail.
            return Some(if inside.support {
                CromatolisSkyCitadelVoxel::Wood
            } else {
                CromatolisSkyCitadelVoxel::Air
            });
        }
        if z == self.roof_top && inside.support {
            return Some(CromatolisSkyCitadelVoxel::Wood);
        }
        if z == self.roof_top && inside.inner_roof {
            return Some(CromatolisSkyCitadelVoxel::Wood);
        }
        Some(if inside.outer_ring || z == inside.terrain_surface {
            CromatolisSkyCitadelVoxel::Stone
        } else {
            CromatolisSkyCitadelVoxel::Air
        })
    }
}

// ---------------------------------------------------------------------
// Free helpers with no config dependency.
// ---------------------------------------------------------------------

fn tower_inward_axis(tower_center: Vec2<i32>) -> Vec2<i32> {
    if tower_center.x.abs() >= tower_center.y.abs() {
        Vec2::new(-tower_center.x.signum(), 0)
    } else {
        Vec2::new(0, -tower_center.y.signum())
    }
}

/// The direction pointing from an outer spiral-ring point back toward the
/// tower's centre axis, used to lay treads inward from the ring.
fn spiral_inward(outer: Vec2<i32>) -> Vec2<i32> {
    if outer.x.abs() >= outer.y.abs() {
        Vec2::new(-outer.x.signum(), 0)
    } else {
        Vec2::new(0, -outer.y.signum())
    }
}

fn tower_spiral_step(step: i32) -> Vec2<i32> {
    // A 52-block octagonal ring follows the inside edge of the round
    // tower. Consecutive points always touch orthogonally or diagonally,
    // avoiding the square zig-zag made by a Chebyshev perimeter.
    let step = step.rem_euclid(52);
    match step {
        0..=8 => Vec2::new(9, -4 + step),
        9..=12 => {
            let step = step - 9;
            Vec2::new(8 - step, 5 + step)
        },
        13..=21 => Vec2::new(4 - (step - 13), 9),
        22..=25 => {
            let step = step - 22;
            Vec2::new(-5 - step, 8 - step)
        },
        26..=34 => Vec2::new(-9, 4 - (step - 26)),
        35..=38 => {
            let step = step - 35;
            Vec2::new(-8 + step, -5 - step)
        },
        39..=47 => Vec2::new(-4 + (step - 39), -9),
        _ => {
            let step = step - 48;
            Vec2::new(5 + step, -8 + step)
        },
    }
}

// ---------------------------------------------------------------------
// Loading / caching.
// ---------------------------------------------------------------------

fn load_config() -> Option<AerialCitadelConfig> {
    match AerialCitadelConfig::load_owned(AERIAL_CITADEL_ASSET) {
        Ok(config) => match config.validate() {
            Ok(()) => Some(config),
            Err(err) => {
                warn!(%err, "Invalid Cromatolis aerial citadel config, skipping");
                None
            },
        },
        Err(err) => {
            warn!(?err, "Failed to load Cromatolis aerial citadel config");
            None
        },
    }
}

// ---------------------------------------------------------------------
// Public anchor accessors, consumed by other code (e.g. entity placement)
// to resolve tower positions without duplicating this module's geometry.
// Each loads (and validates) the config independently -- these are called
// rarely, never in a per-column hot path, so there is no need for the
// `Index`-level cache the per-chunk entry point below uses.
// ---------------------------------------------------------------------

/// Returns the geometric world-space centre of an authored perimeter
/// tower.
///
/// Dynamic structures that belong to a tower (such as the articulated
/// cannon pilots) must resolve their horizontal pivot through this
/// function instead of duplicating a manually measured world coordinate.
pub fn cromatolis_aerial_citadel_wall_tower_world_center(tower_index: usize) -> Option<Vec2<i32>> {
    let config = load_config()?;
    config
        .wall_tower(tower_index)
        .map(|relative_center| config.center() + relative_center)
}

/// Number of authored perimeter tower anchors around the aerial citadel.
pub fn cromatolis_aerial_citadel_wall_tower_count() -> usize {
    load_config()
        .map(|config| config.wall_towers.len())
        .unwrap_or(0)
}

/// Cardinal direction from a tower centre toward open air.
pub fn cromatolis_aerial_citadel_wall_tower_outward_axis(tower_index: usize) -> Option<Vec2<i32>> {
    let config = load_config()?;
    config
        .wall_tower(tower_index)
        .map(|relative_center| -tower_inward_axis(relative_center))
}

/// Height of the upper watch-deck floor at an authored tower centre.
pub fn cromatolis_aerial_citadel_wall_tower_watch_deck_z(tower_index: usize) -> Option<i32> {
    let config = load_config()?;
    let relative_center = config.wall_tower(tower_index)?;
    let sea_level = CONFIG.sea_level as i32;
    let deck = config.deck_z(sea_level);
    config
        .surface_z(relative_center, deck)
        .map(|surface| surface + config.wall_tower_height())
}

/// Number of authored under-island lookout stations.
///
/// The lower ring deliberately mirrors every existing perimeter tower. The
/// tower centre table remains the sole source of positional truth.
pub fn cromatolis_aerial_citadel_lower_tower_count() -> usize {
    cromatolis_aerial_citadel_wall_tower_count()
}

/// Perimeter tower that structurally owns an under-island station.
pub fn cromatolis_aerial_citadel_lower_tower_upper_index(
    lower_tower_index: usize,
) -> Option<usize> {
    (lower_tower_index < cromatolis_aerial_citadel_lower_tower_count()).then_some(lower_tower_index)
}

/// Centre of an under-island lookout's transparent field floor.
pub fn cromatolis_aerial_citadel_lower_tower_platform_center(
    lower_tower_index: usize,
) -> Option<Vec3<i32>> {
    let config = load_config()?;
    let upper_tower_index = cromatolis_aerial_citadel_lower_tower_upper_index(lower_tower_index)?;
    let relative_center = config.wall_tower(upper_tower_index)?;
    let sea_level = CONFIG.sea_level as i32;
    let deck = config.deck_z(sea_level);
    let surface = config.surface_z(relative_center, deck)?;
    let underside = surface - config.island_thickness(relative_center);
    Some(
        (config.center() + relative_center)
            .with_z(underside - config.lower_pilot_tower_height_m + 1),
    )
}

/// Compatibility helper for the first already-playtested under-island
/// tower.
pub fn cromatolis_aerial_citadel_lower_pilot_platform_center() -> Option<Vec3<i32>> {
    cromatolis_aerial_citadel_lower_tower_platform_center(0)
}

// ---------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------

/// Carves the authored Aerial Citadel above Cromatolis.
pub fn apply_cromatolis_local_aerial_features_to(canvas: &mut Canvas) {
    if !canvas.info().chunk().authored_cromatolis_v0 {
        return;
    }

    let info = canvas.info();
    let index_ref = info.index();
    let Some(config) = index_ref.cromatolis_aerial_citadel.get_or_init(load_config) else {
        return;
    };

    let sea_level = CONFIG.sea_level as i32;
    let center = config.center();
    let radius = config.max_radius_m;
    let bounds = Aabr {
        min: center - Vec2::broadcast(radius),
        max: center + Vec2::broadcast(radius + 1),
    };
    canvas.foreach_col_area(bounds, |canvas, wpos2d, _| {
        let deck = config.deck_z(sea_level);
        let bottom = deck - 180;
        let top = deck + config.mountain_height_m;
        // Resolved once per column and reused across every `z` below --
        // see the module doc's "Per-column geometry, not per-voxel"
        // section.
        let column = config.build_column(wpos2d, sea_level);
        for z in bottom..=top {
            let wpos = wpos2d.with_z(z);
            let block = match column.voxel_at(config, z) {
                Some(CromatolisSkyCitadelVoxel::Air) => EMPTY_AIR,
                Some(CromatolisSkyCitadelVoxel::Grass) => {
                    Block::new(BlockKind::Grass, Rgb::new(76, 124, 62))
                },
                Some(CromatolisSkyCitadelVoxel::Earth) => {
                    Block::new(BlockKind::Earth, Rgb::new(102, 75, 50))
                },
                Some(CromatolisSkyCitadelVoxel::Rock) => {
                    Block::new(BlockKind::Rock, Rgb::new(77, 79, 86))
                },
                Some(CromatolisSkyCitadelVoxel::Snow) => {
                    Block::new(BlockKind::ArtSnow, Rgb::new(235, 239, 244))
                },
                Some(CromatolisSkyCitadelVoxel::Stone) => {
                    Block::new(BlockKind::Rock, Rgb::new(91, 91, 98))
                },
                Some(CromatolisSkyCitadelVoxel::Wood) => {
                    Block::new(BlockKind::Wood, Rgb::new(72, 47, 30))
                },
                Some(CromatolisSkyCitadelVoxel::Lantern) => Block::air(SpriteKind::Lantern),
                None => continue,
            };
            canvas.set(wpos, block);
        }
    });
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn test_config() -> AerialCitadelConfig {
        let config = AerialCitadelConfig::load_owned(AERIAL_CITADEL_ASSET)
            .expect("assets/world/map/cromatolis_v0_aerial_features.ron should load and parse");
        config
            .validate()
            .expect("the real authored asset should pass validation");
        config
    }

    #[test]
    fn real_aerial_features_asset_parses_and_validates_without_panicking() {
        let config = test_config();
        assert_eq!(config.wall_towers.len(), 24);
    }

    #[test]
    fn invalid_schema_fails_validation_without_panicking() {
        let mut config = test_config();
        config.schema = "bogus".to_string();
        // Must return an Err, not panic -- mirrors this codebase's other
        // authored-Cromatolis loaders' hard "never panic on bad data" rule.
        assert!(config.validate().is_err());
    }

    #[test]
    fn missing_asset_fails_to_load_without_panicking() {
        let missing = AerialCitadelConfig::load_owned("world.map.this_asset_does_not_exist");
        assert!(missing.is_err());
    }

    #[test]
    fn cromatolis_aerial_citadel_has_an_asymmetric_800_m_footprint() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let western_lobe = Vec2::new(-340, 90);
        let western_surface = config
            .surface_z(western_lobe, deck)
            .expect("western lobe sample must be inside the floating island");

        assert_eq!(
            config.voxel_at(
                (config.center() + western_lobe).with_z(western_surface),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Grass),
        );
        assert_eq!(
            config.voxel_at(
                (config.center() + Vec2::new(390, 150)).with_z(deck - 20),
                sea_level,
            ),
            None,
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_has_a_200_m_snow_capped_mountain() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let peak = deck + config.mountain_height_m;
        let peak_wpos = (config.center() + config.mountain_center()).with_z(peak);

        assert_eq!(config.surface_z(config.mountain_center(), deck), Some(peak));
        assert_eq!(
            config.voxel_at(peak_wpos, sea_level),
            Some(CromatolisSkyCitadelVoxel::Snow),
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_mountain_tapers_to_a_real_summit() {
        let config = test_config();
        let deck = 140 + config.altitude_above_sea_m;
        let near_peak = config.mountain_center() + config.local_offset(40, 0);

        assert!(
            config.surface_z(near_peak, deck) < Some(deck + config.mountain_height_m - 35),
            "the upper mountain must taper instead of forming a broad plateau",
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_has_a_walkable_grassy_shelf() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let shelf_relative = Vec2::new(-100, 100);
        let shelf = config
            .surface_z(shelf_relative, deck)
            .expect("shelf sample must be inside the aerial feature");

        assert_eq!(
            config.voxel_at((config.center() + shelf_relative).with_z(shelf), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Grass),
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_mountain_is_steeper_toward_the_city_shelf() {
        let config = test_config();
        let deck = 140 + config.altitude_above_sea_m;
        let city_face = config.mountain_center() + config.local_offset(95, 0);
        let outer_face = config.mountain_center() + config.local_offset(-95, 0);

        assert!(
            config.surface_z(city_face, deck) < config.surface_z(outer_face, deck),
            "the city-facing mountain side must drop more sharply",
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_castle_has_a_stone_facade_and_open_gate() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let gate = (config.center() + config.castle_center() + config.local_offset(27, 0))
            .with_z(deck + 6);
        let facade = (config.center() + config.castle_center() + config.local_offset(27, 10))
            .with_z(deck + 12);

        assert_eq!(
            config.castle_voxel_at(gate, sea_level),
            Some(CromatolisSkyCitadelVoxel::Air),
        );
        assert_eq!(
            config.castle_voxel_at(facade, sea_level),
            Some(CromatolisSkyCitadelVoxel::Stone),
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_castle_foundation_reaches_terrain_and_has_towers() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let floor = deck + config.castle_floor_above_deck_m;
        let gate_relative = config.castle_center() + config.local_offset(27, 0);
        let terrain_surface = config
            .surface_z(gate_relative, deck)
            .expect("castle entrance must sit inside the island footprint");
        let foundation = (config.center() + gate_relative).with_z(terrain_surface.min(floor));
        let tower = (config.center() + config.castle_center() + config.local_offset(17, 25))
            .with_z(floor + 41);

        assert_eq!(
            config.castle_voxel_at(foundation, sea_level),
            Some(CromatolisSkyCitadelVoxel::Stone),
        );
        assert_eq!(
            config.castle_voxel_at(tower, sea_level),
            Some(CromatolisSkyCitadelVoxel::Stone),
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_has_24_hollow_supported_wall_towers() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        assert_eq!(config.wall_towers.len(), 24);
        assert_eq!(config.wall_tower_height(), config.wall_height_m + 15);

        for (tower_index, tower_center) in config.wall_towers().enumerate() {
            let surface = config
                .surface_z(tower_center, deck)
                .expect("each perimeter tower must sit inside the island footprint");
            let inward = tower_inward_axis(tower_center);
            let wall_edge = if inward.x != 0 {
                tower_center + Vec2::new(0, config.wall_tower_radius_m - 1)
            } else {
                tower_center + Vec2::new(config.wall_tower_radius_m - 1, 0)
            };
            let wall_edge_surface = config
                .surface_z(wall_edge, deck)
                .expect("the sampled tower wall must remain over the island");

            assert_eq!(
                config.wall_voxel_at((config.center() + tower_center).with_z(surface), sea_level,),
                Some(CromatolisSkyCitadelVoxel::Stone),
                "tower {tower_index} must retain a solid main-tower floor above its lower hatch",
            );
            assert_eq!(
                config.wall_voxel_at(
                    (config.center() + tower_center).with_z(surface + 3),
                    sea_level,
                ),
                Some(CromatolisSkyCitadelVoxel::Air),
            );
            assert_eq!(
                config.wall_voxel_at(
                    (config.center() + wall_edge).with_z(wall_edge_surface + 3),
                    sea_level,
                ),
                Some(CromatolisSkyCitadelVoxel::Stone),
            );
            assert_eq!(
                config.wall_voxel_at(
                    (config.center() + wall_edge).with_z(surface + config.wall_tower_height() - 1),
                    sea_level,
                ),
                Some(CromatolisSkyCitadelVoxel::Stone),
            );
        }
    }

    #[test]
    fn cromatolis_aerial_citadel_pilot_tower_keeps_its_entry_spiral_hatch_beneath_the_force_field()
    {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let tower_center = config.wall_tower(0).unwrap();
        let tower_surface = config
            .surface_z(tower_center, deck)
            .expect("tower must sit inside the aerial island");
        let tower_wpos = config.center() + tower_center;
        let inward = tower_inward_axis(tower_center);
        let doorway = tower_center + inward * (config.wall_tower_radius_m - 2);
        let doorway_surface = config
            .surface_z(doorway, deck)
            .expect("tower doorway must remain inside the aerial island");
        let first_stair_offset = tower_spiral_step(0);
        let first_stair = tower_center + first_stair_offset;
        let first_stair_inward = spiral_inward(first_stair_offset);
        let lantern_level = 8;
        let lantern_offset = tower_spiral_step((lantern_level - 3) * 3 + 1);
        let lantern = tower_center + lantern_offset;
        let lantern_inward = spiral_inward(lantern_offset);
        let top = tower_surface + config.wall_tower_height();
        let highest_stair_level = config.wall_tower_final_stair_level();
        let third_from_top_tread = tower_center + tower_spiral_step((highest_stair_level - 2) * 3);
        let final_tread_offset = tower_spiral_step(highest_stair_level * 3);
        let final_tread = tower_center + final_tread_offset;
        let penultimate_tread_exit =
            tower_center + tower_spiral_step((highest_stair_level - 1) * 3 + 2);
        let final_middle_tread_offset = tower_spiral_step(highest_stair_level * 3 + 1);
        let hatch = tower_center + config.tower_hatch_center();
        let final_tread_inward = spiral_inward(final_middle_tread_offset);
        let hatch_access_tread = tower_center
            + final_middle_tread_offset
            + final_tread_inward * (config.tower_stair_width_m - 1);
        let hatch_transition_step = tower_center + config.tower_hatch_transition_step();
        let roof_support = tower_center + Vec2::new(7, 7);
        let railing = tower_center + Vec2::new(12, 0);

        assert_eq!(
            config.wall_voxel_at(
                (config.center() + doorway).with_z(doorway_surface + 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the city-facing tower door must be passable",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + first_stair).with_z(tower_surface + 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the spiral must begin one block above the tower floor",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center()
                    + first_stair
                    + first_stair_inward * (config.tower_stair_width_m - 1))
                    .with_z(tower_surface + 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the spiral must be six blocks wide rather than forcing a wall-hugging climb",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + tower_center + tower_spiral_step(2)).with_z(tower_surface + 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "each stair level must provide a three-block-long tread",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + tower_center + tower_spiral_step(3)).with_z(tower_surface + 2),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the next rise must begin only after the full tread",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + lantern + lantern_inward * config.tower_stair_width_m)
                    .with_z(tower_surface + lantern_level + 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Lantern),
            "the spiral must be lit at regular intervals",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + third_from_top_tread).with_z(top - 3),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the penultimate spiral turn must remain walkable",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + penultimate_tread_exit).with_z(top - 2),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the final turn must continue the same spiral one level higher",
        );
        assert_eq!(
            config.wall_voxel_at((config.center() + final_tread).with_z(top - 1), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the final three-wide tread must be reached without a reverse turn",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + hatch_access_tread).with_z(top - 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the final tread must meet the inner edge of the hatch",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + hatch_transition_step).with_z(top - 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "one intermediate tread must bridge the final stair to the hatch",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + hatch + Vec2::new(-1, -2)).with_z(top - 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the hatch must leave clear turning space beside the final tread",
        );
        assert_eq!(
            config.wall_voxel_at((config.center() + hatch).with_z(top), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the spiral must open into the watch deck",
        );
        assert_eq!(
            config.wall_voxel_at((config.center() + hatch).with_z(top + 1), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the exit needs a second clear block for the player to climb through it",
        );
        assert!(
            config.tower_hatch_center() != final_middle_tread_offset,
            "the hatch must sit inside the final turn instead of cutting the outer tower wall",
        );
        assert!(
            config.tower_hatch_is_at(config.tower_hatch_center() + Vec2::new(1, 1)),
            "the hatch must provide a three-by-three clear shaft",
        );
        assert!(
            !config.tower_hatch_is_at(config.tower_hatch_center() + Vec2::new(2, 0)),
            "the hatch must not spill beyond the compact three-metre footprint",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(top - 1), sea_level),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the two final stair levels must not be capped by a solid ceiling",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(top), sea_level),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the open hatch must be surrounded by a walkable roof deck",
        );
        assert_eq!(
            config.wall_voxel_at((config.center() + roof_support).with_z(top + 3), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the force-field dome replaces the old wooden roof posts",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(top + 7), sea_level),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the hollow field must not fill the pilot cannon's interior",
        );
        assert_eq!(
            config.wall_voxel_at(
                tower_wpos.with_z(top + config.pilot_dome_height_m),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the transparent client field must leave the tower terrain empty at its apex",
        );
        assert_eq!(
            config.wall_voxel_at((config.center() + railing).with_z(top), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Wood),
            "the watch deck must have a guardrail",
        );
        assert_eq!(
            config.wall_voxel_at(
                (config.center() + railing + Vec2::new(0, 3)).with_z(top + 3),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the watch level must keep an open viewing window alongside the cannon",
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_tower_spiral_follows_a_continuous_inner_ring() {
        let config = test_config();
        for step in 0..52 {
            let current = tower_spiral_step(step);
            let next = tower_spiral_step(step + 1);
            let delta = next - current;

            assert!(
                delta.x.abs() <= 1 && delta.y.abs() <= 1 && delta != Vec2::zero(),
                "spiral step {step} must join directly to the next contour step",
            );
            assert!(
                current.magnitude_squared() < config.wall_tower_radius_m.pow(2),
                "spiral step {step} must stay inside the tower wall",
            );
        }
    }

    #[test]
    fn cromatolis_aerial_citadel_wall_towers_are_placed_every_roughly_94_m() {
        let config = test_config();
        let towers: Vec<Vec2<i32>> = config.wall_towers().collect();
        for index in 0..towers.len() {
            let next = towers[(index + 1) % towers.len()];
            let distance_squared = (towers[index] - next).magnitude_squared();
            assert!(
                (78_i32.pow(2)..=116_i32.pow(2)).contains(&distance_squared),
                "tower {index} has an unexpected {distance_squared} m² spacing to its neighbour",
            );
        }
    }

    #[test]
    fn cromatolis_aerial_citadel_exposes_each_tower_geometric_world_center() {
        let config = test_config();
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_world_center(0),
            Some(Vec2::new(16_749, 16_384)),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_world_center(1),
            Some(Vec2::new(16_696, 16_468)),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_count(),
            config.wall_towers.len(),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_outward_axis(0),
            Some(Vec2::unit_x()),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_outward_axis(1),
            Some(Vec2::unit_x()),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_watch_deck_z(0),
            Some(5_163),
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_watch_deck_z(1),
            Some(5_163),
        );
        assert_eq!(
            cromatolis_aerial_citadel_lower_pilot_platform_center(),
            Some(Vec3::new(16_749, 16_384, 5_063)),
        );
        assert_eq!(
            cromatolis_aerial_citadel_lower_tower_count(),
            cromatolis_aerial_citadel_wall_tower_count(),
            "every perimeter tower must now own one under-island station",
        );
        for lower_tower_index in 0..cromatolis_aerial_citadel_lower_tower_count() {
            let upper_tower_index =
                cromatolis_aerial_citadel_lower_tower_upper_index(lower_tower_index)
                    .expect("every authored lower station must name its parent tower");
            let platform = cromatolis_aerial_citadel_lower_tower_platform_center(lower_tower_index)
                .expect("every authored lower station must have a platform");
            assert_eq!(
                platform.xy(),
                cromatolis_aerial_citadel_wall_tower_world_center(upper_tower_index)
                    .expect("the lower station parent must be an authored perimeter tower"),
                "the lower station must be centred on its own parent tower",
            );
            assert!(
                platform.z
                    < cromatolis_aerial_citadel_wall_tower_watch_deck_z(upper_tower_index)
                        .expect("the parent tower has a watch deck"),
                "the lower station must remain below the island",
            );
        }
        assert_eq!(
            cromatolis_aerial_citadel_lower_tower_upper_index(
                cromatolis_aerial_citadel_lower_tower_count(),
            ),
            None,
            "the lower-ring table must not create an out-of-range twenty-fifth tower",
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_world_center(config.wall_towers.len()),
            None,
            "only the 24 authored tower centres are addressable",
        );
        assert_eq!(
            cromatolis_aerial_citadel_wall_tower_outward_axis(config.wall_towers.len()),
            None,
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_lower_towers_are_half_height_with_independent_descents() {
        let config = test_config();
        let sea_level = CONFIG.sea_level as i32;
        let deck = config.deck_z(sea_level);
        let tower_center = config.wall_tower(0).unwrap();
        let tower_surface = config
            .surface_z(tower_center, deck)
            .expect("the lower pilot must remain attached to an authored tower");
        let lower_floor = tower_surface
            - config.island_thickness(tower_center)
            - config.lower_pilot_tower_height_m;
        let tower_wpos = config.center() + tower_center;
        let first_lower_tread = tower_spiral_step(config.lower_tower_stair_phase + 1);
        let lower_hatch = config.lower_tower_hatch_center();
        let lower_entry_floor = config
            .surface_z(tower_center + lower_hatch, deck)
            .expect("the lower hatch must be cut into the authored tower floor")
            .max(tower_surface);
        let lantern_descent = 4;
        let lantern = tower_spiral_step(config.lower_tower_stair_phase + lantern_descent * 3 + 1);
        let lantern_inward = spiral_inward(lantern);
        let first_lower_inward = spiral_inward(first_lower_tread);

        assert_eq!(config.lower_pilot_tower_height_m, 15);
        assert!(
            lower_hatch.x > -(config.wall_tower_radius_m - 3),
            "the lower hatch must remain inside the tower, not in its exterior entrance",
        );
        assert_eq!(
            config.lower_pilot_tower_height_m,
            config.wall_tower_height() - 25,
            "the lower pilot tower stays 5 m below the prior 20 m prototype",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + Vec2::new(11, 0)).with_z(lower_floor + 10),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the lower tower needs a solid exterior wall beneath the island",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(lower_floor + 10), sea_level),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the lower tower must stay hollow for the descending stairs and lookout",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + lower_hatch).with_z(lower_entry_floor),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the lower spiral requires its own full trapdoor, separate from the upper route",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + lower_hatch + Vec2::new(2, 2)).with_z(
                    config
                        .surface_z(tower_center + lower_hatch + Vec2::new(2, 2), deck)
                        .expect("the entire lower hatch must stay inside the island")
                        .max(tower_surface),
                ),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the elongated lower trapdoor must visibly expose the descending route",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + first_lower_tread).with_z(lower_entry_floor - 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the first lower tread must receive the guard below the new entry",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + first_lower_tread + first_lower_inward * config.tower_stair_width_m)
                    .with_z(lower_entry_floor - 1),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Lantern),
            "the first descent must be visibly lit from the main tower floor",
        );
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + lantern + lantern_inward * config.tower_stair_width_m)
                    .with_z(lower_entry_floor - 1 - lantern_descent),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Lantern),
            "the lower spiral needs recurring lanterns for the descent",
        );
        let third_lower_tread = tower_spiral_step(config.lower_tower_stair_phase + 2 * 3 + 1);
        assert_eq!(
            config.wall_voxel_at(
                (tower_wpos + third_lower_tread).with_z(lower_entry_floor),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the hatch must remain open over multiple initial treads, not stop at a square hole",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(lower_floor), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Air),
            "the lower landing keeps its centre open for the energy floor and downward view",
        );
        assert_eq!(
            config.wall_voxel_at(tower_wpos.with_z(lower_floor + 1), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the lower cannon must rest on the centred suspended mounting platform",
        );

        let second_parent = 1;
        let second_wpos = config.center() + config.wall_tower(second_parent).unwrap();
        let second_platform = cromatolis_aerial_citadel_lower_tower_platform_center(1)
            .expect("the second lower tower must have an authored platform");
        assert_eq!(second_platform.xy(), second_wpos);
        assert_eq!(
            config.wall_voxel_at(second_wpos.with_z(second_platform.z), sea_level,),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the second lower lookout needs its centred suspended cannon platform",
        );
        assert_eq!(
            config.wall_voxel_at(
                (second_wpos + Vec2::new(11, 0)).with_z(second_platform.z + 8),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
            "the second lower lookout needs the same compact structural shell",
        );
    }

    #[test]
    fn cromatolis_aerial_citadel_all_upper_towers_are_clear_for_physical_cannons_and_fields() {
        let config = test_config();
        let sea_level = CONFIG.sea_level as i32;
        let deck = config.deck_z(sea_level);

        for (tower_index, tower_center) in config.wall_towers().enumerate() {
            let tower_surface = config
                .surface_z(tower_center, deck)
                .expect("each perimeter tower must remain supported by the island");
            let top = tower_surface + config.wall_tower_height();
            assert_eq!(
                config.wall_voxel_at((config.center() + tower_center).with_z(top + 7), sea_level,),
                Some(CromatolisSkyCitadelVoxel::Air),
                "tower {} must open its former roof inside the energy dome",
                tower_index + 1,
            );
            assert_eq!(
                config.wall_voxel_at((config.center() + tower_center).with_z(top + 20), sea_level,),
                Some(CromatolisSkyCitadelVoxel::Air),
                "tower {} must keep the energy-dome volume free of terrain",
                tower_index + 1,
            );
        }
    }

    #[test]
    fn cromatolis_aerial_citadel_wall_follows_the_irregular_island_contour() {
        let config = test_config();
        let sea_level = 140;
        let deck = config.deck_z(sea_level);
        let contour_relative = (-440..=440)
            .step_by(8)
            .flat_map(|x| (-440..=440).step_by(8).map(move |y| Vec2::new(x, y)))
            .find(|relative| {
                (0.055..=0.105).contains(&config.footprint(*relative))
                    && config.wall_towers().all(|tower| {
                        (*relative - tower).magnitude_squared()
                            > (config.wall_tower_radius_m + 3).pow(2)
                    })
            })
            .expect("the authored island must expose a non-tower contour wall sample");
        let surface = config
            .surface_z(contour_relative, deck)
            .expect("contour sample must be inside the island footprint");

        assert_eq!(
            config.wall_voxel_at(
                (config.center() + contour_relative).with_z(surface + 4),
                sea_level,
            ),
            Some(CromatolisSkyCitadelVoxel::Stone),
        );
    }

    /// Guards against the pruning optimization in `build_column`
    /// silently under-counting: for every authored tower, sampling well
    /// beyond the tower's own radius but still within its dome/cannon
    /// reach must produce the same result as the un-pruned per-position
    /// API, and every tower must actually get included as "reachable" by
    /// `tower_reach_radius_squared` at that distance.
    #[test]
    fn tower_reach_radius_never_excludes_the_extended_cannon_emplacement() {
        let config = test_config();
        let sea_level = CONFIG.sea_level as i32;
        let deck = config.deck_z(sea_level);
        let reach_squared = config.tower_reach_radius_squared();

        for tower_center in config.wall_towers() {
            let outward = -tower_inward_axis(tower_center);
            // The extended-cannon muzzle emplacement's farthest authored
            // point: forward = 18 along the outward axis.
            let far_point = tower_center + outward * 18;
            let offset = far_point - tower_center;
            assert!(
                offset.magnitude_squared() <= reach_squared,
                "the cannon emplacement at {far_point:?} (offset {offset:?}) must remain inside \
                 the reach radius used to prune towers per column",
            );

            let tower_surface = config
                .surface_z(tower_center, deck)
                .expect("tower center must sit inside the island footprint");
            let top = tower_surface + config.wall_tower_height();
            let wpos = (config.center() + far_point).with_z(top + 3);
            assert_eq!(
                config.wall_voxel_at(wpos, sea_level),
                Some(CromatolisSkyCitadelVoxel::Air),
                "the extended-cannon emplacement must stay clear at the reach boundary",
            );
        }
    }

    /// The column-context restructuring must not change any observable
    /// result: for a scatter of positions covering island body, mountain,
    /// castle, contour wall, and every tower's interior/exterior, the
    /// per-position wrapper methods must agree with directly resolving a
    /// column and walking every `z` in it (the same access pattern the
    /// real entry point uses).
    #[test]
    fn column_context_resolution_matches_the_per_position_api_across_a_wide_sample() {
        let config = test_config();
        let sea_level = CONFIG.sea_level as i32;
        let deck = config.deck_z(sea_level);

        let mut sample_columns: Vec<Vec2<i32>> = vec![
            Vec2::new(-340, 90),  // western lobe, island body
            Vec2::new(-100, 100), // grassy shelf
            config.mountain_center(),
            config.castle_center(),
        ];
        for tower_center in config.wall_towers() {
            sample_columns.push(tower_center);
            sample_columns.push(tower_center + Vec2::new(20, 0));
            sample_columns.push(tower_center + Vec2::new(0, -20));
        }

        for relative in sample_columns {
            let wpos2d = config.center() + relative;
            let column = config.build_column(wpos2d, sea_level);
            let bottom = deck - 180;
            let top = deck + config.mountain_height_m;
            for z in bottom..=top {
                let wpos = wpos2d.with_z(z);
                assert_eq!(
                    column.voxel_at(&config, z),
                    config.voxel_at(wpos, sea_level),
                    "column-context resolution diverged from the per-position API at {wpos:?}",
                );
            }
        }
    }
}
