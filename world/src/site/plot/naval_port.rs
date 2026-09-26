//! The naval port plot: real, walkable dock geometry over the waterfront
//! footprint `Site::find_shore_aabr` claims (see `site::shore`).
//!
//! # Only `Jetty` and `Pier` are wired up at a `generate_city` call site
//!
//! [`PortClass::Quay`] and [`PortClass::Harbour`] still claim their
//! footprint via the ordinary placement pass, but no `generate_city` call
//! site turns that claim into a `NavalPort` plot yet -- so those two tiers'
//! tiles render as bare claimed ground/deck with nothing on them, exactly as
//! before this module existed. That is deliberate: it keeps a settlement's
//! waterfront from regressing to a half-built structure before those two
//! tiers get their own dedicated builders (quay wall, warehouse, crane,
//! harbourmaster hall -- more vertical presence than `Jetty`/`Pier` call
//! for). `render_inner` below is still exhaustive over all four
//! [`PortClass`] values, so the two unbuilt tiers fall back to the `Pier`
//! tier's art (a real, if under-detailed, structure) rather than a panic --
//! but that arm is unreached by any plot actually constructed today.
//!
//! # One file, one plot kind
//!
//! The four tiers differ in *scale* (berth count, deck length, vertical
//! presence, prop density), not in biome *art* -- so every tier is one
//! `PlotKind::NavalPort` built from the same small set of sub-builders
//! (`build_causeway`, `build_deck_cap`, `build_pilings`/`build_footings`,
//! `build_bollard_line`, `build_prop_scatter`, `build_cargo_shed`), composed
//! per tier in [`NavalPort::render_jetty`] / [`NavalPort::render_pier`].
//!
//! # Geometry is axis-aligned by construction
//!
//! [`ShorePlacement::outward`] is always a unit cardinal, so every aabr this
//! module works with (apron, hinge, deck) is already axis-aligned in world
//! space -- there is no rotation to undo. [`NavalPort::deck_strip`] and
//! [`NavalPort::edge_sliver`] slice those aabrs generically over whichever
//! cardinal `normal` happens to be, mirroring the style `site::shore`'s own
//! private `shore_face` / `project_deck` helpers use for the same reason.
//!
//! # No new voxel-model assets
//!
//! Every prop below is a shipped `SpriteKind`; every surface is a `Painter`
//! primitive filled with a plain `Fill::Block`. Timber species (and only
//! timber species) are keyed off the local biome via
//! `Land::make_forest_lottery`, the same idiom `Plaza::generate` already uses
//! for its own market-stand roofs.

use super::*;
use crate::{
    Land,
    all::ForestKind,
    util::{RandomField, Sampler},
};
use common::terrain::{Block, BlockKind, SpriteKind};
use rand::prelude::*;
use vek::*;

/// Crates/barrels/rope per deck tile, by tier. `Jetty` and `Pier` are the
/// only two tiers any `generate_city` call site builds a plot for today;
/// `Quay`/`Harbour` are named here so their own builders have a density to
/// read from once they land.
const PROP_DENSITY_JETTY: f32 = 0.05;
const PROP_DENSITY_PIER: f32 = 0.1;
const PROP_DENSITY_QUAY: f32 = 0.2;
const PROP_DENSITY_HARBOUR: f32 = 0.3;

/// Vertical clearance a piling/footing column reaches below the sampled
/// water surface, so it reads as driven into the seabed rather than floating
/// on top of the water.
const SUPPORT_DEPTH_BELOW_WATER: i32 = 4;

/// How far apart (in blocks) pilings/footings and bollards are spaced along
/// a deck's reach.
const SUPPORT_SPACING: i32 = 2 * TILE_SIZE as i32;

/// A weathered-stone tone shared by every tier's stonework, matching the
/// material the authored river-port landmarks already use for their own
/// quays, so any future generator that reuses these sub-builders for those
/// landmarks does not need a second palette.
const STONE_COLOR: Rgb<u8> = Rgb::new(150, 145, 135);

