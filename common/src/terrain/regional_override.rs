//! The Regional Terrain Event Engine's shared payload types.
//!
//! Any region of the world can have its terrain-generation inputs overridden
//! (starting with temperature/humidity — see [`ClimateOverride`]), causing
//! affected chunks to regenerate honoring the override, and the override can
//! later be deactivated (chunks regenerate back to ambient). This module only
//! defines the *data*: how an override is described, and how to ask "what
//! applies at this position, blended by how much". Everything that actually
//! *acts* on it (world-gen hooks reading it at column/chunk-generation time,
//! the server-side activation/deactivation flow, chunk regen enqueueing) is
//! `world/` and `server/`'s job — see those crates' `terrain_override`-shaped
//! modules.
//!
//! This is deliberately a foundational, general mechanism: a family of future
//! features (weather-linked events, craters, authored bespoke biome events)
//! will build on it by adding new [`TerrainOverridePayload`] variants. Only
//! [`TerrainOverridePayload::Climate`] exists yet; the enum is
//! `#[non_exhaustive]` so those additions don't need a breaking change here.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use vek::*;

/// Identifies one active (or formerly active) [`RegionalTerrainOverride`].
///
/// A plain caller-assigned tag, not a `Store`-backed id (there is no owning
/// collection this must be unique within beyond `TerrainOverrides::active`
/// itself) — [`TerrainOverrideId::new_unique`] hands out process-unique
/// values for callers (e.g. the `/terrain_override` admin command) that don't
/// have a more meaningful id of their own to reuse.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TerrainOverrideId(pub u64);

impl TerrainOverrideId {
    pub fn new_unique() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The world-space region an override applies to.
///
/// `#[non_exhaustive]`: only a circle exists yet, but this is expected to
/// grow shapes (e.g. an authored polygon for a bespoke biome event) without
/// that being a breaking change for existing variants.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverrideRegion {
    /// `center`/`radius` are in block (world) coordinates. `edge` is the
    /// width, also in blocks, of the falloff band just outside `radius`
    /// where the override's effect linearly fades back to ambient — this is
    /// what gives every consumer (column-level blends, chunk-level
    /// selection) a soft edge instead of a hard cutoff ring.
    Circle {
        center: Vec2<i32>,
        radius: f32,
        edge: f32,
    },
}

impl OverrideRegion {
    /// A conservative world-space AABB containing every position this region
    /// can have ANY effect on (i.e. out to the far edge of the falloff
    /// band). Intended as a cheap prefilter before the precise
    /// [`Self::blend_factor`] calculation — mirrors a bounding-box hint
    /// checked before an exact geometric test, the same two-step shape
    /// `world/src/civ/mod.rs`'s proximity-requirement checks already use.
    pub fn bounds(&self) -> Aabr<i32> {
        match self {
            OverrideRegion::Circle {
                center,
                radius,
                edge,
            } => {
                let r = (radius + edge).ceil() as i32 + 1;
                Aabr {
                    min: *center - r,
                    max: *center + r,
                }
            },
        }
    }

    /// `1.0` at/within `radius` of the region's center, linearly falling to
    /// `0.0` over the `edge` band, and `0.0` beyond that. Never negative,
    /// never above `1.0`.
    pub fn blend_factor(&self, wpos: Vec2<i32>) -> f32 {
        match self {
            OverrideRegion::Circle {
                center,
                radius,
                edge,
            } => {
                let dist = wpos.map(|e| e as f32).distance(center.map(|e| e as f32));
                if dist <= *radius {
                    1.0
                } else if *edge <= 0.0 {
                    0.0
                } else {
                    (1.0 - (dist - radius) / edge).clamp(0.0, 1.0)
                }
            },
        }
    }

