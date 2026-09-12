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
        terrain::{
            OverrideRegion, RegionalTerrainOverride, TerrainChunkSize, TerrainGrid,
            TerrainOverridePayload, TerrainOverrides,
        },
        vol::RectVolSize,
    };
    use common_state::TerrainChanges;
    use specs::{Entities, Entity as EcsEntity, Join, ReadStorage, WriteStorage};
    use vek::{Aabr, Vec2, Vec3};
    use world::{IndexOwned, World, util::Sampler};

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
                let new_override = bake_biome_profile_flood_to(new_override, ctx);
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
            TerrainOverrideOp::Replace(new_override) => {
                let mut snapshot = (**ctx.terrain_overrides).clone();
                let Some(index) = snapshot.active.iter().position(|o| o.id == new_override.id)
                else {
                    // No active override to replace -- same "already the
                    // state the caller wanted" no-op as `Deactivate`'s
                    // not-found case, e.g. the healing scheduler racing a
                    // manual `/terrain_override` clear.
                    return;
                };
                let old_region = snapshot.active[index].region.clone();
                let new_region = new_override.region.clone();
                snapshot.active[index] = new_override;
                snapshot.version = snapshot.version.wrapping_add(1);
                persist_and_install(snapshot, ctx);

                // Regenerate the UNION of the old and new bounds exactly
                // once -- a deactivate-then-activate pair would double the
                // chunk-regen work and bump `version` twice for what is
                // conceptually a single change (e.g. one healing step).
                // Never wipes player edits: a `Replace` describes an
                // override's state evolving over its own lifetime (healing
                // progress advancing), not a fresh activation.
                regenerate_regions_union(&old_region, &new_region, ctx);
            },
        }
    }

    /// If `new_override`'s payload is a `BiomeProfile` override referencing
    /// a catalog entry with an authored `flood_depth`, bakes that RELATIVE
    /// depth into an ABSOLUTE `flood_to` altitude (ambient ground altitude
    /// plus `flood_depth`) by sampling the region's own ambient ground
    /// altitude at its center -- see
    /// `common::terrain::regional_override::BiomeProfileOverride::flood_to`'s
    /// own doc comment for why this must happen once, HERE, at activation
    /// time, rather than in world-gen (which would have to keep re-sampling
    /// ambient altitude on every chunk regeneration, letting the flood level
    /// silently drift as terrain around it changes). Always overwrites
    /// whatever `flood_to` the caller supplied -- callers should just leave
    /// it `None` and let this compute the real value.
    ///
    /// Leaves `flood_to` as `None` (no flood) if: the payload isn't
    /// `BiomeProfile`; the profile id doesn't resolve against the catalog;
    /// the catalog entry has no `flood_depth`; the region isn't a `Circle`
    /// (future region shapes this crate doesn't know how to find a center
    /// for); or ambient world-gen has no column at that center at all.
    fn bake_biome_profile_flood_to(
        mut new_override: RegionalTerrainOverride,
        ctx: &ApplyContext,
    ) -> RegionalTerrainOverride {
        let TerrainOverridePayload::BiomeProfile(profile_override) = &mut new_override.payload
        else {
            return new_override;
        };

        // `OverrideRegion` is `#[non_exhaustive]`, so this match needs a
        // wildcard arm even though `Circle` is the only variant that exists
        // today -- a future region shape simply can't be flood-baked yet.
        let center = match &new_override.region {
            OverrideRegion::Circle { center, .. } => *center,
            _ => {
                profile_override.flood_to = None;
                return new_override;
            },
        };

        let flood_depth = ctx
            .index
            .biome_profiles()
            .entries
            .iter()
            .find(|profile| profile.id == profile_override.profile)
            .and_then(|profile| profile.flood_depth);

        profile_override.flood_to = flood_depth.and_then(|depth| {
            ctx.world
                .sample_blocks()
                .column_gen
                .get((center, ctx.index.as_index_ref(), None))
                .map(|sample| sample.alt + depth)
        });

        new_override
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
        regenerate_chunks(
            region.bounds(),
            |key| region.touches_chunk(key, TerrainChunkSize::RECT_SIZE),
            wipe_player_edits,
            ctx,
        );
    }

    /// Same as [`regenerate_region`], but for the UNION of two regions' bounds
    /// (a chunk is regenerated if EITHER region touches it) -- used by
    /// [`apply`]'s `Replace` handling so an override's region/payload
    /// changing in place regenerates every affected chunk exactly once,
    /// never via two separate `regenerate_region` calls (which would
    /// double-process any chunk both regions touch, and bump `version`
    /// twice).
    fn regenerate_regions_union(a: &OverrideRegion, b: &OverrideRegion, ctx: &mut ApplyContext) {
        let chunk_size = TerrainChunkSize::RECT_SIZE;
        let bounds_a = a.bounds();
        let bounds_b = b.bounds();
        let union = Aabr {
            min: bounds_a.min.map2(bounds_b.min, |x, y| x.min(y)),
            max: bounds_a.max.map2(bounds_b.max, |x, y| x.max(y)),
        };
        regenerate_chunks(
            union,
            |key| a.touches_chunk(key, chunk_size) || b.touches_chunk(key, chunk_size),
            false,
            ctx,
        );
    }

    /// Every connected player's entity, world-space position, current chunk
    /// key, and current terrain view distance -- one ECS join, shared by
    /// [`regenerate_chunks`]'s per-chunk reposition/view-distance checks and
    /// `server::sys::terrain_damage_heal::Sys`'s "is anyone currently
    /// standing in this override's region" check, so the underlying
    /// `(Entities, Pos, Presence)` join is only ever written once.
    pub fn joined_player_positions(
        entities: &Entities<'_>,
        positions: &ReadStorage<'_, Pos>,
        presences: &ReadStorage<'_, Presence>,
    ) -> Vec<(EcsEntity, Vec3<f32>, Vec2<i32>, u32)> {
        (entities, positions, presences)
            .join()
            .map(|(entity, pos, presence)| {
                (
                    entity,
                    pos.0,
                    TerrainGrid::chunk_key(pos.0.xy().as_::<i32>()),
                    presence.terrain_view_distance.current(),
                )
            })
            .collect()
    }

    /// The shared core of [`regenerate_region`]/[`regenerate_regions_union`]:
    /// unloads every chunk within `bounds` for which `touches(key)` is true
    /// so the next generation of each one honors the new override state, and
    /// (per-chunk) clears persisted edits / repositions players / proactively
    /// re-enqueues generation -- see [`regenerate_region`]'s own doc comment
    /// for the exact behavior, which applies here unchanged.
    fn regenerate_chunks(
        bounds: Aabr<i32>,
        touches: impl Fn(Vec2<i32>) -> bool,
        wipe_player_edits: bool,
        ctx: &mut ApplyContext,
    ) {
        let chunk_size = TerrainChunkSize::RECT_SIZE;
        let center_chunk = bounds
            .center()
            .map2(chunk_size, |e, sz: u32| e.div_euclid(sz as i32));
        let half_extent = (bounds.max - bounds.min) / 2;
        let radius_chunks = (half_extent.x.max(half_extent.y) / chunk_size.x as i32).max(0) + 2;

        // See `joined_player_positions`'s own doc comment -- computed once,
        // outside the per-chunk loop below, and reused inside it for BOTH
        // the reposition check (is a player standing in this chunk?) and
        // the view-distance check (is this chunk in anyone's view?).
        let player_chunks: Vec<(EcsEntity, Vec2<i32>, u32)> =
            joined_player_positions(&ctx.entities, &ctx.positions, &ctx.presences)
                .into_iter()
                .map(|(entity, _pos, chunk_key, vd)| (entity, chunk_key, vd))
                .collect();

        for offset in Spiral2d::with_radius(radius_chunks) {
            let key = center_chunk + offset;
            if !touches(key) {
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
pub use worldgen_impl::{ApplyContext, apply, joined_player_positions};

// ---- Heavy, real-terrain-backed test: requires the real Cromatolis LFS
// assets pulled locally, same precedent as `undercompact_gate.rs`'s own
// `..._against_the_real_world` tests. Not run automatically.
// Recommended: `cargo test -p xindeler-server -- --ignored terrain_override`
// ----
#[cfg(all(test, feature = "worldgen"))]
mod tests {
    use std::sync::Arc;

    use std::time::Duration;

    use common::{
        ViewDistances,
        calendar::Calendar,
        comp::{Pos, Presence, PresenceKind},
        event::{EventBus, SetRegionalTerrainOverrideEvent, TerrainOverrideOp},
        resources::TimeOfDay,
        slowjob::SlowJobPool,
        terrain::{
            ClimateOverride, ClimateValue, DamageOverride, DamageShape, OverrideRegion,
            RegionalTerrainOverride, TerrainChunkSize, TerrainOverrideId, TerrainOverridePayload,
            TerrainOverrides,
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
        sys::{SysScheduler, terrain_damage_heal},
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

    fn crater_override(
        center: vek::Vec2<i32>,
        radius: f32,
        heal_stages: u8,
        heal_interval: f64,
    ) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId::new_unique(),
            region: OverrideRegion::Circle {
                center,
                radius,
                edge: 8.0,
            },
            payload: TerrainOverridePayload::Damage(DamageOverride {
                shapes: vec![DamageShape::Crater {
                    max_depth: 20.0,
                    rim_height: 2.0,
                }],
                scorch: 0.6,
                vegetation_mul: 0.1,
                heal_progress: 0.0,
                heal_stages,
                heal_interval,
                next_heal_at: heal_interval,
            }),
            priority: 100,
            activated_at: 0.0,
            wipe_player_edits: true,
            ephemeral: false,
        }
    }

    /// End-to-end across BOTH the crater and healing halves of the
    /// terrain-damage payload: activating a `Damage::Crater` override must
    /// read a materially LOWER altitude at its own center than the same
    /// position with no override applied (the column-level bowl depression
    /// -- see `world/src/column.rs`'s crater hook). Driving
    /// `server::sys::terrain_damage_heal::Sys` forward through every stage
    /// (each due, unoccupied step advancing `heal_progress` by
    /// `1 / heal_stages` via a `Replace` event) must progressively raise it
    /// and, on the final stage, deactivate the override outright -- at
    /// which point the column reads EXACTLY the ambient altitude again (a
    /// fully healed `Damage` override contributes a bit-exact zero
    /// depth/rim, see `DamageOverride::effects_at`), and every healing step
    /// along the way unloads the touched chunk rather than regenerating it
    /// in place -- the same no-duplication invariant already established
    /// for plain activate/deactivate above.
    #[test]
    #[ignore]
    fn activating_a_crater_lowers_terrain_and_healing_progressively_restores_it() {
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
        // `terrain_damage_heal::Sys`'s own `SystemData` -- not fetched by
        // `fire()`'s direct `ServerEvent::handle` call above, so not
        // otherwise present in this test's `State`.
        state
            .ecs_mut()
            .insert(EventBus::<SetRegionalTerrainOverrideEvent>::default());
        state
            .ecs_mut()
            .insert(SysScheduler::<terrain_damage_heal::Sys>::every(
                Duration::ZERO,
            ));

        let center_wpos = near_map_center(&world);
        let key = state.terrain().pos_key(center_wpos.with_z(0));
        let ambient = world
            .sample_blocks()
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("a real map export must have terrain at its own center");

        const HEAL_STAGES: u8 = 4;
        const HEAL_INTERVAL: f64 = 600.0;
        let new_override = crater_override(center_wpos, 48.0, HEAL_STAGES, HEAL_INTERVAL);
        fire(&state, TerrainOverrideOp::Activate(new_override));

        let after_activate = Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        let overridden = world
            .sample_blocks_with_overrides(Some(&after_activate))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("the override must not change whether this column generates at all");
        assert!(
            overridden.alt < ambient.alt - 1.0,
            "a fresh crater must read materially lower altitude at its own center than ambient \
             (ambient={}, overridden={})",
            ambient.alt,
            overridden.alt
        );

        // Drive the healing scheduler forward through every stage.
        for stage in 1..=HEAL_STAGES {
            state.ecs_mut().write_resource::<TimeOfDay>().0 += HEAL_INTERVAL + 1.0;
            common_ecs::run_now::<terrain_damage_heal::Sys>(state.ecs());

            let emitted: Vec<_> = state
                .ecs()
                .read_resource::<EventBus<SetRegionalTerrainOverrideEvent>>()
                .recv_all()
                .collect();
            assert_eq!(
                emitted.len(),
                1,
                "exactly one heal step must fire per due scheduler run (stage {stage})"
            );
            fire(&state, emitted.into_iter().next().unwrap().op);

            assert!(
                state.terrain().get_key(key).is_none(),
                "every healing step must unload the touched chunk, never regenerate it in place \
                 (stage {stage})"
            );

            if stage < HEAL_STAGES {
                let overrides_now =
                    Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
                let damage = overrides_now.active[0]
                    .damage()
                    .expect("still a Damage payload mid-heal");
                assert!(
                    (damage.heal_progress - stage as f32 / HEAL_STAGES as f32).abs() < 0.001,
                    "heal_progress must advance by exactly 1/heal_stages per stage (stage {stage})"
                );
            }
        }

        let overrides_after_healing =
            Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert!(
            overrides_after_healing.active.is_empty(),
            "the final healing stage must deactivate the override outright"
        );
        assert_eq!(
            state
                .ecs()
                .read_resource::<crate::rtsim::RtSim>()
                .with_terrain_overrides(|o| o.active.len()),
            0,
            "deactivation via the healing scheduler must also clear the persisted rtsim mirror"
        );

        let healed = world
            .sample_blocks_with_overrides(Some(&overrides_after_healing))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("terrain must still generate once the override is gone");
        assert_eq!(
            healed.alt, ambient.alt,
            "once fully healed and deactivated, the column must read EXACTLY the ambient altitude \
             again"
        );
    }

    /// The shipped catalog's own throwaway test fixture (see
    /// `assets/world/manifests/biome_profiles.ron`). `flood_to: None`
    /// because that's ALWAYS baked by `apply` at activation time (see
    /// `bake_biome_profile_flood_to`), never supplied by the caller.
    fn biome_profile_override(
        center: vek::Vec2<i32>,
        radius: f32,
        intensity: f32,
    ) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId::new_unique(),
            region: OverrideRegion::Circle {
                center,
                radius,
                edge: 8.0,
            },
            payload: TerrainOverridePayload::BiomeProfile(common::terrain::BiomeProfileOverride {
                profile: "test_fixture_do_not_use_in_content".to_string(),
                intensity,
                flood_to: None,
            }),
            priority: 100,
            activated_at: 0.0,
            wipe_player_edits: false,
            ephemeral: false,
        }
    }

    /// End-to-end across the whole `BiomeProfile` payload: activating it
    /// bakes `flood_to` from the fixture's authored `flood_depth` (see
    /// `bake_biome_profile_flood_to`), then changes ground color, forces the
    /// fixture's higher-weight forest species (`Swamp` at weight `3.0` over
    /// `Mangrove` at `1.0` -- see `world/src/column.rs`'s
    /// `ColumnGen::get`), forces the fixture's `surface_block`/
    /// `force_no_snow`, and floods the water level up to the baked altitude
    /// -- all read at the override's own center, where radial blend is
    /// `1.0`. Deactivating reverts every one of those back to EXACTLY
    /// ambient, the same full-circle guarantee the crater test above
    /// already established for `Damage`.
    #[test]
    #[ignore]
    fn activating_a_biome_profile_override_changes_ground_forest_and_flood_and_reverts_on_deactivation()
     {
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
        let ambient = world
            .sample_blocks()
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("a real map export must have terrain at its own center");

        const RADIUS: f32 = 48.0;
        let new_override = biome_profile_override(center_wpos, RADIUS, 1.0);
        let id = new_override.id;
        fire(&state, TerrainOverrideOp::Activate(new_override));

        let after_activate = Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert_eq!(after_activate.version, 1);
        assert_eq!(after_activate.active.len(), 1);

        let baked_flood_to = after_activate.active[0]
            .biome_profile()
            .expect("the active override must still carry its BiomeProfile payload")
            .flood_to
            .expect("activation must bake flood_to from the fixture's authored flood_depth (2.0)");
        assert!(
            (baked_flood_to - (ambient.alt + 2.0)).abs() < 0.01,
            "flood_to must equal the region center's own ambient ground altitude plus the \
             fixture's flood_depth of 2.0 (ambient.alt={}, baked_flood_to={})",
            ambient.alt,
            baked_flood_to
        );

        let overridden = world
            .sample_blocks_with_overrides(Some(&after_activate))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("the override must not change whether this column generates at all");

        assert_eq!(
            overridden
                .governing_biome_profile
                .map(|rp| rp.profile.id.as_str()),
            Some("test_fixture_do_not_use_in_content"),
            "the column at the override's own center must be governed by the fixture profile"
        );
        assert_ne!(
            overridden.sub_surface_color, ambient.sub_surface_color,
            "ground color must blend toward the profile's authored color at full strength"
        );
        assert_eq!(
            overridden.forest_kind,
            world::ForestKind::Swamp,
            "the fixture's higher-weight forest entry (Swamp, weight 3.0) must govern over the \
             lower-weight one (Mangrove, weight 1.0)"
        );
        assert_eq!(
            overridden.surface_block_override,
            Some(common::terrain::BlockKind::Earth),
            "the fixture's authored surface_block must force Earth at the surface"
        );
        assert!(
            !overridden.snow_cover,
            "the fixture's force_no_snow must suppress snow regardless of ambient temperature"
        );
        assert!(
            overridden.water_level >= baked_flood_to - 0.01,
            "water level must flood up to (at least) the baked altitude at full blend strength \
             (water_level={}, baked_flood_to={})",
            overridden.water_level,
            baked_flood_to
        );

        fire(&state, TerrainOverrideOp::Deactivate(id));

        let after_deactivate = Arc::clone(&*state.ecs().read_resource::<Arc<TerrainOverrides>>());
        assert_eq!(after_deactivate.version, 2);
        assert!(after_deactivate.active.is_empty());

        let reverted = world
            .sample_blocks_with_overrides(Some(&after_deactivate))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("terrain must still generate once the override is gone");
        assert_eq!(
            reverted.sub_surface_color, ambient.sub_surface_color,
            "deactivating must revert ground color to EXACTLY ambient"
        );
        assert_eq!(
            reverted.forest_kind, ambient.forest_kind,
            "deactivating must revert forest species to EXACTLY ambient"
        );
        assert_eq!(
            reverted.surface_block_override, None,
            "deactivating must clear the forced surface block"
        );
        assert_eq!(
            reverted.water_level, ambient.water_level,
            "deactivating must revert the water level to EXACTLY ambient"
        );
    }

    /// The flood-to-water-level effect must scale by `rp.strength()` (radial
    /// blend × the override's own `intensity`), exactly like every OTHER
    /// continuous effect this payload has (ground/sub-surface color,
    /// tree-density multiplier) -- NOT by radial blend alone. A regression
    /// test for a real bug: the flood lerp originally used `rp.blend` on its
    /// own, so a low-`intensity` override would still flood all the way to
    /// the full baked `flood_to` altitude at the region's core even while
    /// every other effect on the same profile stayed dialed down.
    ///
    /// Bakes `flood_to` by hand (rather than going through the full
    /// activate/deactivate ECS flow the test above already covers) so this
    /// test isolates exactly the column-level blend arithmetic in
    /// `world/src/column.rs`, independent of `bake_biome_profile_flood_to`.
    #[test]
    #[ignore]
    fn a_low_intensity_biome_profile_override_scales_down_the_flood_level_not_just_color() {
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

        // Must match the fixture's own authored `flood_depth` (see
        // `assets/world/manifests/biome_profiles.ron`).
        const FLOOD_DEPTH: f32 = 2.0;
        const INTENSITY: f32 = 0.1;
        let full_flood_to = ambient.alt + FLOOD_DEPTH;

        let mut low_intensity_override = biome_profile_override(center_wpos, 48.0, INTENSITY);
        let TerrainOverridePayload::BiomeProfile(profile) = &mut low_intensity_override.payload
        else {
            unreachable!("biome_profile_override always builds a BiomeProfile payload");
        };
        profile.flood_to = Some(full_flood_to);

        let overrides = TerrainOverrides {
            version: 1,
            active: vec![low_intensity_override],
        };
        let overridden = world
            .sample_blocks_with_overrides(Some(&overrides))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("the override must not change whether this column generates at all");

        // At the region's own center, radial blend is `1.0`, so
        // `strength() == blend * intensity == INTENSITY` exactly.
        let expected_water_level = ambient
            .water_level
            .max(ambient.water_level + (full_flood_to - ambient.water_level) * INTENSITY);
        assert!(
            (overridden.water_level - expected_water_level).abs() < 0.01,
            "the flood level must scale by strength() (blend * intensity = {INTENSITY}), not by \
             blend alone -- expected {expected_water_level}, got {}",
            overridden.water_level
        );
        assert!(
            overridden.water_level < full_flood_to - 0.5,
            "at intensity {INTENSITY} the flood must be scaled far below the full baked flood_to \
             (full_flood_to={full_flood_to}, water_level={})",
            overridden.water_level
        );
    }

    /// `world/src/layer/shrub.rs`'s `apply_shrubs_to` used to call the raw,
    /// un-overridden `WorldSim::make_forest_lottery` directly, completely
    /// ignoring every active regional terrain override (climate, damage, AND
    /// biome-profile alike) -- a real pre-existing bug, fixed alongside this
    /// payload kind by making it consult overrides the same way
    /// `world/src/layer/tree.rs` already does. This is the fix's
    /// verification: under an active `BiomeProfile` override with a
    /// nonempty `forest` list, the resolved `ColumnSample` a shrub-placement
    /// call would consult (`CanvasInfo::col_or_gen`, which `apply_shrubs_to`
    /// calls directly) must expose that list via
    /// `ColumnSample::governing_biome_profile` -- confirming the exact field
    /// `apply_shrubs_to`'s fixed lottery pick now reads actually carries the
    /// override's forest list at the position shrubs are evaluated at,
    /// where the OLD, buggy code path (a bare `make_forest_lottery` call)
    /// would have silently ignored it entirely.
    #[test]
    #[ignore]
    fn shrub_placement_sees_the_governing_biome_profiles_forest_list() {
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
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![biome_profile_override(center_wpos, 48.0, 1.0)],
        };

        // The exact same column lookup `CanvasInfo::col_or_gen` performs
        // (see `world/src/canvas.rs`), which is what `apply_shrubs_to`
        // (`world/src/layer/shrub.rs`) calls before picking a forest
        // lottery -- confirming the override is visible at exactly the call
        // site the fix touches, not a different one.
        let col = world
            .sample_blocks_with_overrides(Some(&overrides))
            .column_gen
            .get((center_wpos, index.as_index_ref(), None))
            .expect("the override must not change whether this column generates at all");

        let governing = col
            .governing_biome_profile
            .expect("a BiomeProfile override active over this exact position must govern it");
        let forest_kinds: Vec<_> = governing
            .profile
            .forest
            .iter()
            .map(|(kind, _)| *kind)
            .collect();
        assert_eq!(
            forest_kinds,
            vec![world::ForestKind::Swamp, world::ForestKind::Mangrove],
            "apply_shrubs_to's fixed lottery pick must see the fixture's own forest list, not an \
             empty/default one"
        );
    }
}
