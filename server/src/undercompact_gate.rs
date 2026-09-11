//! COW-7b: server-side support for the Undercompact gate antechamber's
//! two-lever puzzle -- the shared plug-clearing write path (used by both
//! `server/src/events/undercompact_gate.rs`'s lever-activation event handler
//! and the restart-recovery check below), and the restart-recovery check
//! itself.
//!
//! Mirrors `gate_checkpoint.rs`'s "resolve authored geometry fresh, write
//! blocks via `State::set_block`" shape, but the recovery check here is
//! deliberately a **one-shot check per newly-loaded chunk**, not a perpetual
//! 1Hz reconciliation -- this puzzle never re-closes once solved
//! (`rtsim::data::UndercompactGateLevers::is_solved` is a one-way latch), so
//! there is nothing to keep reconciling once the relevant chunk is live.

use common::terrain::{Block, CoordinateConversions};
use common_state::State;
use specs::WorldExt;
use vek::{Aabb, Vec2, Vec3};
use world::{IndexRef, World, layer::cromatolis_interior};

use crate::rtsim::RtSim;

/// Clears every block within `aabb` for which `contains_column` reports an
/// inclusive `(floor_z, ceiling_z)` range -- scanning only within the AABB,
/// so this never carves further than the caller's own bound.
///
/// Generic over both the shape test and the write path: the shape test lets
/// this be unit-tested with a synthetic region (see this module's tests)
/// without needing a real `WorldSim`, and the write path lets both an
/// ECS-dispatched event handler (which only has a `BlockChange` resource to
/// write through -- see `server/src/events/undercompact_gate.rs`) and
/// outside-dispatch code like [`ensure_undercompact_gate_restart_recovery`]
/// below (which has a `&mut State` and uses [`State::set_block`], the same
/// way `gate_checkpoint::write_gate_state` does) share one implementation.
pub(crate) fn clear_plug_region(
    aabb: Aabb<i32>,
    contains_column: impl Fn(Vec2<i32>) -> Option<(i32, i32)>,
    mut set_block: impl FnMut(Vec3<i32>, Block),
) {
    for x in aabb.min.x..aabb.max.x {
        for y in aabb.min.y..aabb.max.y {
            let wpos2d = Vec2::new(x, y);
            let Some((floor_z, ceiling_z)) = contains_column(wpos2d) else {
                continue;
            };
            for z in floor_z..=ceiling_z {
                set_block(wpos2d.with_z(z), Block::empty());
            }
        }
    }
}

/// Clears `geometry`'s sealed-gate plug in full -- the shape-aware test is
/// [`cromatolis_interior::UndercompactGateAntechamberGeometry::plug_contains_column`],
/// the exact same one world-gen used to fill it, so this never carves
/// outside (or leaves a sliver inside) the authored plug shape.
pub(crate) fn clear_gate_plug(
    geometry: &cromatolis_interior::UndercompactGateAntechamberGeometry,
    set_block: impl FnMut(Vec3<i32>, Block),
) {
    clear_plug_region(
        geometry.plug_aabb,
        |wpos2d| geometry.plug_contains_column(wpos2d),
        set_block,
    );
}

/// Whether `geometry`'s plug is already fully clear, checked via one
/// representative column -- the first the plug's own shape test
/// (`plug_contains_column`) actually accepts within its scan AABB, not the
/// AABB's raw geometric center, which the plug's true (spline-bulge) shape
/// is not guaranteed to cover.
///
/// Exists so both `clear_gate_plug` call sites (this module's restart
/// recovery, and `server/src/events/undercompact_gate.rs`'s lever-activation
/// handler) can skip the AABB-wide rewrite -- and the redundant network
/// diff/client remesh it would otherwise cause -- once the plug has already
/// been cleared once and stays that way (e.g. every subsequent chunk reload
/// after the puzzle is solved, or terrain persistence already having
/// restored the cleared state before this check even runs).
///
/// Generic over the block read the same way [`clear_plug_region`] is
/// generic over the write, so it works from both an outside-dispatch
/// `&State` (via [`State::get_block`]) and an ECS-dispatched event handler's
/// `ReadExpect<TerrainGrid>`.
pub(crate) fn plug_already_clear(
    geometry: &cromatolis_interior::UndercompactGateAntechamberGeometry,
    get_block: impl Fn(Vec3<i32>) -> Option<Block>,
) -> bool {
    for x in geometry.plug_aabb.min.x..geometry.plug_aabb.max.x {
        for y in geometry.plug_aabb.min.y..geometry.plug_aabb.max.y {
            let wpos2d = Vec2::new(x, y);
            if let Some((floor_z, _)) = geometry.plug_contains_column(wpos2d) {
                return get_block(wpos2d.with_z(floor_z)) == Some(Block::empty());
            }
        }
    }
    // No column in the AABB satisfies the plug's own shape test at all --
    // there is nothing to clear either way, so treat it as "already clear".
    true
}

