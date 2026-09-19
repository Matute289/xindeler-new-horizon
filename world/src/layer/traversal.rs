//! Traversability analysis for world generation: can a body still get
//! through this passage after something solid landed in it?
//!
//! World-gen has never asked this question before. Every other layer decides
//! *what to write* from column data alone and nothing reasons about whether
//! the resulting void is passable. This module introduces that concept, and
//! deliberately keeps it **region-agnostic**: nothing here knows about
//! Cromatolis, about boulders, or about any particular carve function. It is
//! driven by two small traits --- [`SolidVolume`] (what the intruder
//! occupies) and [`PassageQuery`] (what a column's carved-open band is) ---
//! so a future intruder (a tree whose roots reach a tunnel, an authored
//! prop) or a future authored map reuses it without editing this file.
//!
//! The three things worth reading before changing anything here:
//!
//! 1. **Obstruction is a connectivity question, not a percentage.** A boulder
//!    can seal the middle of a passage while leaving a perfectly good slot
//!    along one wall, and no scalar "fraction of cross-section filled" metric
//!    can tell that apart from a real seal. So the test is a flood fill over
//!    traversal slots, run twice --- once with the intruder and once without
//!    it.
//! 2. **Nothing is repaired that was not already broken.** The intruder-removed
//!    fill is a *pre-condition*, not an optimisation: without it the mechanism
//!    would quietly start widening thin passages all over the map, which is a
//!    far larger change than the one being asked for.
//! 3. **This engine has no crouch.** `CharacterState::Crawl` is the *downed*
//!    state (`common/src/states/crawl.rs`), entered when death protection has
//!    been consumed --- not a voluntary stance. Sneaking does not shrink the
//!    collider; the low-ceiling squash in `voxygen/src/scene/figure/mod.rs` is
//!    cosmetic rendering only. So no gap this module produces may ever be sized
//!    for a crouching body: two blocks of clear air is the hard floor, full
//!    stop.
//!
//! Every repair is **strictly additive** --- it only ever turns a solid
//! voxel into air, only inside the intruder's own bounding box dilated by
//! [`TraversalParams::target_width`], and never above the passage's own
//! ceiling limit. Nothing is moved and nothing authored is deleted.
//!
//! Two limits of that guarantee, both worth knowing and neither worth
//! machinery:
//!
//! * It is a property of the pass that calls this, not of the finished chunk.
//!   Later layers write solid blocks and can refill a repaired channel; so,
//!   within a single pass, can a second intruder stamped after the first one's
//!   repair. Both are rare and neither can make anything *less* passable than
//!   it was before this module existed, since every write it causes only ever
//!   adds air.
//! * The passage model is whatever the [`PassageQuery`] implementor reports.
//!   Anything else already written into that void --- a structure, another
//!   layer's fill --- is invisible here, which errs toward missing an
//!   obstruction rather than inventing one.

use std::collections::BinaryHeap;
use vek::*;

/// The traversal numbers this module is built on.
///
/// The hard limits --- [`Self::min_height`], [`Self::min_width`] and
/// [`Self::max_navmesh_step`] --- are **read out of the engine**, not
/// chosen: they are what the player collider and the pathfinder's own
/// neighbour set accept, so lowering one produces geometry no body can get
/// through. That is why they live here rather than in a data file: a
/// designer editing them would be filing a bug, not tuning a dial. The
/// remaining three are deliberate design choices --- two targets and one
/// bound on the last-resort repair --- and say so. Each field's doc comment
/// names the engine fact or the reasoning behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraversalParams {
    /// Hard floor on clear air in a passage, in blocks.
    ///
    /// The player collider is `CapsulePrism { radius: 0.4, z_min: 0.0,
    /// z_max: 1.75 }` (`server/src/state_ext.rs`), and the height modifier
    /// that would shrink it applies only during a roll or a glide
    /// (`common/systems/src/phys/mod.rs`) --- momentary states that cannot
    /// be chained down a corridor. Independently, NPC pathing calls a column
    /// walkable only when `pos` *and* `pos + 1` are both non-solid over
    /// ground (`common/src/path.rs`), i.e. exactly two blocks. Both give 2.
    pub min_height: i32,
    /// Design target for clear air in a repaired passage, in blocks.
    ///
    /// Chosen, not derived --- but one block above [`Self::min_height`] for
    /// a reason: three blocks is exactly what the pathfinder wants at the
    /// column a `+1` step leaves from, so a corridor at the target height
    /// can contain a step at all. It also leaves room to top out a climb
    /// rather than bump the ceiling on the way up, and keeps the rendered
    /// figure out of its low-ceiling squash.
    pub target_height: i32,
    /// Hard floor on the width of a repaired channel, in blocks.
    ///
    /// The player capsule is 0.8 blocks across, so two blocks fit only if
    /// the body is well centred.
    pub min_width: i32,
    /// Design target for the width of a repaired channel, in blocks.
    ///
    /// Three blocks is "hug the wall and squeeze past" without the collider
    /// catching on either side.
    pub target_width: i32,
    /// Largest floor-to-floor step a repaired route may contain, in blocks.
    ///
    /// This is the ceiling of the A\* neighbour set in `common/src/path.rs`:
    /// `DIRS` steps `+1` and `JUMPS` steps `+2` (and `JUMPS` additionally
    /// requires `pos + 2` / `pos + 3` non-solid). Above this an agent simply
    /// cannot follow the route --- see [`PassageVerdict::agent_passable`],
    /// which reports that honestly rather than assuming it away. It also
    /// matches the player's own jump apex exactly: the jump impulse is
    /// `0.4 * mass * GRAVITY`, so `dv = 10 m/s` against `GRAVITY = 25`
    /// (`common/src/states/utils.rs`, `common/src/consts.rs`) --- 2.0
    /// blocks.
    pub max_navmesh_step: i32,
    /// How far a repair may lower a passage floor, in blocks.
    ///
    /// A bound on the last-resort repair, and only ever available where the
    /// passage's own tier permits it (see
    /// [`AccommodationTier::allows_floor_dig`]) --- digging under authored
    /// geometry would strand or destroy content placed on the original
    /// floor.
    pub max_floor_dig: i32,
}

impl TraversalParams {
    /// The values derived from this engine's own movement and pathing code.
    /// See each field's doc comment for the source it was read from.
    pub const ENGINE: Self = Self {
        min_height: 2,
        target_height: 3,
        min_width: 2,
        target_width: 3,
        max_navmesh_step: 2,
        max_floor_dig: 4,
    };
}

impl Default for TraversalParams {
    fn default() -> Self { Self::ENGINE }
}

/// How much of a passage's geometry a human actually chose --- which is what
/// decides how much a repair is allowed to change, **not** whether the
/// geometry is authored or procedural.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccommodationTier {
    /// Noise-derived geometry. A local widening of a few blocks is inside
    /// the layer's own variance and nobody authored the radius it changes,
    /// so every repair is available.
    Procedural,
    /// Catalog-parameterised geometry: a human chose the anchor, the size
    /// class and the contents, and the shape was derived from those. Repairs
    /// that only open air are fine; lowering the floor is not, because
    /// content is placed *on* that floor at carve time.
    Catalog,
    /// Hand-authored architecture --- named rooms, gates, authored accesses,
    /// and navigation graphs derived from the authored splines. A channel
    /// cut through a wall here is a content edit that nothing downstream
    /// would know about, so the only acceptable answer is to keep the
    /// intruder out entirely.
    HandAuthored,
}

