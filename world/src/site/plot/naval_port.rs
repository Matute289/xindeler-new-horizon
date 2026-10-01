//! The naval port plot: real, walkable dock geometry over the waterfront
//! footprint `Site::find_shore_aabr` claims (see `site::shore`).
//!
//! # All four tiers are wired up at the `generate_city` call site
//!
//! `Jetty` and `Pier` shipped in Phase 3a. Phase 3b adds
//! [`PortClass::Quay`] and [`PortClass::Harbour`]'s own dedicated builders
//! (quay wall with backfill, warehouse, crane, harbourmaster hall -- more
//! vertical presence than `Jetty`/`Pier` call for) and their own night
//! lighting, so every one of the 13 authored route stops now generates a
//! real `PlotKind::NavalPort` plot rather than a bare claimed tile.
//!
//! # One file, one plot kind
//!
//! The four tiers differ in *scale* (berth count, deck length, vertical
//! presence, prop density), not in biome *art* -- so every tier is one
//! `PlotKind::NavalPort` built from a small set of sub-builders composed per
//! tier: `Jetty`/`Pier` reuse Phase 3a's `build_causeway`, `build_deck_cap`,
//! `build_pilings`/`build_footings`, `build_bollard_line`,
//! `build_prop_scatter`, `build_cargo_shed`
//! ([`NavalPort::render_jetty`] / [`NavalPort::render_pier`]); `Quay`/
//! `Harbour` add `build_quay_wall`, `build_finger_bollards`,
//! `build_quay_prop_scatter`, `build_warehouse`, `build_crane` and
//! `build_harbourmaster_hall` ([`NavalPort::render_quay`] /
//! [`NavalPort::render_harbour`]), since their deck is a wider quay with
//! finger piers projecting off it rather than one narrow lane.
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
use tracing::{debug, warn};
use vek::*;

/// Crates/barrels/rope per deck tile, by tier (spec §4.3).
const PROP_DENSITY_JETTY: f32 = 0.05;
const PROP_DENSITY_PIER: f32 = 0.1;
const PROP_DENSITY_QUAY: f32 = 0.2;
const PROP_DENSITY_HARBOUR: f32 = 0.3;

/// Along-shore spacing (blocks) between night-lighting sprites for the two
/// tiers that get them (task T18). `Jetty` gets no lighting at all -- the
/// absence is deliberate and the cheapest tier difference to get wrong by
/// accident, so there is no constant for it -- and `Pier` gets a single
/// fixed lantern rather than a spaced run (see
/// [`NavalPort::build_pier_lantern`]).
const LIGHT_SPACING_QUAY: i32 = 4 * TILE_SIZE as i32;
const LIGHT_SPACING_HARBOUR: i32 = 3 * TILE_SIZE as i32;

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

// ---------------------------------------------------------------------------
// The berth contract (spec §4.3, §5.2, §11.2).
// ---------------------------------------------------------------------------

/// Quay-face length each berth class's hull occupies alongside a quay/pier/
/// finger face, in blocks (spec §4.3: `Small` 6 tiles, `Large` 9 tiles --
/// the same two figures `PortClass::deck_dims`'s doc comment already names
/// for the finger piers).
const BERTH_FACE_SMALL_BLOCKS: i32 = 6 * TILE_SIZE as i32;
const BERTH_FACE_LARGE_BLOCKS: i32 = 9 * TILE_SIZE as i32;

/// Minimum water depth, in blocks, a berth of each class requires before it
/// is emitted at all (spec §4.3). `Small`'s figure matches
/// `site::shore::SHORE_MIN_BERTH_DEPTH` -- the same minimum placement
/// already gated the deck on -- so a `Small` berth can never be dropped by
/// this check alone; only `Large` berths and anchorages can fail it.
const BERTH_MIN_DEPTH_SMALL: i32 = 3;
const BERTH_MIN_DEPTH_LARGE: i32 = 6;

/// How far seaward of a berth's mooring face its `approach` waypoint sits,
/// in blocks -- one tile of open water to turn into before coming
/// alongside.
const BERTH_APPROACH_STANDOFF_BLOCKS: i32 = TILE_SIZE as i32;

/// Blocks of clear water an anchored `Galleon` needs around its swing point
/// (spec §11.2: roughly half the hull's 48-block length, plus margin).
const ANCHORAGE_SWING_RADIUS_BLOCKS: i32 = 30;

/// How far seaward of the jetty's deck end the anchorage search marches
/// before giving up (spec §11.2): further than this and a rowboat run
/// between the jetty and the anchored `Galleon` reads as absurd. The
/// weakest village waterfront measured so far clears only ~6.6 blocks of
/// depth on its best direction, which is the expected, loud failure this
/// constant exists to produce rather than mask.
const MAX_ANCHORAGE_OFFSET_BLOCKS: i32 = 200;

/// Step size, in blocks, the anchorage search marches along the port's
/// outward normal. One tile: cheap (at most
/// `MAX_ANCHORAGE_OFFSET_BLOCKS / ANCHORAGE_SEARCH_STEP_BLOCKS` chunk
/// samples), and fine enough not to step over a narrow deep channel.
const ANCHORAGE_SEARCH_STEP_BLOCKS: i32 = TILE_SIZE as i32;

/// Points sampled around a swing circle's circumference, plus its centre,
/// when checking it is clear. Eight compass points is the same coarse
/// coverage `site::shore`'s own `SHORE_ALT_SAMPLE_LATTICE` lattice uses for
/// the same reason: a cheap, good-enough area check rather than a full
/// flood-fill, at chunk granularity where finer sampling buys nothing.
const ANCHORAGE_SWING_SAMPLES: usize = 8;

/// Points sampled (inclusive of both ends) along a berth's or anchorage's
/// `approach -> mooring/anchor` segment when checking it crosses open water
/// only.
const WATER_SEGMENT_SAMPLES: i32 = 8;

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
    /// This port's mooring slots, measured against real terrain and emitted
    /// only where they clear their class's depth and have a clear-water
    /// approach (see [`Self::measure_berths`]). A `Jetty` may legitimately
    /// end up with zero.
    berths: Vec<Berth>,
    /// Offshore moorings for hulls too large for any berth above --
    /// non-empty only for a `Jetty` whose [`Self::find_anchorage`] search
    /// succeeded (spec §11.2).
    anchorages: Vec<Anchorage>,
}

