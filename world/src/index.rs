use crate::{
    BiomeProfiles, Colors, Features,
    biome_profile::BIOME_PROFILES_SCHEMA,
    layer::{
        cromatolis_aerial_citadel::AerialCitadelConfig,
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
use tracing::warn;

const WORLD_COLORS_MANIFEST: &str = "world.style.colors";
const WORLD_FEATURES_MANIFEST: &str = "world.features";
const WORLD_BIOME_PROFILES_MANIFEST: &str = "world.manifests.biome_profiles";

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
    /// The authored Aerial Citadel's geometry config, lazily loaded and
    /// cached the first time a chunk needs it (`None` if the asset fails to
    /// load or validate). Same per-`Index` rationale as `cromatolis_interiors`
    /// above.
    pub(crate) cromatolis_aerial_citadel: OnceLock<Option<AerialCitadelConfig>>,
    colors: AssetHandle<Arc<Colors>>,
    features: AssetHandle<Arc<Features>>,
    /// The authored biome-profile catalog (see `crate::biome_profile`),
    /// mirroring `colors`/`features`'s exact loading idiom -- a
    /// `RegionalTerrainOverride`'s `BiomeProfile` payload only ever carries a
    /// catalog id (`common::terrain::regional_override::BiomeProfileOverride
    /// ::profile`); this is what resolves it.
    biome_profiles: AssetHandle<Arc<BiomeProfiles>>,
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
    biome_profiles: Arc<BiomeProfiles>,
    colors_reload_watcher: ReloadWatcher,
    features_reload_watcher: ReloadWatcher,
    biome_profiles_reload_watcher: ReloadWatcher,
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
    pub biome_profiles: &'a BiomeProfiles,
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
        let biome_profiles = Arc::<BiomeProfiles>::load_expect(WORLD_BIOME_PROFILES_MANIFEST);
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
            cromatolis_aerial_citadel: OnceLock::new(),
            colors,
            features,
            biome_profiles,
        }
    }

    pub fn colors(&self) -> impl Deref<Target = Arc<Colors>> + '_ { self.colors.read() }

    pub fn features(&self) -> impl Deref<Target = Arc<Features>> + '_ { self.features.read() }

    pub fn biome_profiles(&self) -> impl Deref<Target = Arc<BiomeProfiles>> + '_ {
        self.biome_profiles.read()
    }

    pub fn get_site_prices(&self, site_id: SiteId) -> Option<SitePrices> {
        self.sites
            .recreate_id(site_id)
            .map(|i| self.sites.get(i))
            .and_then(|s| s.economy.as_ref())
            .map(|econ| econ.get_site_prices())
    }
}

/// Validates a freshly-loaded/reloaded [`BiomeProfiles`] catalog, warning
/// and falling back to an empty catalog on failure -- mirrors
/// `world/src/civ/mod.rs`'s `SettlementTemplateContract`/
/// `AuthoredCromatolisLandmarkProfiles` validate-then-fall-back idiom
/// exactly (load, then `.validate()`, then warn+substitute a harmless
/// fallback on failure rather than propagating the error), so a stale or
/// malformed catalog fails loudly (a warning, not silence) without taking
/// down the whole server -- no `BiomeProfile` override will resolve to any
/// effect until it's fixed, but everything else keeps working.
///
/// Called from [`IndexOwned::new`] and [`IndexOwned::reload_if_changed`]
/// (not [`Index::new`]) because those are what actually clone the value out
/// of the shared, `'static`, immutable-from-outside [`AssetHandle`] and hand
/// it to every real reader (`IndexRef::biome_profiles`,
/// `Index::biome_profiles`) -- `Index` itself only ever holds the handle,
/// so there is nowhere to install a fallback if it validated there instead.
fn validated_biome_profiles(profiles: Arc<BiomeProfiles>) -> Arc<BiomeProfiles> {
    match profiles.validate() {
        Ok(()) => profiles,
        Err(err) => {
            warn!(
                ?err,
                "Could not validate the biome profiles catalog; falling back to an empty catalog \
                 (no BiomeProfile override will resolve to any effect until this is fixed)"
            );
            Arc::new(BiomeProfiles {
                schema: BIOME_PROFILES_SCHEMA.to_string(),
                entries: Vec::new(),
            })
        },
    }
}

