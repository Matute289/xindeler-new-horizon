use crate::metrics::ChunkGenMetrics;
#[cfg(feature = "worldgen")]
use crate::rtsim::RtSim;
#[cfg(not(feature = "worldgen"))]
use crate::test_world::{IndexOwned, World};
use common::{
    calendar::Calendar,
    generation::ChunkSupplement,
    resources::TimeOfDay,
    slowjob::SlowJobPool,
    terrain::{TerrainChunk, TerrainOverrides},
};
use hashbrown::{HashMap, hash_map::Entry};
use rayon::iter::ParallelIterator;
use specs::Entity as EcsEntity;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use vek::*;
#[cfg(feature = "worldgen")]
use world::{IndexOwned, World};

/// The chunk's key, the [`TerrainOverrides::version`] snapshot that was
/// active when this job was requested (used to detect and discard a job
/// that predates a since-changed override -- see
/// `server/src/sys/terrain.rs`'s consumer), and the actual generation
/// result.
type ChunkGenResult = (
    Vec2<i32>,
    u64,
    Result<(TerrainChunk, ChunkSupplement), Option<EcsEntity>>,
);

pub struct ChunkGenerator {
    chunk_tx: crossbeam_channel::Sender<ChunkGenResult>,
    chunk_rx: crossbeam_channel::Receiver<ChunkGenResult>,
    pending_chunks: HashMap<Vec2<i32>, Arc<AtomicBool>>,
    metrics: Arc<ChunkGenMetrics>,
}
impl ChunkGenerator {
    pub fn new(metrics: ChunkGenMetrics) -> Self {
        let (chunk_tx, chunk_rx) = crossbeam_channel::unbounded();
        Self {
            chunk_tx,
            chunk_rx,
            pending_chunks: HashMap::new(),
            metrics: Arc::new(metrics),
        }
    }

    pub fn generate_chunk(
        &mut self,
        entity: Option<EcsEntity>,
        key: Vec2<i32>,
        slowjob_pool: &SlowJobPool,
        world: Arc<World>,
        #[cfg(feature = "worldgen")] rtsim: &RtSim,
        #[cfg(not(feature = "worldgen"))] _rtsim: &(),
        index: IndexOwned,
        time: (TimeOfDay, Calendar),
        overrides: Option<Arc<TerrainOverrides>>,
    ) {
        let v = if let Entry::Vacant(v) = self.pending_chunks.entry(key) {
            v
        } else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        v.insert(Arc::clone(&cancel));
        let chunk_tx = self.chunk_tx.clone();
        self.metrics.chunks_requested.inc();

        // Get state for this chunk from rtsim
        #[cfg(feature = "worldgen")]
        let rtsim_resources = Some(rtsim.get_chunk_resources(key));
        #[cfg(not(feature = "worldgen"))]
        let rtsim_resources = None;

        // Snapshotted on the calling thread (never the version at the time
        // the result eventually arrives) so a stale-in-flight job can be
        // detected and discarded by `recv_new_chunk`'s consumer -- see this
        // module's own `ChunkGenResult` doc comment.
        let overrides_version = overrides.as_ref().map_or(0, |overrides| overrides.version);

        slowjob_pool.spawn("CHUNK_GENERATOR", move || {
            let index = index.as_index_ref();
            let payload = world
                .generate_chunk(
                    index,
                    key,
                    rtsim_resources,
                    || cancel.load(Ordering::Relaxed),
                    Some(time),
                    overrides.as_deref(),
                )
                // FIXME: Since only the first entity who cancels a chunk is notified, we end up
                // delaying chunk re-requests for up to 3 seconds for other clients, which isn't
                // great.  We *could* store all the other requesting clients here, but it could
                // bloat memory a lot.  Currently, this isn't much of an issue because we rarely
                // have large numbers of pending chunks, so most of them are likely to be nearby an
                // actual player most of the time, but that will eventually change.  In the future,
                // some solution that always pushes chunk updates to players (rather than waiting
                // for explicit requests) should adequately solve this kind of issue.
                .map_err(|_| entity);
            let _ = chunk_tx.send((key, overrides_version, payload));
        });
    }

    pub fn recv_new_chunk(&mut self) -> Option<ChunkGenResult> {
        // Make sure chunk wasn't cancelled and if it was check to see if there are more
        // chunks to receive
        while let Ok((key, overrides_version, res)) = self.chunk_rx.try_recv() {
            if self.pending_chunks.remove(&key).is_some() {
                self.metrics.chunks_served.inc();
                // TODO: do anything else if res is an Err?
                return Some((key, overrides_version, res));
            }
        }

        None
    }

    pub fn pending_chunks(&self) -> impl Iterator<Item = Vec2<i32>> + '_ {
        self.pending_chunks.keys().copied()
    }

    pub fn par_pending_chunks(&self) -> impl rayon::iter::ParallelIterator<Item = Vec2<i32>> + '_ {
        self.pending_chunks.par_keys().copied()
    }

    pub fn cancel_if_pending(&mut self, key: Vec2<i32>) {
        if let Some(cancel) = self.pending_chunks.remove(&key) {
            cancel.store(true, Ordering::Relaxed);
            self.metrics.chunks_canceled.inc();
        }
    }

    pub fn cancel_all(&mut self) {
        let metrics = Arc::clone(&self.metrics);
        self.pending_chunks.drain().for_each(|(_, cancel)| {
            cancel.store(true, Ordering::Relaxed);
            metrics.chunks_canceled.inc();
        });
    }
}
