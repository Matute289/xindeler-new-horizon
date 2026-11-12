//! The Regional Terrain Event Engine's healing scheduler.
//!
//! Every `SysScheduler`-throttled tick, advances every active `Damage`-payload
//! [`RegionalTerrainOverride`]'s `heal_progress` by one stage once its
//! `next_heal_at` (a `TimeOfDay` deadline, NOT `Time` -- `Time` resets on
//! every server restart, so a crater's healing schedule would too) has
//! passed, unless a player is currently standing in its region (in which case
//! this step is deferred to the next scheduler run, not skipped forever).
//! Never mutates `Arc<TerrainOverrides>` directly: it emits
//! `SetRegionalTerrainOverrideEvent`s (`Replace` for an in-progress heal,
//! `Deactivate` for the final stage) through the same event bus
//! `server/src/cmd.rs`'s `/terrain_override` admin command uses, so
//! `server/src/terrain_override.rs::apply` remains the single place that
//! actually regenerates chunks and mirrors into rtsim persistence.

use std::sync::Arc;

use common::{
    comp::{Pos, Presence},
    event::{EmitExt, SetRegionalTerrainOverrideEvent, TerrainOverrideOp},
    event_emitters,
    resources::TimeOfDay,
    terrain::{TerrainOverridePayload, TerrainOverrides},
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, ReadExpect, ReadStorage, Write};

use crate::{sys::SysScheduler, terrain_override};

