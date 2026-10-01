//! The shipyard plot: a warehouse on the apron and a slipway running down
//! into the water, with a half-built hull standing on it.
//!
//! A shipyard is its own plot kind rather than a flag on [`NavalPort`]
//! because the simulation picks the places NPCs work by plot kind, and a
//! shipwright needs a plot of their own to belong to. It is built over the
//! same two-part waterfront claim a naval port is (an apron on land, a deck
//! projected over the hazard band and water), and reuses the naval port's own
//! frame -- sampled altitudes, orientation helpers, timber and stone fills,
//! warehouse -- but carries no berths: nothing moors at a slipway.
//!
//! Every surface is a `Painter` primitive filled with a plain block; there is
//! no new voxel-model asset.

use super::*;
use crate::Land;
use common::terrain::SpriteKind;
use rand::prelude::*;
use vek::*;

/// Half the slipway's cross-shore width, in blocks: the lane is
/// `2 * SLIPWAY_HALF_WIDTH + 1` blocks across, narrower than the deck claim so
/// the hull sits on a ramp rather than filling the whole claimed rectangle.
const SLIPWAY_HALF_WIDTH: i32 = 4;

/// How many tiles of the deck, past the causeway that crosses the hazard band,
/// the slipway keeps running down into the water.
const SLIPWAY_WATER_TILES: i32 = 3;

/// How far below the water surface the slipway's lower end runs, in blocks:
/// deep enough that a hull launched from it floats clear of the ramp.
const SLIPWAY_END_DEPTH_BELOW_WATER: i32 = 2;

/// How deep, in blocks, a slipway column is driven below the water surface so
/// the ramp reads as resting on the bed rather than floating.
const SLIPWAY_SUPPORT_DEPTH_BELOW_WATER: i32 = 4;

/// Blocks along the slipway, from its landward end, before the hull's keel
/// begins.
const HULL_START: i32 = 3;

/// Length of the half-built hull along the slipway, in blocks.
const HULL_LENGTH: i32 = 18;

/// Blocks between the hull's ribs.
const RIB_SPACING: i32 = 3;

/// How many blocks a rib rises above the slipway surface.
const RIB_HEIGHT: i32 = 5;

/// How far, in blocks, a rib's foot sits from the keel at the hull's widest.
const RIB_FOOT_OFFSET: i32 = 3;

/// How far, in blocks, a rib's tip sits from the keel: narrower than its foot,
/// so the frame closes in towards the deck like a hull's side.
const RIB_TIP_OFFSET: i32 = 2;

/// Blocks along the slipway between the street lamps flanking its landward end.
const LAMP_SPACING: i32 = 3 * TILE_SIZE as i32;

pub struct Shipyard {
    frame: NavalPort,
}

impl Shipyard {
    /// Build a shipyard over a waterfront claim, with the same district
    /// dressing the settlement's naval port carries.
    pub fn generate(
        land: &Land,
        rng: &mut impl Rng,
        site: &Site,
        placement: ShorePlacement,
        dressing: Option<PortDressing>,
    ) -> Self {
        Self {
            frame: NavalPort::frame(land, rng, site, placement, dressing),
        }
    }

    /// Ground altitude of the yard's apron.
    pub fn alt(&self) -> i32 { self.frame.alt }

    /// Slipway length along the seaward normal, in blocks: the causeway across
    /// the hazard band plus a few tiles into the water, never longer than the
    /// claimed deck.
    fn slipway_length(&self) -> i32 {
        ((self.frame.ramp_tiles + SLIPWAY_WATER_TILES) * TILE_SIZE as i32)
            .min(self.frame.deck_reach_blocks())
    }

    /// Altitude of the slipway's walking surface `offset` blocks from its
    /// landward end: a straight incline from the apron's grade to a little
    /// below the water surface at its far end.
    fn slipway_surface(&self, offset: i32) -> i32 {
        let end = self.frame.water_alt - SLIPWAY_END_DEPTH_BELOW_WATER;
        let frac = (offset + 1) as f32 / self.slipway_length().max(1) as f32;
        self.frame.alt + ((end - self.frame.alt) as f32 * frac.min(1.0)).round() as i32
    }

    /// The slipway lane between `near` and `far` blocks from its landward
    /// end, centred on the deck.
    fn slipway_lane(&self, near: i32, far: i32) -> Aabr<i32> {
        let strip = self.frame.deck_strip(near, far);
        let centre = strip.center();
        if self.frame.seaward_is_x() {
            Aabr {
                min: Vec2::new(strip.min.x, centre.y - SLIPWAY_HALF_WIDTH),
                max: Vec2::new(strip.max.x, centre.y + SLIPWAY_HALF_WIDTH + 1),
            }
        } else {
            Aabr {
                min: Vec2::new(centre.x - SLIPWAY_HALF_WIDTH, strip.min.y),
                max: Vec2::new(centre.x + SLIPWAY_HALF_WIDTH + 1, strip.max.y),
            }
        }
    }

    /// Unit vector across the slipway, perpendicular to the seaward normal.
    fn cross(&self) -> Vec2<i32> { Vec2::new(self.frame.normal.y.abs(), self.frame.normal.x.abs()) }