/// Real, walkable dock geometry for one settlement's waterfront.
///
/// Built once, from a successful [`ShorePlacement`], by
/// [`NavalPort::generate`]. Every field here is world-space (blocks), unlike
/// `ShorePlacement`'s own fields, which are tile-space -- converted once at
/// construction so every sub-builder below works in the same units `Painter`
/// does.
pub struct NavalPort {
    /// The tier this was built for.
    pub class: PortClass,
    /// Landward half: ordinary ground, already flattened by the engine via
    /// its `hard_alt` claim (see `Site::place_naval_port`). Nothing here
    /// paints the ground itself, only structures on top of it.
    pub apron: Aabr<i32>,
    /// Seaward half: the claimed hazard/water footprint this plot actually
    /// has to build a walkable surface over, since `TileKind::Pier` carries
    /// no `hard_alt` for the generic terrain painter to flatten.
    pub deck: Aabr<i32>,
    /// The one-tile-deep shared edge between the two.
    pub hinge: Aabr<i32>,
    /// World position of the apron's landward door tile.
    pub door_tile: Vec2<i32>,
    /// The outward (seaward) unit cardinal.
    pub normal: Vec2<i32>,
    /// Apron ground altitude (`ShorePlacement::apron_hard_alt`).
    pub alt: i32,
    /// Sampled water surface altitude under the deck.
    pub water_alt: i32,
    /// The deck's own walking-surface altitude: `water_alt` plus freeboard.
    /// Not sea level -- see the module doc.
    deck_alt: i32,
    /// Tiles of the deck's own reach that cross the dilated hazard band
    /// before reaching real open water (`ShorePlacement::causeway`), clamped
    /// to at least one so there is always a ramp span even when the apron
    /// happened to grow flush with the waterline.
    ///
    /// Deliberately not widened even when the apron sits well above the
    /// water -- real terrain, not a bug: the apron's `hard_alt` is one
    /// sample at its centre, and land legitimately slopes up away from a
    /// shoreline. Spending more of the deck's own reach on the ramp would
    /// protect the ramp's grade at the cost of the tier's actual berthing
    /// surface, which is the wrong trade for a plot whose entire point is
    /// the berth. `build_causeway` instead steps at single-block
    /// granularity, so even a steep rise over this fixed span reads as a
    /// real (if steep) staircase rather than a sheer wall.
    ramp_tiles: i32,
    wood_color: Rgb<u8>,
}