    /// Whether this region can have ANY effect (even a sliver of falloff) on
    /// the chunk at `chunk_key` (chunk coordinates, not world/block
    /// coordinates). A cheap AABB-vs-AABB test using [`Self::bounds`].
    pub fn touches_chunk(&self, chunk_key: Vec2<i32>, chunk_size: Vec2<u32>) -> bool {
        let chunk_min = chunk_key * chunk_size.as_::<i32>();
        let chunk_max = chunk_min + chunk_size.as_::<i32>();
        let chunk_aabr = Aabr {
            min: chunk_min,
            max: chunk_max,
        };
        let bounds = self.bounds();
        // Standard AABB overlap test.
        bounds.min.x < chunk_aabr.max.x
            && bounds.max.x > chunk_aabr.min.x
            && bounds.min.y < chunk_aabr.max.y
            && bounds.max.y > chunk_aabr.min.y
    }
}

/// One terrain-generation input to override, and to what value.
///
/// `Set` replaces the ambient value outright (at full blend strength);
/// `Offset` adds to whatever the ambient value already was at that position
/// — e.g. "+15C" near a fire event regardless of the base biome's own
/// temperature.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClimateValue {
    Set(f32),
    Offset(f32),
}

impl ClimateValue {
    /// The fully-overridden (blend factor 1.0) target value, given the
    /// ambient value it would otherwise blend from.
    pub fn target(&self, base: f32) -> f32 {
        match self {
            ClimateValue::Set(v) => *v,
            ClimateValue::Offset(v) => base + v,
        }
    }
}

/// A temperature/humidity (and, optionally, tree-density) override — the
/// only [`TerrainOverridePayload`] variant implemented so far. Future
/// payload kinds (a biome-profile override, a damage/crater layer) are
/// siblings of this, not changes to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClimateOverride {
    pub temp: Option<ClimateValue>,
    pub humidity: Option<ClimateValue>,
    /// Multiplies (never re-derives) `SimChunk::tree_density` /
    /// `ColumnSample::tree_density`. Map-gen's own tree-density formula
    /// needs gen-time-only inputs and can't be recomputed at override-apply
    /// time, so this is intentionally a scale, not a replacement.
    pub tree_density_mul: Option<f32>,
}

/// `#[non_exhaustive]`: only climate overrides exist so far. Later, unrelated
/// work is expected to add variants (a biome-profile override, a
/// damage/crater layer) — deliberately not stubbed out speculatively here.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TerrainOverridePayload {
    Climate(ClimateOverride),
}

/// One active (or, once deactivated, about-to-be-removed) regional terrain
/// override.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionalTerrainOverride {
    pub id: TerrainOverrideId,
    pub region: OverrideRegion,
    pub payload: TerrainOverridePayload,
    /// When multiple overrides' regions overlap a position, the
    /// highest-`priority` override wins outright for that position (ties
    /// broken by position in [`TerrainOverrides::active`]) — overlapping
    /// overrides are never blended together, only a single override's
    /// value against ambient.
    pub priority: i32,
    /// `Time`'s inner value (`common::resources::Time(pub f64)`) at
    /// activation. Not currently read by anything here (no override has a
    /// duration/expiry yet) — carried for the events that will build on
    /// this (e.g. a weather-linked event that expires after N seconds).
    pub activated_at: f64,
    /// If true, a player's persisted terrain edits under this override's
    /// region are cleared when it activates (gated behind the
    /// `persistent_world` feature and the `experimental_terrain_persistence`
    /// setting at the call site — this type itself has no opinion on
    /// whether that's currently possible).
    pub wipe_player_edits: bool,
    /// If true, this override is never written into `rtsim::data::Data` and
    /// so does not survive a server restart — intended for admin/test
    /// overrides (e.g. the `/terrain_override` command).
    pub ephemeral: bool,
}

impl RegionalTerrainOverride {
    pub fn climate(&self) -> Option<&ClimateOverride> {
        match &self.payload {
            TerrainOverridePayload::Climate(climate) => Some(climate),
        }
    }
}

