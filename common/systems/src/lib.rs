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
    dispatch::<controller::Sys>(dispatch_builder, &[&mount::Sys::sys_name()]);
    dispatch::<character_behavior::Sys>(dispatch_builder, &[&controller::Sys::sys_name()]);
    // Rebuilds the `DerivedStats` cache before its two per-tick consumers read
    // it, so a gear/skill/body change lands on the same tick it happened.
    dispatch::<derived_stats::Sys>(dispatch_builder, &[]);
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