impl NavalPort {
    /// Build a `NavalPort` from a successful waterfront placement.
    ///
    /// Called from `Site::generate_city` immediately after
    /// `Site::place_naval_port` succeeds, before the plot `Lottery` loop --
    /// see that call site for why a plot (rather than just the tile claim)
    /// is only created for `Jetty`/`Pier` today.
    pub fn generate(
        land: &Land,
        rng: &mut impl Rng,
        site: &Site,
        placement: ShorePlacement,
    ) -> Self {
        let to_wpos_aabr = |aabr: Aabr<i32>| Aabr {
            min: site.tile_wpos(aabr.min),
            max: site.tile_wpos(aabr.max),
        };
        let apron = to_wpos_aabr(placement.apron);
        let deck = to_wpos_aabr(placement.deck);
        let hinge = to_wpos_aabr(placement.hinge);
        let door_tile = site.tile_center_wpos(placement.door_tile);

        // Per-corner + centre sampling, applied to the *same* field
        // `Site::deck_centre_line_depth` already measures the placement
        // against (`SimChunk::water_alt`) rather than `ColumnSample`'s own
        // `water_level` -- the column sampler's fallback reads as sea level
        // wherever it cannot resolve a local river/lake surface, which is
        // wrong by a hundred-odd blocks for a settlement sitting well above
        // the world's sea level (see `Plaza::generate`'s own comment on
        // "any_water": it only cares whether a corner is wet at all, not the
        // exact height, so this failure mode never mattered there). The max
        // (not the mean) is taken deliberately: a deck has to clear the
        // *highest* local water reading along its own footprint, not an
        // average that some of it would sit under.
        let sample_water_alt = |wpos: Vec2<i32>| {
            land.get_chunk_wpos(wpos)
                .map(|chunk| chunk.water_alt as i32)
        };
        let water_alt = [
            deck.min,
            deck.max - 1,
            Vec2::new(deck.min.x, deck.max.y - 1),
            Vec2::new(deck.max.x - 1, deck.min.y),
            deck.center(),
        ]
        .into_iter()
        .filter_map(sample_water_alt)
        .max()
        .unwrap_or(placement.apron_hard_alt);

        // The one concrete number named for any tier is `Jetty`'s "water_alt
        // + 2", applied uniformly here: nothing calls for any other tier's
        // deck to sit any lower, and a shared freeboard keeps every tier's
        // causeway ramp comparable.
        const DECK_FREEBOARD: i32 = 2;
        let deck_alt = water_alt + DECK_FREEBOARD;

        // The causeway gap, clamped to at least one tile so there is always
        // a ramp span even when the apron happened to grow flush with the
        // waterline. Kept to the causeway alone (rather than eating into the
        // tier's own nominal deck reach) so a settlement whose apron sits
        // well above the water -- real terrain, not a bug: the apron's
        // `hard_alt` is one sample at its centre, and land legitimately
        // slopes up away from a shoreline -- loses ramp grade rather than
        // losing its actual berthing surface. `build_causeway` steps at
        // single-block granularity for exactly this reason: a steep rise
        // over a short run reads as a real, if steep, staircase rather than
        // a wall only because every block of run gets its own step.
        let ramp_tiles = placement.causeway.max(1);

        let wood_color = match land
            .make_forest_lottery(apron.center())
            .choose_seeded(rng.random())
        {
            Some(
                ForestKind::Cedar
                | ForestKind::AutumnTree
                | ForestKind::Frostpine
                | ForestKind::Mangrove,
            ) => Rgb::new(63, 28, 12),
            Some(ForestKind::Oak | ForestKind::Swamp | ForestKind::Baobab) => Rgb::new(102, 87, 63),
            Some(ForestKind::Acacia | ForestKind::Birch | ForestKind::Palm) => {
                Rgb::new(130, 104, 102)
            },
            Some(
                ForestKind::Mapletree | ForestKind::Redwood | ForestKind::Pine | ForestKind::Cherry,
            ) => Rgb::new(117, 95, 46),
            _ => Rgb::new(63, 28, 12),
        };

        Self {
            class: placement.class,
            apron,
            deck,
            hinge,
            door_tile,
            normal: placement.outward,
            alt: placement.apron_hard_alt,
            water_alt,
            deck_alt,
            ramp_tiles,
            wood_color,
        }
    }

    fn wood_fill(&self) -> Fill { Fill::Block(Block::new(BlockKind::Wood, self.wood_color)) }

    fn stone_fill(&self) -> Fill { Fill::Block(Block::new(BlockKind::Rock, STONE_COLOR)) }

    /// Whether the seaward normal runs along the X axis (as opposed to Y).
    fn seaward_is_x(&self) -> bool { self.normal.x != 0 }

    /// How many blocks the deck reaches, measured along the seaward normal.
    fn deck_reach_blocks(&self) -> i32 {
        if self.seaward_is_x() {
            self.deck.size().w
        } else {
            self.deck.size().h
        }
    }

    /// The deck's footprint between `near` and `far` blocks from its
    /// landward edge, measured along the seaward normal, at the deck's own
    /// cross-shore width. Mirrors `site::shore`'s private `shore_face` /
    /// `project_deck` -- same axis-generic slicing, over the deck's own
    /// aabr instead of the tile grid.
    fn deck_strip(&self, near: i32, far: i32) -> Aabr<i32> {
        if self.normal.x > 0 {
            Aabr {
                min: Vec2::new(self.deck.min.x + near, self.deck.min.y),
                max: Vec2::new(self.deck.min.x + far, self.deck.max.y),
            }
        } else if self.normal.x < 0 {
            Aabr {
                min: Vec2::new(self.deck.max.x - far, self.deck.min.y),
                max: Vec2::new(self.deck.max.x - near, self.deck.max.y),
            }
        } else if self.normal.y > 0 {
            Aabr {
                min: Vec2::new(self.deck.min.x, self.deck.min.y + near),
                max: Vec2::new(self.deck.max.x, self.deck.min.y + far),
            }
        } else {
            Aabr {
                min: Vec2::new(self.deck.min.x, self.deck.max.y - far),
                max: Vec2::new(self.deck.max.x, self.deck.max.y - near),
            }
        }
    }