/// A versioned snapshot of every currently-active regional terrain override.
///
/// `version` bumps on every activate/deactivate (see
/// `server/src/terrain_override.rs::apply`) — used to detect and discard a
/// chunk-generation job that was in flight before an override changed and
/// would otherwise land after, silently overwriting the correct regenerated
/// result with stale data (see `server/src/chunk_generator.rs` and
/// `server/src/sys/terrain.rs`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TerrainOverrides {
    pub version: u64,
    pub active: Vec<RegionalTerrainOverride>,
}

impl TerrainOverrides {
    /// Every active override whose region has any effect at all (even a
    /// sliver of falloff) on the given chunk. AABB-prefiltered first
    /// (`OverrideRegion::touches_chunk`) before any exact check.
    pub fn overrides_touching_chunk(
        &self,
        chunk_key: Vec2<i32>,
        chunk_size: Vec2<u32>,
    ) -> impl Iterator<Item = &RegionalTerrainOverride> {
        self.active
            .iter()
            .filter(move |o| o.region.touches_chunk(chunk_key, chunk_size))
    }

    /// Whether ANY active override touches the given chunk (even a sliver of
    /// falloff) — the cheap check used to decide whether a chunk's
    /// generation needs the `Cow::Owned` (cloned + patched) `SimChunk` path
    /// at all, or can stay on the zero-cost `Cow::Borrowed` path.
    pub fn touches_chunk(&self, chunk_key: Vec2<i32>, chunk_size: Vec2<u32>) -> bool {
        self.overrides_touching_chunk(chunk_key, chunk_size)
            .next()
            .is_some()
    }

    /// Picks the single override that governs climate at `wpos`, if any:
    /// the highest-`priority` active `Climate` override whose blend factor
    /// at `wpos` is greater than zero (ties broken by position in
    /// `active`). Overlapping overrides are never blended together — see
    /// `RegionalTerrainOverride::priority`'s doc comment.
    fn governing_climate_override(&self, wpos: Vec2<i32>) -> Option<(&ClimateOverride, f32)> {
        self.active
            .iter()
            .filter_map(|o| {
                let climate = o.climate()?;
                let blend = o.region.blend_factor(wpos);
                (blend > 0.0).then_some((o, climate, blend))
            })
            .max_by(|(a, ..), (b, ..)| {
                a.priority
                    .cmp(&b.priority)
                    .then(std::cmp::Ordering::Greater)
            })
            .map(|(_, climate, blend)| (climate, blend))
    }

    /// Blends `base_temp`/`base_humidity` (whatever ambient world-gen already
    /// computed at `wpos`) toward the governing active `Climate` override's
    /// value, by that override's radial falloff at `wpos`. Returns the
    /// inputs unchanged if no `Climate` override applies here.
    ///
    /// Called at COLUMN granularity (once per block-column, in
    /// `world/src/column.rs`'s `ColumnGen::get`), which is what gives the
    /// override's edge its soft radial falloff rather than a hard
    /// per-chunk cutoff.
    pub fn climate_at(&self, wpos: Vec2<i32>, base_temp: f32, base_humidity: f32) -> (f32, f32) {
        let Some((climate, blend)) = self.governing_climate_override(wpos) else {
            return (base_temp, base_humidity);
        };
        let temp = climate
            .temp
            .map(|v| lerp(base_temp, v.target(base_temp), blend))
            .unwrap_or(base_temp);
        let humidity = climate
            .humidity
            .map(|v| lerp(base_humidity, v.target(base_humidity), blend))
            .unwrap_or(base_humidity);
        (temp, humidity)
    }

