//! Keeping the rock layer out of the way of carved passages.
//!
//! The rock pass runs after every carve, so a boulder whose bounding box
//! reaches a cave or a tunnel simply overwrites the air there. Usually that
//! is wanted --- a rock wedged in a ceiling or a wall reads as real --- and
//! occasionally it seals the way through, which is not.
//!
//! This is the adapter between [`super::rock`]'s boulders and
//! [`super::traversal`]'s region-agnostic analysis: it decides which
//! passages a given boulder could possibly reach, asks the analysis what the
//! boulder does to each of them, and hands back either the repair to apply
//! or the answer that the boulder must not be placed. Everything specific to
//! *which* passage families this world has lives here, so neither the rock
//! layer nor the analysis has to know.

use crate::{
    CanvasInfo, Land,
    layer::{
        authored_regions::authored_voids_for as authored_voids,
        authored_voids::AuthoredVoidPassage,
        cave,
        rock::Rock,
        traversal::{self, Accommodation, AccommodationTier, PassageQuery, TraversalParams},
    },
};
use vek::*;

/// Sampling stride for the traversal analysis, in blocks.
///
/// Every column, because a coarser stride was measured against the real map
/// and does **not** preserve the verdict: sampling every other column found
/// a rock obstructing a passage that sampling every column shows is still
/// open along a gap narrower than the stride. The error is in the safe
/// direction (a repair that was not needed, never a seal left in place), but
/// "nothing is repaired that was not broken" is a property this analysis is
/// supposed to have, not one to trade away for speed on the handful of rocks
/// per map that reach a passage at all. The stride stays a parameter of the
/// analysis so the trade can be re-measured; a regression test reports the
/// disagreement.
const ANALYSIS_STRIDE: i32 = 1;

/// Every rock the global structure lattice would place in `min .. max`,
/// with the traversal verdict for each passage it lands in.
///
/// A read-only mirror of the generation above: it makes the same RNG draws
/// in the same order and stops short of writing anything, so it enumerates
/// the same candidates `apply_rocks_to` does. Used by the real-asset
/// regression tests, which need to reason about every colliding rock in a
/// region rather than one chunk's worth.
///
/// Two deliberate differences from generation, both so the probe can answer
/// questions generation cannot:
///
/// * It consults the procedural tunnel layer whether or not this region
///   currently *runs* it. A region with the tunnel layer switched off still has
///   tunnel geometry --- the shapes are hash-derived on demand, not stored ---
///   and the question the probe exists to answer is how much of it a rock would
///   meet if the layer were switched on.
/// * It reports rocks that reach a hand-authored interior, which generation
///   simply does not place.
#[cfg(test)]
pub(crate) fn probe_rocks_in(
    info: &CanvasInfo,
    min: Vec2<i32>,
    max: Vec2<i32>,
    stride: i32,
) -> Vec<ProbedRock> {
    let land = info.land();
    let params = TraversalParams::ENGINE;
    crate::layer::rock::rock_lattice(info.index().seed)
        .iter(min, max)
        .filter_map(|(wpos, seed)| {
            let rock = crate::layer::rock::rock_at(wpos, seed, info.col_or_gen(wpos)?.as_ref())?;
            let bounds = rock.world_bounds();
            // Unlike generation, the probe asks the tunnel layer about its
            // geometry whether or not this region runs it.
            let near = NearbyPassages {
                tunnels: cave::tunnel_passage_near(
                    rock.wpos.xy(),
                    (bounds.max.xy() - bounds.min.xy())
                        .map(|e| e as f64)
                        .magnitude()
                        / 2.0
                        + params.target_width as f64,
                    info,
                    &land,
                ),
                ..passages_near(&rock, info, &land).unwrap_or_default()
            };
            // The same cheap gate production applies before analysing a
            // rock, so the two agree about which rocks are even looked at.
            if !passages_reach(bounds, &near) {
                return None;
            }

            let mut probed = ProbedRock {
                wpos: rock.wpos,
                size: rock.nominal_size(),
                in_interior: false,
                tunnel: None,
                cave: None,
            };
            if let Some(tunnels) = &near.tunnels {
                let v = traversal::analyse(&rock, tunnels, &params, stride);
                if v.class != traversal::ObstructionClass::Clear {
                    probed.tunnel = Some(v);
                }
            }
            for authored in &near.authored {
                let v = traversal::analyse(&rock, authored, &params, stride);
                if v.class == traversal::ObstructionClass::Clear {
                    continue;
                }
                match PassageQuery::tier(authored) {
                    AccommodationTier::HandAuthored => probed.in_interior = true,
                    _ => probed.cave = Some(v),
                }
            }
            (probed.tunnel.is_some() || probed.cave.is_some() || probed.in_interior)
                .then_some(probed)
        })
        .collect()
}