    /// A one-block-wide sliver of `strip` at one of its two cross-shore
    /// edges (the edges running *along* the seaward normal), used to place
    /// piles/footings/bollards at the sides of the deck rather than across
    /// its whole width.
    fn edge_sliver(&self, strip: Aabr<i32>, min_edge: bool) -> Aabr<i32> {
        if self.seaward_is_x() {
            if min_edge {
                Aabr {
                    min: strip.min,
                    max: Vec2::new(strip.max.x, strip.min.y + 1),
                }
            } else {
                Aabr {
                    min: Vec2::new(strip.min.x, strip.max.y - 1),
                    max: strip.max,
                }
            }
        } else if min_edge {
            Aabr {
                min: strip.min,
                max: Vec2::new(strip.min.x + 1, strip.max.y),
            }
        } else {
            Aabr {
                min: Vec2::new(strip.max.x - 1, strip.min.y),
                max: strip.max,
            }
        }
    }

    /// A staircase of solid columns bridging the apron's grade to the deck's
    /// own walking-surface grade, across the causeway span -- the part of
    /// the deck crossing the dilated hazard band rather than standing over
    /// real open water. Each column is solid from the lower of the two
    /// grades up to its own step height, so the result has no floating gaps
    /// regardless of which side is higher.
    fn build_causeway(&self, painter: &Painter, fill: Fill) {
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let low = self.alt.min(self.deck_alt);
        // One column per block, not per tile: the apron can sit many blocks
        // above the water on sloped terrain (see `ramp_tiles`'s own doc
        // comment), and stepping at tile granularity would turn that rise
        // into a handful of sheer, un-scalable risers. Stepping every block
        // instead spreads the same rise over every block of the available
        // run, which is what actually keeps it climbable when the run is
        // short relative to the rise.
        for block in 0..run_blocks {
            let frac = (block + 1) as f32 / run_blocks as f32;
            let top = self.alt + ((self.deck_alt - self.alt) as f32 * frac).round() as i32;
            let strip = self.deck_strip(block, block + 1);
            painter
                .aabb(Aabb {
                    min: strip.min.with_z(low),
                    max: strip.max.with_z(top.max(low) + 1),
                })
                .fill(fill.clone());
        }
    }

    /// The flat pier/jetty head beyond the causeway: a single solid deck cap
    /// at the walking-surface altitude, one block thick.
    fn build_deck_cap(&self, painter: &Painter, fill: Fill) {
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        if reach <= run_blocks {
            return;
        }
        let head = self.deck_strip(run_blocks, reach);
        painter
            .aabb(Aabb {
                min: head.min.with_z(self.deck_alt),
                max: head.max.with_z(self.deck_alt + 1),
            })
            .fill(fill);
    }

    /// Visible timber piles under the jetty head, one pair (both cross-shore
    /// edges) every [`SUPPORT_SPACING`] blocks, driven from below the water
    /// surface up to just under the deck cap.
    fn build_pilings(&self, painter: &Painter) {
        let fill = self.wood_fill();
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        let mut offset = run_blocks + SUPPORT_SPACING / 2;
        while offset < reach {
            let post = self.deck_strip(offset, offset + 1);
            for min_edge in [true, false] {
                let sliver = self.edge_sliver(post, min_edge);
                painter
                    .aabb(Aabb {
                        min: sliver
                            .min
                            .with_z(self.water_alt - SUPPORT_DEPTH_BELOW_WATER),
                        max: sliver.max.with_z(self.deck_alt),
                    })
                    .fill(fill.clone());
            }
            offset += SUPPORT_SPACING;
        }
    }

    /// Stone footings under the pier head: full-width blocks (not thin
    /// piles -- a stone-footed pier reads differently from a jetty on
    /// pilings), every [`SUPPORT_SPACING`] blocks, driven the same depth as
    /// a piling.
    fn build_footings(&self, painter: &Painter) {
        let fill = self.stone_fill();
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        let mut offset = run_blocks + SUPPORT_SPACING / 2;
        while offset + 2 <= reach {
            let footing = self.deck_strip(offset, offset + 2);
            painter
                .aabb(Aabb {
                    min: footing
                        .min
                        .with_z(self.water_alt - SUPPORT_DEPTH_BELOW_WATER),
                    max: footing.max.with_z(self.deck_alt),
                })
                .fill(fill.clone());
            offset += SUPPORT_SPACING;
        }
    }

