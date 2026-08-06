//! `pilot::Sys` — the input-forwarding half of `mount::Sys`
//! (`common/systems/src/mount.rs:192-198`), plus the eye's own minimal
//! flight model.
//!
//! ## Why this lives in `add_local_systems` (client *and* server), unlike
//! every other remote-sensing system
//!
//! `server/src/sys/remote_sense.rs`'s own module doc explains why every
//! other piece of this mechanism is server-only: the client must not be able
//! to grant itself a viewpoint, so the code that grants one does not live in
//! a crate the client dispatches. Piloting is different for the same reason
//! `mount::Sys` (which this system otherwise deliberately mirrors) is
//! different: the `Piloting` *link* (the `Is<Pilot>`/`Is<Piloted>` pair) is
//! created only server-side, by `server/src/events/remote_sense.rs`'s
//! `resolve_piloted` — the client has no message that names a foreign entity
//! to drive (`server/src/sys/msg/in_game.rs` only ever writes the sending
//! client's own `Controller`). Given that link already exists, this system's
//! job — copying the pilot's own, already-server-validated inputs onto the
//! eye's `Controller`, and turning those inputs into the eye's own motion —
//! is pure client-side prediction: running it locally lets the eye respond
//! the instant a key is pressed instead of waiting a round trip, exactly
//! like `mount::Sys` already does for a rider's mount. The server runs the
//! identical deterministic computation and corrects the client if it ever
//! drifts, the same reconciliation every other predicted system relies on.
//!
//! ## What this does NOT do
//!
//! It never writes the pilot's own `Pos`/`Ori`/`Vel`
//! (`common/systems/src/mount.rs:167-191`'s position-slaving half) — that
//! would teleport the caster's body onto the eye, which is the opposite of
//! every remote-sensing spell's whole point. It never forwards
//! `Controller.events` or any `ControlAction` (`Jump`/`WallJump`/`Fly`/`Roll`/
//! `Primary`/`Secondary`/`Ability(_)`/etc — see the module-level whitelist
//! note below): the eye has no `CharacterState` to interpret any of them, and
//! forwarding zero is strictly narrower than `mount.rs`'s own
//! `Jump | WallJump | Fly | Roll` whitelist.
//!
//! ## The eye's own flight model
//!
//! Every other movable entity gets `Controller.inputs` turned into `Vel`/
//! `Ori` by `character_behavior::Sys`'s `CharacterState::behavior()` (via
//! `handle_move`/`handle_orientation`). The eye deliberately has no
//! `CharacterState` at all — it is a lean `Object` prop, not an NPC — so that
//! translation has nowhere else to happen; this system is where it happens,
//! for the eye only. Neutral buoyancy (`Body::ArcaneEye`'s `AIR_DENSITY`
//! density, mirroring `Body::Crux`) means `phys::Sys` never needs to be
//! fought with thrust to counter gravity — only horizontal and (via
//! `move_z`) vertical cruise motion need to be supplied here.

use common::{
    comp::{Controller, Ori, Vel, controller::ControllerInputs},
    link::Is,
    piloting::Piloted,
    uid::IdMaps,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Join, Read, ReadStorage, WriteStorage};
use vek::Vec3;

/// Horizontal/vertical cruise speed of a piloted arcane eye, in blocks per
/// second. A `game-balance-designer` pass can retune this without touching
/// anything else in this file.
const EYE_FLIGHT_SPEED: f32 = 6.0;

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Read<'a, IdMaps>,
        Entities<'a>,
        ReadStorage<'a, Is<Piloted>>,
        WriteStorage<'a, Controller>,
        WriteStorage<'a, Vel>,
        WriteStorage<'a, Ori>,
    );

    const NAME: &'static str = "pilot";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (id_maps, entities, is_piloted, mut controllers, mut velocities, mut orientations): Self::SystemData,
    ) {
        for (eye, is_piloted) in (&entities, &is_piloted).join() {
            let Some(pilot) = id_maps.uid_entity(is_piloted.pilot) else {
                continue;
            };

            // Copy the pilot's inputs onto the eye's own controller -- the
            // input-forwarding half of `mount.rs:192-198`, and nothing else
            // (see the module doc for what is deliberately NOT copied).
            let Some(pilot_inputs) = controllers.get(pilot).map(|c| c.inputs.clone()) else {
                continue;
            };
            if let Some(dst) = controllers.get_mut(eye) {
                dst.inputs = pilot_inputs.clone();
                // Movement-only: `ControllerInputs` (move_dir/move_z/look_dir/
                // strafing) is continuous state with no attack/ability
                // surface by construction. `actions` (the queue of discrete
                // `ControlAction`s) is never forwarded at all -- the eye has
                // no `CharacterState` to interpret any of them, and
                // forwarding none trivially satisfies "no Primary/Secondary/
                // Ability(_)" rather than relying on an easy-to-get-wrong
                // filter.
                dst.actions.clear();
            }

            drive_eye(eye, &pilot_inputs, &mut velocities, &mut orientations);
        }
    }
}

