#![expect(clippy::option_map_unit_fn)]

mod arcing;
mod aura;
mod beam;
mod buff;
pub mod character_behavior;
pub mod controller;
mod derived_stats;
mod interpolation;
pub mod melee;
mod mount;
pub mod phys;
mod phys_events;
mod pilot;
mod pool;
pub mod projectile;
mod shockwave;
mod stats;
mod telemetry;
mod tether;

// External
use common_ecs::{System, dispatch};
use specs::DispatcherBuilder;

pub fn add_local_systems(dispatch_builder: &mut DispatcherBuilder) {
    //TODO: don't run interpolation on server
    dispatch::<interpolation::Sys>(dispatch_builder, &[]);
    dispatch::<tether::Sys>(dispatch_builder, &[]);
    dispatch::<mount::Sys>(dispatch_builder, &[]);
    // The one deliberate exception to server-only remote-sensing systems
    // (`server/src/sys/remote_sense.rs`'s own doc comment) -- see
    // `pilot::Sys`'s module doc for why it is safe here, alongside
    // `mount::Sys`, for the same reason.
    // Depends on `mount::Sys` (rather than `&[]`) so `phys::Sys`'s existing
    // dependency on `controller::Sys` transitively guarantees the eye's
    // pilot-forwarded `Vel`/`Ori` land before `phys::Sys` integrates
    // position the same tick -- the "moves same tick" client-prediction
    // requirement `pilot::Sys`'s module doc describes. `mount::Sys` and
    // `pilot::Sys` already write-conflict on `Controller`/`Vel`/`Ori`
    // regardless of a declared edge, so this costs no real parallelism.
    // Also depends on `interpolation::Sys` for the same reason `phys::Sys`
    // (a few lines below) does: `interpolation::Sys` writes `Pos`/`Vel`/`Ori`
    // for every entity carrying `InterpData` -- every synced remote entity on
    // a client, which includes the piloted eye (its only exclusion is the
    // *local player's own* entity, not the eye) -- and `pilot::Sys` also
    // writes the eye's `Vel`/`Ori`. Without a declared edge, which of the two
    // "wins" on a given tick is only an insertion-order tie-break (both
    // conflict on `Vel`/`Ori`, so specs serialises them either way, but not
    // to a documented order) -- a future reordering of this function could
    // silently flip the eye from responsive local prediction to laggy
    // interpolated movement with nothing to catch it, the same class of bug
    // `phys::Sys`'s own explicit dependency guards against.
    dispatch::<pilot::Sys>(dispatch_builder, &[
        &mount::Sys::sys_name(),
        &interpolation::Sys::sys_name(),
    ]);
    dispatch::<controller::Sys>(dispatch_builder, &[
        &mount::Sys::sys_name(),
        &pilot::Sys::sys_name(),
    ]);
    // Rebuilds the `DerivedStats` cache before ANY of its per-tick consumers
    // read it, so a gear/skill/body change lands on the same tick it
    // happened. Must be registered before `character_behavior::Sys` (which
    // depends on it below) -- specs requires a dependency's `dispatch` call
    // to have already run before it can be named as a dependency.
    dispatch::<derived_stats::Sys>(dispatch_builder, &[]);
    dispatch::<character_behavior::Sys>(dispatch_builder, &[
        &controller::Sys::sys_name(),
        &derived_stats::Sys::sys_name(),
    ]);
    dispatch::<buff::Sys>(dispatch_builder, &[&derived_stats::Sys::sys_name()]);
    dispatch::<stats::Sys>(dispatch_builder, &[
        &buff::Sys::sys_name(),
        &derived_stats::Sys::sys_name(),
    ]);
    dispatch::<phys::Sys>(dispatch_builder, &[
        &interpolation::Sys::sys_name(),
        &controller::Sys::sys_name(),
        &mount::Sys::sys_name(),
        &stats::Sys::sys_name(),
    ]);
    dispatch::<phys_events::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<projectile::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<shockwave::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<arcing::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<beam::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<pool::Sys>(dispatch_builder, &[&phys::Sys::sys_name()]);
    dispatch::<aura::Sys>(dispatch_builder, &[]);
    dispatch::<telemetry::Sys>(dispatch_builder, &[]);
}