/// One rock the probe found intersecting something.
#[cfg(test)]
pub(crate) struct ProbedRock {
    pub wpos: Vec3<i32>,
    pub size: f32,
    pub in_interior: bool,
    pub tunnel: Option<traversal::PassageVerdict>,
    pub cave: Option<traversal::PassageVerdict>,
}

/// Whether this region asked for boulders that land in a passage to be
/// accommodated rather than left to seal it.
///
/// Opt-in, and off wherever nothing says otherwise: the analysis changes
/// generated terrain wherever a boulder meets a void, so a world that never
/// asked for it -- every plain procedural world, upstream's included --
/// generates exactly as it did before.
pub(crate) fn repair_enabled(info: &CanvasInfo) -> bool {
    info.chunks()
        .authored_procedural_layers()
        .is_some_and(|layers| layers.rock_traversal_repair)
}

/// Decide what, if anything, this rock has to do to keep the passages it
/// lands in traversable. Returns `None` when the rock must not be placed at
/// all.
///
/// The analysis is anchored to the **rock**, not to the chunk: a rock's
/// anchor, seed, units and kind come from the global structure lattice, and
/// the passages it is tested against are pure functions of world position.
/// So the same answer is derived in every chunk the rock touches, with no
/// cross-chunk state --- which is what lets this be a post-stamp repair
/// rather than a re-architecture of the carve pipeline.
pub(crate) fn accommodate(mut rock: Rock, info: &CanvasInfo) -> Option<Rock> {
    let bounds = rock.world_bounds();
    let land = info.land();
    let params = TraversalParams::ENGINE;

    let Some(passages) = passages_near(&rock, info, &land) else {
        return Some(rock);
    };
    if !passages.reach(bounds) {
        return Some(rock);
    }

    // A rock may land in more than one passage family at once. Each is
    // analysed on its own terms -- the tier picks the repair budget -- and
    // the repairs are unioned.
    //
    // This analysis is purely geometric, and deliberately consults no
    // authored policy about what a passage is for. Whether two generators
    // may join at a point is a connectivity question decided elsewhere and
    // has no bearing on whether a body fits through: a cave someone was
    // happy to have a tunnel break into is not one they wanted filled with
    // impassable rubble, so an authored cave is accommodated identically
    // however its connectivity is set.
    let mut merged: Option<Accommodation> = None;
    for passage in passages.queries() {
        let verdict = traversal::analyse(&rock, passage, &params, ANALYSIS_STRIDE);
        if verdict.reject {
            return None;
        }
        if let Some(repair) = verdict.repair {
            merged = Some(match merged {
                Some(mut acc) => {
                    acc.union(&repair);
                    acc
                },
                None => repair,
            });
        }
    }
    rock.accommodation = merged;
    Some(rock)
}

/// The passages that come near one intruder.
///
/// Two sources, and only two: the procedural tunnel layer, whose geometry is
/// derived on demand from noise, and the shared authored-void index, which
/// every authored layer registers its own shapes with. **Nothing here names a
/// region** --- an authored map joins by registering, exactly as it already
/// must to be protected from procedural tunnels, and inherits an accommodation
/// tier with it.
#[derive(Default)]
pub(crate) struct NearbyPassages<'a> {
    tunnels: Option<cave::TunnelPassage<'a>>,
    /// The authored voids, split by tier, because a tier is a repair budget
    /// and the analysis needs one answer for the geometry it is looking at.
    authored: Vec<AuthoredVoidPassage<'a>>,
}