impl AccommodationTier {
    /// Whether this tier may lower a passage floor (the last-resort repair).
    pub fn allows_floor_dig(self) -> bool { matches!(self, Self::Procedural) }

    /// Whether this tier may be repaired at all, as opposed to requiring the
    /// intruder to be rejected outright.
    pub fn allows_repair(self) -> bool { !matches!(self, Self::HandAuthored) }
}

/// What an intruding solid occupies. The first implementor is a boulder, and
/// the contract is deliberately narrow so any analytic solid can satisfy it
/// without being stamped into a chunk first.
pub trait SolidVolume {
    /// World-space bounding box fully containing the solid, both ends
    /// inclusive. An implementor whose own extent is half-open may report
    /// the exclusive bound as the inclusive one: this is only ever used to
    /// bound a search and a repair, so a row of slack costs nothing.
    fn bounds(&self) -> Aabb<i32>;

    /// Push the solidity of `z_lo ..= z_hi` at this column onto `out`, in
    /// ascending `z` order, appending exactly `z_hi - z_lo + 1` entries.
    ///
    /// Implementors whose solidity depends on a top-down scan of the whole
    /// column (a de-floating rule, say) must run that scan internally, so
    /// that the answer never depends on which sub-range was asked for.
    fn column_solid(&self, wpos2d: Vec2<i32>, z_lo: i32, z_hi: i32, out: &mut Vec<bool>);
}

/// One column of a passage: the carved-open vertical bands, and the highest
/// `z` any repair is allowed to reach at this column.
#[derive(Clone, Debug, Default)]
pub struct PassageColumn {
    /// Disjoint, ascending `(floor, ceiling)` pairs, both inclusive. More
    /// than one band is normal where two passages of the same family cross
    /// the same column at different depths.
    pub bands: Vec<(i32, i32)>,
    /// No repair may carve above this `z`. For every passage family in this
    /// engine that is the same surface clamp the carve itself uses, so a
    /// repair can never punch a hole through to daylight.
    pub ceiling_limit: i32,
}

/// What a passage's carved-open geometry is, column by column. The first two
/// implementors are the procedural tunnel layer and the authored cave
/// catalogue; a future authored map becomes a third without this file
/// changing.
pub trait PassageQuery {
    /// Which repair budget this passage's geometry earns.
    fn tier(&self) -> AccommodationTier;

    /// The open bands at this column, or `None` if this passage does not
    /// claim the column at all.
    fn column(&self, wpos2d: Vec2<i32>) -> Option<PassageColumn>;
}

/// How an intruder sits in a passage. Classification runs cheapest-first;
/// only [`ObstructionClass::Sealed`] pays for the flood fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObstructionClass {
    /// The intruder does not reach any open band. Nothing to do.
    Clear,
    /// The intruder hangs from the ceiling with the full minimum height
    /// still clear beneath it, in every column it touches. This is the
    /// wanted case --- a rock wedged in the roof reads as real --- and it
    /// costs a scalar comparison, no fill.
    CeilingPendant,
    /// The intruder is rooted in the floor and the minimum height is still
    /// clear above its crest. Passable by climbing over; whether an *agent*
    /// can follow is a separate question ([`PassageVerdict::agent_passable`]).
    FloorRooted,
    /// Anything else that reaches an open band --- the only class that runs
    /// the flood fill, and the only one that can turn out to need a repair.
    Sealed,
}

/// Which repairs an [`Accommodation`] actually used. Reported rather than
/// inferred so a regression suite can assert on the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepairKinds {
    /// R1 --- the local ceiling was lifted over the intruder's crest.
    pub headroom_lift: bool,
    /// R2 --- a channel was cut along the passage, past the intruder.
    pub side_channel: bool,
    /// R3 --- the channel's floor was lowered below the passage floor.
    pub floor_dig: bool,
}

/// The outcome of analysing one intruder against one passage.
#[derive(Clone, Debug)]
pub struct PassageVerdict {
    /// How the intruder sits in the passage.
    pub class: ObstructionClass,
    /// Whether the passage was already impassable *before* the intruder
    /// arrived. When it was, nothing is repaired --- the intruder is not the
    /// cause.
    pub broken_before: bool,
    /// Whether the intruder actually disconnects the passage.
    pub obstructed: bool,
    /// The repair, if one was needed and one was found.
    pub repair: Option<Accommodation>,
    /// Set when the passage is obstructed, a repair was needed, and none
    /// could be found inside this tier's budget --- the terminal fallback,
    /// where the caller must keep the intruder out instead.
    pub reject: bool,
    /// Whether a route across the intruder exists that an *agent* can
    /// follow: every floor-to-floor step at most
    /// [`TraversalParams::max_navmesh_step`].
    ///
    /// A climb-over chokepoint is invisible to the pathfinder: it needs two
    /// free blocks over ground and steps at most `+2`, and it has no climb
    /// edge at any height for anything. So a crest more than two blocks
    /// above the passage floor is passable to a human player and a dead end
    /// to every NPC and creature alike --- `Body::can_climb` does not change
    /// that, since pathing consults it only to re-enable the `+2` step over
    /// liquid.
    ///
    /// The whole analysis is therefore run at *agent* capability: the flood
    /// fill's step limit is the navmesh's own, so "obstructed" means "no
    /// agent can get through" and a successful repair means "an agent can".
    /// The one case where this comes back `false` is an intruder rooted in
    /// the floor, with headroom above its crest, that no channel can be cut
    /// past --- the player climbs it and nothing else can. That is reported,
    /// never assumed away, and never answered by throwing the intruder out:
    /// a rock wedged in a passage is wanted, and whether AI can climb is a
    /// separate, unsolved problem this module does not pretend to fix.
    pub agent_passable: bool,
    /// Which repairs were used, for reporting.
    pub kinds: RepairKinds,
}

/// A repair, resolved into per-column carve intervals so the caller can
/// apply exactly this column's slice while it walks a chunk.
///
/// Stored as a dense grid over the intruder's dilated footprint rather than
/// a map: the footprint is bounded by the intruder's own bounding box, so
/// this is a few kilobytes for the rare intruder that needs one, and the
/// per-column lookup is an index rather than a hash.
#[derive(Clone, Debug)]
pub struct Accommodation {
    origin: Vec2<i32>,
    size: Vec2<i32>,
    /// Per column, the disjoint `z` intervals to clear, ascending.
    ///
    /// Several intervals rather than one span, because a single intruder can
    /// land in two passages at different depths and each is repaired on its
    /// own terms. Collapsing those into one span would clear the native rock
    /// between them, joining two passage families world-gen deliberately
    /// kept apart --- a change to the world's topology, not a repair.
    cells: Vec<Vec<(i32, i32)>>,
}

impl Accommodation {
    fn new(origin: Vec2<i32>, size: Vec2<i32>) -> Self {
        Self {
            origin,
            size,
            cells: vec![Vec::new(); (size.x * size.y) as usize],
        }
    }

    fn index(&self, wpos2d: Vec2<i32>) -> Option<usize> {
        let rel = wpos2d - self.origin;
        (rel.x >= 0 && rel.y >= 0 && rel.x < self.size.x && rel.y < self.size.y)
            .then(|| (rel.y * self.size.x + rel.x) as usize)
    }