impl NavalPort {
    /// Build a `NavalPort` from a successful waterfront placement.
    ///
    /// Called from `Site::generate_city` immediately after
    /// `Site::place_naval_port` succeeds, before the plot `Lottery` loop --
    /// see that call site for why the waterfront is claimed unconditionally,
    /// ahead of anything else that could compete for it.
    pub fn generate(
        land: &Land,
        rng: &mut impl Rng,
        site: &Site,
        site_name: &str,
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

        let mut port = Self {
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
            berths: Vec::new(),
            anchorages: Vec::new(),
        };
        port.berths = port.measure_berths(land, site_name);
        port.anchorages = port.find_anchorage(land, site_name, &port.berths);
        port
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

    /// `Quay`/`Harbour`-only tier parameters: `(quay_depth_tiles,
    /// finger_count, finger_width_tiles)`. The quay wall's own depth plus
    /// the finger length is always `class.deck_dims().h` exactly (the deck
    /// is projected at a fixed size, never grown -- see `ShorePlacement`'s
    /// own doc comment), so the finger length itself is derived from
    /// [`Self::deck_reach_blocks`] rather than named again here.
    ///
    /// Spec §4.3: `Harbour` is a 4-tile-deep quay with 3 finger piers 4
    /// tiles wide; `Quay` a 3-tile-deep quay with 2 finger piers 3 tiles
    /// wide.
    fn quay_params(&self) -> (i32, i32, i32) {
        match self.class {
            PortClass::Harbour => (4, 3, 4),
            PortClass::Quay => (3, 2, 3),
            PortClass::Jetty | PortClass::Pier => {
                unreachable!("quay_params is only meaningful for Quay/Harbour")
            },
        }
    }

    /// The deck's own along-shore extent, in blocks -- the axis
    /// perpendicular to the seaward normal, as opposed to
    /// [`Self::deck_reach_blocks`]'s seaward one.
    fn deck_along_shore_blocks(&self) -> i32 {
        if self.seaward_is_x() {
            self.deck.size().h
        } else {
            self.deck.size().w
        }
    }

    /// `(from, to)` block offsets, along the deck's own along-shore extent,
    /// of each finger pier this tier's [`Self::quay_params`] calls for --
    /// evenly spaced with a gap before the first, after the last, and
    /// between every pair, so the fingers read as a comb rather than
    /// touching the apron's own edges.
    fn finger_bands(&self) -> Vec<(i32, i32)> {
        let (_, count, width_tiles) = self.quay_params();
        let width = width_tiles * TILE_SIZE as i32;
        let total = self.deck_along_shore_blocks();
        let gaps = count + 1;
        let gap = (total - count * width) / gaps;
        let mut bands = Vec::with_capacity(count as usize);
        let mut cursor = gap;
        for _ in 0..count {
            bands.push((cursor, cursor + width));
            cursor += width + gap;
        }
        bands
    }

    /// A sub-rectangle of `strip` (itself already sliced along the seaward
    /// normal via [`Self::deck_strip`]) restricted to the along-shore band
    /// `[from, to)` blocks from the deck's own along-shore minimum edge --
    /// the perpendicular-axis counterpart to `deck_strip`, used to carve
    /// individual finger piers out of the deck's full along-shore width.
    fn along_shore_strip(&self, strip: Aabr<i32>, from: i32, to: i32) -> Aabr<i32> {
        if self.seaward_is_x() {
            Aabr {
                min: Vec2::new(strip.min.x, self.deck.min.y + from),
                max: Vec2::new(strip.max.x, self.deck.min.y + to),
            }
        } else {
            Aabr {
                min: Vec2::new(self.deck.min.x + from, strip.min.y),
                max: Vec2::new(self.deck.min.x + to, strip.max.y),
            }
        }
    }

    /// A world-space point `inland` blocks landward of the hinge and
    /// `along` blocks from the apron's own along-shore centre -- used to
    /// lay out `Quay`/`Harbour`'s vertical structures on the apron without
    /// hand-picking coordinates per settlement. Mirrors `build_cargo_shed`'s
    /// own `center` computation, generalised with an along-shore offset for
    /// tiers that place more than one structure.
    fn apron_center(&self, inland: i32, along: i32) -> Vec2<i32> {
        let base = self.hinge.center() + (-self.normal) * inland;
        if self.seaward_is_x() {
            base + Vec2::new(0, along)
        } else {
            base + Vec2::new(along, 0)
        }
    }

    /// Maps a symmetric `(along_half, inland_half)` extent onto world
    /// `(x, y)` half-extents, oriented onto whichever axis
    /// [`Self::seaward_is_x`] happens to be -- so a structure's footprint
    /// can be described in the port's own along-shore/inland terms and
    /// still come out axis-aligned in world space, the same way
    /// [`Self::deck_strip`] does for the deck.
    fn oriented_half(&self, along_half: i32, inland_half: i32) -> Vec2<i32> {
        if self.seaward_is_x() {
            Vec2::new(inland_half, along_half)
        } else {
            Vec2::new(along_half, inland_half)
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
            self.scatter_cell(painter, cell, field, density);
            offset += TILE_SIZE as i32;
        }
    }

    /// One roll of the prop scatter for a single deck-tile-sized `cell`,
    /// shared by [`Self::build_prop_scatter`] (`Jetty`/`Pier`'s single-lane
    /// deck) and [`Self::build_quay_prop_scatter`] (`Quay`/`Harbour`'s wider
    /// quay body plus finger piers, which cannot iterate the deck's full
    /// width as one strip without scattering props into the gaps between
    /// fingers).
    fn scatter_cell(&self, painter: &Painter, cell: Aabr<i32>, field: RandomField, density: f32) {
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
    }

    /// A solid dressed-stone quay wall with fill behind it, replacing
    /// `Jetty`/`Pier`'s spaced pilings/footings with one continuous
    /// retaining wall across the quay body's full along-shore width, then
    /// one per finger pier -- the material difference that marks
    /// `Quay`/`Harbour` as the two heavier tiers. The causeway ramp is
    /// shared with every other tier.
    fn build_quay_wall(&self, painter: &Painter) {
        let fill = self.stone_fill();
        self.build_causeway(painter, fill.clone());

        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let (quay_depth_tiles, ..) = self.quay_params();
        let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();

        if quay_depth_blocks > run_blocks {
            let body = self.deck_strip(run_blocks, quay_depth_blocks);
            painter
                .aabb(Aabb {
                    min: body.min.with_z(self.water_alt - SUPPORT_DEPTH_BELOW_WATER),
                    max: body.max.with_z(self.deck_alt + 1),
                })
                .fill(fill.clone());
        }

        for (from, to) in self.finger_bands() {
            let finger =
                self.along_shore_strip(self.deck_strip(quay_depth_blocks, reach), from, to);
            painter
                .aabb(Aabb {
                    min: finger
                        .min
                        .with_z(self.water_alt - SUPPORT_DEPTH_BELOW_WATER),
                    max: finger.max.with_z(self.deck_alt + 1),
                })
                .fill(fill.clone());
        }
    }

    /// Mooring posts along both edges of each finger pier, spaced
    /// [`SUPPORT_SPACING`] blocks apart -- the `Quay`/`Harbour` counterpart
    /// to [`Self::build_bollard_line`], restricted to the finger bands so a
    /// bollard never lands in the water gap between two fingers.
    fn build_finger_bollards(&self, painter: &Painter) {
        let fill = self.wood_fill();
        let (quay_depth_tiles, ..) = self.quay_params();
        let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();
        for (from, to) in self.finger_bands() {
            let mut offset = quay_depth_blocks + SUPPORT_SPACING / 2;
            while offset < reach {
                let strip = self.along_shore_strip(self.deck_strip(offset, offset + 1), from, to);
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
    }

    /// [`Self::build_prop_scatter`]'s counterpart for `Quay`/`Harbour`: rolls
    /// the same per-cell scatter over the quay body (the causeway-to-quay-
    /// depth run, full along-shore width) and then over each finger pier's
    /// own band, rather than over the deck's full width as one strip --
    /// which would scatter crates into the open-water gaps between fingers.
    fn build_quay_prop_scatter(&self, painter: &Painter, density: f32) {
        let field = RandomField::new(0x506C_5254);
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let (quay_depth_tiles, ..) = self.quay_params();
        let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
        let reach = self.deck_reach_blocks();

        let mut offset = run_blocks;
        while offset + TILE_SIZE as i32 <= quay_depth_blocks {
            let cell = self.deck_strip(offset, offset + TILE_SIZE as i32);
            self.scatter_cell(painter, cell, field, density);
            offset += TILE_SIZE as i32;
        }

        for (from, to) in self.finger_bands() {
            let mut offset = quay_depth_blocks;
            while offset + TILE_SIZE as i32 <= reach {
                let cell = self.along_shore_strip(
                    self.deck_strip(offset, offset + TILE_SIZE as i32),
                    from,
                    to,
                );
                self.scatter_cell(painter, cell, field, density);
                offset += TILE_SIZE as i32;
            }
        }
    }

    /// A stone warehouse on the apron (~8 blocks tall) -- the `Quay` tier's
    /// vertical presence, and one of `Harbour`'s two. `along_offset` lets
    /// `Harbour` place a second one beside the first without overlapping it.
    fn build_warehouse(&self, painter: &Painter, along_offset: i32) {
        const HALF_ALONG: i32 = 4;
        const HALF_INLAND: i32 = 5;
        const HEIGHT: i32 = 8;
        const SETBACK: i32 = 5;

        let half = self.oriented_half(HALF_ALONG, HALF_INLAND);
        let center = self.apron_center(SETBACK + HALF_INLAND, along_offset);
        let base = self.alt;
        let wall = self.stone_fill();
        let roof = self.wood_fill();

        painter
            .aabb(Aabb {
                min: (center - half).with_z(base),
                max: (center + half).with_z(base + HEIGHT),
            })
            .fill(wall);
        painter
            .pyramid(Aabb {
                min: (center - half - 1).with_z(base + HEIGHT),
                max: (center + half + 1).with_z(base + HEIGHT + 3),
            })
            .fill(roof);
    }

    /// A dockside crane standing right at the hinge (the quay's own edge,
    /// not set back on the apron -- a crane's whole job is to reach cargo on
    /// a hull moored just beyond it): a stone mast ~12 blocks tall with a
    /// timber boom projecting seaward and a short hanging cable cue.
    /// `along_offset` spaces `Harbour`'s two cranes apart.
    fn build_crane(&self, painter: &Painter, along_offset: i32) {
        const MAST_HEIGHT: i32 = 12;
        const MAST_RADIUS: f32 = 1.0;
        const BOOM_LEN: i32 = 6;

        let base = self.apron_center(0, along_offset);
        let mast_fill = self.stone_fill();
        let boom_fill = self.wood_fill();

        painter
            .cylinder_with_radius(base.with_z(self.alt), MAST_RADIUS, MAST_HEIGHT as f32)
            .fill(mast_fill);

        let boom_z = self.alt + MAST_HEIGHT - 1;
        let boom_tip = base + self.normal * BOOM_LEN;
        painter
            .line(base.with_z(boom_z), boom_tip.with_z(boom_z), 0.6)
            .fill(boom_fill.clone());
        painter
            .line(boom_tip.with_z(boom_z), boom_tip.with_z(boom_z - 3), 0.2)
            .fill(boom_fill);
    }

    /// The `Harbour` tier's harbourmaster hall: a two-storey stone hall
    /// ~14 blocks tall with a bell on the roof and an enterable, hollow
    /// ground floor -- the tallest single structure any tier builds, which
    /// is deliberate: this is a settlement's vertical silhouette from the
    /// air, and the four tiers are meant to read as different things from
    /// up there.
    fn build_harbourmaster_hall(&self, painter: &Painter) {
        const HALF_ALONG: i32 = 5;
        const HALF_INLAND: i32 = 6;
        const GROUND_HEIGHT: i32 = 7;
        const UPPER_HEIGHT: i32 = 6;
        const SETBACK: i32 = 6;
        const DOOR_HALF_WIDTH: i32 = 1;

        let half = self.oriented_half(HALF_ALONG, HALF_INLAND);
        let center = self.apron_center(SETBACK + HALF_INLAND, 0);
        let base = self.alt;
        let top = base + GROUND_HEIGHT + UPPER_HEIGHT;
        let wall = self.stone_fill();
        let roof = self.wood_fill();

        let outer = painter.aabb(Aabb {
            min: (center - half).with_z(base),
            max: (center + half).with_z(top),
        });
        let interior = painter.aabb(Aabb {
            min: (center - half + 1).with_z(base + 1),
            max: (center + half - 1).with_z(top - 1),
        });
        outer.without(interior).fill(wall);

        // The seaward wall, punched through so the ground floor is
        // genuinely enterable from the deck side rather than merely hollow.
        let door_half = self.oriented_half(DOOR_HALF_WIDTH, 2);
        let door_centre = center + self.normal * HALF_INLAND;
        painter
            .aabb(Aabb {
                min: (door_centre - door_half).with_z(base + 1),
                max: (door_centre + door_half).with_z(base + 4),
            })
            .clear();

        painter
            .pyramid(Aabb {
                min: (center - half - 1).with_z(top),
                max: (center + half + 1).with_z(top + 4),
            })
            .fill(roof);
        painter.sprite(center.with_z(top + 4), SpriteKind::Bell);
    }

    /// `sprite` every `spacing` blocks along the seaward-facing edge of the
    /// quay wall body (just landward of the fingers) -- the shared
    /// implementation behind [`Self::build_quay_lighting`] and
    /// [`Self::build_harbour_lighting`].
    fn build_edge_lights(&self, painter: &Painter, sprite: SpriteKind, spacing: i32) {
        let (quay_depth_tiles, ..) = self.quay_params();
        let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
        let face = self.deck_strip(quay_depth_blocks - 1, quay_depth_blocks);
        let total = self.deck_along_shore_blocks();
        let mut offset = spacing / 2;
        while offset < total {
            let lit = self.along_shore_strip(face, offset, offset + 1).center();
            painter.sprite(lit.with_z(self.deck_alt + 1), sprite);
            offset += spacing;
        }
    }

    /// `StreetLamp` every 4 tiles along the quay wall -- the `Quay` tier's
    /// night lighting.
    fn build_quay_lighting(&self, painter: &Painter) {
        self.build_edge_lights(painter, SpriteKind::StreetLamp, LIGHT_SPACING_QUAY);
    }

    /// `StreetLampTall` every 3 tiles along the quay wall, plus one
    /// `StreetLamp` at each finger pier's head -- the `Harbour` tier's night
    /// lighting, and the only tier that combines both a spaced run and a
    /// per-finger light.
    fn build_harbour_lighting(&self, painter: &Painter) {
        self.build_edge_lights(painter, SpriteKind::StreetLampTall, LIGHT_SPACING_HARBOUR);

        let reach = self.deck_reach_blocks();
        let tip = self.deck_strip(reach - 1, reach);
        for (from, to) in self.finger_bands() {
            let lit = self.along_shore_strip(tip, from, to).center();
            painter.sprite(lit.with_z(self.deck_alt + 1), SpriteKind::StreetLamp);
        }
    }

    /// One `LanternpostWoodLantern` at the pier head -- the only lighting a
    /// `Pier` gets. `Jetty` gets none; `Quay`/`Harbour` get their own spaced
    /// street lighting above.
    fn build_pier_lantern(&self, painter: &Painter) {
        let reach = self.deck_reach_blocks();
        let tip = self.deck_strip(reach - 1, reach).center();
        painter.sprite(
            tip.with_z(self.deck_alt + 1),
            SpriteKind::LanternpostWoodLantern,
        );
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

    /// Stone footings with a timber deck, a cargo shed on the apron, and one
    /// lantern at the pier head -- the `Pier` tier.
    fn render_pier(&self, painter: &Painter) {
        let stone = self.stone_fill();
        let wood = self.wood_fill();
        self.build_causeway(painter, stone);
        self.build_deck_cap(painter, wood);
        self.build_footings(painter);
        self.build_bollard_line(painter);
        self.build_prop_scatter(painter, PROP_DENSITY_PIER);
        self.build_cargo_shed(painter);
        self.build_pier_lantern(painter);
    }

    /// A dressed-stone quay wall with one finger pier's worth of berthing on
    /// each side, one warehouse and one crane on the apron, and street
    /// lighting along the quay -- the `Quay` tier.
    fn render_quay(&self, painter: &Painter) {
        self.build_quay_wall(painter);
        self.build_finger_bollards(painter);
        self.build_quay_prop_scatter(painter, PROP_DENSITY_QUAY);
        self.build_warehouse(painter, 0);
        self.build_crane(painter, 0);
        self.build_quay_lighting(painter);
    }

    /// The capital-scale tier: a dressed-stone quay wall with three finger
    /// piers, two warehouses, two cranes, the harbourmaster hall, and the
    /// only tier with both a spaced quay-light run and per-finger lighting
    /// -- the `Harbour` tier.
    fn render_harbour(&self, painter: &Painter) {
        const STRUCTURE_SPACING: i32 = 10;

        self.build_quay_wall(painter);
        self.build_finger_bollards(painter);
        self.build_quay_prop_scatter(painter, PROP_DENSITY_HARBOUR);
        self.build_warehouse(painter, -STRUCTURE_SPACING);
        self.build_warehouse(painter, STRUCTURE_SPACING);
        self.build_crane(painter, -STRUCTURE_SPACING);
        self.build_crane(painter, STRUCTURE_SPACING);
        self.build_harbourmaster_hall(painter);
        self.build_harbour_lighting(painter);
    }

    /// How many blocks of the deck's own seaward reach lie beyond the
    /// causeway -- the span `Jetty`/`Pier` berths are laid out over, since
    /// neither tier has fingers to subdivide. Mirrors `quay_depth_blocks`'s
    /// role for `Quay`/`Harbour`.
    fn single_lane_reach_blocks(&self) -> i32 {
        self.deck_reach_blocks() - self.ramp_tiles * TILE_SIZE as i32
    }

    /// The one-block step, perpendicular to the seaward normal, from a
    /// berth's gangway (on the deck) out to its mooring position (in open
    /// water) -- away from the deck's own body on whichever edge `min_edge`
    /// selects.
    fn edge_outward_offset(&self, min_edge: bool) -> Vec2<i32> {
        if self.seaward_is_x() {
            if min_edge {
                Vec2::new(0, -1)
            } else {
                Vec2::new(0, 1)
            }
        } else if min_edge {
            Vec2::new(-1, 0)
        } else {
            Vec2::new(1, 0)
        }
    }

    /// One berth's geometry, from a seaward-sliced `strip` (already
    /// restricted to the berth's own along-shore band for a finger pier) and
    /// which of its two edges to moor against.
    ///
    /// A berth's mooring edge runs along the lane/finger's own seaward
    /// reach -- the same axis `build_bollard_line`/`build_pilings` already
    /// walk to place mooring posts -- not along the shoreline. A hull tied
    /// up against that edge therefore points along `self.normal` (out to
    /// sea), not across it.
    fn edge_berth(&self, class: BerthClass, min_edge: bool, strip: Aabr<i32>) -> BerthSlot {
        let sliver = self.edge_sliver(strip, min_edge);
        let gangway = sliver.center();
        let mooring = gangway + self.edge_outward_offset(min_edge);
        BerthSlot {
            class,
            side: if min_edge {
                BerthSide::Port
            } else {
                BerthSide::Starboard
            },
            gangway,
            mooring,
            heading: self.normal.as_::<f32>(),
        }
    }

    /// A `Jetty`/`Pier` berth: the single lane has no fingers to subdivide,
    /// so the berth's seaward span starts right after the causeway and runs
    /// `span` blocks, clamped to the lane's own reach.
    fn lane_berth(&self, class: BerthClass, min_edge: bool, span: i32) -> BerthSlot {
        let run_blocks = self.ramp_tiles * TILE_SIZE as i32;
        let span = span.min(self.single_lane_reach_blocks());
        let strip = self.deck_strip(run_blocks, run_blocks + span);
        self.edge_berth(class, min_edge, strip)
    }

    /// A `Quay`/`Harbour` berth on one edge of one finger pier, `(from,
    /// to)` being that finger's own along-shore band from
    /// [`Self::finger_bands`].
    fn finger_berth(
        &self,
        class: BerthClass,
        min_edge: bool,
        (from, to): (i32, i32),
        span: i32,
    ) -> BerthSlot {
        let (quay_depth_tiles, ..) = self.quay_params();
        let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
        let finger_len = self.deck_reach_blocks() - quay_depth_blocks;
        let span = span.min(finger_len);
        let strip = self.along_shore_strip(
            self.deck_strip(quay_depth_blocks, quay_depth_blocks + span),
            from,
            to,
        );
        self.edge_berth(class, min_edge, strip)
    }

    /// Every berth slot this tier's geometry offers, in pure tile/block
    /// space -- no terrain sampling, so it is unit-testable against the
    /// synthetic fixtures the rest of this module's tests already use.
    /// [`Self::measure_berths`] is what turns these into real [`Berth`]s by
    /// checking each one against real depth and water.
    ///
    /// Berth counts and classes follow spec §4.3 exactly (`Jetty` 1 `Small`;
    /// `Pier` 1 `Large` + 1 `Small`; `Quay` 2 `Large` + 2 `Small`; `Harbour`
    /// 4 `Large` + 2 `Small`). `Quay`/`Harbour` assign a whole finger pier's
    /// both edges to the same class -- `Large`-designated fingers first --
    /// which is the only split that keeps a finger's two berths from
    /// disagreeing about how much clear water the hull on each side needs.
    ///
    /// ⚠️ This function assumes a finger pier's two berths always share one
    /// class. A port whose layout instead splits a single finger's two
    /// berths by class along its length (e.g. `Large` at the head, `Small`
    /// inshore on a longer finger) cannot be expressed by overriding
    /// [`PortClass::berth_counts`] alone -- this match (and
    /// `berth_counts`) would need to branch on finger length/position, not
    /// just on [`PortClass`]. The `Berth`/`Anchorage`/[`NavalDockInfo`]
    /// data shapes themselves place no such constraint; only this
    /// generation algorithm does.
    fn berth_layout(&self) -> Vec<BerthSlot> {
        match self.class {
            PortClass::Jetty => {
                vec![self.lane_berth(BerthClass::Small, true, BERTH_FACE_SMALL_BLOCKS)]
            },
            PortClass::Pier => vec![
                self.lane_berth(BerthClass::Large, true, BERTH_FACE_LARGE_BLOCKS),
                self.lane_berth(BerthClass::Small, false, BERTH_FACE_SMALL_BLOCKS),
            ],
            PortClass::Quay | PortClass::Harbour => {
                let (_small, large) = self.class.berth_counts();
                debug_assert_eq!(
                    large % 2,
                    0,
                    "a finger pier offers both its berths to the same class, so a tier's Large \
                     count must be even"
                );
                let large_fingers = large / 2;
                self.finger_bands()
                    .into_iter()
                    .enumerate()
                    .flat_map(|(i, band)| {
                        let class = if i < large_fingers {
                            BerthClass::Large
                        } else {
                            BerthClass::Small
                        };
                        let span = match class {
                            BerthClass::Large => BERTH_FACE_LARGE_BLOCKS,
                            BerthClass::Small => BERTH_FACE_SMALL_BLOCKS,
                        };
                        [true, false]
                            .into_iter()
                            .map(move |min_edge| self.finger_berth(class, min_edge, band, span))
                    })
                    .collect()
            },
        }
    }

    /// Water depth at `wpos`, in blocks -- the same `SimChunk::water_alt -
    /// alt` field `Site::deck_centre_line_depth` measures the deck's own
    /// placement against, so a berth's recorded depth and the placement
    /// that put the deck there can never silently disagree about which
    /// field "depth" means.
    fn sample_depth(&self, land: &Land, wpos: Vec2<i32>) -> Option<i32> {
        land.get_chunk_wpos(wpos)
            .map(|chunk| (chunk.water_alt - chunk.alt) as i32)
    }

    /// Whether every sampled point of the straight segment `from -> to`
    /// lands in a chunk `wpos_is_hazard` would itself call water
    /// (`river.near_water()`) -- the same predicate the engine's own
    /// shoreline hazard stamp is built from, reused here so "is this open
    /// water" means one thing across the whole naval-port feature.
    fn segment_is_over_water(&self, land: &Land, from: Vec2<i32>, to: Vec2<i32>) -> bool {
        (0..=WATER_SEGMENT_SAMPLES).all(|i| {
            let p = from + (to - from) * i / WATER_SEGMENT_SAMPLES;
            land.get_chunk_wpos(p)
                .is_some_and(|chunk| chunk.river.near_water())
        })
    }

    /// Turns [`Self::berth_layout`]'s pure geometry into real [`Berth`]s,
    /// dropping any slot whose measured depth or approach fails: a `Jetty`
    /// may legitimately end up with zero berths, and that must be visible
    /// in the generation log rather than silently becoming a decorative
    /// plank.
    fn measure_berths(&self, land: &Land, site_name: &str) -> Vec<Berth> {
        // `local_id` must be assigned AFTER filtering, not before: a
        // surviving berth's id has to be a dense `0..len()` range with no
        // gaps, because `all_naval_berths` advances its global counter by
        // `berths.len()` (not by `max(id) + 1`) to assign the next site's
        // ids. Numbering before the filter would leave a gap whenever a
        // non-last slot is dropped (shallow water, blocked approach),
        // colliding this port's surviving high id with the next port's
        // first id.
        self.berth_layout()
            .into_iter()
            .filter_map(|slot| {
                let min_depth = match slot.class {
                    BerthClass::Small => BERTH_MIN_DEPTH_SMALL,
                    BerthClass::Large => BERTH_MIN_DEPTH_LARGE,
                };
                let Some(depth) = self.sample_depth(land, slot.mooring) else {
                    debug!(
                        site = %site_name,
                        class = ?slot.class,
                        mooring = ?slot.mooring,
                        "naval berth dropped: mooring position samples no terrain"
                    );
                    return None;
                };
                if depth < min_depth {
                    debug!(
                        site = %site_name,
                        class = ?slot.class,
                        mooring = ?slot.mooring,
                        depth,
                        min_depth,
                        "naval berth dropped: water too shallow for its class"
                    );
                    return None;
                }
                let approach = slot.mooring + self.normal * BERTH_APPROACH_STANDOFF_BLOCKS;
                if !self.segment_is_over_water(land, slot.mooring, approach) {
                    debug!(
                        site = %site_name,
                        class = ?slot.class,
                        mooring = ?slot.mooring,
                        ?approach,
                        "naval berth dropped: approach does not cross open water only"
                    );
                    return None;
                }
                Some(Berth {
                    // Placeholder; overwritten below once filtering is done.
                    id: 0,
                    class: slot.class,
                    mooring_pos: slot.mooring.with_z(self.water_alt),
                    heading: slot.heading,
                    side: slot.side,
                    gangway: slot.gangway.with_z(self.deck_alt + 1),
                    approach,
                    depth,
                })
            })
            .enumerate()
            .map(|(local_id, mut berth)| {
                berth.id = local_id as u32;
                berth
            })
            .collect()
    }

    /// Whether a `swing_radius`-block circle around `centre` is clear water
    /// at least [`BERTH_MIN_DEPTH_LARGE`] deep, sampled at its centre and
    /// around its circumference.
    fn swing_circle_is_clear(&self, land: &Land, centre: Vec2<i32>, swing_radius: i32) -> bool {
        let deep_enough = |p: Vec2<i32>| {
            self.sample_depth(land, p)
                .is_some_and(|d| d >= BERTH_MIN_DEPTH_LARGE)
        };
        deep_enough(centre)
            && (0..ANCHORAGE_SWING_SAMPLES).all(|i| {
                let angle = i as f32 / ANCHORAGE_SWING_SAMPLES as f32 * std::f32::consts::TAU;
                let probe = centre
                    + Vec2::new(
                        (swing_radius as f32 * angle.cos()) as i32,
                        (swing_radius as f32 * angle.sin()) as i32,
                    );
                deep_enough(probe)
            })
    }

    /// A `Jetty`'s offshore anchorage (spec §11.2): marches seaward from the
    /// deck's outer end along the port's own normal, stopping at the first
    /// column clearing a `Large` berth's depth with a clear swing circle
    /// around it. Not found within range -- or the jetty has no `Small`
    /// berth for a tender to shuttle from in the first place -- emits
    /// nothing and warns naming the settlement, rather than fabricating an
    /// anchorage the real water cannot support.
    fn find_anchorage(&self, land: &Land, site_name: &str, berths: &[Berth]) -> Vec<Anchorage> {
        if self.class != PortClass::Jetty {
            return Vec::new();
        }
        let Some(jetty_berth) = berths.iter().find(|berth| berth.class == BerthClass::Small) else {
            warn!(
                site = %site_name,
                "no offshore anchorage search attempted: this jetty has no Small berth for a \
                 tender to shuttle from"
            );
            return Vec::new();
        };

        let reach = self.deck_reach_blocks();
        let tip = self.deck_strip(reach - 1, reach).center();
        let mut offset = ANCHORAGE_SEARCH_STEP_BLOCKS;
        while offset <= MAX_ANCHORAGE_OFFSET_BLOCKS {
            let pos = tip + self.normal * offset;
            if let Some(depth) = self.sample_depth(land, pos)
                && depth >= BERTH_MIN_DEPTH_LARGE
                && self.swing_circle_is_clear(land, pos, ANCHORAGE_SWING_RADIUS_BLOCKS)
                && self.segment_is_over_water(land, jetty_berth.mooring_pos.xy(), pos)
            {
                let approach = pos + self.normal * BERTH_APPROACH_STANDOFF_BLOCKS;
                return vec![Anchorage {
                    id: berths.len() as u32,
                    class: BerthClass::Large,
                    pos: pos.with_z(self.water_alt),
                    swing_radius: ANCHORAGE_SWING_RADIUS_BLOCKS,
                    depth,
                    approach,
                    tender_berth: jetty_berth.id,
                }];
            }
            offset += ANCHORAGE_SEARCH_STEP_BLOCKS;
        }

        warn!(
            site = %site_name,
            max_offset = MAX_ANCHORAGE_OFFSET_BLOCKS,
            "no offshore anchorage found within range; this village's routes stay \
             SailBoat-capped"
        );
        Vec::new()
    }
}

/// One computed berth slot before depth/approach are checked against real
/// terrain -- the pure-geometry half of berth placement
/// ([`NavalPort::berth_layout`]), kept separate so it can be unit-tested
/// against the synthetic fixtures the rest of this module already uses,
/// with no [`Land`] in reach.
#[derive(Debug, Clone, Copy)]
struct BerthSlot {
    class: BerthClass,
    side: BerthSide,
    /// Deck tile the hull's rail touches -- the walkable gangway block.
    gangway: Vec2<i32>,
    /// One block beyond `gangway`, off the deck's edge -- the hull's
    /// mooring position.
    mooring: Vec2<i32>,
    heading: Vec2<f32>,
}

impl PortClass {
    /// `(small, large)` berth counts for the two finger-pier tiers (spec
    /// §4.3): `Quay` 2 + 2; `Harbour` 2 + 4. `Jetty` and `Pier` have no
    /// finger piers -- [`NavalPort::berth_layout`] builds their single/
    /// double lane berth directly and never calls this, so this function
    /// only covers the two tiers that do.
    fn berth_counts(self) -> (usize, usize) {
        match self {
            Self::Quay => (2, 2),
            Self::Harbour => (2, 4),
            Self::Jetty | Self::Pier => {
                unreachable!("berth_counts is only called for Quay/Harbour; see berth_layout")
            },
        }
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
            PortClass::Pier => self.render_pier(painter),
            PortClass::Quay => self.render_quay(painter),
            PortClass::Harbour => self.render_harbour(painter),
        }
    }

    fn door_tile(&self) -> Option<Vec2<i32>> { Some(self.door_tile) }

    fn naval_dock_info(&self) -> Option<NavalDockInfo<'_>> {
        Some(NavalDockInfo {
            door_tile: self.door_tile,
            center: self.hinge.center(),
            class: self.class,
            berths: &self.berths,
            anchorages: &self.anchorages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `NavalPort` with a deck of `along` × `seaward` blocks, oriented onto
    /// `normal`, for whichever `class` the test needs. `port` below is the
    /// original fixture (`Pier`'s own deck dims), kept as-is so the existing
    /// geometry tests are untouched.
    fn port_of(class: PortClass, normal: Vec2<i32>, along: i32, seaward: i32) -> NavalPort {
        let (dx, dy) = if normal.x != 0 {
            (seaward, along)
        } else {
            (along, seaward)
        };
        let deck = match (normal.x, normal.y) {
            (1, 0) => Aabr {
                min: Vec2::new(30, -dy / 2),
                max: Vec2::new(30 + dx, dy / 2),
            },
            (-1, 0) => Aabr {
                min: Vec2::new(-30 - dx, -dy / 2),
                max: Vec2::new(-30, dy / 2),
            },
            (0, 1) => Aabr {
                min: Vec2::new(-dx / 2, 18),
                max: Vec2::new(dx / 2, 18 + dy),
            },
            _ => Aabr {
                min: Vec2::new(-dx / 2, -18 - dy),
                max: Vec2::new(dx / 2, -18),
            },
        };
        NavalPort {
            class,
            apron: Aabr {
                min: Vec2::new(-60, -60),
                max: Vec2::new(60, 60),
            },
            deck,
            hinge: Aabr::new_empty(Vec2::zero()),
            door_tile: Vec2::zero(),
            normal,
            alt: 10,
            water_alt: 4,
            deck_alt: 6,
            ramp_tiles: 2,
            wood_color: Rgb::new(102, 87, 63),
            berths: Vec::new(),
            anchorages: Vec::new(),
        }
    }

    fn port(normal: Vec2<i32>) -> NavalPort { port_of(PortClass::Pier, normal, 18, 66) }

    /// `along`/`seaward` blocks matching `class.deck_dims()` (spec §4.3),
    /// so `finger_bands`/`quay_params` are tested against the same deck size
    /// the real placement pass would actually hand them.
    fn quay_port(class: PortClass, normal: Vec2<i32>) -> NavalPort {
        let (along_tiles, seaward_tiles) = match class {
            PortClass::Harbour => (24, 13),
            PortClass::Quay => (16, 12),
            PortClass::Jetty | PortClass::Pier => unreachable!("test fixture is Quay/Harbour only"),
        };
        port_of(
            class,
            normal,
            along_tiles * TILE_SIZE as i32,
            seaward_tiles * TILE_SIZE as i32,
        )
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

    #[test]
    fn finger_bands_tile_the_deck_without_overlapping_or_touching_the_edges() {
        for class in [PortClass::Quay, PortClass::Harbour] {
            for normal in [
                Vec2::new(1, 0),
                Vec2::new(-1, 0),
                Vec2::new(0, 1),
                Vec2::new(0, -1),
            ] {
                let port = quay_port(class, normal);
                let (_, count, width_tiles) = port.quay_params();
                let width = width_tiles * TILE_SIZE as i32;
                let total = port.deck_along_shore_blocks();
                let bands = port.finger_bands();

                assert_eq!(
                    bands.len(),
                    count as usize,
                    "{class:?} should place exactly {count} finger piers, normal {normal:?}"
                );

                let mut prev_to = 0;
                for &(from, to) in &bands {
                    assert_eq!(
                        to - from,
                        width,
                        "{class:?}'s finger band is not {width} blocks wide, normal {normal:?}"
                    );
                    assert!(
                        from > prev_to,
                        "{class:?}'s finger bands must not touch or overlap, normal {normal:?}"
                    );
                    assert!(
                        to <= total,
                        "{class:?}'s finger band must stay within the deck's along-shore extent, \
                         normal {normal:?}"
                    );
                    prev_to = to;
                }
                assert!(
                    prev_to < total,
                    "{class:?} must leave a gap after the last finger, normal {normal:?}"
                );
            }
        }
    }

    #[test]
    fn quay_depth_leaves_room_for_the_fingers_to_actually_project() {
        for class in [PortClass::Quay, PortClass::Harbour] {
            let port = quay_port(class, Vec2::new(1, 0));
            let (quay_depth_tiles, ..) = port.quay_params();
            let quay_depth_blocks = quay_depth_tiles * TILE_SIZE as i32;
            assert!(
                quay_depth_blocks < port.deck_reach_blocks(),
                "{class:?}'s quay body must not consume the deck's entire seaward reach, leaving \
                 no room for finger piers"
            );
        }
    }

    #[test]
    fn oriented_half_maps_along_inland_onto_world_axes_by_seaward_direction() {
        let along_x = port(Vec2::new(1, 0)).oriented_half(3, 7);
        assert_eq!(
            along_x,
            Vec2::new(7, 3),
            "seaward-along-x: inland maps to world x, along-shore to world y"
        );
        let along_y = port(Vec2::new(0, 1)).oriented_half(3, 7);
        assert_eq!(
            along_y,
            Vec2::new(3, 7),
            "seaward-along-y: along-shore maps to world x, inland to world y"
        );
    }

    #[test]
    fn apron_center_moves_inland_of_the_hinge_and_offsets_along_shore() {
        // The fixture's hinge is the empty aabr at the origin, so
        // `hinge.center()` is `Vec2::zero()` and every offset below is
        // relative to world zero.
        let seaward_x = port(Vec2::new(1, 0));
        assert_eq!(
            seaward_x.apron_center(5, 3),
            Vec2::new(-5, 3),
            "inland is opposite the seaward normal (world x here); along-shore is world y"
        );
        let seaward_y = port(Vec2::new(0, -1));
        assert_eq!(
            seaward_y.apron_center(5, 3),
            Vec2::new(3, 5),
            "inland is opposite the seaward normal (world y here); along-shore is world x"
        );
    }

    /// `Jetty`'s single lane of `single_lane_reach_blocks` -- as sized by
    /// the real deck dims -- fits the `Small` berth span exactly.
    fn jetty_port(normal: Vec2<i32>) -> NavalPort {
        port_of(
            PortClass::Jetty,
            normal,
            2 * TILE_SIZE as i32,
            7 * TILE_SIZE as i32,
        )
    }

    #[test]
    fn berth_layout_matches_the_per_tier_counts_and_classes() {
        for (class, expected) in [
            (PortClass::Jetty, vec![BerthClass::Small]),
            (PortClass::Pier, vec![BerthClass::Large, BerthClass::Small]),
            (PortClass::Quay, vec![
                BerthClass::Large,
                BerthClass::Large,
                BerthClass::Small,
                BerthClass::Small,
            ]),
            (PortClass::Harbour, vec![
                BerthClass::Large,
                BerthClass::Large,
                BerthClass::Large,
                BerthClass::Large,
                BerthClass::Small,
                BerthClass::Small,
            ]),
        ] {
            let port = match class {
                PortClass::Jetty => jetty_port(Vec2::new(1, 0)),
                PortClass::Pier => port(Vec2::new(1, 0)),
                PortClass::Quay | PortClass::Harbour => quay_port(class, Vec2::new(1, 0)),
            };
            let layout = port.berth_layout();
            let classes: Vec<BerthClass> = layout.iter().map(|slot| slot.class).collect();
            assert_eq!(
                classes, expected,
                "{class:?} did not lay out the berth classes spec §4.3 calls for"
            );
        }
    }

    #[test]
    fn berth_slots_never_share_a_mooring_position() {
        for class in [
            PortClass::Jetty,
            PortClass::Pier,
            PortClass::Quay,
            PortClass::Harbour,
        ] {
            let port = match class {
                PortClass::Jetty => jetty_port(Vec2::new(1, 0)),
                PortClass::Pier => port(Vec2::new(1, 0)),
                PortClass::Quay | PortClass::Harbour => quay_port(class, Vec2::new(1, 0)),
            };
            let layout = port.berth_layout();
            for (i, a) in layout.iter().enumerate() {
                for b in &layout[i + 1..] {
                    assert_ne!(
                        a.mooring, b.mooring,
                        "{class:?} laid out two berths at the same mooring position"
                    );
                }
            }
        }
    }

    #[test]
    fn berth_mooring_is_one_block_beyond_its_own_gangway() {
        let port = port(Vec2::new(1, 0));
        for slot in port.berth_layout() {
            let delta = slot.mooring - slot.gangway;
            assert_eq!(
                delta.map(|e| e.abs()).sum(),
                1,
                "a berth's mooring position must be exactly one block off its gangway"
            );
        }
    }

    #[test]
    fn berth_heading_points_out_to_sea_along_the_port_s_own_normal() {
        for normal in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            for class in [
                PortClass::Jetty,
                PortClass::Pier,
                PortClass::Quay,
                PortClass::Harbour,
            ] {
                let port = match class {
                    PortClass::Jetty => jetty_port(normal),
                    PortClass::Pier => port(normal),
                    PortClass::Quay | PortClass::Harbour => quay_port(class, normal),
                };
                for slot in port.berth_layout() {
                    assert_eq!(
                        slot.heading,
                        normal.as_::<f32>(),
                        "{class:?}'s berth heading must point out to sea along the port's own \
                         normal, not across the mooring edge, for normal {normal:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn single_lane_reach_excludes_the_causeway() {
        let port = port(Vec2::new(1, 0));
        assert_eq!(
            port.single_lane_reach_blocks(),
            port.deck_reach_blocks() - port.ramp_tiles * TILE_SIZE as i32,
        );
    }
}