/// Turns the forwarded `ControllerInputs` into the eye's own `Vel`/`Ori` --
/// the translation `character_behavior::Sys` would normally do via a
/// `CharacterState`, which the eye deliberately has none of. See the module
/// doc's "The eye's own flight model" section.
fn drive_eye(
    eye: specs::Entity,
    inputs: &ControllerInputs,
    velocities: &mut WriteStorage<Vel>,
    orientations: &mut WriteStorage<Ori>,
) {
    let look_ori = Ori::from_unnormalized_vec(*inputs.look_dir).unwrap_or_default();
    let horizontal_ori = look_ori.to_horizontal();
    let forward = horizontal_ori.look_dir().to_vec();
    let right = horizontal_ori.right().to_vec();

    let raw_horizontal = forward * inputs.move_dir.y + right * inputs.move_dir.x;
    let horizontal =
        raw_horizontal.try_normalized().unwrap_or_default() * raw_horizontal.magnitude().min(1.0);
    let vertical = Vec3::unit_z() * inputs.move_z.clamp(-1.0, 1.0);

    if let Some(vel) = velocities.get_mut(eye) {
        vel.0 = (horizontal + vertical) * EYE_FLIGHT_SPEED;
    }
    if let Some(ori) = orientations.get_mut(eye) {
        *ori = look_ori;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{Body, Pos, controller::InputKind},
        link::LinkHandle,
        piloting::Piloting,
        uid::Uid,
    };
    use specs::{Builder, WorldExt};
    use std::num::NonZeroU64;
    use vek::Vec2;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn test_piloting_link(pilot: Uid, piloted: Uid) -> LinkHandle<Piloting> {
        LinkHandle::from_link(Piloting { pilot, piloted })
    }

    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Controller>();
        world.register::<Vel>();
        world.register::<Ori>();
        world.register::<Pos>();
        world.register::<Body>();
        world.register::<Is<Piloted>>();
        world.insert(IdMaps::default());
        world.insert(common_ecs::SysMetrics::default());
        world
    }

    #[test]
    fn forwards_move_inputs_without_touching_the_pilots_own_transform() {
        let mut world = setup_world();

        let pilot_uid = uid(1);
        let eye_uid = uid(2);

        let mut pilot_controller = Controller::default();
        pilot_controller.inputs.move_dir = Vec2::new(0.3, 0.7);
        pilot_controller.inputs.move_z = 1.0;
        // A non-movement action that must NEVER reach the eye's controller.
        pilot_controller.push_basic_input(InputKind::Primary);

        let pilot_pos = Pos(Vec3::new(5.0, 5.0, 5.0));
        let pilot_ori = Ori::default();
        let pilot_vel = Vel(Vec3::zero());
        let pilot = world
            .create_entity()
            .with(pilot_controller.clone())
            .with(pilot_pos)
            .with(pilot_ori)
            .with(pilot_vel)
            .build();

        let eye = world
            .create_entity()
            .with(Controller::default())
            .with(Vel(Vec3::zero()))
            .with(Ori::default())
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(pilot_uid, pilot);
            id_maps.add_entity(eye_uid, eye);
        }

        let handle = test_piloting_link(pilot_uid, eye_uid);
        world
            .write_storage::<Is<Piloted>>()
            .insert(eye, handle.make_role())
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        let eye_controller = world.read_storage::<Controller>();
        let eye_controller = eye_controller.get(eye).unwrap();
        assert_eq!(
            eye_controller.inputs.move_dir, pilot_controller.inputs.move_dir,
            "the eye's controller must receive the pilot's own move_dir"
        );
        assert!(
            eye_controller.actions.is_empty(),
            "no ControlAction (Primary included) may ever reach the eye's controller"
        );

        // Invariant 5: never slave the pilot's own Pos/Ori/Vel to the eye.
        assert_eq!(
            world.read_storage::<Pos>().get(pilot).copied(),
            Some(pilot_pos),
            "pilot::Sys must never write the pilot's own Pos"
        );
        assert_eq!(
            world.read_storage::<Vel>().get(pilot).copied(),
            Some(pilot_vel),
            "pilot::Sys must never write the pilot's own Vel"
        );
        assert_eq!(
            world.read_storage::<Ori>().get(pilot).copied(),
            Some(pilot_ori),
            "pilot::Sys must never write the pilot's own Ori"
        );

        // The eye itself does get a velocity derived from the forwarded
        // inputs -- otherwise "drivable" would be a lie.
        let eye_vel = world.read_storage::<Vel>().get(eye).copied().unwrap();
        assert!(
            eye_vel.0.magnitude() > 0.0,
            "non-zero move input must produce non-zero eye velocity"
        );
    }

    #[test]
    fn a_stationary_pilot_leaves_the_eye_hovering() {
        let mut world = setup_world();

        let pilot_uid = uid(1);
        let eye_uid = uid(2);

        let pilot = world.create_entity().with(Controller::default()).build();
        let eye = world
            .create_entity()
            .with(Controller::default())
            .with(Vel(Vec3::new(9.0, 9.0, 9.0)))
            .with(Ori::default())
            .build();

        {
            let mut id_maps = world.write_resource::<IdMaps>();
            id_maps.add_entity(pilot_uid, pilot);
            id_maps.add_entity(eye_uid, eye);
        }

        let handle = test_piloting_link(pilot_uid, eye_uid);
        world
            .write_storage::<Is<Piloted>>()
            .insert(eye, handle.make_role())
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        assert_eq!(
            world.read_storage::<Vel>().get(eye).copied(),
            Some(Vel(Vec3::zero())),
            "zero move input must zero the eye's velocity, even if it was moving before"
        );
    }
}
