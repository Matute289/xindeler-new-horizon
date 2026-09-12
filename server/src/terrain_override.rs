//! The Regional Terrain Event Engine's server-side activate/deactivate flow.
//!
//! [`apply`] is the plain, directly-callable core that
//! `server/src/events/terrain_override.rs`'s `ServerEvent` handler and the
//! `/terrain_override` admin command (`server/src/cmd.rs`) both funnel
//! through. `worldgen`-feature-gated: without it there is no real
//! `world::World` to regenerate chunks against (the `test_world` fallback
//! `World` never reads a regional terrain override at all), so there is
//! nothing meaningful for this to do.

#[cfg(feature = "worldgen")]
mod worldgen_impl {
    use std::sync::Arc;

    use common::{
        calendar::Calendar,
        comp::{Pos, Presence},
        event::TerrainOverrideOp,
        resources::TimeOfDay,
        slowjob::SlowJobPool,
        spiral::Spiral2d,
        terrain::{OverrideRegion, TerrainChunkSize, TerrainGrid, TerrainOverrides},
        vol::RectVolSize,
    };
    use common_state::TerrainChanges;
    use specs::{Entities, Join, ReadStorage, WriteStorage};
    use vek::Vec2;
    use world::{IndexOwned, World};

    use crate::{chunk_generator::ChunkGenerator, presence::RepositionToFreeSpace, rtsim::RtSim};

    #[cfg(feature = "persistent_world")]
    use crate::terrain_persistence::TerrainPersistence;

    #[cfg(feature = "persistent_world")]
    type TerrainPersistenceRef<'a> = Option<&'a mut TerrainPersistence>;
    #[cfg(not(feature = "persistent_world"))]
    type TerrainPersistenceRef<'a> = ();

