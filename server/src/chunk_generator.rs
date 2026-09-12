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

/// The chunk's key, the per-chunk-key invalidation epoch (see
/// [`ChunkGenerator::chunk_version`]) that was current when this job was
/// requested (used to detect and discard a job that predates a
/// since-changed override touching THIS chunk specifically -- see
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
    /// Per-chunk-key invalidation epoch. Bumped (via
    /// [`Self::invalidate_chunk`]) ONLY for chunk keys a regional terrain
    /// override actually touches when it activates/deactivates -- this is
    /// deliberately *not* one global counter shared across every chunk on
    /// the server. A global counter would mean invalidating one region's
    /// handful of chunks discards and re-requests every chunk-generation
    /// job currently in flight anywhere on the server, regardless of
    /// whether it has anything to do with the changed region -- under a
    /// busy generation backlog that could double server-wide terrain-gen
    /// cost on every single override activation. Scoping invalidation to
    /// exactly the touched keys avoids that blast radius entirely. An
    /// absent key means epoch 0 (never invalidated) for both the requester
    /// and the checker, so ordinary chunk generation unrelated to any
    /// override is completely unaffected.
    chunk_versions: HashMap<Vec2<i32>, u64>,
}
impl ChunkGenerator {
    pub fn new(metrics: ChunkGenMetrics) -> Self {
        let (chunk_tx, chunk_rx) = crossbeam_channel::unbounded();
        Self {
            chunk_tx,
            chunk_rx,
            pending_chunks: HashMap::new(),
            metrics: Arc::new(metrics),
            chunk_versions: HashMap::new(),
        }
    }

    /// The current invalidation epoch for `key` (`0` if it has never been
    /// invalidated). Compared against the epoch a generation job for `key`
    /// was tagged with at request time to detect staleness.
    pub fn chunk_version(&self, key: Vec2<i32>) -> u64 {
        self.chunk_versions.get(&key).copied().unwrap_or(0)
    }

    /// Bumps `key`'s invalidation epoch, so any generation job for it
    /// already in flight (tagged with the previous epoch) will be detected
    /// as stale and re-requested once it completes. Called by
    /// `server::terrain_override::apply` for every chunk key a regional
    /// terrain override's region actually touches -- never for chunks
    /// outside that region.
    pub fn invalidate_chunk(&mut self, key: Vec2<i32>) {
        *self.chunk_versions.entry(key).or_insert(0) += 1;
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

        // Snapshotted on the calling thread (never the epoch at the time the
        // result eventually arrives) so a stale-in-flight job can be
        // detected and discarded by `recv_new_chunk`'s consumer -- see this
        // module's own `ChunkGenResult` and `chunk_versions` doc comments.
        // Deliberately keyed by THIS chunk, not by the override set as a
        // whole.
        let chunk_version = self.chunk_version(key);

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
            let _ = chunk_tx.send((key, chunk_version, payload));
        });
    }

    pub fn recv_new_chunk(&mut self) -> Option<ChunkGenResult> {
        // Make sure chunk wasn't cancelled and if it was check to see if there are more
        // chunks to receive
        while let Ok((key, chunk_version, res)) = self.chunk_rx.try_recv() {
            if self.pending_chunks.remove(&key).is_some() {
                self.metrics.chunks_served.inc();
                // TODO: do anything else if res is an Err?
                return Some((key, chunk_version, res));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The core guarantee `chunk_versions` exists for: invalidating one
    /// chunk key must never bump another, unrelated key's epoch. A global
    /// counter would fail this trivially (bumping ANY key would change the
    /// "current" value every other key's in-flight job gets compared
    /// against too) -- this is what would have silently re-requested every
    /// chunk-generation job server-wide on every single override
    /// activation/deactivation.
    #[test]
    fn invalidating_one_chunk_never_bumps_an_unrelated_chunks_version() {
        let metrics = ChunkGenMetrics::new(&prometheus::Registry::new()).unwrap();
        let mut generator = ChunkGenerator::new(metrics);

        let touched = Vec2::new(5, 5);
        let unrelated = Vec2::new(500, -500);

        assert_eq!(generator.chunk_version(touched), 0);
        assert_eq!(generator.chunk_version(unrelated), 0);

        generator.invalidate_chunk(touched);

        assert_eq!(
            generator.chunk_version(touched),
            1,
            "the invalidated key's own epoch must move"
        );
        assert_eq!(
            generator.chunk_version(unrelated),
            0,
            "an unrelated key's epoch must be completely unaffected by invalidating a different \
             key -- this is the whole point of per-chunk (not global) invalidation"
        );

        // A second invalidation of the SAME key (e.g. deactivate after
        // activate) must move it again, still without touching the
        // unrelated key.
        generator.invalidate_chunk(touched);
        assert_eq!(generator.chunk_version(touched), 2);
        assert_eq!(generator.chunk_version(unrelated), 0);
    }
}