event_emitters! {
    struct Events[Emitters] {
        set_override: SetRegionalTerrainOverrideEvent,
    }
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        ReadExpect<'a, Arc<TerrainOverrides>>,
        ReadExpect<'a, TimeOfDay>,
        Events<'a>,
        Entities<'a>,
        ReadStorage<'a, Pos>,
        ReadStorage<'a, Presence>,
        Write<'a, SysScheduler<Self>>,
    );

    const NAME: &'static str = "terrain_damage_heal";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (terrain_overrides, time_of_day, events, entities, positions, presences, mut scheduler): Self::SystemData,
    ) {
        if !scheduler.should_run() {
            return;
        }

        let now = time_of_day.0;
        // Shared with `terrain_override::regenerate_chunks`'s own
        // reposition/view-distance checks -- see that function's doc
        // comment. Only the world-space position half is used here.
        let player_positions =
            terrain_override::joined_player_positions(&entities, &positions, &presences);

        let mut emitters = events.get_emitters();
        for o in &terrain_overrides.active {
            let TerrainOverridePayload::Damage(damage) = &o.payload else {
                continue;
            };
            if now < damage.next_heal_at {
                continue;
            }

            let occupied = player_positions
                .iter()
                .any(|(_, pos, _, _)| o.region.blend_factor(pos.xy().as_::<i32>()) > 0.0);
            if occupied {
                // Defer this healing step to the next scheduler run rather
                // than skipping it outright -- a player camping the region
                // forever would otherwise freeze it half-healed permanently.
                continue;
            }

            let heal_stages = damage.heal_stages.max(1) as f32;
            let heal_progress = damage.heal_progress + 1.0 / heal_stages;

            if heal_progress >= 1.0 {
                emitters.emit(SetRegionalTerrainOverrideEvent {
                    op: TerrainOverrideOp::Deactivate(o.id),
                });
            } else {
                let mut healed = o.clone();
                if let TerrainOverridePayload::Damage(healed_damage) = &mut healed.payload {
                    healed_damage.heal_progress = heal_progress;
                    healed_damage.next_heal_at = now + healed_damage.heal_interval;
                }
                emitters.emit(SetRegionalTerrainOverrideEvent {
                    op: TerrainOverrideOp::Replace(healed),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        ViewDistances,
        comp::PresenceKind,
        event::EventBus,
        terrain::{
            DamageOverride, DamageShape, OverrideRegion, RegionalTerrainOverride, TerrainOverrideId,
        },
    };
    use specs::{Builder, WorldExt};
    use vek::{Vec2, Vec3};

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Presence>();
        world.insert(EventBus::<SetRegionalTerrainOverrideEvent>::default());
        world.insert(common_ecs::SysMetrics::default());
        world
    }

    fn fresh_crater(id: u64, heal_progress: f32, next_heal_at: f64) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId(id),
            region: OverrideRegion::Circle {
                center: Vec2::zero(),
                radius: 32.0,
                edge: 8.0,
            },
            payload: TerrainOverridePayload::Damage(DamageOverride {
                shapes: vec![DamageShape::Crater {
                    max_depth: 10.0,
                    rim_height: 2.0,
                }],
                scorch: 0.5,
                vegetation_mul: 0.1,
                heal_progress,
                heal_stages: 4,
                heal_interval: 600.0,
                next_heal_at,
            }),
            priority: 0,
            activated_at: 0.0,
            wipe_player_edits: false,
            ephemeral: false,
            transition: Default::default(),
        }
    }

    /// A due, unoccupied heal step must advance `heal_progress` by
    /// `1.0 / heal_stages` and reschedule `next_heal_at`, via a `Replace`
    /// event (never mutating the resource directly).
    #[test]
    fn a_due_unoccupied_heal_step_emits_a_replace_with_advanced_progress() {
        let mut world = setup_world();
        world.insert(TimeOfDay(1000.0));
        world.insert(Arc::new(TerrainOverrides {
            version: 1,
            active: vec![fresh_crater(1, 0.0, 500.0)],
        }));
        world.insert(SysScheduler::<Sys>::every(std::time::Duration::ZERO));

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<_> = world
            .read_resource::<EventBus<SetRegionalTerrainOverrideEvent>>()
            .recv_all()
            .collect();
        assert_eq!(events.len(), 1);
        match &events[0].op {
            TerrainOverrideOp::Replace(healed) => {
                let damage = healed.damage().expect("still a Damage payload");
                assert!((damage.heal_progress - 0.25).abs() < 0.001);
                assert!((damage.next_heal_at - 1600.0).abs() < 0.001);
            },
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    /// The final healing stage must deactivate the override outright, not
    /// `Replace` it with `heal_progress == 1.0`.
    #[test]
    fn the_final_heal_stage_deactivates_instead_of_replacing() {
        let mut world = setup_world();
        world.insert(TimeOfDay(1000.0));
        world.insert(Arc::new(TerrainOverrides {
            version: 1,
            active: vec![fresh_crater(7, 0.75, 500.0)],
        }));
        world.insert(SysScheduler::<Sys>::every(std::time::Duration::ZERO));

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<_> = world
            .read_resource::<EventBus<SetRegionalTerrainOverrideEvent>>()
            .recv_all()
            .collect();
        assert_eq!(events.len(), 1);
        match &events[0].op {
            TerrainOverrideOp::Deactivate(id) => assert_eq!(*id, TerrainOverrideId(7)),
            other => panic!("expected Deactivate, got {other:?}"),
        }
    }

    /// A heal step whose deadline hasn't passed yet must not fire.
    #[test]
    fn a_not_yet_due_heal_step_emits_nothing() {
        let mut world = setup_world();
        world.insert(TimeOfDay(100.0));
        world.insert(Arc::new(TerrainOverrides {
            version: 1,
            active: vec![fresh_crater(1, 0.0, 500.0)],
        }));
        world.insert(SysScheduler::<Sys>::every(std::time::Duration::ZERO));

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<_> = world
            .read_resource::<EventBus<SetRegionalTerrainOverrideEvent>>()
            .recv_all()
            .collect();
        assert!(events.is_empty());
    }

    /// A player standing inside the override's region must defer (not skip
    /// outright) the due heal step.
    #[test]
    fn a_player_standing_in_the_region_defers_the_due_heal_step() {
        let mut world = setup_world();
        world.insert(TimeOfDay(1000.0));
        world.insert(Arc::new(TerrainOverrides {
            version: 1,
            active: vec![fresh_crater(1, 0.0, 500.0)],
        }));
        world.insert(SysScheduler::<Sys>::every(std::time::Duration::ZERO));

        world
            .create_entity()
            .with(Pos(Vec3::new(0.0, 0.0, 0.0)))
            .with(Presence::new(
                ViewDistances {
                    terrain: 10,
                    entity: 10,
                },
                PresenceKind::Spectator,
            ))
            .build();

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<_> = world
            .read_resource::<EventBus<SetRegionalTerrainOverrideEvent>>()
            .recv_all()
            .collect();
        assert!(
            events.is_empty(),
            "a due heal step must be deferred, not fired, while a player is standing in the region"
        );
    }
}
