//! COW-7b: the Undercompact gate antechamber's two-lever puzzle --
//! `ActivateVaultLeverEvent`'s handler. Pulls the interacted lever's own
//! visual flip (mirroring `ToggleSpriteLightEvent`'s handler shape exactly),
//! records the activation in the persisted `UndercompactGateLevers`
//! registry, and does the one-shot plug-clear write the moment both levers
//! are active.
//!
//! `worldgen`-feature-gated like `BanishEvent`
//! (`server/src/events/banishment.rs`): without it there is no
//! `world::World` to resolve the antechamber's geometry against and no
//! rtsim `Data` to persist a solve into, so the event is simply drained.

use common::event::ActivateVaultLeverEvent;

use super::ServerEvent;

#[cfg(not(feature = "worldgen"))]
impl ServerEvent for ActivateVaultLeverEvent {
    type SystemData<'a> = ();

    fn handle(_events: impl ExactSizeIterator<Item = Self>, (): Self::SystemData<'_>) {}
}

#[cfg(feature = "worldgen")]
mod worldgen_impl {
    use std::sync::Arc;

    use common::{
        comp,
        consts::MAX_INTERACT_RANGE,
        terrain::{Block, SpriteKind, TerrainGrid},
        vol::ReadVol,
    };
    use common_state::BlockChange;
    use rtsim::data::UndercompactGateLever;
    use specs::{ReadExpect, ReadStorage, WriteExpect};
    use world::{World, layer::cromatolis_interior};

    use crate::{
        rtsim::RtSim,
        undercompact_gate::{clear_gate_plug, plug_already_clear},
    };

    use super::{ActivateVaultLeverEvent, ServerEvent};

    impl ServerEvent for ActivateVaultLeverEvent {
        type SystemData<'a> = (
            WriteExpect<'a, BlockChange>,
            ReadExpect<'a, TerrainGrid>,
            ReadStorage<'a, comp::Pos>,
            WriteExpect<'a, RtSim>,
            ReadExpect<'a, Arc<World>>,
            ReadExpect<'a, world::IndexOwned>,
        );

        fn handle(
            events: impl ExactSizeIterator<Item = Self>,
            (mut block_change, terrain, positions, rtsim, world, index): Self::SystemData<'_>,
        ) {
            // Resolved once for the whole batch, not per event -- reads
            // from the same per-`Index` cache world-gen itself populated
            // when this chunk was first generated (see the accessor's own
            // doc comment), so this is a cheap `OnceLock` read, not a fresh
            // RON-parse-plus-BFS-rebuild.
            let Some(geometry) = cromatolis_interior::undercompact_gate_antechamber_world_geometry(
                index.as_index_ref(),
                world.sim(),
            ) else {
                return;
            };

            for ev in events {
                if !positions.get(ev.entity).is_some_and(|entity_pos| {
                    entity_pos.0.distance_squared(ev.pos.as_()) < MAX_INTERACT_RANGE.powi(2)
                }) {
                    continue;
                }
                if !block_change.can_set_block(ev.pos) {
                    continue;
                }

                // `ev.pos` has to actually be one of the two known levers --
                // checked before writing anything, so a stale/malicious
                // client position never places a floating lever sprite
                // somewhere unrelated.
                let lever = geometry
                    .lever_positions
                    .iter()
                    .position(|&lever_pos| lever_pos == ev.pos)
                    .map(|lever_index| {
                        if lever_index == 0 {
                            UndercompactGateLever::A
                        } else {
                            UndercompactGateLever::B
                        }
                    });
                let Some(lever) = lever else { continue };

                // Visual flip of the lever's own block: a `SpriteKind` swap
                // (`VaultLever` <-> `VaultLeverPulled`, a lowered
                // `sprite_manifest.ron` Z offset), not an `Ori` rotation --
                // the reused `gear_wheel-0` model is point-symmetric, so a
                // yaw rotation would have been visually identical.
                block_change.set(
                    ev.pos,
                    Block::air(if ev.enable {
                        SpriteKind::VaultLeverPulled
                    } else {
                        SpriteKind::VaultLever
                    }),
                );

                // Only a forward pull (`enable == true`, the only value the
                // generic sprite-interact path ever sends -- see
                // `SpriteInteractKind::LeverPull`'s own doc comment)
                // activates a lever; there is no "un-pull" affordance in
                // this puzzle.
                if !ev.enable {
                    continue;
                }

                let newly_solved = rtsim.with_undercompact_gate(|levers| levers.activate(lever));
                if newly_solved
                    && !plug_already_clear(&geometry, |pos| terrain.get(pos).ok().copied())
                {
                    clear_gate_plug(&geometry, |pos, block| {
                        if block_change.can_set_block(pos) {
                            block_change.set(pos, block);
                        }
                    });
                }
            }
        }
    }

    // ---- Heavy, real-terrain-backed tests: require the real Cromatolis LFS
    // assets pulled locally, same precedent as `gate_checkpoint.rs`'s own
    // `..._against_the_real_world` tests. Not run automatically.
    // Recommended: `cargo test -p xindeler-server -- --ignored undercompact_gate`
    // ----
    #[cfg(test)]
    mod tests {
        use common::terrain::{Block, BlockKind, SpriteKind};
        use specs::{Builder, SystemData as _, WorldExt};
        use vek::{Rgb, Vec3};