    /// Mooring posts along both edges of the pier head, spaced
    /// [`SUPPORT_SPACING`] blocks apart -- the visible cue for where a hull
    /// would tie up, standing in for the addressable berth contract a later
    /// change wires in.
    fn build_bollard_line(&self, painter: &Painter) {
        let fill = self.wood_fill();
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        let mut offset = run_blocks + SUPPORT_SPACING / 2;
        while offset < reach {
            let strip = self.deck_strip(offset, offset + 1);
            for min_edge in [true, false] {
                let sliver = self.edge_sliver(strip, min_edge);
                painter
                    .aabb(Aabb {
                        min: sliver.min.with_z(self.deck_alt + 1),
                        max: sliver.max.with_z(self.deck_alt + 2),
                    })
                    .fill(fill.clone());
            }
            offset += SUPPORT_SPACING;
        }
    }

    /// Crates, barrels and rope scattered over the pier head at `density`
    /// props per deck tile, one roll per tile so the count scales with the
    /// tier's own footprint rather than being fixed.
    fn build_prop_scatter(&self, painter: &Painter, density: f32) {
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        let field = RandomField::new(0x506C_5254); // "pLRT", distinct from other fields in this file
        let mut offset = run_blocks;
        while offset + TILE_SIZE as i32 <= reach {
            let cell = self.deck_strip(offset, offset + TILE_SIZE as i32);
            let seed_pos = cell.min.with_z(self.deck_alt);
            if field.chance(seed_pos, density) {
                let inset_w = (cell.size().w - 2).max(1);
                let inset_h = (cell.size().h - 2).max(1);
                let x = cell.min.x
                    + 1
                    + (field.get_f32(seed_pos + Vec3::new(1, 0, 0)) * inset_w as f32) as i32;
                let y = cell.min.y
                    + 1
                    + (field.get_f32(seed_pos + Vec3::new(0, 1, 0)) * inset_h as f32) as i32;
                let sprite = match field.get(seed_pos + Vec3::new(0, 0, 1)) % 3 {
                    0 => SpriteKind::Barrel,
                    1 => SpriteKind::CrateBlock,
                    _ => SpriteKind::Crate,
                };
                painter.sprite(Vec2::new(x, y).with_z(self.deck_alt + 1), sprite);
            }
            offset += TILE_SIZE as i32;
        }
    }

    /// A small cargo shed on the apron, set back from the hinge so the
    /// approach from the door tile stays clear. The `Pier` tier's vertical
    /// presence above the apron.
    fn build_cargo_shed(&self, painter: &Painter) {
        const SHED_HALF: Vec2<i32> = Vec2::new(3, 3);
        const SHED_HEIGHT: i32 = 5;
        const SETBACK: i32 = 4;

        let inland = -self.normal;
        let center = self.hinge.center() + inland * (SETBACK + SHED_HALF.x);
        let base = self.alt;
        let wall = self.stone_fill();
        let roof = self.wood_fill();

        painter
            .aabb(Aabb {
                min: (center - SHED_HALF).with_z(base),
                max: (center + SHED_HALF).with_z(base + SHED_HEIGHT),
            })
            .fill(wall);
        painter
            .pyramid(Aabb {
                min: (center - SHED_HALF - 1).with_z(base + SHED_HEIGHT),
                max: (center + SHED_HALF + 1).with_z(base + SHED_HEIGHT + 3),
            })
            .fill(roof);
    }

    /// Timber on visible pilings, deck at `water_alt + 2`, no vertical
    /// presence above deck and no night lighting.
    fn render_jetty(&self, painter: &Painter) {
        let wood = self.wood_fill();
        self.build_causeway(painter, wood.clone());
        self.build_deck_cap(painter, wood);
        self.build_pilings(painter);
        self.build_bollard_line(painter);
        self.build_prop_scatter(painter, PROP_DENSITY_JETTY);
    }

