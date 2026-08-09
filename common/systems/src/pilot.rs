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
    comp::{Controller, Ori, RemoteSense, Vel, controller::ControllerInputs},
    link::Is,
    piloting::Piloted,
    uid::IdMaps,
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Join, Read, ReadStorage, WriteStorage};
use vek::Vec3;

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Read<'a, IdMaps>,
        Entities<'a>,
        ReadStorage<'a, Is<Piloted>>,
        ReadStorage<'a, RemoteSense>,
        WriteStorage<'a, Controller>,
        WriteStorage<'a, Vel>,
        WriteStorage<'a, Ori>,
    );

    const NAME: &'static str = "pilot";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            id_maps,
            entities,
            is_piloted,
            remote_senses,
            mut controllers,
            mut velocities,
            mut orientations,
        ): Self::SystemData,
    ) {
        for (eye, is_piloted) in (&entities, &is_piloted).join() {
            let Some(pilot) = id_maps.uid_entity(is_piloted.pilot) else {
                // The pilot no longer resolves (disconnected -- their entity
                // is deleted synchronously on disconnect, so this branch
                // fires immediately, not just after some delay). Left alone,
                // the eye would keep drifting at whatever velocity it had at
                // the exact moment of disconnect until
                // `Object::DeleteAfter` reaps it, up to
                // `SENSOR_MAX_LIFETIME` (`server/src/events/remote_sense.rs`)
                // later. Zeroing `Vel` here stops the drift immediately;
                // `Ori` is left as-is since a stale facing is harmless and
                // the eye is about to be reaped anyway.
                if let Some(vel) = velocities.get_mut(eye) {
                    vel.0 = Vec3::zero();
                }
                continue;
            };

            // Copy the pilot's inputs onto the eye's own controller -- the
            // input-forwarding half of `mount.rs:192-198`, and nothing else
            // (see the module doc for what is deliberately NOT copied).
            let Some(pilot_inputs) = controllers.get(pilot).map(|c| c.inputs.clone()) else {
                continue;
            };

            // RON-authored per-ability (`comp::buff::MiscBuffData::RemoteSense
            // ::flight_speed`), forwarded onto the caster's own `RemoteSense`
            // component at cast time (`server/src/events/remote_sense.rs`'s
            // `resolve_piloted`) so it can be read back here every tick
            // without touching `Buffs` at all. A pilot whose `RemoteSense`
            // has since ended (but whose `Is<Pilot>`/`Is<Piloted>` link
            // hasn't been torn down yet -- a single-tick race, not a steady
            // state) falls back to `arcane_eye`'s own shipped speed rather
            // than freezing the eye outright.
            let flight_speed = remote_senses
                .get(pilot)
                .map_or(FALLBACK_FLIGHT_SPEED, |rs| rs.flight_speed);

            // `drive_eye` only ever needs a `&ControllerInputs`, so it runs
            // before `pilot_inputs` is moved into the eye's own controller
            // below -- one clone (out of the pilot's own `Controller`)
            // instead of two.
            drive_eye(
                eye,
                &pilot_inputs,
                flight_speed,
                &mut velocities,
                &mut orientations,
            );

            if let Some(dst) = controllers.get_mut(eye) {
                dst.inputs = pilot_inputs;
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
        }
    }
}

/// Used only if a pilot's `RemoteSense` has already ended by the tick this
/// runs on (see the call site's doc comment) -- not a balance value in its
/// own right, just `arcane_eye`'s own shipped flight speed as a safety net
/// so a mid-teardown eye still moves sanely for one tick instead of
/// freezing. The real, tunable value lives in `arcane_eye.ron`'s
/// `flight_speed` field.
const FALLBACK_FLIGHT_SPEED: f32 = 6.0;

