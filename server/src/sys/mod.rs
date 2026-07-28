pub mod agent;
pub mod attunement;
pub mod chunk_send;
pub mod chunk_serialize;
pub mod detection;
pub mod entity_sync;
pub mod invite_timeout;
pub mod item;
pub mod loot;
pub mod metrics;
pub mod msg;
pub mod object;
pub mod oracle;
pub mod persistence;
pub mod pets;
pub mod remote_sense;
pub mod sentinel;
pub mod server_info;
pub mod subscription;
pub mod teleporter;
pub mod terrain;
pub mod terrain_sync;
pub mod waypoint;
pub mod wiring;

use common_ecs::{System, dispatch, run_now};
use common_systems::{melee, projectile};
use specs::DispatcherBuilder;
use std::{
    marker::PhantomData,
    time::{Duration, Instant},
};

pub type PersistenceScheduler = SysScheduler<persistence::Sys>;

pub fn add_server_systems(dispatch_builder: &mut DispatcherBuilder) {
    dispatch::<melee::Sys>(dispatch_builder, &[&projectile::Sys::sys_name()]);
    //Note: server should not depend on interpolation system
    dispatch::<agent::Sys>(dispatch_builder, &[]);
    dispatch::<terrain::Sys>(dispatch_builder, &[&msg::terrain::Sys::sys_name()]);
    dispatch::<waypoint::Sys>(dispatch_builder, &[]);
    dispatch::<teleporter::Sys>(dispatch_builder, &[]);
    dispatch::<invite_timeout::Sys>(dispatch_builder, &[]);
    dispatch::<persistence::Sys>(dispatch_builder, &[]);
    dispatch::<object::Sys>(dispatch_builder, &[]);
    dispatch::<oracle::Sys>(dispatch_builder, &[]);
    dispatch::<wiring::Sys>(dispatch_builder, &[]);
    // no dependency, as we only work once per sec anyway.
    dispatch::<chunk_serialize::Sys>(dispatch_builder, &[]);
    // don't depend on chunk_serialize, as we assume everything is done in a SlowJow
    dispatch::<chunk_send::Sys>(dispatch_builder, &[]);
    dispatch::<item::Sys>(dispatch_builder, &[]);
    dispatch::<server_info::Sys>(dispatch_builder, &[]);
    // Observes the loadout after inventory changes are applied; the D2c
    // effect-gating consumer must run after this.
    dispatch::<attunement::Sys>(dispatch_builder, &[]);
    // Depends on the buff system (`common::systems::buff::Sys`, dispatched by
    // `common_systems::add_local_systems` before server-only systems run) so
    // it sees this tick's buff removals before deciding whether a link is
    // still sustained. That system isn't reachable from here (its module is
    // private to `common-systems`), so the dependency is named by its
    // computed `sys_name()`: `"Common_buff_sys"`.
    dispatch::<remote_sense::Sys>(dispatch_builder, &["Common_buff_sys"]);
    // Must run after the buff system, which rebuilds `Stats` (and with it the
    // active-sense declarations this reads) from scratch every tick; running
    // earlier would evaluate the previous tick's declarations and keep honouring
    // a sense for one tick after its buff was removed. Same naming caveat as
    // above: that system is private to `common-systems`, so the dependency is
    // named by its computed `sys_name()`.
    //
    // Deliberately registered here and never in
    // `common_systems::add_local_systems`: keeping the code that computes a
    // reveal set out of every crate the client dispatches is what makes it
    // structurally impossible for a client to grant itself one.
    dispatch::<detection::Sys>(dispatch_builder, &["Common_buff_sys"]);
}

pub fn run_sync_systems(ecs: &mut specs::World) {
    // Setup for entity sync
    // If I'm not mistaken, these two could be ran in parallel
    run_now::<sentinel::Sys>(ecs);
    run_now::<subscription::Sys>(ecs);

    // Sync
    run_now::<terrain_sync::Sys>(ecs);
    run_now::<entity_sync::Sys>(ecs);
}

/// Used to schedule systems to run at an interval
pub struct SysScheduler<S> {
    interval: Duration,
    last_run: Instant,
    _phantom: PhantomData<S>,
}

impl<S> SysScheduler<S> {
    pub fn every(interval: Duration) -> Self {
        Self {
            interval,
            last_run: Instant::now(),
            _phantom: PhantomData,
        }
    }

    pub fn should_run(&mut self) -> bool {
        if self.last_run.elapsed() > self.interval {
            self.last_run = Instant::now();

            true
        } else {
            false
        }
    }
}

impl<S> Default for SysScheduler<S> {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            last_run: Instant::now(),
            _phantom: PhantomData,
        }
    }
}