/// The set of chunk keys `geometry`'s plug AABB's footprint touches --
/// almost always one, but the plug can straddle a chunk boundary, so every
/// corner of its 2D footprint is checked rather than just its center.
fn plug_chunk_keys(
    geometry: &cromatolis_interior::UndercompactGateAntechamberGeometry,
) -> impl Iterator<Item = Vec2<i32>> {
    let min = geometry.plug_aabb.min.xy();
    let max = geometry.plug_aabb.max.xy();
    [
        Vec2::new(min.x, min.y),
        Vec2::new(max.x, min.y),
        Vec2::new(min.x, max.y),
        Vec2::new(max.x, max.y),
    ]
    .into_iter()
    .map(|wpos2d| wpos2d.wpos_to_cpos())
}

/// One-shot restart-recovery check: if the antechamber's sealed-gate plug
/// sits in a chunk that just became live this tick (`State::terrain_changes`'s
/// freshly-inserted chunks -- true on server start for every already-solved
/// save, and true the first time any player approaches a never-before-loaded
/// area) and the puzzle's persisted state says already solved, clears the
/// plug immediately.
///
/// World-gen re-carves the plug deterministically from RON on every first
/// chunk load, with no memory of its own of a solved flag -- this check is
/// what makes a solve actually survive a server restart.
///
/// Called every tick (mirroring `gate_checkpoint`'s call sites in
/// `Server::tick`), but it is not itself a poll, and deliberately does
/// **not** touch `world.sim()`/the geometry accessor at all on the common
/// (almost every tick) case: the very first thing checked is
/// `terrain_changes().new_chunks`, which is empty on almost every tick, and
/// the persisted `solved` flag never reverts to `false`, so once the
/// relevant chunk has been checked once there is nothing left for later
/// ticks to do here. This ordering matters even though
/// [`cromatolis_interior::undercompact_gate_antechamber_world_geometry`]
/// itself is normally a cheap cached read (see that function's own doc
/// comment) -- it still isn't `free`, and there is no reason to pay even a
/// cache lookup every tick forever when the vastly more common case can
/// bail out before ever calling it.
///
/// Returns whether the plug was cleared this call.
pub fn ensure_undercompact_gate_restart_recovery(
    state: &mut State,
    world: &World,
    index: IndexRef,
) -> bool {
    ensure_undercompact_gate_restart_recovery_with(state, || {
        cromatolis_interior::undercompact_gate_antechamber_world_geometry(index, world.sim())
    })
}

/// The testable core of [`ensure_undercompact_gate_restart_recovery`],
/// taking the geometry resolution as an injected closure rather than calling
/// [`cromatolis_interior::undercompact_gate_antechamber_world_geometry`]
/// directly. This is what lets a test assert that closure is never even
/// invoked when `new_chunks` is empty (the common, every-tick-forever case
/// in production) without needing a real `World`/`Index` just to prove a
/// negative -- see this module's own tests.
fn ensure_undercompact_gate_restart_recovery_with(
    state: &mut State,
    resolve_geometry: impl FnOnce() -> Option<cromatolis_interior::UndercompactGateAntechamberGeometry>,
) -> bool {
    let new_chunks = &state.terrain_changes().new_chunks;
    if new_chunks.is_empty() {
        return false;
    }

    let Some(geometry) = resolve_geometry() else {
        return false;
    };

    if !plug_chunk_keys(&geometry).any(|key| new_chunks.contains(&key)) {
        return false;
    }

    let solved = state
        .ecs()
        .read_resource::<RtSim>()
        .with_undercompact_gate(|levers| levers.is_solved());
    if !solved {
        return false;
    }

    if plug_already_clear(&geometry, |pos| state.get_block(pos)) {
        return false;
    }

    clear_gate_plug(&geometry, |pos, block| state.set_block(pos, block));
    true
}