/// Turns the forwarded `ControllerInputs` into the eye's own `Vel`/`Ori` --
/// the translation `character_behavior::Sys` would normally do via a
/// `CharacterState`, which the eye deliberately has none of. See the module
/// doc's "The eye's own flight model" section.
fn drive_eye(
    eye: specs::Entity,
    inputs: &ControllerInputs,
    flight_speed: f32,
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
        vel.0 = (horizontal + vertical) * flight_speed;
    }
    if let Some(ori) = orientations.get_mut(eye) {
        *ori = look_ori;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpolation;
    use common::{
        comp::{Body, Pos, controller::InputKind, remote_sense::SenseAnchor},
        link::LinkHandle,
        piloting::Piloting,
        resources::{PlayerEntity, Time},
        uid::Uid,
    };
    use common_net::sync::interpolation::InterpBuffer;
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
        world.register::<RemoteSense>();
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

    /// `flight_speed` is RON-authored per-ability
    /// (`comp::buff::MiscBuffData::RemoteSense::flight_speed`) and forwarded
    /// onto the pilot's own `RemoteSense` component at cast time -- this
    /// system must read it back from there, not from a hardcoded constant.
    /// A deliberately non-default value (double `FALLBACK_FLIGHT_SPEED`)
    /// proves the value in play is the one carried by `RemoteSense`.
    #[test]
    fn drive_speed_comes_from_the_pilots_remote_sense_not_a_constant() {
        let mut world = setup_world();

        let pilot_uid = uid(1);
        let eye_uid = uid(2);
        const CUSTOM_FLIGHT_SPEED: f32 = FALLBACK_FLIGHT_SPEED * 2.0;

        let mut pilot_controller = Controller::default();
        pilot_controller.inputs.move_dir = Vec2::new(0.0, 1.0);
        let pilot = world
            .create_entity()
            .with(pilot_controller)
            .with(RemoteSense {
                anchor: SenseAnchor::Piloted(eye_uid),
                free_look: false,
                piloted: true,
                caster: pilot_uid,
                flight_speed: CUSTOM_FLIGHT_SPEED,
            })
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

        let eye_vel = world.read_storage::<Vel>().get(eye).copied().unwrap();
        assert!(
            (eye_vel.0.magnitude() - CUSTOM_FLIGHT_SPEED).abs() < f32::EPSILON,
            "the eye's speed must come from the pilot's own RemoteSense::flight_speed \
             ({CUSTOM_FLIGHT_SPEED}), got magnitude {}",
            eye_vel.0.magnitude()
        );
    }

    /// A pilot whose `Uid` no longer resolves -- e.g. disconnected, whose
    /// entity is deleted synchronously on disconnect -- must not leave the
    /// eye drifting at its last velocity. Before this fix, the early
    /// `continue` skipped the eye entirely; now it must zero `Vel` first.
    #[test]
    fn an_orphaned_eye_has_its_velocity_zeroed() {
        let mut world = setup_world();

        // Deliberately never registered in `IdMaps` -- must never resolve,
        // modelling a pilot whose entity is already gone.
        let orphan_pilot_uid = uid(1);
        let eye_uid = uid(2);

        let eye = world
            .create_entity()
            .with(Controller::default())
            .with(Vel(Vec3::new(9.0, 9.0, 9.0)))
            .with(Ori::default())
            .build();

        world.write_resource::<IdMaps>().add_entity(eye_uid, eye);

        let handle = test_piloting_link(orphan_pilot_uid, eye_uid);
        world
            .write_storage::<Is<Piloted>>()
            .insert(eye, handle.make_role())
            .unwrap();

        common_ecs::run_now::<Sys>(&world);

        assert_eq!(
            world.read_storage::<Vel>().get(eye).copied(),
            Some(Vel(Vec3::zero())),
            "an eye whose pilot no longer resolves must have its velocity zeroed, not left \
             drifting at its last known value"
        );
    }

    /// Regression for the missing `interpolation::Sys` dependency:
    /// `add_local_systems` now declares `pilot::Sys` to depend on
    /// `interpolation::Sys` precisely so that, on any given tick, the
    /// eye's `Vel` reflects the pilot's *fresh* input rather than a stale
    /// value `interpolation::Sys` wrote from the eye's `InterpData` (every
    /// synced remote entity on a client carries one, including the eye --
    /// its only exclusion is the *local player's own* entity). This test
    /// exercises that exact ordering directly: run `interpolation::Sys`
    /// first (as the declared dependency now guarantees), then
    /// `pilot::Sys`, and confirm the pilot-forwarded write is what's left
    /// standing, not the interpolated one.
    #[test]
    fn pilot_write_wins_over_a_stale_interpolated_velocity_when_run_after_it() {
        let mut world = setup_world();
        world.register::<InterpBuffer<Pos>>();
        world.register::<InterpBuffer<Vel>>();
        world.register::<InterpBuffer<Ori>>();
        world.insert(PlayerEntity(None));
        world.insert(Time(1.0));

        let pilot_uid = uid(1);
        let eye_uid = uid(2);

        let mut pilot_controller = Controller::default();
        pilot_controller.inputs.move_dir = Vec2::new(0.3, 0.7);
        pilot_controller.inputs.move_z = 1.0;
        let pilot = world.create_entity().with(pilot_controller).build();

        // A stale interpolation buffer standing in for a remote-synced
        // `InterpData` write: on its own (`t0=0.0 -> Vel(100,0,0)`,
        // `t1=1.0 -> Vel(200,0,0)`, sampled at `Time = 1.0`), this drives
        // the eye's `Vel` to a large, obviously-not-pilot-driven magnitude
        // (~20 on the x axis) -- see
        // `common/net/src/sync/interpolation.rs`'s `Vel::interpolate`.
        let stale_interp = InterpBuffer::<Vel> {
            buf: [
                (0.0, Vel(Vec3::new(100.0, 0.0, 0.0))),
                (1.0, Vel(Vec3::new(200.0, 0.0, 0.0))),
                (0.0, Vel(Vec3::zero())),
                (0.0, Vel(Vec3::zero())),
            ],
            i: 1,
        };

        let eye = world
            .create_entity()
            .with(Controller::default())
            .with(Vel(Vec3::zero()))
            .with(Ori::default())
            .with(stale_interp)
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

        // Sanity check: interpolation alone really does push the eye's Vel
        // to the large stale value this test is guarding against.
        common_ecs::run_now::<interpolation::Sys>(&world);
        let interpolated_only = world.read_storage::<Vel>().get(eye).copied().unwrap();
        assert!(
            interpolated_only.0.magnitude() > FALLBACK_FLIGHT_SPEED * 2.0,
            "test setup sanity check failed: the stale InterpData must produce a velocity clearly \
             larger than any pilot-driven one, got {interpolated_only:?}"
        );

        // Now run pilot::Sys, exactly as `add_local_systems`'s declared
        // dependency guarantees happens *after* interpolation::Sys.
        common_ecs::run_now::<Sys>(&world);

        let eye_vel = world.read_storage::<Vel>().get(eye).copied().unwrap();
        assert!(
            eye_vel.0.magnitude() < FALLBACK_FLIGHT_SPEED * 2.0,
            "pilot::Sys's own forwarded write must be what's left standing when it runs after \
             interpolation::Sys, not the stale interpolated value -- got {eye_vel:?}"
        );
    }
}
