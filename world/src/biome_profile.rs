//! The authored, named biome-profile catalog (a bespoke swamp, a volcano) --
//! see `common::terrain::regional_override::BiomeProfileOverride` for the
//! reference-by-id payload a `RegionalTerrainOverride` actually carries, and
//! `world/src/index.rs`'s `Index::biome_profiles`/`IndexRef::biome_profiles`
//! for how the real catalog is threaded through to world-gen.
//!
//! This mirrors the loading idiom `Colors`/`Features` (`world/src/lib.rs`,
//! `world/src/config.rs`) already use: a `FileAsset`-backed RON manifest,
//! loaded once per `Index` and cached, with a versioned schema string
//! checked at parse time so a stale/malformed catalog fails loudly instead of
//! silently doing the wrong thing.

use crate::all::ForestKind;
use common::{
    assets::{BoxedError, FileAsset, load_ron},
    terrain::{BlockKind, SpriteKind},
};
use serde::Deserialize;
use std::borrow::Cow;

/// The full shipped catalog (`assets/world/manifests/biome_profiles.ron`).
#[derive(Debug, Deserialize)]
pub struct BiomeProfiles {
    pub schema: String,
    pub entries: Vec<BiomeProfile>,
}

impl FileAsset for BiomeProfiles {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

/// The schema this crate knows how to read. Bumped (and validated against)
/// whenever the shape of [`BiomeProfile`] changes in a way that would
/// silently misinterpret an older catalog.
pub const BIOME_PROFILES_SCHEMA: &str = "xindeler_open_world.authored_biome_profiles.v1";

/// One authored biome (a bespoke swamp, a volcano). Referenced by id from
/// `common::terrain::regional_override::BiomeProfileOverride::profile` --
/// this struct itself never crosses into `common` (it uses `world`-only
/// types like [`ForestKind`]), which is exactly why the override payload
/// only carries a `String` id rather than this data inlined.
#[derive(Debug, Deserialize)]
pub struct BiomeProfile {
    pub id: String,
    /// Ground surface color this profile blends the ambient ground color
    /// toward (see `world/src/column.rs`'s `ColumnGen::get`), the same way
    /// `DamageOverride::scorch` already blends toward `Colors::scorch`.
    pub ground: (f32, f32, f32),
    /// Sub-surface color, blended the same way as `ground`.
    pub sub_surface: (f32, f32, f32),
    /// If set, forces this exact block kind at the surface wherever this
    /// profile governs (e.g. `Rock` for a volcano's bare slopes), bypassing
    /// the normal Earth/Snow/Grass decision entirely (see
    /// `world/src/block.rs`'s `BlockGen::get_with_z_cache`).
    pub surface_block: Option<BlockKind>,
    /// Forces snow off wherever this profile governs, the same way
    /// `DamageOverride`'s scorch effect already does.
    #[serde(default)]
    pub force_no_snow: bool,
    /// Multiplies tree/vegetation density wherever this profile governs,
    /// lerped in by radial blend × the override's own `intensity` -- reuses
    /// the exact same multiplier mechanism `ClimateOverride::tree_density_mul`
    /// / `DamageOverride::vegetation_mul` already thread through, rather
    /// than a parallel path. `1.0` = no change.
    #[serde(default = "default_tree_density_mul")]
    pub tree_density_mul: f32,
    /// A weighted mini-lottery of forest species this profile places
    /// instead of the ambient temperature/humidity-driven lottery, when
    /// nonempty. Empty means this profile has no opinion on tree species
    /// (the ambient lottery still governs).
    #[serde(default)]
    pub forest: Vec<(ForestKind, f32)>,
    /// How many blocks ABOVE this profile's own ambient ground altitude its
    /// water/lava should flood, if it floods at all -- e.g. `2.0` means the
    /// flood's surface sits 2 blocks above where the ground would otherwise
    /// be. Always non-negative (see [`BiomeProfiles::validate`]); a profile
    /// with no flood at all leaves this `None`. This is a RELATIVE value
    /// read straight from the catalog -- baking it to an absolute world-z
    /// (`flood_to = ambient_ground_altitude + flood_depth`,
    /// `BiomeProfileOverride::flood_to`) happens once, at override
    /// activation time, in `server/src/terrain_override.rs`, never here.
    pub flood_depth: Option<f32>,
    /// The block kind that fills a flooded area (`Water` for a swamp,
    /// `Lava` for a volcano). Only consulted when the governing override
    /// actually has a baked `flood_to` -- see
    /// `world/src/block.rs`/`world/src/column.rs`.
    #[serde(default = "default_flood_block")]
    pub flood_block: BlockKind,
    /// Sprite kinds the normal scatter pass (`world/src/layer/scatter.rs`)
    /// must never place wherever this profile governs, regardless of what
    /// density its own formula would otherwise compute.
    #[serde(default)]
    pub scatter_deny: Vec<SpriteKind>,
    /// Multiplies a scatter entry's own computed density wherever this
    /// profile governs. A kind not listed here gets a `1.0` (no-op)
    /// multiplier -- this only ever boosts/dampens existing `ScatterConfig`
    /// entries, never adds new ones.
    #[serde(default)]
    pub scatter_boost: Vec<(SpriteKind, f32)>,
}

fn default_tree_density_mul() -> f32 { 1.0 }

fn default_flood_block() -> BlockKind { BlockKind::Water }

impl BiomeProfiles {
    /// Validates schema, id uniqueness/non-emptiness, `flood_depth`
    /// non-negativity, and that every authored weight is finite -- mirrors
    /// `world/src/civ/mod.rs`'s `AuthoredCromatolisLandmarkProfiles::validate`
    /// in spirit (schema check + structural invariants a RON file can't
    /// enforce on its own).
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != BIOME_PROFILES_SCHEMA {
            return Err(format!(
                "expected schema {BIOME_PROFILES_SCHEMA}, got {}",
                self.schema
            ));
        }