    /// Everything [`apply`] needs, bundled so its own signature (and its
    /// callers') stay manageable. Mirrors the shape of a `System`'s
    /// `SystemData` tuple -- every field here is exactly what
    /// `server/src/events/terrain_override.rs`'s `ServerEvent::SystemData`
    /// fetches, just named for readability at the call site.
    pub struct ApplyContext<'a> {
        pub terrain_overrides: &'a mut Arc<TerrainOverrides>,
        pub terrain: &'a mut TerrainGrid,
        pub terrain_changes: &'a mut TerrainChanges,
        pub chunk_generator: &'a mut ChunkGenerator,
        pub slow_jobs: &'a SlowJobPool,
        pub world: &'a Arc<World>,
        pub index: &'a IndexOwned,
        pub time_of_day: TimeOfDay,
        pub calendar: &'a Calendar,
        pub rtsim: &'a RtSim,
        pub terrain_persistence: TerrainPersistenceRef<'a>,
        pub entities: Entities<'a>,
        pub positions: ReadStorage<'a, Pos>,
        pub presences: ReadStorage<'a, Presence>,
        pub reposition: WriteStorage<'a, RepositionToFreeSpace>,
    }

    /// Activates or deactivates one regional terrain override: bumps
    /// [`TerrainOverrides::version`] exactly once, updates the live ECS
    /// snapshot and (for non-`ephemeral` overrides) the persisted rtsim
    /// mirror, then regenerates every affected chunk.
    pub fn apply(op: TerrainOverrideOp, ctx: &mut ApplyContext) {
        match op {
            TerrainOverrideOp::Activate(new_override) => {
                let region = new_override.region.clone();
                let wipe_player_edits = new_override.wipe_player_edits;

                let mut snapshot = (**ctx.terrain_overrides).clone();
                snapshot.version = snapshot.version.wrapping_add(1);
                snapshot.active.push(new_override);
                persist_and_install(snapshot, ctx);

                regenerate_region(&region, wipe_player_edits, ctx);
            },
            TerrainOverrideOp::Deactivate(id) => {
                let mut snapshot = (**ctx.terrain_overrides).clone();
                let Some(index) = snapshot.active.iter().position(|o| o.id == id) else {
                    // Already inactive (or never existed) -- a harmless
                    // no-op, not an error: the caller asked for "this
                    // override is not active" to be true, and it already
                    // is.
                    return;
                };
                let removed = snapshot.active.remove(index);
                snapshot.version = snapshot.version.wrapping_add(1);
                persist_and_install(snapshot, ctx);

                // Deactivation never wipes player edits, even if the
                // override activated with `wipe_player_edits: true` -- that
                // flag describes what should happen when the event STARTS
                // (e.g. a crater clearing whatever was built there), not
                // when it ENDS. A curse ending, or a weather event passing,
                // should not retroactively discard whatever players built
                // *during* it.
                regenerate_region(&removed.region, false, ctx);
            },
        }
    }

    /// Replaces the live ECS `Arc<TerrainOverrides>` with `snapshot`, and
    /// mirrors its non-`ephemeral` overrides into the persisted rtsim
    /// registry (see `rtsim::data::Data::terrain_overrides`) under the same
    /// version number, so a save/reload comes back with the same active set
    /// an ephemeral-free equivalent of this snapshot would have had.
    fn persist_and_install(snapshot: TerrainOverrides, ctx: &mut ApplyContext) {
        ctx.rtsim.with_terrain_overrides(|persisted| {
            persisted.version = snapshot.version;
            persisted.active = snapshot
                .active
                .iter()
                .filter(|o| !o.ephemeral)
                .cloned()
                .collect();
        });
        *ctx.terrain_overrides = Arc::new(snapshot);
    }

    /// Unloads every chunk `region` touches (even a sliver of falloff --
    /// same `OverrideRegion::touches_chunk` AABB-then-exact check the
    /// world-gen hooks use to decide whether to patch a chunk at all) so
    /// the next generation of each one honors the new override state, then:
    /// - if `wipe_player_edits`, clears that chunk's persisted terrain diff
    ///   first (gated behind the `persistent_world` feature AND the
    ///   `TerrainPersistence` resource actually being present -- it's only
    ///   inserted when `experimental_terrain_persistence` is set, so its
    ///   absence already means "can't wipe", matching `server/src/cmd.rs`'s own
    ///   `handle_clear_persisted_terrain`/ `reload_chunks_inner` gating
    ///   exactly).
    /// - repositions any player currently standing in that chunk
    ///   (`RepositionToFreeSpace`; its own consumer in
    ///   `server::sys::terrain::Sys` already waits for the chunk to be loaded
    ///   again before acting, so attaching it now -- while the chunk is
    ///   momentarily gone -- is safe).
    /// - proactively re-enqueues generation for the chunk if it's within ANY
    ///   connected player's current view distance (the client never re-requests
    ///   a chunk it already holds, and the server's own
    ///   auto-request-missing-chunks logic is bounded to a small minimum radius
    ///   -- see `server/src/sys/msg/terrain.rs` -- so without this, a
    ///   distant-but-in-view override would never visibly regenerate). Chunks
    ///   outside every player's view distance are left to regenerate lazily on
    ///   demand, same as any other unloaded chunk.
    fn regenerate_region(region: &OverrideRegion, wipe_player_edits: bool, ctx: &mut ApplyContext) {
        let chunk_size = TerrainChunkSize::RECT_SIZE;
        let bounds = region.bounds();
        let center_chunk = bounds
            .center()
            .map2(chunk_size, |e, sz: u32| e.div_euclid(sz as i32));
        let half_extent = (bounds.max - bounds.min) / 2;
        let radius_chunks = (half_extent.x.max(half_extent.y) / chunk_size.x as i32).max(0) + 2;

        // Every connected player's entity, chunk position, and current view
        // distance -- computed once, outside the per-chunk loop below, via
        // a single ECS join. Reused inside the loop for BOTH the
        // reposition check (is a player standing in this chunk?) and the
        // view-distance check (is this chunk in anyone's view?) in one
        // pass over this small `Vec`, rather than re-joining
        // `(&ctx.entities, &ctx.positions)` from scratch for every touched
        // chunk.
        let player_chunks: Vec<(specs::Entity, Vec2<i32>, u32)> =
            (&ctx.entities, &ctx.positions, &ctx.presences)
                .join()
                .map(|(entity, pos, presence)| {
                    (
                        entity,
                        TerrainGrid::chunk_key(pos.0.xy().as_::<i32>()),
                        presence.terrain_view_distance.current(),
                    )
                })
                .collect();

        for offset in Spiral2d::with_radius(radius_chunks) {
            let key = center_chunk + offset;
            if !region.touches_chunk(key, chunk_size) {
                continue;
            }

            // Scoped invalidation: only chunks THIS region actually touches
            // get their generation-job epoch bumped -- see
            // `ChunkGenerator::chunk_versions`'s own doc comment for why
            // this must never be a global counter.
            ctx.chunk_generator.invalidate_chunk(key);

            #[cfg(feature = "persistent_world")]
            if wipe_player_edits && let Some(persistence) = ctx.terrain_persistence.as_mut() {
                persistence.clear_chunk(key);
            }
            #[cfg(not(feature = "persistent_world"))]
            let _ = wipe_player_edits;

            remove_chunk(ctx.terrain, ctx.terrain_changes, key);

            let mut within_any_players_vd = false;
            for (entity, player_chunk, vd) in &player_chunks {
                if *player_chunk == key {
                    let _ = ctx.reposition.insert(*entity, RepositionToFreeSpace {
                        needs_ground: true,
                        modify_waypoints: false,
                    });
                }
                if (key - player_chunk).map(|e| e.unsigned_abs()).reduce_max() <= *vd {
                    within_any_players_vd = true;
                }
            }
            if within_any_players_vd {
                ctx.chunk_generator.generate_chunk(
                    None,
                    key,
                    ctx.slow_jobs,
                    Arc::clone(ctx.world),
                    ctx.rtsim,
                    ctx.index.clone(),
                    (ctx.time_of_day, ctx.calendar.clone()),
                    Some(Arc::clone(ctx.terrain_overrides)),
                );
            }
        }
    }

    /// Matches `common_state::State::remove_chunk` exactly -- reimplemented
    /// here (rather than called through `&mut State`) because
    /// `ApplyContext` only has the individual ECS resources a
    /// `ServerEvent`'s `SystemData` fetched, not a whole `&mut State`.
    fn remove_chunk(
        terrain: &mut TerrainGrid,
        terrain_changes: &mut TerrainChanges,
        key: Vec2<i32>,
    ) {
        if terrain.remove(key).is_some() {
            terrain_changes.removed_chunks.insert(key);
        }
    }
}