/// Every passage whose geometry could reach this rock, or `None` when none
/// can --- which is the overwhelmingly common case and the cheap answer.
fn passages_near<'a>(
    rock: &Rock,
    info: &CanvasInfo<'a>,
    land: &Land,
) -> Option<NearbyPassages<'a>> {
    let bounds = rock.world_bounds();
    let reach = (bounds.max.xy() - bounds.min.xy())
        .map(|e| e as f64)
        .magnitude()
        / 2.0
        + TraversalParams::ENGINE.target_width as f64;
    let centre = rock.wpos.xy();

    // The procedural tunnel layer exists only where it is actually run: its
    // geometry is derived on demand, so a region that switched it off has no
    // tunnels for a rock to land in. Authored voids are the other way round --
    // they are carved wherever the authored layer runs, which is what the
    // index being non-empty already says.
    let tunnels_run = info.index().features.caves
        && info
            .chunks()
            .authored_procedural_layers()
            .is_none_or(|l| l.caves);
    let near = NearbyPassages {
        tunnels: tunnels_run
            .then(|| cave::tunnel_passage_near(centre, reach, info, land))
            .flatten(),
        authored: authored_voids(info)
            .map(|voids| {
                [AccommodationTier::Catalog, AccommodationTier::HandAuthored]
                    .into_iter()
                    .filter_map(|tier| voids.passage(info, tier))
                    .collect()
            })
            .unwrap_or_default(),
    };
    (near.tunnels.is_some() || !near.authored.is_empty()).then_some(near)
}

impl<'a> NearbyPassages<'a> {
    /// Each of them, as something the analysis can query.
    fn queries(&self) -> impl Iterator<Item = &dyn PassageQuery> {
        self.tunnels
            .as_ref()
            .map(|p| p as &dyn PassageQuery)
            .into_iter()
            .chain(self.authored.iter().map(|p| p as &dyn PassageQuery))
    }

    /// Whether any of them has an open band near this box's `z` extent.
    fn reach(&self, bounds: Aabb<i32>) -> bool { passages_reach(bounds, self) }
}

/// Narrowest passage the pre-check below must not sample past, in blocks.
///
/// Taken from the smallest radius any passage family in this engine gives a
/// tunnel: sampling more finely than this would cost more for nothing, and
/// sampling more coarsely could thread a grid line either side of a real
/// tunnel and miss it entirely.
const PRECHECK_FEATURE_WIDTH: i32 = 8;