    fn add(&mut self, wpos2d: Vec2<i32>, lo: i32, hi: i32) {
        if lo > hi {
            return;
        }
        let Some(i) = self.index(wpos2d) else { return };
        let cell = &mut self.cells[i];
        // Absorb every interval this one now touches, then insert the union
        // of them, so the column's intervals stay disjoint and ascending.
        let (mut lo, mut hi) = (lo, hi);
        cell.retain(|&(l, h)| {
            if l <= hi + 1 && h + 1 >= lo {
                lo = lo.min(l);
                hi = hi.max(h);
                false
            } else {
                true
            }
        });
        let at = cell.partition_point(|&(l, _)| l < lo);
        cell.insert(at, (lo, hi));
    }

    fn is_empty(&self) -> bool { self.cells.iter().all(Vec::is_empty) }

    /// Merge another accommodation for the same intruder into this one.
    ///
    /// An intruder can land in more than one passage at once --- a tunnel
    /// crossing a chamber, say --- and each is analysed on its own terms.
    pub fn union(&mut self, other: &Self) {
        for (wpos2d, (lo, hi)) in other.columns() {
            self.add(wpos2d, lo, hi);
        }
    }

    /// The inclusive `z` intervals to clear at this column.
    pub fn carve_at(&self, wpos2d: Vec2<i32>) -> &[(i32, i32)] {
        self.index(wpos2d)
            .map(|i| self.cells[i].as_slice())
            .unwrap_or(&[])
    }

    /// Every column this accommodation touches, once per interval. Used by
    /// the regression suite's additivity check.
    pub fn columns(&self) -> impl Iterator<Item = (Vec2<i32>, (i32, i32))> + '_ {
        self.cells.iter().enumerate().flat_map(move |(i, cell)| {
            let i = i as i32;
            let wpos2d = self.origin + Vec2::new(i % self.size.x, i / self.size.x);
            cell.iter().map(move |&iv| (wpos2d, iv))
        })
    }

    /// Total voxels this accommodation would clear, if every one of them
    /// were solid. Reporting only.
    pub fn voxel_budget(&self) -> i64 {
        self.columns()
            .map(|(_, (lo, hi))| (hi - lo + 1) as i64)
            .sum()
    }
}

// ---------------------------------------------------------------------
// The analysis itself.
// ---------------------------------------------------------------------

/// One sampled column of the analysis strip.
struct Cell {
    wpos2d: Vec2<i32>,
    ceiling_limit: i32,
    bands: Vec<(i32, i32)>,
    /// Maximal runs of non-solid voxels of at least `min_height`, with the
    /// intruder present. `(floor, top)`, inclusive.
    slots_after: Vec<(i32, i32)>,
    /// The same, with the intruder removed --- i.e. the bands themselves,
    /// filtered for height.
    slots_before: Vec<(i32, i32)>,
    /// The intruder's solid `z` extent within this column's bands, if it
    /// reaches them at all.
    intruder_z: Option<(i32, i32)>,
}

struct Strip {
    origin: Vec2<i32>,
    stride: i32,
    dims: Vec2<i32>,
    cells: Vec<Option<Cell>>,
}

impl Strip {
    fn at(&self, g: Vec2<i32>) -> Option<&Cell> {
        (g.x >= 0 && g.y >= 0 && g.x < self.dims.x && g.y < self.dims.y)
            .then(|| self.cells[(g.y * self.dims.x + g.x) as usize].as_ref())
            .flatten()
    }

    fn grid_of(&self, i: usize) -> Vec2<i32> {
        let i = i as i32;
        Vec2::new(i % self.dims.x, i / self.dims.x)
    }

    fn is_boundary(&self, g: Vec2<i32>) -> bool {
        g.x == 0 || g.y == 0 || g.x == self.dims.x - 1 || g.y == self.dims.y - 1
    }
}

/// Maximal runs of non-solid voxels inside `bands`, keeping only those at
/// least `min_height` tall.
///
/// A run's floor always has a solid voxel beneath it --- either the band's
/// own floor rock, or the intruder --- because the runs are maximal inside a
/// band whose outside is solid by definition. That is the "has a floor to
/// stand on" half of the slot definition, satisfied by construction.
fn slots_in(bands: &[(i32, i32)], solid: &[bool], base_z: i32, min_height: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for &(lo, hi) in bands {
        let mut run: Option<i32> = None;
        for z in lo..=hi {
            let is_solid = solid.get((z - base_z) as usize).copied().unwrap_or(false);
            match (is_solid, run) {
                (false, None) => run = Some(z),
                (true, Some(start)) => {
                    if z - start >= min_height {
                        out.push((start, z - 1));
                    }
                    run = None;
                },
                _ => {},
            }
        }
        if let Some(start) = run
            && hi - start + 1 >= min_height
        {
            out.push((start, hi));
        }
    }
    out
}

/// Whether two slots in neighbouring columns are mutually reachable,
/// reproducing what the engine's own A\* neighbour set will accept.
///
/// The step itself must be inside that set's limit (`DIRS` steps `+1`,
/// `JUMPS` steps `+2`), but the limit does not travel alone: a `+1` step
/// additionally requires `pos + 2` non-solid at the column being left, and a
/// `+2` step requires `pos + 2` *and* `pos + 3` --- i.e. climbing `d` blocks
/// needs `d + 2` blocks of clear air at the source. Dropping that half would
/// certify a repaired channel with a two-block step and three blocks of
/// headroom as walkable when the real pathfinder cannot follow it.
///
/// Each slot already carries the minimum height by construction, so the
/// destination clearance `walkable` asks for is guaranteed and is not
/// re-tested here.
///
/// **Descents are modelled as symmetric, and the engine's are not.** Real
/// pathing also has a fall edge that drops far further than two blocks, so a
/// route this rejects may in fact be walkable downhill. The bias is
/// deliberate and one-directional: it can only make the analysis repair
/// something that did not need it, never leave a real seal in place.
fn slots_connect(a: (i32, i32), b: (i32, i32), params: &TraversalParams) -> bool {
    let step = (a.0 - b.0).abs();
    if step > params.max_navmesh_step {
        return false;
    }
    let lower = if a.0 <= b.0 { a } else { b };
    lower.1 - lower.0 + 1 >= step + params.min_height
}

struct DisjointSet(Vec<usize>);