    /// The inclined timber ramp, driven on stone columns from just below the
    /// water up to its surface so it never floats.
    fn build_slipway(&self, painter: &Painter) {
        let support = self.frame.stone_fill();
        let surface = self.frame.wood_fill();
        let base = self.frame.water_alt - SLIPWAY_SUPPORT_DEPTH_BELOW_WATER;
        for offset in 0..self.slipway_length() {
            let top = self.slipway_surface(offset);
            let lane = self.slipway_lane(offset, offset + 1);
            let low = base.min(top);
            painter
                .aabb(Aabb {
                    min: lane.min.with_z(low),
                    max: lane.max.with_z(top),
                })
                .fill(support.clone());
            painter
                .aabb(Aabb {
                    min: lane.min.with_z(top),
                    max: lane.max.with_z(top + 1),
                })
                .fill(surface.clone());
        }
    }

    /// A keel along the slipway's centre line with ribs rising from it, the
    /// first of them planked over, so the yard reads as one hull in the making.
    fn build_hull_frame(&self, painter: &Painter) {
        let timber = self.frame.wood_fill();
        let length = self.slipway_length();
        let start = HULL_START.min(length);
        let end = (HULL_START + HULL_LENGTH).min(length);
        if end <= start {
            return;
        }

        let centre_at = |offset: i32| self.frame.deck_strip(offset, offset + 1).center();
        let keel_z = |offset: i32| self.slipway_surface(offset) + 2;
        painter
            .line(
                centre_at(start).with_z(keel_z(start)),
                centre_at(end - 1).with_z(keel_z(end - 1)),
                0.8,
            )
            .fill(timber.clone());

        let cross = self.cross();
        let mut tips: [Vec<Vec3<i32>>; 2] = [Vec::new(), Vec::new()];
        let mut offset = start;
        while offset < end {
            let centre = centre_at(offset);
            let z = self.slipway_surface(offset) + 1;
            for (side_index, side) in [-1, 1].into_iter().enumerate() {
                let foot = centre + cross * (RIB_FOOT_OFFSET * side);
                let tip = centre + cross * (RIB_TIP_OFFSET * side);
                painter
                    .line(foot.with_z(z + 1), tip.with_z(z + RIB_HEIGHT), 0.6)
                    .fill(timber.clone());
                tips[side_index].push(tip.with_z(z + RIB_HEIGHT));
            }
            offset += RIB_SPACING;
        }

        // A gunwale along each side joins the rib tips into one hull.
        for side_tips in &tips {
            for pair in side_tips.windows(2) {
                painter.line(pair[0], pair[1], 0.6).fill(timber.clone());
            }
        }
    }

    /// A pair of tall street lamps flanking the slipway's landward end, in the
    /// same style the district's quay walls carry.
    fn build_lighting(&self, painter: &Painter) {
        let cross = self.cross();
        let mut offset = 0;
        while offset < self.slipway_length().min(2 * LAMP_SPACING) {
            let centre = self.frame.deck_strip(offset, offset + 1).center();
            for side in [-1, 1] {
                let pos = centre + cross * ((SLIPWAY_HALF_WIDTH + 1) * side);
                painter.sprite(
                    pos.with_z(self.slipway_surface(offset) + 1),
                    SpriteKind::StreetLampTall,
                );
            }
            offset += LAMP_SPACING;
        }
    }
}

impl Structure for Shipyard {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_shipyard")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_shipyard"))
    }

    fn render_inner(&self, _site: &Site, _land: &Land, painter: &Painter) {
        self.frame.build_warehouse(painter, 0);
        self.build_slipway(painter);
        self.build_hull_frame(painter);
        self.build_lighting(painter);
    }

    fn door_tile(&self) -> Option<Vec2<i32>> { Some(self.frame.door_tile) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yard(normal: Vec2<i32>, alt: i32, water_alt: i32) -> Shipyard {
        let mut frame = NavalPort::test_frame(normal);
        frame.alt = alt;
        frame.water_alt = water_alt;
        frame.deck_alt = water_alt + 2;
        Shipyard { frame }
    }

    #[test]
    fn slipway_surface_falls_monotonically_from_the_apron_to_below_the_water() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let yard = yard(normal, 12, 4);
            let mut previous = i32::MAX;
            for offset in 0..yard.slipway_length() {
                let z = yard.slipway_surface(offset);
                assert!(
                    z <= previous,
                    "the slipway rises at {offset} for {normal:?}"
                );
                previous = z;
            }
            let end = yard.slipway_surface(yard.slipway_length() - 1);
            assert_eq!(end, 4 - SLIPWAY_END_DEPTH_BELOW_WATER);
        }
    }

    #[test]
    fn slipway_never_runs_past_the_claimed_deck() {
        for normal in [Vec2::new(1, 0), Vec2::new(0, -1)] {
            let yard = yard(normal, 12, 4);
            assert!(yard.slipway_length() <= yard.frame.deck_reach_blocks());
            assert!(yard.slipway_length() > 0);
        }
    }

    #[test]
    fn slipway_lane_is_centred_on_the_deck_and_narrower_than_it() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let yard = yard(normal, 12, 4);
            let strip = yard.frame.deck_strip(0, 1);
            let lane = yard.slipway_lane(0, 1);
            let width = if yard.frame.seaward_is_x() {
                lane.size().h
            } else {
                lane.size().w
            };
            assert_eq!(width, 2 * SLIPWAY_HALF_WIDTH + 1);
            assert!(
                lane.min.x >= strip.min.x
                    && lane.min.y >= strip.min.y
                    && lane.max.x <= strip.max.x
                    && lane.max.y <= strip.max.y,
                "the lane left the deck for {normal:?}"
            );
        }
    }

    #[test]
    fn cross_axis_is_perpendicular_to_the_seaward_normal() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let yard = yard(normal, 12, 4);
            assert_eq!(yard.cross().dot(normal), 0);
            assert_eq!(yard.cross().map(|e| e.abs()).sum(), 1);
        }
    }
}