    /// Stone footings with a timber deck, and a cargo shed on the apron, at
    /// the given prop density -- shared by the `Pier` tier and, until
    /// `Quay`/`Harbour` get their own dedicated builders, by those two tiers
    /// as well (see `render_inner`).
    fn render_pier(&self, painter: &Painter, density: f32) {
        let stone = self.stone_fill();
        let wood = self.wood_fill();
        self.build_causeway(painter, stone);
        self.build_deck_cap(painter, wood);
        self.build_footings(painter);
        self.build_bollard_line(painter);
        self.build_prop_scatter(painter, density);
        self.build_cargo_shed(painter);
    }
}

impl Structure for NavalPort {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_navalport")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_navalport"))
    }

    fn render_inner(&self, _site: &Site, _land: &Land, painter: &Painter) {
        match self.class {
            PortClass::Jetty => self.render_jetty(painter),
            PortClass::Pier => self.render_pier(painter, PROP_DENSITY_PIER),
            // `Quay`/`Harbour` are meant to get their own quay-wall /
            // warehouse / crane / harbourmaster-hall builders. Until then
            // this falls back to the `Pier` tier's art (at each tier's own
            // prop density) rather than panicking on an exhaustiveness gap
            // -- but no `generate_city` call site constructs a `NavalPort`
            // plot for these two tiers yet (see the module doc), so this arm
            // is unreached by any plot actually built today.
            PortClass::Quay => self.render_pier(painter, PROP_DENSITY_QUAY),
            PortClass::Harbour => self.render_pier(painter, PROP_DENSITY_HARBOUR),
        }
    }

    fn door_tile(&self) -> Option<Vec2<i32>> { Some(self.door_tile) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(normal: Vec2<i32>) -> NavalPort {
        NavalPort {
            class: PortClass::Pier,
            apron: Aabr {
                min: Vec2::new(-30, -18),
                max: Vec2::new(30, 18),
            },
            deck: match (normal.x, normal.y) {
                (1, 0) => Aabr {
                    min: Vec2::new(30, -9),
                    max: Vec2::new(96, 9),
                },
                (-1, 0) => Aabr {
                    min: Vec2::new(-96, -9),
                    max: Vec2::new(-30, 9),
                },
                (0, 1) => Aabr {
                    min: Vec2::new(-9, 18),
                    max: Vec2::new(9, 84),
                },
                _ => Aabr {
                    min: Vec2::new(-9, -84),
                    max: Vec2::new(9, -18),
                },
            },
            hinge: Aabr::new_empty(Vec2::zero()),
            door_tile: Vec2::zero(),
            normal,
            alt: 10,
            water_alt: 4,
            deck_alt: 6,
            ramp_tiles: 2,
            wood_color: Rgb::new(102, 87, 63),
        }
    }

    #[test]
    fn deck_strip_stays_within_the_deck_for_every_cardinal() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let port = port(normal);
            let reach = port.deck_reach_blocks();
            let strip = port.deck_strip(0, reach);
            assert_eq!(
                strip, port.deck,
                "the full-reach strip must equal the deck itself for normal {normal:?}"
            );

            let head = port.deck_strip(reach - 6, reach);
            let clamped = Aabr {
                min: Vec2::partial_max(head.min, port.deck.min),
                max: Vec2::partial_min(head.max, port.deck.max),
            };
            assert_eq!(
                head, clamped,
                "a strip must never extend past the deck it was sliced from, normal {normal:?}"
            );
        }
    }

    #[test]
    fn edge_slivers_are_one_block_wide_and_on_opposite_sides() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let port = port(normal);
            let strip = port.deck_strip(0, 6);
            let a = port.edge_sliver(strip, true);
            let b = port.edge_sliver(strip, false);
            // A sliver keeps the strip's full extent along the seaward
            // normal (here, 6) and shrinks the cross-shore extent to 1.
            assert_eq!(a.size().product(), 6);
            assert_eq!(b.size().product(), 6);
            assert!(
                a.intersection(b).size().w <= 0 || a.intersection(b).size().h <= 0,
                "the two edge slivers must not overlap for normal {normal:?}"
            );
        }
    }

    #[test]
    fn door_tile_is_reported_through_the_structure_trait() {
        let port = port(Vec2::new(1, 0));
        assert_eq!(Structure::door_tile(&port), Some(Vec2::zero()));
    }
}