    /// The tree-density multiplier the governing active `Climate` override
    /// applies at `wpos`, blended by that override's radial falloff (`1.0`,
    /// i.e. no-op, when none applies or the override doesn't set one).
    pub fn tree_density_mul_at(&self, wpos: Vec2<i32>) -> f32 {
        let Some((climate, blend)) = self.governing_climate_override(wpos) else {
            return 1.0;
        };
        climate
            .tree_density_mul
            .map(|mul| lerp(1.0, mul, blend))
            .unwrap_or(1.0)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

#[cfg(test)]
mod tests {
    use super::*;

    fn climate_override(
        id: u64,
        center: Vec2<i32>,
        radius: f32,
        edge: f32,
        temp: Option<ClimateValue>,
        priority: i32,
    ) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId(id),
            region: OverrideRegion::Circle {
                center,
                radius,
                edge,
            },
            payload: TerrainOverridePayload::Climate(ClimateOverride {
                temp,
                humidity: None,
                tree_density_mul: None,
            }),
            priority,
            activated_at: 0.0,
            wipe_player_edits: false,
            ephemeral: true,
        }
    }

    #[test]
    fn no_active_overrides_leaves_climate_unchanged() {
        let overrides = TerrainOverrides::default();
        assert_eq!(
            overrides.climate_at(Vec2::new(10, 10), 20.0, 0.5),
            (20.0, 0.5)
        );
    }

    #[test]
    fn inside_radius_is_fully_overridden() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![climate_override(
                1,
                Vec2::zero(),
                50.0,
                20.0,
                Some(ClimateValue::Set(-10.0)),
                0,
            )],
        };
        let (temp, _) = overrides.climate_at(Vec2::new(5, 0), 20.0, 0.5);
        assert_eq!(temp, -10.0);
    }

    #[test]
    fn falloff_band_blends_linearly_and_beyond_it_is_ambient() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![climate_override(
                1,
                Vec2::zero(),
                50.0,
                20.0,
                Some(ClimateValue::Set(-10.0)),
                0,
            )],
        };
        // Halfway through the 20-block falloff band: blend factor 0.5.
        let (temp_mid, _) = overrides.climate_at(Vec2::new(60, 0), 20.0, 0.5);
        assert!(
            (temp_mid - 5.0).abs() < 0.001,
            "expected the midpoint of a -10..20 blend at factor 0.5 to be 5.0, got {temp_mid}"
        );

        // Well beyond the falloff band: fully ambient.
        let (temp_far, humidity_far) = overrides.climate_at(Vec2::new(1000, 0), 20.0, 0.5);
        assert_eq!((temp_far, humidity_far), (20.0, 0.5));
    }

    #[test]
    fn offset_adds_to_whatever_ambient_already_was() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![climate_override(
                1,
                Vec2::zero(),
                50.0,
                0.0,
                Some(ClimateValue::Offset(15.0)),
                0,
            )],
        };
        let (temp, _) = overrides.climate_at(Vec2::new(0, 0), 3.0, 0.5);
        assert_eq!(temp, 18.0);
    }

    #[test]
    fn higher_priority_override_wins_when_regions_overlap() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![
                climate_override(1, Vec2::zero(), 100.0, 0.0, Some(ClimateValue::Set(5.0)), 0),
                climate_override(
                    2,
                    Vec2::zero(),
                    100.0,
                    0.0,
                    Some(ClimateValue::Set(99.0)),
                    10,
                ),
            ],
        };
        let (temp, _) = overrides.climate_at(Vec2::zero(), 20.0, 0.5);
        assert_eq!(temp, 99.0);
    }

    #[test]
    fn tree_density_mul_defaults_to_noop() {
        let overrides = TerrainOverrides::default();
        assert_eq!(overrides.tree_density_mul_at(Vec2::new(1, 1)), 1.0);
    }

    #[test]
    fn touches_chunk_uses_aabb_prefilter() {
        let region = OverrideRegion::Circle {
            center: Vec2::new(1000, 1000),
            radius: 10.0,
            edge: 5.0,
        };
        assert!(region.touches_chunk(Vec2::new(1000 / 32, 1000 / 32), Vec2::new(32, 32)));
        assert!(!region.touches_chunk(Vec2::new(0, 0), Vec2::new(32, 32)));
    }
}