impl DisjointSet {
    fn new(n: usize) -> Self { Self((0..n).collect()) }

    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

/// Component labels for every traversal slot in a strip. Two slots share a
/// label when a connected slot path runs between them.
///
/// Labels are per *slot*, never per column: two slots in the same column at
/// different depths --- two passages crossing --- are not mutually reachable
/// and must not be merged.
struct SlotLabels {
    /// Slot ids are `(cell index, slot index)` flattened; this is the offset
    /// of each cell's first slot.
    base: Vec<usize>,
    root: Vec<usize>,
}

impl SlotLabels {
    fn label(&self, cell: usize, slot: usize) -> usize { self.root[self.base[cell] + slot] }
}

/// Flood fill over traversal slots.
fn components(
    strip: &Strip,
    pick: impl Fn(&Cell) -> &Vec<(i32, i32)>,
    params: &TraversalParams,
) -> SlotLabels {
    let mut base = vec![0usize; strip.cells.len()];
    let mut total = 0usize;
    for (i, cell) in strip.cells.iter().enumerate() {
        base[i] = total;
        if let Some(cell) = cell {
            total += pick(cell).len();
        }
    }
    let mut ds = DisjointSet::new(total);

    for (i, cell) in strip.cells.iter().enumerate() {
        let Some(cell) = cell else { continue };
        let g = strip.grid_of(i);
        for dir in [Vec2::new(1, 0), Vec2::new(0, 1)] {
            let ng = g + dir;
            let Some(ncell) = strip.at(ng) else { continue };
            let j = (ng.y * strip.dims.x + ng.x) as usize;
            for (si, &s) in pick(cell).iter().enumerate() {
                for (ti, &t) in pick(ncell).iter().enumerate() {
                    if slots_connect(s, t, params) {
                        ds.union(base[i] + si, base[j] + ti);
                    }
                }
            }
        }
    }

    // Every union first, then every label: reading a root while unions are
    // still being made would hand the cells visited early a root that a
    // later union invalidates.
    let root = (0..total).map(|id| ds.find(id)).collect();
    SlotLabels { base, root }
}

/// The passage's routes across this strip, and what the intruder did to
/// them.
///
/// A *port* is a boundary cell of the strip that has a traversal slot, i.e.
/// a place where the passage leaves the analysed neighbourhood. Ports that
/// were mutually reachable before the intruder arrived form one group. If
/// that group is still one connected piece afterwards, nothing was broken.
/// If it fell into several pieces, each piece is represented once here, and
/// reconnecting the representatives is exactly what the repair has to do.
///
/// Whole cells rather than individual slots, so the answer stays stable when
/// the intruder splits one slot into several; and one representative per
/// piece rather than every pair of ports, so the repair runs a handful of
/// searches rather than thousands.
fn port_groups(strip: &Strip, before: &SlotLabels, after: &SlotLabels) -> Vec<Vec<(usize, i32)>> {
    // `Vec` keyed by label rather than a map, so the iteration order is the
    // strip's own index order and the result is deterministic --- a repair
    // derived from a hash iteration order would differ between the chunks a
    // single intruder spans.
    let mut labels: Vec<usize> = Vec::new();
    let mut groups: Vec<Vec<(Option<usize>, usize, i32)>> = Vec::new();
    for i in 0..strip.cells.len() {
        let Some(cell) = strip.cells[i].as_ref() else {
            continue;
        };
        if !strip.is_boundary(strip.grid_of(i)) {
            continue;
        }
        for (s, &(floor, _)) in cell.slots_before.iter().enumerate() {
            let before_label = before.label(i, s);
            // The boundary ring sits outside the intruder's own bounding
            // box, so a boundary cell's slots are normally untouched and
            // match one-for-one. Match on the floor rather than the index so
            // that the rare exception degrades into "this port was lost"
            // rather than into a mislabelled pairing.
            let after_label = cell
                .slots_after
                .iter()
                .position(|&(f, _)| f == floor)
                .map(|t| after.label(i, t));
            let slot = match labels.iter().position(|l| *l == before_label) {
                Some(slot) => slot,
                None => {
                    labels.push(before_label);
                    groups.push(Vec::new());
                    groups.len() - 1
                },
            };
            if !groups[slot].iter().any(|(l, _, _)| *l == after_label) {
                groups[slot].push((after_label, i, floor));
            }
        }
    }
    groups
        .into_iter()
        .map(|g| g.into_iter().map(|(_, i, f)| (i, f)).collect())
        .collect()
}

/// Every column [`analyse`] will sample for an intruder of these bounds: its
/// own 2D footprint, dilated so the analysis sees clear passage on both sides
/// of it rather than starting flush against the obstruction.
///
/// Exposed, and read by `analyse` itself rather than duplicated, because a
/// caller deciding *which passages are worth building* for this intruder has
/// to cover exactly the columns the analysis will ask about. Two copies of the
/// padding rule would make that guarantee depend on nobody editing one of
/// them; one copy makes it structural.
pub fn analysis_footprint(bounds: Aabb<i32>, params: &TraversalParams) -> Aabr<i32> {
    let pad = 2 * params.min_width;
    Aabr {
        min: bounds.min.xy() - pad,
        max: bounds.max.xy() + pad,
    }
}

/// Analyse one intruder against one passage, and produce the repair it needs
/// --- or the verdict that it needs none, or that none fits.
///
/// `stride` samples the strip every `stride` blocks in x and y; the repair is
/// dilated to compensate, so a coarser stride produces a slightly wider
/// channel rather than a hole. Intruders are blobby and a one-block sampling
/// error is irrelevant to a three-block corridor.
pub fn analyse(
    solid: &dyn SolidVolume,
    passage: &dyn PassageQuery,
    params: &TraversalParams,
    stride: i32,
) -> PassageVerdict {
    let stride = stride.max(1);
    let bounds = solid.bounds();
    let tier = passage.tier();

    // The strip: the intruder's footprint, dilated so the analysis sees
    // clear passage on both sides of it rather than starting flush against
    // the obstruction.
    let strip = analysis_footprint(bounds, params);
    let (min, max) = (strip.min, strip.max);
    let dims = ((max - min) / stride + 1).map(|e| e.max(1));

    let mut solid_col = Vec::new();
    let mut cells: Vec<Option<Cell>> = Vec::with_capacity((dims.x * dims.y) as usize);
    let mut any_intruder = false;
    let mut all_pendant = true;
    let mut all_floor_rooted = true;

    for gy in 0..dims.y {
        for gx in 0..dims.x {
            let wpos2d = min + Vec2::new(gx, gy) * stride;
            let Some(column) = passage.column(wpos2d) else {
                cells.push(None);
                continue;
            };
            let mut bands: Vec<(i32, i32)> = column
                .bands
                .iter()
                .copied()
                .filter(|(lo, hi)| hi >= lo)
                .collect();
            bands.sort_unstable();
            if bands.is_empty() {
                cells.push(None);
                continue;
            }
            let z_lo = bands.first().map(|b| b.0).unwrap_or(0);
            let z_hi = bands.last().map(|b| b.1).unwrap_or(0);

            solid_col.clear();
            solid.column_solid(wpos2d, z_lo, z_hi, &mut solid_col);

            let mut intruder_z: Option<(i32, i32)> = None;
            for (idx, &is_solid) in solid_col.iter().enumerate() {
                if is_solid {
                    let z = z_lo + idx as i32;
                    // Only count solidity that actually sits inside a band:
                    // between two bands is native rock, not the intruder's
                    // doing.
                    if bands.iter().any(|&(lo, hi)| z >= lo && z <= hi) {
                        intruder_z = Some(match intruder_z {
                            Some((lo, hi)) => (lo.min(z), hi.max(z)),
                            None => (z, z),
                        });
                    }
                }
            }

            if let Some((r_lo, r_hi)) = intruder_z {
                any_intruder = true;
                let band = bands
                    .iter()
                    .copied()
                    .find(|&(lo, hi)| r_lo <= hi && r_hi >= lo)
                    .unwrap_or((z_lo, z_hi));
                if !(r_lo > band.0 && r_lo - band.0 >= params.min_height) {
                    all_pendant = false;
                }
                if !(r_lo <= band.0 && band.1 - r_hi >= params.min_height) {
                    all_floor_rooted = false;
                }
            }

            let slots_before = slots_in(&bands, &[], z_lo, params.min_height);
            let slots_after = slots_in(&bands, &solid_col, z_lo, params.min_height);

            cells.push(Some(Cell {
                wpos2d,
                ceiling_limit: column.ceiling_limit,
                bands,
                slots_after,
                slots_before,
                intruder_z,
            }));
        }
    }

    let strip = Strip {
        origin: min,
        stride,
        dims,
        cells,
    };

    let clear = PassageVerdict {
        class: ObstructionClass::Clear,
        broken_before: false,
        obstructed: false,
        repair: None,
        reject: false,
        agent_passable: true,
        kinds: RepairKinds::default(),
    };

    if !any_intruder {
        return clear;
    }

    // Hand-authored architecture is never edited by a hash: the intruder
    // simply does not belong there.
    if !tier.allows_repair() {
        return PassageVerdict {
            class: ObstructionClass::Sealed,
            reject: true,
            obstructed: true,
            agent_passable: false,
            ..clear
        };
    }

    // C0 --- the wanted case. The intruder hangs from the ceiling with the
    // full minimum height clear beneath it everywhere it reaches. Scalar
    // test, no fill.
    if all_pendant {
        return PassageVerdict {
            class: ObstructionClass::CeilingPendant,
            ..clear
        };
    }

    let class = if all_floor_rooted {
        ObstructionClass::FloorRooted
    } else {
        ObstructionClass::Sealed
    };

    // The pre-condition. If the passage was already impassable without the
    // intruder, the intruder is not the cause and nothing is repaired.
    let before = components(&strip, |c| &c.slots_before, params);
    let after = components(&strip, |c| &c.slots_after, params);
    let groups = port_groups(&strip, &before, &after);

    if groups.is_empty() {
        // Either the passage was already impassable across this strip, or it
        // never left the analysed neighbourhood. Either way there is no
        // through-route the intruder can be blamed for breaking.
        return PassageVerdict {
            class,
            broken_before: true,
            ..clear
        };
    }

    let obstructed = groups.iter().any(|g| g.len() > 1);

    if !obstructed {
        // Nothing is sealed, and because the fill runs at agent capability
        // that also means an agent still gets through. The one thing still
        // worth doing is guaranteeing headroom over a crest the player may
        // choose to climb, which is additive and cheap.
        let mut kinds = RepairKinds::default();
        let repair = (class == ObstructionClass::FloorRooted)
            .then(|| headroom_lift(&strip, &bounds, params, &mut kinds))
            .flatten();
        return PassageVerdict {
            class,
            broken_before: false,
            obstructed: false,
            repair,
            reject: false,
            agent_passable: true,
            kinds,
        };
    }

    let mut kinds = RepairKinds::default();
    let mut repair = accommodation_for(&bounds, params);

    if let Some(lift) = headroom_lift(&strip, &bounds, params, &mut kinds) {
        for (wpos2d, (lo, hi)) in lift.columns() {
            repair.add(wpos2d, lo, hi);
        }
    }

    // Open a channel that restores every connection the intruder broke.
    // Digging below a passage floor is available to the purely procedural
    // tier only; every other tier has content sitting on that floor.
    let max_dig = if tier.allows_floor_dig() {
        params.max_floor_dig
    } else {
        0
    };
    // Built once: it depends only on the strip, and the handful of port
    // pairs below all search the same graph.
    let nodes = channel_nodes(&strip, &bounds, params, max_dig);
    let mut restored = true;
    for group in &groups {
        for &other in &group[1..] {
            match channel_between(&strip, &bounds, &nodes, group[0], other, params, &mut kinds) {
                Some(channel) => {
                    for (wpos2d, interval) in channel.columns() {
                        repair.add(wpos2d, interval.0, interval.1);
                    }
                },
                None => restored = false,
            }
        }
    }

    // Verify: re-run the fill with the repair applied. This is what makes
    // "every repaired passage is passable" true by construction rather than
    // by argument.
    let restored =
        restored && !repair.is_empty() && verify(&strip, solid, &repair, params, &before);

    if restored {
        return PassageVerdict {
            class,
            broken_before: false,
            obstructed: true,
            repair: Some(repair),
            reject: false,
            agent_passable: true,
            kinds,
        };
    }

    // No walkable route could be opened inside this tier's budget.
    //
    // If the intruder is rooted in the floor with headroom above its crest,
    // a player can still climb over it, and throwing the intruder out would
    // be exactly the answer that was ruled out --- a rock wedged in a
    // passage is wanted. Keep it, keep the headroom, and report honestly
    // that the route is player-only. Anything else (a true seal, floor to
    // ceiling) has no route at all and the intruder must go.
    if class == ObstructionClass::FloorRooted {
        let mut kinds = RepairKinds::default();
        return PassageVerdict {
            class,
            broken_before: false,
            obstructed: true,
            repair: headroom_lift(&strip, &bounds, params, &mut kinds),
            reject: false,
            agent_passable: false,
            kinds,
        };
    }

    PassageVerdict {
        class,
        broken_before: false,
        obstructed: true,
        repair: None,
        reject: true,
        agent_passable: false,
        kinds,
    }
}

/// An empty accommodation sized to the intruder's footprint dilated by
/// [`TraversalParams::target_width`] --- the hard bound on where any repair
/// may write.
fn accommodation_for(bounds: &Aabb<i32>, params: &TraversalParams) -> Accommodation {
    Accommodation::new(
        bounds.min.xy() - params.target_width,
        (bounds.max.xy() - bounds.min.xy()) + 2 * params.target_width + 1,
    )
}

/// R1 --- raise the local ceiling over the intruder's crest to the target
/// height, never above the passage's own ceiling limit.
fn headroom_lift(
    strip: &Strip,
    bounds: &Aabb<i32>,
    params: &TraversalParams,
    kinds: &mut RepairKinds,
) -> Option<Accommodation> {
    let mut acc = accommodation_for(bounds, params);
    let mut any = false;
    for cell in strip.cells.iter().flatten() {
        let Some((r_lo, r_hi)) = cell.intruder_z else {
            continue;
        };
        let Some(band) = cell
            .bands
            .iter()
            .copied()
            .find(|&(lo, hi)| r_lo <= hi && r_hi >= lo)
        else {
            continue;
        };
        if r_lo > band.0 {
            // Not rooted in this band's floor; there is nothing to climb on
            // to and the ceiling above it is not what is in the way.
            continue;
        }
        let want_top = (r_hi + params.target_height)
            .min(cell.ceiling_limit)
            .min(bounds.max.z + params.target_width);
        if want_top > band.1 {
            for dx in 0..strip.stride {
                for dy in 0..strip.stride {
                    acc.add(cell.wpos2d + Vec2::new(dx, dy), band.1 + 1, want_top);
                }
            }
            any = true;
        }
    }
    if any {
        kinds.headroom_lift = true;
        Some(acc)
    } else {
        None
    }
}

/// Every candidate the channel search may stand a corridor on, flattened.
///
/// A node is "a channel floored at this `z`, in this column". The candidates
/// are a column's own passage floors, the dig allowance beneath each of them
/// where the tier permits digging, and --- the one that matters for the
/// commonest case --- the top of the intruder itself, so the search can ramp
/// up and over a rock rooted in the floor instead of only around it.
///
/// Built once per intruder rather than once per search: it depends only on
/// the strip, which does not change between the handful of port pairs a
/// repair has to reconnect.
struct ChannelNodes {
    /// Prefix-sum offsets into [`Self::floor`], one per cell plus a tail.
    offset: Vec<u32>,
    /// The owning cell of each node, so a node id resolves without a search.
    cell: Vec<u32>,
    floor: Vec<i32>,
    /// `(cost, top)` for each node, or `None` where no corridor fits.
    ///
    /// Precomputed because the search reads it on every edge relaxation and
    /// again on every step of the walk-back, and it depends on nothing the
    /// search changes.
    fit: Vec<Option<(u32, i32)>>,
}

impl ChannelNodes {
    fn of(&self, cell: usize) -> std::ops::Range<usize> {
        self.offset[cell] as usize..self.offset[cell + 1] as usize
    }
}

/// Weigh every candidate corridor position in the strip.
///
/// The bound on where a repair may write applies to the *carving*, not to
/// the route: a column the channel merely walks through, because the passage
/// is already open at that height, costs nothing and is allowed anywhere in
/// the strip.
fn channel_nodes(
    strip: &Strip,
    bounds: &Aabb<i32>,
    params: &TraversalParams,
    max_dig: i32,
) -> ChannelNodes {
    let carve_min = bounds.min.xy() - params.target_width;
    let carve_max = bounds.max.xy() + params.target_width;

    let mut nodes = ChannelNodes {
        offset: Vec::with_capacity(strip.cells.len() + 1),
        cell: Vec::new(),
        floor: Vec::new(),
        fit: Vec::new(),
    };
    let mut candidates: Vec<i32> = Vec::new();

    for (i, cell) in strip.cells.iter().enumerate() {
        nodes.offset.push(nodes.floor.len() as u32);
        let Some(cell) = cell else { continue };

        candidates.clear();
        for &(lo, _) in &cell.bands {
            for d in 0..=max_dig {
                candidates.push(lo - d);
            }
        }
        // Standing on the intruder. Without this the search can only route
        // *around* a rock rooted in the passage floor, so the commonest case
        // of all -- a boulder sitting on the floor of a tunnel barely wider
        // than itself -- would have no walkable answer at all.
        if let Some((_, r_hi)) = cell.intruder_z {
            candidates.push(r_hi + 1);
        }
        candidates.sort_unstable();
        candidates.dedup();

        for &floor in &candidates {
            // The band this corridor would sit in: the nearest one, since a
            // corridor belongs to the passage it is closest to.
            let Some(band) = cell
                .bands
                .iter()
                .copied()
                .min_by_key(|&(lo, _)| (lo - floor).abs())
            else {
                continue;
            };
            let head = (cell.ceiling_limit - floor + 1).min(params.target_height);
            if head < params.min_height {
                continue;
            }
            let top = floor + head - 1;

            // Everything in `floor ..= top` that is not already open: the
            // band's own floor rock below it, its ceiling rock above it, and
            // whatever the intruder occupies inside it.
            let dig = (band.0 - floor).max(0);
            let lift = (top - band.1).max(0);
            let blocked = cell
                .intruder_z
                .map(|(r_lo, r_hi)| (r_hi.min(top) - r_lo.max(floor) + 1).max(0))
                .unwrap_or(0);
            let carve = dig + lift + blocked;
            let in_carve_region = cell.wpos2d.x >= carve_min.x
                && cell.wpos2d.y >= carve_min.y
                && cell.wpos2d.x <= carve_max.x
                && cell.wpos2d.y <= carve_max.y;
            let fits = carve == 0
                || (in_carve_region
                    && floor >= bounds.min.z - params.target_width
                    && top <= bounds.max.z + params.target_width);

            nodes.cell.push(i as u32);
            nodes.floor.push(floor);
            nodes.fit.push(fits.then_some((
                // Digging is charged four times over: it is the operation of
                // last resort, and where a tier forbids it the floors that
                // would need it are not offered at all.
                (blocked + lift + dig * 4) as u32 + 1,
                // The realised top, so the walker can record the interval.
                top,
            )));
        }
    }
    nodes.offset.push(nodes.floor.len() as u32);
    nodes
}

/// Upper bound on nodes the channel search may settle before it gives up.
///
/// The search is over a strip bounded by the intruder's own size, so it
/// terminates regardless; this bounds the *latency* of the pathological
/// case rather than its correctness, and giving up simply falls through to
/// the repair machinery's existing "no channel fits" answer.
const MAX_CHANNEL_SETTLES: usize = 200_000;

/// The cheapest corridor reconnecting two ports the intruder disconnected.
///
/// A least-cost search over the strip is what "hug whichever wall has the
/// most free width" means in code, without needing a passage axis to hug a
/// wall *of*: an already-open column costs nothing, so the cheapest route is
/// exactly the one that uses the most existing free space --- and it
/// degrades gracefully inside a chamber, where no axis is defined at all.
fn channel_between(
    strip: &Strip,
    bounds: &Aabb<i32>,
    nodes: &ChannelNodes,
    from: (usize, i32),
    to: (usize, i32),
    params: &TraversalParams,
    kinds: &mut RepairKinds,
) -> Option<Accommodation> {
    let (from, from_floor) = from;
    let (to, to_floor) = to;

    let mut best = vec![u32::MAX; nodes.floor.len()];
    let mut prev = vec![u32::MAX; nodes.floor.len()];
    let mut heap: BinaryHeap<(std::cmp::Reverse<u32>, u32)> = BinaryHeap::new();

    for id in nodes.of(from) {
        if nodes.floor[id] == from_floor && nodes.fit[id].is_some() {
            best[id] = 0;
            heap.push((std::cmp::Reverse(0), id as u32));
        }
    }

    let mut goal = None;
    let mut settled = 0usize;
    while let Some((std::cmp::Reverse(d), id)) = heap.pop() {
        let id = id as usize;
        if best[id] < d {
            continue;
        }
        settled += 1;
        if settled > MAX_CHANNEL_SETTLES {
            return None;
        }
        if nodes.cell[id] as usize == to && nodes.floor[id] == to_floor {
            goal = Some(id);
            break;
        }
        let f = nodes.floor[id];
        let Some((_, top)) = nodes.fit[id] else {
            continue;
        };
        let g = strip.grid_of(nodes.cell[id] as usize);
        for dir in [
            Vec2::new(1, 0),
            Vec2::new(-1, 0),
            Vec2::new(0, 1),
            Vec2::new(0, -1),
        ] {
            let ng = g + dir;
            if ng.x < 0 || ng.y < 0 || ng.x >= strip.dims.x || ng.y >= strip.dims.y {
                continue;
            }
            let j = (ng.y * strip.dims.x + ng.x) as usize;
            for nid in nodes.of(j) {
                let Some((c, ntop)) = nodes.fit[nid] else {
                    continue;
                };
                // The same reachability rule the verification uses, so a
                // channel cannot be routed through a step the finished
                // geometry would not support.
                if !slots_connect((f, top), (nodes.floor[nid], ntop), params) {
                    continue;
                }
                let nd = d.saturating_add(c);
                if nd < best[nid] {
                    best[nid] = nd;
                    prev[nid] = id as u32;
                    heap.push((std::cmp::Reverse(nd), nid as u32));
                }
            }
        }
    }

    let carve_min = bounds.min.xy() - params.target_width;
    let carve_max = bounds.max.xy() + params.target_width;
    let mut id = goal?;
    let mut acc = accommodation_for(bounds, params);
    let mut dug = false;
    loop {
        let cell = strip.cells[nodes.cell[id] as usize].as_ref()?;
        let f = nodes.floor[id];
        let (_, top) = nodes.fit[id]?;
        if cell.bands.iter().all(|&(lo, _)| f < lo) {
            dug = true;
        }
        // Widen: the sampled cell stands for `stride` blocks in each axis,
        // and the channel is dilated by one block on each side so a coarse
        // stride produces a slightly wider corridor rather than a gap.
        for dx in -1..(strip.stride + 1) {
            for dy in -1..(strip.stride + 1) {
                let wpos2d = cell.wpos2d + Vec2::new(dx, dy);
                if wpos2d.x >= carve_min.x
                    && wpos2d.y >= carve_min.y
                    && wpos2d.x <= carve_max.x
                    && wpos2d.y <= carve_max.y
                {
                    acc.add(wpos2d, f, top);
                }
            }
        }
        if prev[id] == u32::MAX {
            break;
        }
        id = prev[id] as usize;
    }

    kinds.side_channel = true;
    kinds.floor_dig |= dug;
    Some(acc)
}

/// Re-run the connectivity test with the repair applied --- the step that
/// makes "every repaired passage is passable" a checked property rather than
/// an argument.
fn verify(
    strip: &Strip,
    solid: &dyn SolidVolume,
    repair: &Accommodation,
    params: &TraversalParams,
    before: &SlotLabels,
) -> bool {
    let mut cells: Vec<Option<Cell>> = Vec::with_capacity(strip.cells.len());
    let mut solid_col = Vec::new();
    for cell in &strip.cells {
        let Some(cell) = cell else {
            cells.push(None);
            continue;
        };
        // The repaired bands: the original ones plus whatever the repair
        // opened at this column, with overlaps merged so the slot scan sees
        // one continuous run where a repair joined two.
        let carved = repair.carve_at(cell.wpos2d);
        let mut bands = cell.bands.clone();
        if !carved.is_empty() {
            bands.extend_from_slice(carved);
            bands.sort_unstable();
            let mut merged: Vec<(i32, i32)> = Vec::with_capacity(bands.len());
            for (lo, hi) in bands {
                match merged.last_mut() {
                    Some(last) if lo <= last.1 + 1 => last.1 = last.1.max(hi),
                    _ => merged.push((lo, hi)),
                }
            }
            bands = merged;
        }
        let z_lo = bands.first().map(|b| b.0).unwrap_or(0);
        let z_hi = bands.last().map(|b| b.1).unwrap_or(0);
        solid_col.clear();
        solid.column_solid(cell.wpos2d, z_lo, z_hi, &mut solid_col);
        // Everything the repair cleared is air regardless of the intruder.
        for &(lo, hi) in carved {
            for z in lo.max(z_lo)..=hi.min(z_hi) {
                if let Some(s) = solid_col.get_mut((z - z_lo) as usize) {
                    *s = false;
                }
            }
        }
        let slots_after = slots_in(&bands, &solid_col, z_lo, params.min_height);
        cells.push(Some(Cell {
            wpos2d: cell.wpos2d,
            ceiling_limit: cell.ceiling_limit,
            bands,
            slots_before: cell.slots_before.clone(),
            slots_after,
            intruder_z: cell.intruder_z,
        }));
    }
    let repaired = Strip {
        origin: strip.origin,
        stride: strip.stride,
        dims: strip.dims,
        cells,
    };
    let after = components(&repaired, |c| &c.slots_after, params);
    port_groups(&repaired, before, &after)
        .iter()
        .all(|g| g.len() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight tunnel running along +x with a constant open band.
    struct FlatTunnel {
        floor: i32,
        ceiling: i32,
        half_width: i32,
        ceiling_limit: i32,
        tier: AccommodationTier,
    }

    impl PassageQuery for FlatTunnel {
        fn tier(&self) -> AccommodationTier { self.tier }

        fn column(&self, wpos2d: Vec2<i32>) -> Option<PassageColumn> {
            (wpos2d.y.abs() <= self.half_width).then(|| PassageColumn {
                bands: vec![(self.floor, self.ceiling)],
                ceiling_limit: self.ceiling_limit,
            })
        }
    }

    /// A rectangular block of stone, the simplest possible intruder.
    struct Box3 {
        aabb: Aabb<i32>,
    }

    impl SolidVolume for Box3 {
        fn bounds(&self) -> Aabb<i32> { self.aabb }

        fn column_solid(&self, wpos2d: Vec2<i32>, z_lo: i32, z_hi: i32, out: &mut Vec<bool>) {
            let inside2d = wpos2d.x >= self.aabb.min.x
                && wpos2d.x <= self.aabb.max.x
                && wpos2d.y >= self.aabb.min.y
                && wpos2d.y <= self.aabb.max.y;
            for z in z_lo..=z_hi {
                out.push(inside2d && z >= self.aabb.min.z && z <= self.aabb.max.z);
            }
        }
    }

    fn params() -> TraversalParams { TraversalParams::ENGINE }

    /// A tunnel with nothing in it is `Clear` and earns no repair.
    #[test]
    fn clear_tunnel_needs_nothing() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        // A rock sitting well above the band.
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, 30),
                max: Vec3::new(3, 3, 36),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert_eq!(v.class, ObstructionClass::Clear);
        assert!(v.repair.is_none());
        assert!(!v.reject);
    }