#[cfg(feature = "worldgen")]
pub use worldgen_impl::{ApplyContext, apply};

// ---- Heavy, real-terrain-backed test: requires the real Cromatolis LFS
// assets pulled locally, same precedent as `undercompact_gate.rs`'s own
// `..._against_the_real_world` tests. Not run automatically.
// Recommended: `cargo test -p xindeler-server -- --ignored terrain_override`
// ----
#[cfg(all(test, feature = "worldgen"))]
mod tests {
    use std::sync::Arc;

    use common::{
        ViewDistances,
        calendar::Calendar,
        comp::{Pos, Presence, PresenceKind},
        event::{SetRegionalTerrainOverrideEvent, TerrainOverrideOp},
        resources::TimeOfDay,
        slowjob::SlowJobPool,
        terrain::{
            ClimateOverride, ClimateValue, OverrideRegion, RegionalTerrainOverride,
            TerrainChunkSize, TerrainOverrideId, TerrainOverridePayload, TerrainOverrides,
        },
        vol::RectVolSize,
    };
    use prometheus::Registry;
    use specs::{Builder, WorldExt};
    use world::{
        World,
        sim::{FileOpts, WorldOpts},
        util::Sampler,
    };

    use crate::{
        chunk_generator::ChunkGenerator,
        events::ServerEvent,
        metrics::ChunkGenMetrics,
        presence::RepositionToFreeSpace,
        undercompact_gate::test_support::{insert_rtsim, setup},
    };

    /// A world-position near the real map export's own center -- virtually
    /// guaranteed to have real generated terrain, without depending on any
    /// method (`get_center`) that only exists on the non-worldgen test-only
    /// `World` stub.
    fn near_map_center(world: &World) -> vek::Vec2<i32> {
        let center_chunk = (world.sim().get_size() / 2).as_::<i32>();
        center_chunk * TerrainChunkSize::RECT_SIZE.as_::<i32>()
            + (TerrainChunkSize::RECT_SIZE / 2).as_::<i32>()
    }

    fn fire(state: &common_state::State, op: TerrainOverrideOp) {
        let ecs = state.ecs();
        let data =
            ecs.system_data::<<SetRegionalTerrainOverrideEvent as ServerEvent>::SystemData<'_>>();
        <SetRegionalTerrainOverrideEvent as ServerEvent>::handle(
            std::iter::once(SetRegionalTerrainOverrideEvent { op }),
            data,
        );
    }