        use super::*;
        use crate::undercompact_gate::test_support::{insert_rtsim, load_chunk_containing, setup};

        fn build_world_and_index() -> (Arc<World>, world::IndexOwned) {
            let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
            let (world, index) = World::generate(
                0,
                world::sim::WorldOpts {
                    seed_elements: true,
                    world_file: world::sim::FileOpts::LoadAsset(
                        "world.map.cromatolis_v0".to_string(),
                    ),
                    calendar: None,
                },
                &threadpool,
                &|_| {},
            );
            (Arc::new(world), index)
        }

        fn build_scenario(
            world: &Arc<World>,
            index: &world::IndexOwned,
        ) -> (
            common_state::State,
            cromatolis_interior::UndercompactGateAntechamberGeometry,
            vek::Vec3<i32>,
        ) {
            let geometry = cromatolis_interior::undercompact_gate_antechamber_world_geometry(
                index.as_index_ref(),
                world.sim(),
            )
            .expect("the real export must carry the antechamber");

            let mut state = setup();
            insert_rtsim(&mut state, world, index);
            state.ecs_mut().insert(Arc::clone(world));
            state.ecs_mut().insert(index.clone());

            // Load the chunks under both levers and the plug's own sample
            // column, and carve both lever sprites plus a solid plug block
            // -- exactly what world-gen would have already done before any
            // player ever interacts.
            for lever_pos in geometry.lever_positions {
                load_chunk_containing(&mut state, lever_pos.map(|e| e as f32));
                state.set_block(lever_pos, Block::air(SpriteKind::VaultLever));
            }
            let plug_sample = geometry.plug_aabb.center();
            load_chunk_containing(&mut state, plug_sample.map(|e| e as f32));
            state.set_block(
                plug_sample,
                Block::new(BlockKind::Rock, Rgb::new(60, 55, 60)),
            );
            state.apply_terrain_changes(|_, _| {});

            (state, geometry, plug_sample)
        }

        fn fire_lever_event(
            state: &mut common_state::State,
            entity: specs::Entity,
            pos: Vec3<i32>,
        ) {
            let ecs = state.ecs();
            let data = <ActivateVaultLeverEvent as ServerEvent>::SystemData::fetch(ecs);
            <ActivateVaultLeverEvent as ServerEvent>::handle(
                std::iter::once(ActivateVaultLeverEvent {
                    entity,
                    pos,
                    enable: true,
                }),
                data,
            );
        }

        #[test]
        #[ignore]
        fn pulling_one_lever_alone_does_not_open_the_gate() {
            let (world, index) = build_world_and_index();
            let (mut state, geometry, plug_sample) = build_scenario(&world, &index);

            let entity = state
                .ecs_mut()
                .create_entity()
                .with(comp::Pos(geometry.lever_positions[0].as_()))
                .build();
            fire_lever_event(&mut state, entity, geometry.lever_positions[0]);
            state.apply_terrain_changes(|_, _| {});

            let solved = state
                .ecs()
                .read_resource::<RtSim>()
                .with_undercompact_gate(|levers| levers.is_solved());
            assert!(!solved, "one lever alone must never solve the puzzle");
            assert_eq!(
                state.get_block(plug_sample),
                Some(Block::new(BlockKind::Rock, Rgb::new(60, 55, 60))),
                "the plug must stay intact while only one lever is active"
            );
        }

        #[test]
        #[ignore]
        fn pulling_both_levers_opens_the_gate_exactly_once() {
            let (world, index) = build_world_and_index();
            let (mut state, geometry, plug_sample) = build_scenario(&world, &index);

            let entity_a = state
                .ecs_mut()
                .create_entity()
                .with(comp::Pos(geometry.lever_positions[0].as_()))
                .build();
            fire_lever_event(&mut state, entity_a, geometry.lever_positions[0]);
            state.apply_terrain_changes(|_, _| {});

            let entity_b = state
                .ecs_mut()
                .create_entity()
                .with(comp::Pos(geometry.lever_positions[1].as_()))
                .build();
            fire_lever_event(&mut state, entity_b, geometry.lever_positions[1]);
            state.apply_terrain_changes(|_, _| {});

            let solved = state
                .ecs()
                .read_resource::<RtSim>()
                .with_undercompact_gate(|levers| levers.is_solved());
            assert!(solved, "both levers active must solve the puzzle");
            assert_eq!(
                state.get_block(plug_sample),
                Some(Block::empty()),
                "the plug must be cleared once both levers are active"
            );

            // Re-activating an already-active lever (B again) must be a
            // harmless no-op: no panic, still solved, plug still clear --
            // never a double clear.
            fire_lever_event(&mut state, entity_b, geometry.lever_positions[1]);
            state.apply_terrain_changes(|_, _| {});
            let still_solved = state
                .ecs()
                .read_resource::<RtSim>()
                .with_undercompact_gate(|levers| levers.is_solved());
            assert!(still_solved);
            assert_eq!(state.get_block(plug_sample), Some(Block::empty()));
        }
    }
}