    /// A rock hanging from the ceiling with clear air beneath it is the
    /// wanted case: recognised without a fill, and left exactly as it is.
    #[test]
    fn ceiling_pendant_is_left_alone() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, 6),
                max: Vec3::new(3, 3, 12),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert_eq!(v.class, ObstructionClass::CeilingPendant);
        assert!(v.repair.is_none());
        assert!(!v.obstructed);
    }

    /// A rock rooted in the floor with headroom above it is passable by
    /// climbing over. Where that headroom is only the bare minimum, it is
    /// lifted to the target --- never answered with a rejection.
    #[test]
    fn floor_rooted_with_headroom_gets_a_lift_not_a_rejection() {
        let tunnel = FlatTunnel {
            floor: 0,
            // The crest tops out at z = 5, leaving exactly `min_height`
            // clear above it, so the lift has something to do.
            ceiling: 7,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, -4),
                max: Vec3::new(3, 3, 5),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert_eq!(v.class, ObstructionClass::FloorRooted);
        assert!(!v.reject);
        let repair = v.repair.expect("a floor-rooted crest earns headroom");
        // The lift must open air above the old ceiling, never below it.
        for (_, (lo, hi)) in repair.columns() {
            assert!(
                lo > tunnel.ceiling,
                "lift must start above the band ceiling"
            );
            assert!(hi <= tunnel.ceiling_limit, "lift must respect the clamp");
        }
    }

    /// A crest higher than a navmesh step, where the passage's own surface
    /// clamp leaves no room to cut a walkable channel past it, is a
    /// player-only route. The rock must be kept --- it is wanted --- and the
    /// verdict must say honestly that no agent can follow, rather than
    /// silently claiming it can or answering the problem by throwing the
    /// rock out.
    #[test]
    fn an_unroutable_tall_crest_is_kept_and_reported_player_only() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 20,
            half_width: 3,
            // A shallow passage: the clamp that keeps a repair from reaching
            // daylight sits at the passage floor itself, so no channel of
            // any height fits.
            ceiling_limit: 0,
            tier: AccommodationTier::Procedural,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, -4),
                max: Vec3::new(3, 3, 8),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert_eq!(v.class, ObstructionClass::FloorRooted);
        assert!(v.obstructed);
        assert!(!v.reject, "a climbable rock is never thrown out");
        assert!(
            !v.agent_passable,
            "a 9-block crest cannot be followed by an agent that steps 2"
        );
    }

    /// The terminal fallback. A rock that seals a passage no repair can get
    /// through --- here because the passage's surface clamp leaves no
    /// headroom for a channel --- must be kept out rather than left to
    /// produce an impassable tunnel.
    #[test]
    fn a_seal_no_repair_can_reach_keeps_the_rock_out() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 5,
            half_width: 3,
            ceiling_limit: 0,
            tier: AccommodationTier::Procedural,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, 0),
                max: Vec3::new(3, 3, 5),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert_eq!(v.class, ObstructionClass::Sealed);
        assert!(v.obstructed);
        assert!(v.reject);
        assert!(v.repair.is_none());
    }

    /// A rock that seals the middle of a passage but leaves a usable slot
    /// along one wall is passable, and must NOT be repaired --- this is the
    /// case no scalar "percent blocked" metric can get right.
    #[test]
    fn a_wall_hugging_gap_is_passable_and_untouched() {
        struct SideGap {
            aabb: Aabb<i32>,
        }
        impl SolidVolume for SideGap {
            fn bounds(&self) -> Aabb<i32> { self.aabb }

            fn column_solid(&self, wpos2d: Vec2<i32>, z_lo: i32, z_hi: i32, out: &mut Vec<bool>) {
                // Fills the whole cross-section except the y = +4/+5 wall.
                let inside2d = wpos2d.x >= self.aabb.min.x
                    && wpos2d.x <= self.aabb.max.x
                    && wpos2d.y >= self.aabb.min.y
                    && wpos2d.y <= self.aabb.max.y;
                for z in z_lo..=z_hi {
                    out.push(inside2d && z >= self.aabb.min.z && z <= self.aabb.max.z);
                }
            }
        }
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        let rock = SideGap {
            aabb: Aabb {
                min: Vec3::new(-3, -5, -4),
                max: Vec3::new(3, 3, 14),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert!(!v.obstructed, "the y = 4..5 wall slot is still open");
        assert!(!v.reject);
        assert!(v.repair.is_none(), "nothing was broken, so nothing is cut");
    }

    /// A rock that genuinely seals the passage is repaired, and the repair
    /// only ever opens air.
    #[test]
    fn a_true_seal_is_repaired() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -8, -4),
                max: Vec3::new(3, 8, 14),
            },
        };
        let v = analyse(&rock, &tunnel, &params(), 1);
        assert!(v.obstructed);
        assert!(!v.reject, "a wide tunnel has room for a channel");
        let repair = v.repair.expect("a sealed passage earns a channel");
        assert!(repair.voxel_budget() > 0);
        // Additivity: every carved column lies inside the dilated footprint.
        let b = rock.bounds();
        for (wpos2d, _) in repair.columns() {
            assert!(wpos2d.x >= b.min.x - 3 && wpos2d.x <= b.max.x + 3);
            assert!(wpos2d.y >= b.min.y - 3 && wpos2d.y <= b.max.y + 3);
        }
    }

    /// A passage that was already impassable before the intruder arrived is
    /// not the intruder's fault, and must produce no carve at all.
    #[test]
    fn an_already_impassable_passage_is_never_widened() {
        struct TooShort;
        impl PassageQuery for TooShort {
            fn tier(&self) -> AccommodationTier { AccommodationTier::Procedural }

            fn column(&self, wpos2d: Vec2<i32>) -> Option<PassageColumn> {
                // A one-block-tall passage: below `min_height`, so it has no
                // traversal slot anywhere, with or without the intruder.
                (wpos2d.y.abs() <= 5).then(|| PassageColumn {
                    bands: vec![(0, 0)],
                    ceiling_limit: 40,
                })
            }
        }
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -8, -4),
                max: Vec3::new(3, 8, 4),
            },
        };
        let v = analyse(&rock, &TooShort, &params(), 1);
        assert!(v.repair.is_none(), "nothing was passable to break");
        assert!(!v.reject);
    }

    /// Hand-authored architecture is never edited: the intruder is rejected
    /// outright, with no channel cut through a room wall.
    #[test]
    fn hand_authored_geometry_rejects_rather_than_repairs() {
        let room = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 5,
            ceiling_limit: 40,
            tier: AccommodationTier::HandAuthored,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -3, -4),
                max: Vec3::new(3, 3, 5),
            },
        };
        let v = analyse(&rock, &room, &params(), 1);
        assert!(v.reject);
        assert!(v.repair.is_none());
    }

    /// A catalog passage may be repaired, but never by digging its floor ---
    /// content is placed on that floor at carve time.
    #[test]
    fn catalog_geometry_never_digs_the_floor() {
        let cave = FlatTunnel {
            floor: 0,
            // A low chamber, so a headroom lift alone cannot clear the rock
            // and the search is pushed toward the floor.
            ceiling: 4,
            half_width: 6,
            ceiling_limit: 5,
            tier: AccommodationTier::Catalog,
        };
        let rock = Box3 {
            aabb: Aabb {
                min: Vec3::new(-3, -9, -4),
                max: Vec3::new(3, 9, 3),
            },
        };
        let v = analyse(&rock, &cave, &params(), 1);
        assert!(!v.kinds.floor_dig, "R3 is forbidden in catalog geometry");
        if let Some(repair) = v.repair {
            for (_, (lo, _)) in repair.columns() {
                assert!(lo >= cave.floor, "no carve below the authored floor");
            }
        }
    }

    /// Sampling the strip every other column must reach the same verdict as
    /// sampling every column.
    #[test]
    fn stride_two_agrees_with_stride_one() {
        let tunnel = FlatTunnel {
            floor: 0,
            ceiling: 9,
            half_width: 6,
            ceiling_limit: 40,
            tier: AccommodationTier::Procedural,
        };
        for (min, max) in [
            (Vec3::new(-3, -3, 6), Vec3::new(3, 3, 12)),
            (Vec3::new(-3, -3, -4), Vec3::new(3, 3, 5)),
            (Vec3::new(-4, -9, -4), Vec3::new(4, 9, 14)),
            (Vec3::new(-4, -5, -4), Vec3::new(4, 3, 14)),
        ] {
            let rock = Box3 {
                aabb: Aabb { min, max },
            };
            let one = analyse(&rock, &tunnel, &params(), 1);
            let two = analyse(&rock, &tunnel, &params(), 2);
            assert_eq!(
                (one.class, one.obstructed, one.reject),
                (two.class, two.obstructed, two.reject),
                "stride-2 disagreed for a rock at {min:?}..{max:?}"
            );
        }
    }
}