/// Test-only helpers shared with `server/src/events/undercompact_gate.rs`'s
/// own test module -- `pub(crate)` (not fully private) specifically so that
/// sibling module can reuse the same real-`RtSim`/real-`World` setup rather
/// than duplicating the (non-trivial) `RtSim::new` construction dance.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use common::{
        resources::GameMode,
        terrain::{BlockKind, MapSizeLg, TerrainChunk, TerrainGrid},
    };
    use std::sync::Arc;

    pub(crate) const WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    pub(crate) fn setup() -> State {
        let mut state = State::new(
            GameMode::Server,
            State::pools(GameMode::Server),
            WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                common_systems::add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        // `State::apply_terrain_changes` reads this resource to report
        // sprite-removal outcomes -- normally inserted by `Server::new`, and
        // needed here since the plug-clearing tests below apply real
        // terrain changes. Mirrors `gate_checkpoint.rs`'s own test `setup`.
        state
            .ecs_mut()
            .insert(common::event::EventBus::<common::event::BonkEvent>::default());
        state
    }

    pub(crate) fn load_chunk_containing(state: &mut State, pos: Vec3<f32>) {
        let key = state.terrain().pos_key(pos.map(|axis| axis.floor() as i32));
        state
            .ecs_mut()
            .write_resource::<TerrainGrid>()
            .insert(key, Arc::new(TerrainChunk::water(0)));
    }

    /// Constructs and inserts a real `RtSim` resource -- normally done by
    /// `Server::new`, needed here so
    /// `ensure_undercompact_gate_restart_recovery` (and its own
    /// `RtSim::with_undercompact_gate` calls) have a real resource to
    /// fetch. Uses a fresh, unique temp directory as its data
    /// dir so it always takes the "no existing save" generate-fresh path.
    pub(crate) fn insert_rtsim(state: &mut State, world: &World, index: &world::IndexOwned) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let data_dir = std::env::temp_dir().join(format!(
            "xindeler-cow7b-undercompact-gate-test-{}-{unique}",
            std::process::id(),
        ));
        let rtsim = RtSim::new(
            &common::rtsim::WorldSettings::default(),
            index.as_index_ref(),
            world,
            data_dir,
        )
        .expect("constructing a fresh RtSim against a real generated World must not fail");
        state.ecs_mut().insert(rtsim);
    }

    /// A synthetic "plug": every column within 2 blocks (Chebyshev) of
    /// `center2d` is inside, spanning `floor_z..=ceiling_z`.
    fn synthetic_plug_column(
        center2d: Vec2<i32>,
        floor_z: i32,
        ceiling_z: i32,
        wpos2d: Vec2<i32>,
    ) -> Option<(i32, i32)> {
        let within = (wpos2d.x - center2d.x).abs() <= 2 && (wpos2d.y - center2d.y).abs() <= 2;
        within.then_some((floor_z, ceiling_z))
    }

    #[test]
    fn clear_plug_region_only_writes_columns_the_shape_test_accepts() {
        let center = Vec2::new(10, 10);
        let aabb = Aabb {
            min: Vec2::new(0, 0).with_z(0),
            max: Vec2::new(20, 20).with_z(3),
        };

        let mut written = Vec::new();
        clear_plug_region(
            aabb,
            |wpos2d| synthetic_plug_column(center, 0, 2, wpos2d),
            |pos, block| {
                assert_eq!(block, Block::empty());
                written.push(pos);
            },
        );

        // Every written column must be within the synthetic plug's own
        // shape, never outside it even though the scan AABB is much larger.
        assert!(!written.is_empty());
        for pos in &written {
            assert!((pos.x - center.x).abs() <= 2);
            assert!((pos.y - center.y).abs() <= 2);
            assert!((0..=2).contains(&pos.z));
        }
        // A column right at the shape's own boundary must be included.
        assert!(written.contains(&Vec3::new(center.x + 2, center.y, 0)));
        // One block past the boundary must not be.
        assert!(!written.iter().any(|pos| pos.x == center.x + 3));
    }

    #[test]
    fn clear_plug_region_is_a_no_op_when_the_shape_test_never_matches() {
        let aabb = Aabb {
            min: Vec2::new(0, 0).with_z(0),
            max: Vec2::new(5, 5).with_z(5),
        };
        let mut calls = 0;
        clear_plug_region(aabb, |_| None, |_, _| calls += 1);
        assert_eq!(calls, 0);
    }

    #[test]
    fn plug_already_clear_returns_false_when_the_sample_column_is_solid() {
        let center = Vec2::new(0, 0);
        let aabb = Aabb {
            min: Vec2::new(-2, -2).with_z(0),
            max: Vec2::new(2, 2).with_z(2),
        };
        // Reuse `UndercompactGateAntechamberGeometry`'s own shape by hand is
        // not possible from here (its fields are crate-private to the
        // `world` crate), so this exercises `plug_already_clear` directly
        // through its own generic parameters instead.
        let contains_column = |wpos2d: Vec2<i32>| synthetic_plug_column(center, 0, 1, wpos2d);

        for x in aabb.min.x..aabb.max.x {
            for y in aabb.min.y..aabb.max.y {
                let wpos2d = Vec2::new(x, y);
                if let Some((floor_z, _)) = contains_column(wpos2d) {
                    // A solid block at the first in-shape column: not clear.
                    let get_block = |pos: Vec3<i32>| {
                        if pos == wpos2d.with_z(floor_z) {
                            Some(Block::new(BlockKind::Rock, vek::Rgb::new(60, 55, 60)))
                        } else {
                            Some(Block::empty())
                        }
                    };
                    assert!(!plug_already_clear_via(aabb, contains_column, get_block));
                    return;
                }
            }
        }
        panic!("test setup: synthetic plug shape must accept at least one column");
    }

    #[test]
    fn plug_already_clear_returns_true_when_every_in_shape_column_reads_as_air() {
        let center = Vec2::new(0, 0);
        let aabb = Aabb {
            min: Vec2::new(-2, -2).with_z(0),
            max: Vec2::new(2, 2).with_z(2),
        };
        let contains_column = |wpos2d: Vec2<i32>| synthetic_plug_column(center, 0, 1, wpos2d);
        assert!(plug_already_clear_via(aabb, contains_column, |_| {
            Some(Block::empty())
        }));
    }

    #[test]
    fn plug_already_clear_treats_unloaded_unknown_terrain_as_not_clear() {
        let center = Vec2::new(0, 0);
        let aabb = Aabb {
            min: Vec2::new(-2, -2).with_z(0),
            max: Vec2::new(2, 2).with_z(2),
        };
        let contains_column = |wpos2d: Vec2<i32>| synthetic_plug_column(center, 0, 1, wpos2d);
        // `None` (unloaded/unknown) must never be mistaken for "already
        // clear" -- the real write must be attempted whenever this can't be
        // confirmed, never skipped by assuming the best case.
        assert!(!plug_already_clear_via(aabb, contains_column, |_| None));
    }

    #[test]
    fn plug_already_clear_is_vacuously_true_when_the_shape_test_never_matches() {
        let aabb = Aabb {
            min: Vec2::new(0, 0).with_z(0),
            max: Vec2::new(5, 5).with_z(5),
        };
        assert!(plug_already_clear_via(
            aabb,
            |_| None,
            |_| panic!("get_block must never be called when no column is in-shape")
        ));
    }

    /// `plug_already_clear` takes `&UndercompactGateAntechamberGeometry`
    /// (crate-private to `world`, unconstructable from a plain test), so
    /// this mirrors its exact scan logic against the same generic shape/read
    /// closures the tests above already use, keeping the two in lockstep by
    /// inlining the identical loop rather than re-deriving it differently.
    fn plug_already_clear_via(
        aabb: Aabb<i32>,
        contains_column: impl Fn(Vec2<i32>) -> Option<(i32, i32)>,
        get_block: impl Fn(Vec3<i32>) -> Option<Block>,
    ) -> bool {
        for x in aabb.min.x..aabb.max.x {
            for y in aabb.min.y..aabb.max.y {
                let wpos2d = Vec2::new(x, y);
                if let Some((floor_z, _)) = contains_column(wpos2d) {
                    return get_block(wpos2d.with_z(floor_z)) == Some(Block::empty());
                }
            }
        }
        true
    }

    /// COW-7b review finding (ECS + perf, independently): the restart-
    /// recovery check must never resolve the (potentially expensive, and
    /// definitely non-free) antechamber geometry when there are no newly-
    /// loaded chunks this tick -- which is every tick, forever, in the
    /// common case. Proven here without a real `World`/`Index` at all, via
    /// the injectable-resolver seam
    /// (`ensure_undercompact_gate_restart_recovery_with`): the resolver
    /// closure increments a counter and would panic if actually called with
    /// a nonsensical `None` in a way this test can tell apart from "never
    /// called".
    #[test]
    fn geometry_is_not_resolved_when_there_are_no_newly_loaded_chunks() {
        let mut state = setup();
        // Deliberately no `TerrainChanges::new_chunks` entry inserted --
        // this is the state on almost every real tick.
        let mut resolver_calls = 0u32;

        let cleared = ensure_undercompact_gate_restart_recovery_with(&mut state, || {
            resolver_calls += 1;
            None
        });

        assert!(!cleared);
        assert_eq!(
            resolver_calls, 0,
            "the geometry resolver must not run at all when new_chunks is empty"
        );
    }

    /// Same seam, opposite case: once a relevant chunk *has* just loaded,
    /// the resolver must actually run (exactly once per call).
    #[test]
    fn geometry_is_resolved_exactly_once_when_a_chunk_just_loaded() {
        let mut state = setup();
        state
            .ecs_mut()
            .write_resource::<common_state::TerrainChanges>()
            .new_chunks
            .insert(Vec2::new(0, 0));
        let mut resolver_calls = 0u32;

        let cleared = ensure_undercompact_gate_restart_recovery_with(&mut state, || {
            resolver_calls += 1;
            None
        });

        assert!(!cleared, "a `None` geometry must still report no clear");
        assert_eq!(resolver_calls, 1);
    }

    // ---- Heavy, real-terrain-backed tests: require the real Cromatolis LFS
    // assets pulled locally, same precedent as `gate_checkpoint.rs`'s own
    // `..._against_the_real_world` tests. Not run automatically.
    // Recommended: `cargo test -p xindeler-server -- --ignored undercompact_gate`
    // ----

    /// Loads the chunks under every corner (plus the center) of `aabb`'s 2D
    /// footprint -- the plug can in principle straddle a chunk boundary
    /// (see `plug_chunk_keys`), so a test that only loads the center's own
    /// chunk could leave `plug_already_clear`'s scan (which starts at
    /// `aabb.min`, not the center) reading unloaded terrain.
    fn load_chunks_covering(state: &mut State, aabb: Aabb<i32>) {
        let corners = [
            Vec2::new(aabb.min.x, aabb.min.y),
            Vec2::new(aabb.max.x, aabb.min.y),
            Vec2::new(aabb.min.x, aabb.max.y),
            Vec2::new(aabb.max.x, aabb.max.y),
            aabb.center().xy(),
        ];
        for corner in corners {
            load_chunk_containing(state, corner.map(|e| e as f32).with_z(0.0));
        }
    }

    /// The restart-recovery check must do nothing while the persisted puzzle
    /// state is unsolved, even once the antechamber's chunk is live -- a
    /// fresh save must never spuriously clear the plug.
    #[test]
    #[ignore]
    fn restart_recovery_does_nothing_while_unsolved() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = World::generate(
            0,
            world::sim::WorldOpts {
                seed_elements: true,
                world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let geometry = cromatolis_interior::undercompact_gate_antechamber_world_geometry(
            index.as_index_ref(),
            world.sim(),
        )
        .expect("the real export must carry the antechamber");

        let mut state = setup();
        insert_rtsim(&mut state, &world, &index);
        load_chunks_covering(&mut state, geometry.plug_aabb);
        // `load_chunk_containing` inserts directly into `TerrainGrid`, which
        // does not itself register a `TerrainChanges::new_chunks` entry --
        // mirror the real chunk-insertion path (`server/src/sys/terrain.rs`)
        // by recording it by hand for this test.
        let plug_center = geometry.plug_aabb.center().map(|e| e as f32);
        let key = state.terrain().pos_key(plug_center.map(|e| e as i32));
        state
            .ecs_mut()
            .write_resource::<common_state::TerrainChanges>()
            .new_chunks
            .insert(key);

        assert!(!ensure_undercompact_gate_restart_recovery(
            &mut state,
            &world,
            index.as_index_ref()
        ));
    }

    /// The core restart-recovery guarantee: a save that was already solved
    /// before a restart must have its plug cleared the moment its chunk
    /// becomes live again, with no perpetual re-checking needed afterward.
    #[test]
    #[ignore]
    fn restart_recovery_clears_the_plug_once_when_already_solved() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = World::generate(
            0,
            world::sim::WorldOpts {
                seed_elements: true,
                world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let geometry = cromatolis_interior::undercompact_gate_antechamber_world_geometry(
            index.as_index_ref(),
            world.sim(),
        )
        .expect("the real export must carry the antechamber");

        let mut state = setup();
        insert_rtsim(&mut state, &world, &index);
        load_chunks_covering(&mut state, geometry.plug_aabb);
        let plug_center = geometry.plug_aabb.center().map(|e| e as f32);
        let key = state.terrain().pos_key(plug_center.map(|e| e as i32));
        state
            .ecs_mut()
            .write_resource::<common_state::TerrainChanges>()
            .new_chunks
            .insert(key);

        // Pre-fill the plug's sample column with solid rock, as world-gen
        // would have, so clearing it is actually observable.
        let sample = plug_center.map(|e| e as i32);
        state.set_block(
            sample,
            common::terrain::Block::new(BlockKind::Rock, vek::Rgb::new(60, 55, 60)),
        );
        state.apply_terrain_changes(|_, _| {});

        state
            .ecs()
            .read_resource::<RtSim>()
            .with_undercompact_gate(|levers| {
                levers.activate(rtsim::data::UndercompactGateLever::A);
                levers.activate(rtsim::data::UndercompactGateLever::B);
            });

        assert!(ensure_undercompact_gate_restart_recovery(
            &mut state,
            &world,
            index.as_index_ref()
        ));
        state.apply_terrain_changes(|_, _| {});
        assert_eq!(
            state.get_block(sample),
            Some(Block::empty()),
            "the plug must be cleared once the puzzle's persisted state says solved"
        );

        // A second call, on a later tick (`TerrainChanges::new_chunks` reset
        // by `State::cleanup`, exactly as `Server::tick` does every tick),
        // must not panic or do anything further -- there is nothing left
        // for it to reconcile once the chunk has already been checked once.
        state.cleanup();
        assert!(!ensure_undercompact_gate_restart_recovery(
            &mut state,
            &world,
            index.as_index_ref()
        ));
    }

    /// COW-7b review finding (perf): once the plug has already been cleared
    /// (e.g. terrain persistence already restored the cleared state before
    /// this check runs, or a previous call already cleared it), a later
    /// call for the same already-loaded chunk must not rewrite the plug
    /// region again -- no redundant `BlockChange`/network diff.
    #[test]
    #[ignore]
    fn restart_recovery_does_not_rewrite_a_plug_that_is_already_clear() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = World::generate(
            0,
            world::sim::WorldOpts {
                seed_elements: true,
                world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let geometry = cromatolis_interior::undercompact_gate_antechamber_world_geometry(
            index.as_index_ref(),
            world.sim(),
        )
        .expect("the real export must carry the antechamber");

        let mut state = setup();
        insert_rtsim(&mut state, &world, &index);
        load_chunks_covering(&mut state, geometry.plug_aabb);
        let plug_center = geometry.plug_aabb.center().map(|e| e as f32);
        let key = state.terrain().pos_key(plug_center.map(|e| e as i32));
        state
            .ecs_mut()
            .write_resource::<common_state::TerrainChanges>()
            .new_chunks
            .insert(key);

        // Explicitly clear the *whole* plug region first -- unlike the
        // sibling test above, which pre-fills solid rock at just the
        // sample column. `plug_already_clear`'s scan starts at the AABB's
        // corner, not its center, so only clearing the center (as
        // `restart_recovery_clears_the_plug_once_when_already_solved` reads
        // back) would not actually make the region "already clear" from
        // that scan's point of view. `TerrainChunk::water`'s own default
        // fill is also *not* air this deep underground (it is below that
        // constructor's sea level), so this can't be left to a default.
        clear_gate_plug(&geometry, |pos, block| state.set_block(pos, block));
        state.apply_terrain_changes(|_, _| {});

        state
            .ecs()
            .read_resource::<RtSim>()
            .with_undercompact_gate(|levers| {
                levers.activate(rtsim::data::UndercompactGateLever::A);
                levers.activate(rtsim::data::UndercompactGateLever::B);
            });

        assert!(!ensure_undercompact_gate_restart_recovery(
            &mut state,
            &world,
            index.as_index_ref()
        ));
    }
}