    fn snow_override(center: vek::Vec2<i32>, radius: f32) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId::new_unique(),
            region: OverrideRegion::Circle {
                center,
                radius,
                edge: 8.0,
            },
            payload: TerrainOverridePayload::Climate(ClimateOverride {
                temp: Some(ClimateValue::Set(-10.0)),
                humidity: Some(ClimateValue::Set(0.9)),
                tree_density_mul: Some(0.1),
            }),
            priority: 100,
            activated_at: 0.0,
            // Not ephemeral, so the rtsim-persistence half of `apply` is
            // exercised too.
            wipe_player_edits: false,
            ephemeral: false,
        }
    }

    /// End-to-end: activating a climate override removes every chunk it
    /// touches from `TerrainGrid` (never regenerates one still resident --
    /// see this module's own top-level doc comment and constraint #1 in the
    /// implementation notes) and proactively re-enqueues generation for any
    /// chunk within a connected player's view distance; deactivating does
    /// the same, without wiping. Both bump `TerrainOverrides::version`
    /// exactly once and mirror into the persisted rtsim registry.
    #[test]
    #[ignore]
    fn activating_and_deactivating_unloads_and_reenqueues_in_range_chunks() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = World::generate(
            0,
            WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let world = Arc::new(world);

        let mut state = setup();
        insert_rtsim(&mut state, &world, &index);
        state.ecs_mut().register::<Pos>();
        state.ecs_mut().register::<Presence>();
        state.ecs_mut().register::<RepositionToFreeSpace>();

        let slow_jobs = SlowJobPool::new(4, 8, Arc::new(threadpool));
        slow_jobs.configure("CHUNK_GENERATOR", |n| n.max(1));
        state.ecs_mut().insert(slow_jobs);
        state.ecs_mut().insert(ChunkGenerator::new(
            ChunkGenMetrics::new(&Registry::new()).unwrap(),
        ));
        state.ecs_mut().insert(Arc::clone(&world));
        state.ecs_mut().insert(index.clone());
        state.ecs_mut().insert(TimeOfDay(0.0));
        state.ecs_mut().insert(Calendar::default());
        state
            .ecs_mut()
            .insert(Arc::new(TerrainOverrides::default()));

        let center_wpos = near_map_center(&world);
        let key = state.terrain().pos_key(center_wpos.with_z(0));

        // A resident chunk (so removal is observable) with a player
        // standing inside it (so reposition + proactive regen both fire).
        state
            .ecs_mut()
            .write_resource::<common::terrain::TerrainGrid>()
            .insert(key, Arc::new(common::terrain::TerrainChunk::water(0)));
        let player = state
            .ecs_mut()
            .create_entity()
            .with(Pos(center_wpos.as_::<f32>().with_z(0.0)))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .build();

        let new_override = snow_override(center_wpos, 64.0);
        let id = new_override.id;

        fire(&state, TerrainOverrideOp::Activate(new_override));

        let overrides_after_activate =
            Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert_eq!(overrides_after_activate.version, 1);
        assert_eq!(overrides_after_activate.active.len(), 1);
        assert_eq!(
            state
                .ecs()
                .read_resource::<crate::rtsim::RtSim>()
                .with_terrain_overrides(|o| o.active.len()),
            1,
            "a non-ephemeral override must be mirrored into the persisted rtsim registry"
        );
        assert!(
            state.terrain().get_key(key).is_none(),
            "the chunk must be unloaded, never regenerated in place"
        );
        assert!(
            state
                .ecs()
                .write_resource::<ChunkGenerator>()
                .pending_chunks()
                .any(|pending| pending == key),
            "an in-range chunk must be proactively re-enqueued for regeneration"
        );
        assert!(
            state
                .ecs()
                .read_storage::<RepositionToFreeSpace>()
                .get(player)
                .is_some(),
            "a player standing in the unloaded chunk must be queued for repositioning"
        );

        fire(&state, TerrainOverrideOp::Deactivate(id));

        let overrides_after_deactivate =
            Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert_eq!(overrides_after_deactivate.version, 2);
        assert!(overrides_after_deactivate.active.is_empty());
        assert_eq!(
            state
                .ecs()
                .read_resource::<crate::rtsim::RtSim>()
                .with_terrain_overrides(|o| o.active.len()),
            0
        );
        assert!(
            state
                .ecs()
                .write_resource::<ChunkGenerator>()
                .pending_chunks()
                .any(|pending| pending == key),
            "deactivation must also re-enqueue the chunk so it regenerates back to ambient"
        );

        // Deactivating a not-currently-active id (double-deactivate) must be
        // a harmless no-op, not a panic or a spurious extra version bump.
        fire(&state, TerrainOverrideOp::Deactivate(id));
        let overrides_after_second_deactivate =
            Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert_eq!(overrides_after_second_deactivate.version, 2);
    }

    /// The column-level (per-block, radially-blended) half of the mechanism:
    /// `ColumnGen::with_overrides` must read a materially different
    /// temperature/humidity at the override's center than the same position
    /// with no override applied. This is the concrete, testable proxy for
    /// "surface terrain (e.g. snow cover) visibly changes" -- actually
    /// eyeballing a snow-covered chunk in a live client is a manual-QA
    /// follow-up, not something this test claims to cover.
    #[test]
    #[ignore]
    fn column_level_climate_blend_changes_temp_and_humidity_at_the_override_center() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = World::generate(
            0,
            WorldOpts {
                seed_elements: true,
                world_file: FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );

        let center_wpos = near_map_center(&world);
        let ambient = world
            .sample_blocks()
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("a real map export must have terrain at its own center");

        let overrides = TerrainOverrides {
            version: 1,
            active: vec![snow_override(center_wpos, 64.0)],
        };
        let overridden = world
            .sample_blocks_with_overrides(Some(&overrides))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("the override must not change whether this column generates at all");

        assert!(
            overridden.temp < ambient.temp,
            "a -10C `Set` climate override at full blend strength must read colder than ambient \
             (ambient={}, overridden={})",
            ambient.temp,
            overridden.temp
        );
        assert!(overridden.humidity > ambient.humidity || ambient.humidity >= 0.9);
    }
}