        let mut ids = std::collections::HashSet::new();
        for profile in &self.entries {
            if profile.id.is_empty() || !ids.insert(profile.id.as_str()) {
                return Err(format!(
                    "duplicate or empty biome profile id {}",
                    profile.id
                ));
            }
            if let Some(flood_depth) = profile.flood_depth
                && flood_depth < 0.0
            {
                return Err(format!(
                    "biome profile {} has a negative flood_depth ({flood_depth}); flood_depth is \
                     a magnitude, sign is implied by the flood mechanism",
                    profile.id
                ));
            }
            if !profile.tree_density_mul.is_finite() || profile.tree_density_mul < 0.0 {
                return Err(format!(
                    "biome profile {} has a non-finite or negative tree_density_mul",
                    profile.id
                ));
            }
            for (kind, weight) in &profile.forest {
                if !weight.is_finite() || *weight < 0.0 {
                    return Err(format!(
                        "biome profile {}'s forest entry {kind:?} has a non-finite or negative \
                         weight",
                        profile.id
                    ));
                }
            }
            for (kind, weight) in &profile.scatter_boost {
                if !weight.is_finite() || *weight < 0.0 {
                    return Err(format!(
                        "biome profile {}'s scatter_boost entry {kind:?} has a non-finite or \
                         negative weight",
                        profile.id
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::assets::load_ron;

    fn real_biome_profiles() -> BiomeProfiles {
        load_ron(include_bytes!(
            "../../assets/world/manifests/biome_profiles.ron"
        ))
        .expect("the real biome profiles catalog must parse")
    }

    #[test]
    fn real_biome_profiles_catalog_parses_and_validates_without_panicking() {
        let profiles = real_biome_profiles();
        assert_eq!(profiles.schema, BIOME_PROFILES_SCHEMA);
        profiles
            .validate()
            .expect("the real biome profiles catalog must be internally valid");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut profiles = real_biome_profiles();
        let first = profiles.entries[0].id.clone();
        profiles.entries.push(BiomeProfile {
            id: first,
            ground: (0.0, 0.0, 0.0),
            sub_surface: (0.0, 0.0, 0.0),
            surface_block: None,
            force_no_snow: false,
            tree_density_mul: 1.0,
            forest: Vec::new(),
            flood_depth: None,
            flood_block: BlockKind::Water,
            scatter_deny: Vec::new(),
            scatter_boost: Vec::new(),
        });
        assert!(profiles.validate().is_err());
    }

    #[test]
    fn negative_flood_depth_is_rejected() {
        let mut profiles = real_biome_profiles();
        profiles.entries[0].flood_depth = Some(-1.0);
        assert!(profiles.validate().is_err());
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let mut profiles = real_biome_profiles();
        profiles.schema = "not_the_real_schema".to_string();
        assert!(profiles.validate().is_err());
    }
}