impl IndexOwned {
    pub fn new(index: Index) -> Self {
        let colors = index.colors.cloned();
        let features = index.features.cloned();
        let biome_profiles = validated_biome_profiles(index.biome_profiles.cloned());
        let colors_reload_watcher = index.colors.reload_watcher();
        let features_reload_watcher = index.features.reload_watcher();
        let biome_profiles_reload_watcher = index.biome_profiles.reload_watcher();

        Self {
            index: Arc::new(index),
            colors,
            features,
            biome_profiles,
            colors_reload_watcher,
            features_reload_watcher,
            biome_profiles_reload_watcher,
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
        let biome_profiles_reloaded = self.biome_profiles_reload_watcher.reloaded();
        let reloaded = colors_reloaded || features_reloaded || biome_profiles_reloaded;
        reloaded.then(move || {
            // Reload the fields from the asset handle, which is updated automatically
            self.colors = self.index.colors.cloned();
            self.features = self.index.features.cloned();
            self.biome_profiles = validated_biome_profiles(self.index.biome_profiles.cloned());
            // Update wildlife spawns which is based on base_density in features
            reload(self)
        })
    }

    pub fn as_index_ref(&self) -> IndexRef<'_> {
        IndexRef {
            colors: &self.colors,
            features: &self.features,
            biome_profiles: &self.biome_profiles,
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

    /// Same independence requirement as `cromatolis_interiors` above,
    /// applied to `cromatolis_aerial_citadel`.
    #[test]
    fn cromatolis_aerial_citadel_cache_is_independent_per_index_instance() {
        let a = Index::new(0);
        let b = Index::new(0);

        a.cromatolis_aerial_citadel.get_or_init(|| None);
        assert!(
            a.cromatolis_aerial_citadel.get().is_some(),
            "index a should have its cache initialized"
        );
        assert!(
            b.cromatolis_aerial_citadel.get().is_none(),
            "index b's cache must still be empty -- it must not share state with index a"
        );
    }

    /// The real shipped catalog must validate cleanly and pass through
    /// unchanged -- confirms the happy path of `validated_biome_profiles`
    /// isn't itself broken before testing the fallback path below.
    #[test]
    fn validated_biome_profiles_passes_through_a_valid_catalog_unchanged() {
        let real = Arc::<BiomeProfiles>::load_expect(WORLD_BIOME_PROFILES_MANIFEST).cloned();
        let real_entry_count = real.entries.len();
        let validated = validated_biome_profiles(real);
        assert_eq!(validated.entries.len(), real_entry_count);
    }

    /// A malformed catalog (here: a duplicate id, one of the invariants
    /// `BiomeProfiles::validate` checks) must not panic or propagate an
    /// error -- it must warn and fall back to a harmless EMPTY catalog, the
    /// same graceful-degradation posture
    /// `SettlementTemplateContract`/`AuthoredCromatolisLandmarkProfiles`
    /// (`world/src/civ/mod.rs`) already establish for their own catalogs.
    #[test]
    fn validated_biome_profiles_falls_back_to_empty_on_a_malformed_catalog() {
        use crate::biome_profile::BiomeProfile;

        let malformed = Arc::new(BiomeProfiles {
            schema: BIOME_PROFILES_SCHEMA.to_string(),
            entries: vec![
                BiomeProfile {
                    id: "duplicate".to_string(),
                    ground: (0.0, 0.0, 0.0),
                    sub_surface: (0.0, 0.0, 0.0),
                    surface_block: None,
                    force_no_snow: false,
                    tree_density_mul: 1.0,
                    forest: Vec::new(),
                    flood_depth: None,
                    flood_block: common::terrain::BlockKind::Water,
                    scatter_deny: Vec::new(),
                    scatter_boost: Vec::new(),
                },
                BiomeProfile {
                    id: "duplicate".to_string(),
                    ground: (0.0, 0.0, 0.0),
                    sub_surface: (0.0, 0.0, 0.0),
                    surface_block: None,
                    force_no_snow: false,
                    tree_density_mul: 1.0,
                    forest: Vec::new(),
                    flood_depth: None,
                    flood_block: common::terrain::BlockKind::Water,
                    scatter_deny: Vec::new(),
                    scatter_boost: Vec::new(),
                },
            ],
        });
        // Sanity-check the fixture actually IS invalid before relying on
        // that to exercise the fallback branch below.
        assert!(malformed.validate().is_err());

        let validated = validated_biome_profiles(malformed);
        assert!(
            validated.entries.is_empty(),
            "a malformed catalog must fall back to an EMPTY catalog, not panic or keep the bad \
             data"
        );
    }
}
