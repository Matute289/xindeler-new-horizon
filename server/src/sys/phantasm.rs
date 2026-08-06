//! Server-only authority system for phantasm dispel.
//!
//! Consumes `DispelIllusionEvent` (raised by `combat::Attack::apply_attack`'s
//! `PhantomIllusion` early-out, on the first single-target hostile attack
//! resolved against a phantasm) and despawns the named entity, announcing an
//! `Outcome::PhantasmDissipated` for the client-side dissipate VFX.
//!
//! Deliberately registered ONLY in `server/src/sys/mod.rs`, never in
//! `common/systems/src/lib.rs::add_local_systems`: despawning an entity is
//! server authority, and there is no client-prediction need here (unlike,
//! say, movement) that would justify running this on the client too.

use common::{
    comp::{Body, Pos},
    event::{DeleteEvent, DispelIllusionEvent, EmitExt, EventBus},
    event_emitters,
    outcome::Outcome,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Read, ReadStorage};

event_emitters! {
    struct Events[Emitters] {
        delete: DeleteEvent,
    }
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Read<'a, EventBus<DispelIllusionEvent>>,
        Events<'a>,
        Read<'a, EventBus<Outcome>>,
        ReadStorage<'a, Pos>,
        ReadStorage<'a, Body>,
    );

    const NAME: &'static str = "phantasm";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (dispel_illusions, events, outcomes, positions, bodies): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();
        let mut outcomes_emitter = outcomes.emitter();

        for DispelIllusionEvent(entity) in dispel_illusions.recv_all() {
            if let Some(pos) = positions.get(entity)
                && let Some(&body) = bodies.get(entity)
            {
                outcomes_emitter.emit(Outcome::PhantasmDissipated { pos: pos.0, body });
            }

            emitters.emit(DeleteEvent(entity));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::comp::{Body, humanoid};
    use specs::{Builder, WorldExt};

    /// Registers every storage/resource `Sys::SystemData` needs to run,
    /// mirroring `server/src/sys/remote_sense.rs`'s `setup_world` idiom.
    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Pos>();
        world.register::<Body>();
        world.insert(EventBus::<DispelIllusionEvent>::default());
        world.insert(EventBus::<DeleteEvent>::default());
        world.insert(EventBus::<Outcome>::default());
        // `common_ecs::run_now`'s `Job<T>` wrapper records CPU-time metrics
        // per system and expects this resource to already exist.
        world.insert(common_ecs::SysMetrics::default());
        world
    }

    /// The core deliverable: receiving a `DispelIllusionEvent` for an entity
    /// must despawn it (via `DeleteEvent`, the shipped removal path -- this
    /// system never touches the entity directly) and announce the dissipate
    /// VFX at its position/body.
    #[test]
    fn a_dispel_event_despawns_the_phantasm_and_announces_the_vfx() {
        let mut world = setup_world();

        let body = Body::Humanoid(humanoid::Body::random());
        let phantasm = world
            .create_entity()
            .with(Pos(vek::Vec3::new(1.0, 2.0, 3.0)))
            .with(body)
            .build();

        world
            .read_resource::<EventBus<DispelIllusionEvent>>()
            .emit_now(DispelIllusionEvent(phantasm));

        common_ecs::run_now::<Sys>(&world);

        let deletes: Vec<_> = world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .collect();
        assert_eq!(deletes.len(), 1, "exactly one DeleteEvent must be emitted");
        assert_eq!(deletes[0].0, phantasm);

        let outcomes: Vec<_> = world
            .read_resource::<EventBus<Outcome>>()
            .recv_all()
            .collect();
        assert_eq!(
            outcomes.len(),
            1,
            "exactly one dissipate-VFX Outcome must be emitted"
        );
        assert!(matches!(outcomes[0], Outcome::PhantasmDissipated { .. }));
    }

    /// A dispel event naming an entity that no longer has `Pos`/`Body` (e.g.
    /// already despawned by something else this tick) must still be deleted
    /// -- the VFX is a nice-to-have, not a precondition for cleanup.
    #[test]
    fn a_dispel_event_for_an_entity_missing_pos_or_body_still_deletes_it() {
        let mut world = setup_world();

        let phantasm = world.create_entity().build();

        world
            .read_resource::<EventBus<DispelIllusionEvent>>()
            .emit_now(DispelIllusionEvent(phantasm));

        common_ecs::run_now::<Sys>(&world);

        let deletes: Vec<_> = world
            .read_resource::<EventBus<DeleteEvent>>()
            .recv_all()
            .collect();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].0, phantasm);

        let outcomes: Vec<_> = world
            .read_resource::<EventBus<Outcome>>()
            .recv_all()
            .collect();
        assert!(
            outcomes.is_empty(),
            "no VFX Outcome without a Pos/Body to render it at"
        );
    }
}