/// Does any of these passages have an open band anywhere near this box's `z`
/// extent?
///
/// A cheap gate in front of the real analysis: it samples a coarse grid over
/// the footprint and asks each passage for its *unclamped* band, which costs
/// a few noise lookups per passage, where the analysis costs a full column
/// generation per column of the footprint. Over-reporting is harmless (the
/// analysis then says `Clear`); under-reporting would silently skip a real
/// obstruction, which is why the grid is scaled to the narrowest passage
/// rather than pinned at a fixed count --- a fixed count is either wasteful
/// on a small rock or too coarse on a large one, and which of the two it is
/// depends on the rock rather than on anything knowable here.
fn passages_reach(bounds: Aabb<i32>, near: &NearbyPassages) -> bool {
    let span = bounds.max.xy() - bounds.min.xy();
    let samples = (span.map(|e| e / PRECHECK_FEATURE_WIDTH).reduce_max() + 1).clamp(2, 9);
    let mut coarse = Vec::new();
    for sx in 0..samples {
        for sy in 0..samples {
            let wpos2d = bounds.min.xy()
                + Vec2::new(span.x * sx / (samples - 1), span.y * sy / (samples - 1));
            coarse.clear();
            if let Some(t) = &near.tunnels {
                t.coarse_bands(wpos2d, &mut coarse);
            }
            for authored in &near.authored {
                authored.coarse_bands(wpos2d, &mut coarse);
            }
            if coarse
                .iter()
                .any(|&(lo, hi)| bounds.min.z <= hi && bounds.max.z >= lo)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{canvas::CanvasInfo, layer::traversal::ObstructionClass, sim::FileOpts};

    /// The real-asset measurement and regression suite.
    ///
    /// Sweeps every rock the structure lattice places over the whole
    /// Cromatolis map, analyses each against both passage families, and
    /// asserts the invariants the accommodation is supposed to guarantee.
    /// The counts it prints are the measured figures the design is sized
    /// against; they are reported rather than asserted on, because they move
    /// whenever the map's terrain does.
    ///
    /// `#[ignore]`d: it generates a full world and sweeps roughly two million
    /// lattice candidates, which is minutes rather than seconds.
    #[test]
    #[ignore]
    fn real_world_rock_traversal_measurement() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let sim = world.sim();
        let size = sim.get_size().map(|e| e as i32) * 32;

        CanvasInfo::with_mock_canvas_info(index_ref, sim, |info| {
            let started = std::time::Instant::now();
            let probed = probe_rocks_in(info, Vec2::zero(), size, ANALYSIS_STRIDE);
            let elapsed = started.elapsed();
            // Per *candidate*, not per chunk: the sweep visits each lattice
            // candidate once, where generation re-derives each one in every
            // chunk whose structure-cache neighbourhood reaches it. Dividing
            // by the chunk count would understate the per-chunk cost by that
            // factor, so the conversion is left to the reader who knows it.
            let candidates = (size.x as u64 / 24) * (size.y as u64 / 24);
            println!(
                "swept {candidates} lattice candidates in {elapsed:?} ({:.2} us each, including \
                 the column generation the rock pass already paid for before this change)",
                elapsed.as_secs_f64() * 1e6 / candidates as f64
            );

            let mut tunnel_hits = 0usize;
            let mut cave_hits = 0usize;
            let mut interior_hits = 0usize;
            let mut classes = [0usize; 3];
            let mut obstructed = 0usize;
            let mut broken_before = 0usize;
            let mut repaired = 0usize;
            let mut rejected = 0usize;
            let mut player_only = 0usize;
            let mut repair_voxels = 0i64;

            for rock in &probed {
                if rock.in_interior {
                    interior_hits += 1;
                }
                if rock.tunnel.is_some() {
                    tunnel_hits += 1;
                }
                if rock.cave.is_some() {
                    cave_hits += 1;
                }
                for verdict in [&rock.tunnel, &rock.cave].into_iter().flatten() {
                    match verdict.class {
                        ObstructionClass::Clear => {},
                        ObstructionClass::CeilingPendant => classes[0] += 1,
                        ObstructionClass::FloorRooted => classes[1] += 1,
                        ObstructionClass::Sealed => classes[2] += 1,
                    }
                    if verdict.broken_before {
                        broken_before += 1;
                    }
                    if verdict.obstructed {
                        obstructed += 1;
                    }
                    if verdict.reject {
                        rejected += 1;
                    }
                    if let Some(repair) = &verdict.repair {
                        repaired += 1;
                        repair_voxels += repair.voxel_budget();
                    }
                    if verdict.obstructed && !verdict.reject && !verdict.agent_passable {
                        player_only += 1;
                    }
                    if !verdict.agent_passable && !verdict.reject {
                        println!(
                            "  player-only chokepoint at {:?}: a body can climb this rock but no \
                             agent can path past it",
                            rock.wpos
                        );
                    }
                }
            }

            println!(
                "rock/passage intersections over the whole region:\n  rocks intersecting a \
                 PROCEDURAL tunnel: {tunnel_hits}\n  rocks intersecting an AUTHORED cave:    \
                 {cave_hits}\n  rocks reaching a hand-authored interior: {interior_hits}\n  class \
                 C0 ceiling-pendant: {}\n  class C1 floor-rooted:    {}\n  class C2 candidate \
                 seal:  {}\n  already impassable before the rock: {broken_before}\n  genuinely \
                 obstructed: {obstructed}\n  repaired: {repaired} ({repair_voxels} voxels of air \
                 opened)\n  rejected: {rejected}\n  repaired but player-only (no agent route): \
                 {player_only}",
                classes[0], classes[1], classes[2]
            );

            // Locators for the in-client visual pass (COW-23 T28), which is
            // the only part of this row a test cannot answer. Capped, and
            // ordered repaired-first within the probe's own lattice sweep
            // order, so the list is short, stable and copy-pasteable into a
            // `/goto`. Repaired rocks lead because R2's cut channel is the
            // operation T28 exists to look at.
            //
            // This counts rocks the analysis *classified* in an authored cave,
            // which is a subset of the `rocks intersecting an AUTHORED cave`
            // line above: a rock whose verdict came back `Clear` reaches the
            // cave but has nothing to look at.
            let mut locators: Vec<(bool, Vec3<i32>, f32, &str)> = Vec::new();
            for rock in &probed {
                let Some(verdict) = &rock.cave else { continue };
                let class = match verdict.class {
                    ObstructionClass::Clear => continue,
                    ObstructionClass::CeilingPendant => "ceiling pendant",
                    ObstructionClass::FloorRooted => "floor-rooted",
                    ObstructionClass::Sealed => "candidate seal",
                };
                locators.push((verdict.repair.is_some(), rock.wpos, rock.size, class));
            }
            locators.sort_by_key(|(repaired, ..)| !repaired);
            println!(
                "boulders CLASSIFIED in an authored cave, for the in-client look ({} of them; the \
                 rest of the {cave_hits} reach one but come back Clear):",
                locators.len()
            );
            for (repaired, wpos, size, class) in locators.iter().take(24) {
                println!(
                    "  rock@{wpos:?} size {size:.1} -- {class}{}",
                    if *repaired { ", REPAIRED (R2)" } else { "" }
                );
            }

            // Every rock that genuinely obstructs a passage either gets a
            // repair that restores the route or is kept out; and a passage
            // that was already impassable is never touched.
            for rock in &probed {
                for verdict in [&rock.tunnel, &rock.cave].into_iter().flatten() {
                    if verdict.broken_before {
                        assert!(
                            verdict.repair.is_none(),
                            "rock at {:?} widened a passage that was already impassable",
                            rock.wpos
                        );
                    }
                    if verdict.obstructed {
                        assert!(
                            verdict.repair.is_some() || verdict.reject,
                            "rock at {:?} left a passage obstructed with no repair and no \
                             rejection",
                            rock.wpos
                        );
                    } else {
                        assert!(
                            !verdict.reject,
                            "rock at {:?} was kept out without obstructing anything",
                            rock.wpos
                        );
                    }
                }
            }

            // A tripwire, not a target. Today nothing on this map produces a
            // route only a climbing player can take, and if that changes it
            // is a content decision someone has to make rather than a number
            // that quietly drifts: whether a passage may become a one-way
            // valve for every agent and creature in it is exactly the
            // question this analysis exists to surface.
            assert_eq!(
                player_only, 0,
                "a rock left a passage passable only by climbing; see the per-rock lines above"
            );

            // Additivity: every carved column lies inside the rock's own
            // footprint dilated by the channel width.
            let params = TraversalParams::ENGINE;
            for rock in &probed {
                for verdict in [&rock.tunnel, &rock.cave].into_iter().flatten() {
                    let Some(repair) = &verdict.repair else {
                        continue;
                    };
                    for (wpos2d, (lo, hi)) in repair.columns() {
                        let d = (wpos2d - rock.wpos.xy()).map(|e| e.abs());
                        let reach = rock.size.ceil() as i32 + params.target_width + 1;
                        assert!(
                            d.x <= reach && d.y <= reach,
                            "rock at {:?} carved {wpos2d:?}, outside its dilated footprint",
                            rock.wpos
                        );
                        assert!(hi >= lo);
                    }
                }
            }
        });
    }

    /// A world that never asked for the repair must not get it --- which is
    /// every plain procedural world, upstream's included, since none of them
    /// ships a region policy at all.
    #[test]
    fn a_world_without_a_region_policy_never_repairs() {
        assert!(
            !crate::sim::AuthoredProceduralLayers::default().rock_traversal_repair,
            "the permissive fallback must not switch this on: it adds geometry rather than \
             suppressing a layer, so its upstream-behaviour value is off"
        );
    }

    /// The accommodation is anchored to the rock, not to the chunk: the same
    /// rock must be analysed identically no matter which neighbourhood it was
    /// reached from. That is the property that lets a rock spanning a chunk
    /// border be repaired the same way from either side.
    #[test]
    #[ignore]
    fn the_verdict_does_not_depend_on_the_region_it_was_analysed_from() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let sim = world.sim();

        CanvasInfo::with_mock_canvas_info(index_ref, sim, |info| {
            let size = sim.get_size().map(|e| e as i32) * 32;
            let key = |rock: &ProbedRock| {
                let verdict = |v: &Option<traversal::PassageVerdict>| {
                    v.as_ref()
                        .map(|v| {
                            (
                                v.class,
                                v.obstructed,
                                v.reject,
                                v.repair.as_ref().map(|r| r.voxel_budget()),
                            )
                        })
                        .unwrap_or((traversal::ObstructionClass::Clear, false, false, None))
                };
                (
                    rock.wpos.into_array(),
                    verdict(&rock.tunnel),
                    verdict(&rock.cave),
                )
            };
            let all = probe_rocks_in(info, Vec2::zero(), size, ANALYSIS_STRIDE);
            assert!(
                !all.is_empty(),
                "the region must contain rocks that reach a passage"
            );
            // Re-reach each of them from a tight window of its own, i.e. from
            // a completely different neighbourhood than the whole-region
            // sweep enumerated it in.
            for rock in &all {
                let window = probe_rocks_in(
                    info,
                    rock.wpos.xy() - 48,
                    rock.wpos.xy() + 48,
                    ANALYSIS_STRIDE,
                );
                let same = window
                    .iter()
                    .find(|other| other.wpos == rock.wpos)
                    .unwrap_or_else(|| {
                        panic!("rock at {:?} was not found from its own window", rock.wpos)
                    });
                assert_eq!(
                    key(rock),
                    key(same),
                    "rock at {:?} was analysed differently depending on the neighbourhood it was \
                     reached from",
                    rock.wpos
                );
            }
        });
    }

    /// How many catalogue chambers the slab test tries before giving up. The
    /// catalogue's own order is deterministic, so this is a bound on the test's
    /// runtime, not a sampling choice.
    const SLAB_ANCHORS_TRIED: usize = 40;
    /// Half the slab's span across the chamber, in blocks -- comfortably past
    /// any catalogue chamber radius, so it really is a wall and not a pillar.
    const SLAB_HALF_SPAN: i32 = 60;
    /// Half the slab's thickness along the other axis, in blocks.
    const SLAB_HALF_THICKNESS: i32 = 3;

    /// A solid slab dropped across a real authored cave must be reported as
    /// sealing it, and must earn a repair.
    ///
    /// The whole-region sweep above finds no rock that actually seals a
    /// passage, which is a real result but a weak one on its own: an
    /// analysis that always answered "passable" would produce it too. This
    /// pins the other direction against the same real geometry --- when
    /// something genuinely does block the way, it is seen.
    #[test]
    #[ignore]
    fn a_slab_across_a_real_authored_cave_is_seen_as_a_seal() {
        /// A solid rectangular block, as an intruder.
        struct Slab(Aabb<i32>);
        impl crate::layer::traversal::SolidVolume for Slab {
            fn bounds(&self) -> Aabb<i32> { self.0 }

            fn column_solid(&self, wpos2d: Vec2<i32>, z_lo: i32, z_hi: i32, out: &mut Vec<bool>) {
                let inside = wpos2d.x >= self.0.min.x
                    && wpos2d.x <= self.0.max.x
                    && wpos2d.y >= self.0.min.y
                    && wpos2d.y <= self.0.max.y;
                for z in z_lo..=z_hi {
                    out.push(inside && z >= self.0.min.z && z <= self.0.max.z);
                }
            }
        }

        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let sim = world.sim();
        let params = TraversalParams::ENGINE;

        CanvasInfo::with_mock_canvas_info(index_ref, sim, |info| {
            let voids = authored_voids(info).expect("the real region must register voids");
            let anchors = voids.disc_anchors(AccommodationTier::Catalog);
            assert!(!anchors.is_empty(), "the real catalogue must resolve caves");
            let passage = voids
                .passage(info, AccommodationTier::Catalog)
                .expect("the real catalogue registers catalog-tier shapes");

            // Walk the catalogue's own chamber anchors, in the catalogue's own
            // order, until one is narrow enough for the slab to span.
            let mut found = None;
            for (anchor, floor_z) in anchors.into_iter().take(SLAB_ANCHORS_TRIED) {
                // A wall across the chamber: `SLAB_HALF_SPAN` blocks either
                // side of the anchor so it reaches past any catalogue chamber
                // radius, `SLAB_HALF_THICKNESS` thick along the other axis,
                // standing a little below the carved floor and reaching well
                // above the ceiling. Nothing about it is rock-shaped -- the
                // point is a shape that unambiguously *does* seal, to check
                // the analysis says so.
                let slab = Slab(Aabb {
                    min: Vec3::new(
                        anchor.x - SLAB_HALF_SPAN,
                        anchor.y - SLAB_HALF_THICKNESS,
                        floor_z - 4,
                    ),
                    max: Vec3::new(
                        anchor.x + SLAB_HALF_SPAN,
                        anchor.y + SLAB_HALF_THICKNESS,
                        floor_z + 40,
                    ),
                });
                let verdict = traversal::analyse(&slab, &passage, &params, 1);
                if verdict.obstructed {
                    found = Some((anchor, floor_z, verdict));
                    break;
                }
            }

            let (anchor, floor_z, verdict) = found
                .expect("a slab spanning a real authored cave was never reported as sealing it");
            // Printed so this doubles as the locator for an in-client look at
            // the repair, once the region runs its rock layer: this is where a
            // sealing intruder and its repair actually are on the real map.
            println!(
                "sealed a real authored chamber at ({}, {}), carved floor z = {floor_z}",
                anchor.x, anchor.y
            );
            println!(
                "  slab spans x {} .. {}, y {} .. {}, z {} .. {}",
                anchor.x - SLAB_HALF_SPAN,
                anchor.x + SLAB_HALF_SPAN,
                anchor.y - SLAB_HALF_THICKNESS,
                anchor.y + SLAB_HALF_THICKNESS,
                floor_z - 4,
                floor_z + 40,
            );
            println!(
                "  verdict: {:?}, repaired = {}, kept out = {}, agent-passable = {}, air opened = \
                 {} voxels",
                verdict.class,
                verdict.repair.is_some(),
                verdict.reject,
                verdict.agent_passable,
                verdict.repair.as_ref().map_or(0, |r| r.voxel_budget()),
            );

            assert!(
                verdict.repair.is_some() || verdict.reject,
                "a sealed authored cave must be repaired or refuse the intruder"
            );
            if let Some(repair) = &verdict.repair {
                assert!(repair.voxel_budget() > 0);
                for (_, (lo, _)) in repair.columns() {
                    assert!(
                        lo >= floor_z - 1,
                        "an authored cave's floor must never be dug"
                    );
                }
            }
        });
    }

    /// What a coarser sampling stride actually costs, measured rather than
    /// assumed.
    ///
    /// The cheap-sampling idea is that rocks are blobby, so a one-block
    /// sampling error is irrelevant to a three-block corridor. Against the
    /// real map that is not quite true: a passage can stay open along a gap
    /// narrower than the stride, and the coarse pass then reports a seal
    /// that is not there. This asserts only the direction that matters ---
    /// the coarse pass never *misses* an obstruction the fine pass finds ---
    /// and prints the disagreement count, which is the number that decides
    /// whether the stride is worth taking.
    #[test]
    #[ignore]
    fn stride_two_agrees_with_stride_one_on_real_rocks() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        let sim = world.sim();
        let size = sim.get_size().map(|e| e as i32) * 32;
        // One quarter of the map: enough real collisions to be meaningful
        // without paying for two full sweeps.
        let slice_max = Vec2::new(size.x / 2, size.y / 2);

        CanvasInfo::with_mock_canvas_info(index_ref, sim, |info| {
            let one = probe_rocks_in(info, Vec2::zero(), slice_max, 1);
            let two = probe_rocks_in(info, Vec2::zero(), slice_max, 2);

            // The set of rocks the probe reports as *touching* a passage at
            // all is allowed to differ: a finer stride notices a rock that
            // clips one corner of a band, which the coarser one samples past.
            // That is the sampling error the stride deliberately accepts, and
            // it only ever concerns rocks that earn no repair either way.
            //
            // What must not differ is the outcome: whether a rock obstructs a
            // passage, and whether it is kept out. A rock absent from a run
            // did neither.
            let outcome = |probed: &Vec<ProbedRock>| {
                probed
                    .iter()
                    .map(|rock| {
                        let verdict = |v: &Option<traversal::PassageVerdict>| {
                            v.as_ref()
                                .map(|v| (v.obstructed, v.reject))
                                .unwrap_or((false, false))
                        };
                        (
                            rock.wpos.into_array(),
                            (verdict(&rock.tunnel), verdict(&rock.cave)),
                        )
                    })
                    .filter(|(_, o)| *o != ((false, false), (false, false)))
                    .collect::<std::collections::BTreeMap<_, _>>()
            };
            let (one_out, two_out) = (outcome(&one), outcome(&two));
            let only_fine: Vec<_> = one_out
                .keys()
                .filter(|k| !two_out.contains_key(*k))
                .collect();
            let only_coarse: Vec<_> = two_out
                .keys()
                .filter(|k| !one_out.contains_key(*k))
                .collect();
            println!(
                "stride-1 touched {} rocks ({} with an outcome); stride-2 touched {} ({} with an \
                 outcome)\n  found only by the fine pass:   {only_fine:?}\n  found only by the \
                 coarse pass: {only_coarse:?}",
                one.len(),
                one_out.len(),
                two.len(),
                two_out.len(),
            );
            assert!(
                only_fine.is_empty(),
                "the coarse pass missed an obstruction the fine pass found, which is the one \
                 direction it may not err in"
            );
        });
    }
}
