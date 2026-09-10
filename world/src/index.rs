use crate::{
    Colors, Features,
    layer::{
        cromatolis_cave_features::GeneratedCave,
        cromatolis_interior::InteriorLayout,
        wildlife::{self, DensityFn, SpawnEntry},
    },
    site::{Site, economy::TradeInformation},
};
use common::{
    assets::{AssetExt, AssetHandle, ReloadWatcher, Ron},
    store::Store,
    trade::{SiteId, SitePrices},
};
use core::ops::Deref;
use noise::{Fbm, MultiFractal, Perlin, SuperSimplex};
use std::sync::{Arc, OnceLock};

const WORLD_COLORS_MANIFEST: &str = "world.style.colors";
const WORLD_FEATURES_MANIFEST: &str = "world.features";

pub struct Index {
    pub seed: u32,
    pub time: f32,
    pub noise: Noise,
    pub sites: Store<Site>,
    pub trade: TradeInformation,
    pub wildlife_spawns: Vec<(AssetHandle<Ron<SpawnEntry>>, DensityFn)>,
    /// Authored Cromatolis interior geometry (`the_undercompact`,
    /// `kharvun_reach`), lazily built and cached the first time a chunk
    /// needs it. Per-`Index` rather than a process-global `static` so a
    /// fresh world (a new `Index::new` call) never reuses a previous
    /// world's layout.
    pub(crate) cromatolis_interiors: OnceLock<Vec<InteriorLayout>>,
    /// Generic, size-class-scaled Cromatolis cave geometry, lazily built and
    /// cached the first time a chunk needs it. Same per-`Index` rationale as
    /// `cromatolis_interiors` above.
    pub(crate) cromatolis_cave_features: OnceLock<Vec<GeneratedCave>>,
    colors: AssetHandle<Arc<Colors>>,
    features: AssetHandle<Arc<Features>>,
}

/// An owned reference to indexed data.
///
/// The data are split out so that we can replace the colors without disturbing
/// the rest of the index, while also keeping all the data within a single
/// indirection.
#[derive(Clone)]
pub struct IndexOwned {
    colors: Arc<Colors>,
    features: Arc<Features>,
    colors_reload_watcher: ReloadWatcher,
    features_reload_watcher: ReloadWatcher,
    index: Arc<Index>,
}

impl Deref for IndexOwned {
    type Target = Index;

    fn deref(&self) -> &Self::Target { &self.index }
}

/// A shared reference to indexed data.
///
/// This is copyable and can be used from either style of index.
#[derive(Clone, Copy)]
pub struct IndexRef<'a> {
    pub colors: &'a Colors,
    pub features: &'a Features,
    pub index: &'a Index,
}

impl Deref for IndexRef<'_> {
    type Target = Index;

    fn deref(&self) -> &Self::Target { self.index }
}

impl Index {
    /// NOTE: Panics if the color manifest cannot be loaded.
    pub fn new(seed: u32) -> Self {
        let colors = Arc::<Colors>::load_expect(WORLD_COLORS_MANIFEST);
        let features = Arc::<Features>::load_expect(WORLD_FEATURES_MANIFEST);
        let wildlife_spawns = wildlife::spawn_manifest()
            .into_iter()
            .map(|(e, f)| (Ron::<SpawnEntry>::load_expect(e), f))
            .collect();

        Self {
            seed,
            time: 0.0,
            noise: Noise::new(seed),
            sites: Store::default(),
            trade: Default::default(),
            wildlife_spawns,
            cromatolis_interiors: OnceLock::new(),
            cromatolis_cave_features: OnceLock::new(),
            colors,
            features,
        }
    }

    pub fn colors(&self) -> impl Deref<Target = Arc<Colors>> + '_ { self.colors.read() }

    pub fn features(&self) -> impl Deref<Target = Arc<Features>> + '_ { self.features.read() }

    pub fn get_site_prices(&self, site_id: SiteId) -> Option<SitePrices> {
        self.sites
            .recreate_id(site_id)
            .map(|i| self.sites.get(i))
            .and_then(|s| s.economy.as_ref())
            .map(|econ| econ.get_site_prices())
    }
}

impl IndexOwned {
    pub fn new(index: Index) -> Self {
        let colors = index.colors.cloned();
        let features = index.features.cloned();
        let colors_reload_watcher = index.colors.reload_watcher();
        let features_reload_watcher = index.features.reload_watcher();

        Self {
            index: Arc::new(index),
            colors,
            features,
            colors_reload_watcher,
            features_reload_watcher,
        }
    }

    /// NOTE: Callback is called only when colors actually have to be reloaded.
    /// The server is responsible for making sure that all affected chunks are
    /// reloaded; a naive approach will just regenerate every chunk on the
    /// server, but it is possible that eventually we can find a better
    /// solution.
    ///
    /// Ideally, this should be called about once per tick.
    pub fn reload_if_changed<R>(&mut self, reload: impl FnOnce(&mut Self) -> R) -> Option<R> {
        let colors_reloaded = self.colors_reload_watcher.reloaded();
        let features_reloaded = self.features_reload_watcher.reloaded();
        let reloaded = colors_reloaded || features_reloaded;
        reloaded.then(move || {
            // Reload the fields from the asset handle, which is updated automatically
            self.colors = self.index.colors.cloned();
            self.features = self.index.features.cloned();
            // Update wildlife spawns which is based on base_density in features
            reload(self)
        })
    }

    pub fn as_index_ref(&self) -> IndexRef<'_> {
        IndexRef {
            colors: &self.colors,
            features: &self.features,
            index: &self.index,
        }
    }
}

pub struct Noise {
    pub cave_nz: SuperSimplex,
    pub scatter_nz: SuperSimplex,
    pub cave_fbm_nz: Fbm<Perlin>,
}

impl Noise {
    fn new(seed: u32) -> Self {
        Self {
            cave_nz: SuperSimplex::new(seed + 0),
            scatter_nz: SuperSimplex::new(seed + 1),
            cave_fbm_nz: Fbm::new(seed + 2).set_octaves(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cromatolis_interiors` must be cached per-`Index` (per world), not
    /// in a process-global `static`: a binary that calls `World::generate`
    /// (and so `Index::new`) more than once in the same process -- a batch
    /// export tool, a multi-world test harness -- must get an independently
    /// initialized cache for each world, never the first world's leftover
    /// value.
    #[test]
    fn cromatolis_interiors_cache_is_independent_per_index_instance() {
        let a = Index::new(0);
        let b = Index::new(0);

        a.cromatolis_interiors.get_or_init(Vec::new);
        assert!(
            a.cromatolis_interiors.get().is_some(),
            "index a should have its cache initialized"
        );
        assert!(
            b.cromatolis_interiors.get().is_none(),
            "index b's cache must still be empty -- it must not share state with index a"
        );
    }

    /// Same independence requirement as `cromatolis_interiors` above,
    /// applied to `cromatolis_cave_features`.
    #[test]
    fn cromatolis_cave_features_cache_is_independent_per_index_instance() {
        let a = Index::new(0);
        let b = Index::new(0);

        a.cromatolis_cave_features.get_or_init(Vec::new);
        assert!(
            a.cromatolis_cave_features.get().is_some(),
            "index a should have its cache initialized"
        );
        assert!(
            b.cromatolis_cave_features.get().is_none(),
            "index b's cache must still be empty -- it must not share state with index a"
        );
    }
}
