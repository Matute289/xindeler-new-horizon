//! `SetRegionalTerrainOverrideEvent`'s handler. Mirrors
//! `ActivateVaultLeverEvent`'s handler shape
//! (`server/src/events/undercompact_gate.rs`): a `worldgen`-feature-gated
//! `ServerEvent` impl that's a thin wrapper -- all the actual logic is in
//! `crate::terrain_override::apply`, which this just assembles an
//! `ApplyContext` for from its `SystemData`.

use common::event::SetRegionalTerrainOverrideEvent;
use specs::DispatcherBuilder;

use super::{ServerEvent, event_dispatch};

pub(super) fn register_event_systems(builder: &mut DispatcherBuilder) {
    event_dispatch::<SetRegionalTerrainOverrideEvent>(builder, &[]);
}

#[cfg(not(feature = "worldgen"))]
impl ServerEvent for SetRegionalTerrainOverrideEvent {
    type SystemData<'a> = ();

    fn handle(_events: impl ExactSizeIterator<Item = Self>, (): Self::SystemData<'_>) {}
}

#[cfg(feature = "worldgen")]
mod worldgen_impl {
    use std::sync::Arc;

    use common::{
        calendar::Calendar,
        comp::{Pos, Presence},
        event::SetRegionalTerrainOverrideEvent,
        resources::TimeOfDay,
        slowjob::SlowJobPool,
        terrain::{TerrainGrid, TerrainOverrides},
    };
    use common_state::TerrainChanges;
    use specs::{Entities, ReadExpect, ReadStorage, Write, WriteExpect, WriteStorage};
    use world::{IndexOwned, World};

    use crate::{
        chunk_generator::ChunkGenerator,
        client::Client,
        presence::RepositionToFreeSpace,
        rtsim::RtSim,
        terrain_override::{self, ApplyContext},
    };

    #[cfg(feature = "persistent_world")]
    use crate::terrain_persistence::TerrainPersistence;

    use super::ServerEvent;

    #[cfg(feature = "persistent_world")]
    type TerrainPersistenceData<'a> = Option<WriteExpect<'a, TerrainPersistence>>;
    #[cfg(not(feature = "persistent_world"))]
    type TerrainPersistenceData<'a> = ();

    impl ServerEvent for SetRegionalTerrainOverrideEvent {
        type SystemData<'a> = (
            WriteExpect<'a, Arc<TerrainOverrides>>,
            WriteExpect<'a, TerrainGrid>,
            Write<'a, TerrainChanges>,
            WriteExpect<'a, ChunkGenerator>,
            ReadExpect<'a, SlowJobPool>,
            ReadExpect<'a, Arc<World>>,
            ReadExpect<'a, IndexOwned>,
            ReadExpect<'a, TimeOfDay>,
            ReadExpect<'a, Calendar>,
            WriteExpect<'a, RtSim>,
            TerrainPersistenceData<'a>,
            Entities<'a>,
            ReadStorage<'a, Pos>,
            ReadStorage<'a, Presence>,
            WriteStorage<'a, RepositionToFreeSpace>,
            ReadStorage<'a, Client>,
        );

        fn handle(
            events: impl ExactSizeIterator<Item = Self>,
            (
                mut terrain_overrides,
                mut terrain,
                mut terrain_changes,
                mut chunk_generator,
                slow_jobs,
                world,
                index,
                time_of_day,
                calendar,
                rtsim,
                terrain_persistence,
                entities,
                positions,
                presences,
                reposition,
                clients,
            ): Self::SystemData<'_>,
        ) {
            #[cfg(feature = "persistent_world")]
            let mut terrain_persistence = terrain_persistence;

            let mut ctx = ApplyContext {
                terrain_overrides: &mut terrain_overrides,
                terrain: &mut terrain,
                terrain_changes: &mut terrain_changes,
                chunk_generator: &mut chunk_generator,
                slow_jobs: &slow_jobs,
                world: &world,
                index: &index,
                time_of_day: *time_of_day,
                calendar: &calendar,
                rtsim: &rtsim,
                #[cfg(feature = "persistent_world")]
                terrain_persistence: terrain_persistence.as_deref_mut(),
                #[cfg(not(feature = "persistent_world"))]
                terrain_persistence,
                entities,
                positions,
                presences,
                reposition,
                clients,
            };

            for ev in events {
                terrain_override::apply(ev.op, &mut ctx);
            }
        }
    }
}
