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

    /// `0.0` at the region's center, `1.0` at `radius`, and `None` outside
    /// the falloff band entirely (i.e. beyond `radius + edge`) -- unlike
    /// [`Self::blend_factor`], this is never flat inside `radius`, so it's
    /// what a crater's bowl-depth formula (or anything else that needs to
    /// taper smoothly from the center outward, rather than snap to full
    /// strength anywhere inside `radius`) should use. Values above `1.0`
    /// (inside the falloff band, past `radius` but within `radius + edge`)
    /// are meaningful too -- e.g. a crater's raised rim uses them.
    pub fn normalized_distance(&self, wpos: Vec2<i32>) -> Option<f32> {
        match self {
            OverrideRegion::Circle {
                center,
                radius,
                edge,
            } => {
                if *radius <= 0.0 {
                    return None;
                }
                let dist = wpos.map(|e| e as f32).distance(center.map(|e| e as f32));
                let far_edge = radius + edge.max(0.0);
                (dist <= far_edge).then_some(dist / radius)
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

/// A single shape contributing to a [`DamageOverride`]'s terrain-level
/// effect. `#[non_exhaustive]`: only a crater and a debris field exist so
/// far.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DamageShape {
    /// A bowl-shaped depression, `max_depth` deep at the region's center,
    /// tapering smoothly to `0.0` at `radius` (see
    /// [`OverrideRegion::normalized_distance`]), with an optional raised rim
    /// of `rim_height` just outside `radius`, tapering to `0.0` by the far
    /// edge of the falloff band.
    Crater { max_depth: f32, rim_height: f32 },
    /// A scattered field of felled trees/rubble, no terrain-height change.
    /// `rubble_density`/`felled_tree_chance` are per-candidate-position
    /// probabilities (see `world/src/layer/terrain_damage.rs`), not
    /// fractions of the region's area.
    Debris {
        rubble_density: f32,
        felled_tree_chance: f32,
    },
}

/// A terrain-damage override (craters, storm/blast debris) that heals back
/// to ambient over time as [`Self::heal_progress`] advances (see
/// `server/src/sys/terrain_damage_heal.rs`). Siblings, not replacements, of
/// [`ClimateOverride`] -- both payload kinds can be active over the same
/// region (though typically aren't the same override, since a single
/// override has exactly one payload).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DamageOverride {
    pub shapes: Vec<DamageShape>,
    /// 0..1: how far to blend surface/sub-surface color toward a
    /// scorched/burnt look, and whether to force snow off. Fades toward
    /// `0.0` as `heal_progress` rises, same as everything else here.
    pub scorch: f32,
    /// Multiplies tree/rock-placement-relevant density at `heal_progress ==
    /// 0.0`, lerped back toward `1.0` (no-op) as `heal_progress` rises.
    /// Reuses the exact mechanism [`ClimateOverride::tree_density_mul`]
    /// already threads through -- not a parallel path.
    pub vegetation_mul: f32,
    /// `0.0` fresh (just activated) .. `1.0` fully healed. The only field
    /// the healing scheduler advances; everything else on this struct is
    /// static for the override's lifetime.
    pub heal_progress: f32,
    /// How many discrete steps `heal_progress` takes from `0.0` to `1.0`
    /// (i.e. each scheduler step advances it by `1.0 / heal_stages`).
    pub heal_stages: u8,
    /// `TimeOfDay` units between healing steps.
    pub heal_interval: f64,
    /// `TimeOfDay` units at which the next healing step should occur.
    pub next_heal_at: f64,
}

/// `#[non_exhaustive]`: only climate and damage overrides exist so far.
/// Later, unrelated work is expected to add variants (a biome-profile
/// override, an authored bespoke event) — deliberately not stubbed out
/// speculatively here.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TerrainOverridePayload {
    Climate(ClimateOverride),
    Damage(DamageOverride),
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
            _ => None,
        }
    }

    pub fn damage(&self) -> Option<&DamageOverride> {
        match &self.payload {
            TerrainOverridePayload::Damage(damage) => Some(damage),
            _ => None,
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

    /// Blends `base_temp`/`base_humidity` (whatever ambient world-gen already
    /// computed at `wpos`) toward the governing active `Climate` override's
    /// value, computes its tree-density multiplier, AND computes the
    /// governing active `Damage` override's crater/rim/scorch/vegetation
    /// effect at `wpos` -- all from a SINGLE pass over `active` (one `for o
    /// in &self.active` loop tracking the best `Climate` candidate and the
    /// best `Damage` candidate side by side, each independently
    /// priority-maxed with first-registered-wins tie-breaking, exactly like
    /// the old climate-only scan). This is the method `world/src/column.rs`'s
    /// `ColumnGen::get` actually calls (once per block-column, the hottest
    /// per-chunk-generation path in the engine), specifically so it never
    /// pays for a second independent scan over `active` for the damage
    /// payload -- see this module's and `ColumnGen::get`'s own doc comments
    /// for why that matters.
    /// [`Self::climate_and_tree_density_mul_at`]/[`Self::climate_at`]/
    /// [`Self::tree_density_mul_at`]/[`Self::damage_effects_at`] are thin
    /// single-purpose wrappers around this for callers (tests, future
    /// one-off consumers, `world/src/lib.rs`'s chunk-level patch) that only
    /// want a subset of the outputs and don't care about paying for the scan
    /// twice.
    ///
    /// Called at COLUMN granularity (once per block-column), which is what
    /// gives every override's edge its soft radial falloff rather than a
    /// hard per-chunk cutoff. Returns the ambient climate inputs unchanged
    /// (and a `1.0` tree-density multiplier / [`DamageEffectsAt::neutral`])
    /// wherever no matching override applies.
    pub fn climate_tree_density_and_damage_at(
        &self,
        wpos: Vec2<i32>,
        base_temp: f32,
        base_humidity: f32,
    ) -> (f32, f32, f32, DamageEffectsAt) {
        let mut best_climate: Option<(&RegionalTerrainOverride, &ClimateOverride, f32)> = None;
        let mut best_damage: Option<(&RegionalTerrainOverride, &DamageOverride, f32)> = None;

        for o in &self.active {
            let blend = o.region.blend_factor(wpos);
            if blend <= 0.0 {
                continue;
            }
            match &o.payload {
                TerrainOverridePayload::Climate(climate) => {
                    let replace = best_climate.is_none_or(|(best, ..)| o.priority > best.priority);
                    if replace {
                        best_climate = Some((o, climate, blend));
                    }
                },
                TerrainOverridePayload::Damage(damage) => {
                    let replace = best_damage.is_none_or(|(best, ..)| o.priority > best.priority);
                    if replace {
                        best_damage = Some((o, damage, blend));
                    }
                },
            }
        }

        let (temp, humidity, tree_density_mul) = match best_climate {
            Some((_, climate, blend)) => {
                let temp = climate
                    .temp
                    .map(|v| lerp(base_temp, v.target(base_temp), blend))
                    .unwrap_or(base_temp);
                let humidity = climate
                    .humidity
                    .map(|v| lerp(base_humidity, v.target(base_humidity), blend))
                    .unwrap_or(base_humidity);
                let tree_density_mul = climate
                    .tree_density_mul
                    .map(|mul| lerp(1.0, mul, blend))
                    .unwrap_or(1.0);
                (temp, humidity, tree_density_mul)
            },
            None => (base_temp, base_humidity, 1.0),
        };

        let damage = match best_damage {
            Some((o, damage, blend)) => damage.effects_at(&o.region, wpos, blend),
            None => DamageEffectsAt::neutral(),
        };

        (temp, humidity, tree_density_mul, damage)
    }

    /// See [`Self::climate_tree_density_and_damage_at`] -- this discards its
    /// damage-effect output. Prefer the combined method in a hot loop that
    /// needs both.
    pub fn climate_and_tree_density_mul_at(
        &self,
        wpos: Vec2<i32>,
        base_temp: f32,
        base_humidity: f32,
    ) -> (f32, f32, f32) {
        let (temp, humidity, tree_density_mul, _) =
            self.climate_tree_density_and_damage_at(wpos, base_temp, base_humidity);
        (temp, humidity, tree_density_mul)
    }

    /// See [`Self::climate_tree_density_and_damage_at`] -- this discards its
    /// `tree_density_mul`/damage output. Prefer the combined method in a hot
    /// loop that needs more than one piece.
    pub fn climate_at(&self, wpos: Vec2<i32>, base_temp: f32, base_humidity: f32) -> (f32, f32) {
        let (temp, humidity, _) =
            self.climate_and_tree_density_mul_at(wpos, base_temp, base_humidity);
        (temp, humidity)
    }

    /// See [`Self::climate_tree_density_and_damage_at`] -- this discards its
    /// temp/humidity/damage output. Prefer the combined method in a hot loop
    /// that needs more than one piece.
    pub fn tree_density_mul_at(&self, wpos: Vec2<i32>) -> f32 {
        let (_, _, tree_density_mul) = self.climate_and_tree_density_mul_at(wpos, 0.0, 0.0);
        tree_density_mul
    }

    /// See [`Self::climate_tree_density_and_damage_at`] -- this discards its
    /// climate output. Prefer the combined method in a hot loop that needs
    /// more than one piece.
    pub fn damage_effects_at(&self, wpos: Vec2<i32>) -> DamageEffectsAt {
        let (_, _, _, damage) = self.climate_tree_density_and_damage_at(wpos, 0.0, 0.0);
        damage
    }
}

/// The governing `Damage` override's terrain-level effect at one column,
/// computed by [`TerrainOverrides::climate_tree_density_and_damage_at`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DamageEffectsAt {
    /// World-unit depression to subtract from `alt`/`basement`/
    /// `riverless_alt` (bowl profile, `0.0` outside the override's
    /// `radius`).
    pub depth: f32,
    /// World-unit rise to add to `alt`/`basement`/`riverless_alt` just
    /// outside `radius`, tapering to `0.0` by the far edge of the falloff
    /// band (`0.0` inside `radius` or beyond the falloff band).
    pub rim: f32,
    /// 0..1: how far to blend surface/sub-surface color toward a
    /// scorched/burnt look at this column, already faded by `heal_progress`
    /// and radial blend strength.
    pub scorch: f32,
    /// Multiplies tree/rock-placement-relevant density; `1.0` = no change.
    /// Already faded toward `1.0` by `heal_progress` and radial blend
    /// strength.
    pub vegetation_mul: f32,
    /// Whether snow cover should be forced off at this column (tied to
    /// `scorch`, since a scorched crater/debris field shouldn't have snow
    /// sitting on it).
    pub force_no_snow: bool,
}

impl DamageEffectsAt {
    /// No active `Damage` override applies here.
    pub fn neutral() -> Self {
        Self {
            depth: 0.0,
            rim: 0.0,
            scorch: 0.0,
            vegetation_mul: 1.0,
            force_no_snow: false,
        }
    }
}

impl DamageOverride {
    /// `region`/`blend` must be the SAME [`OverrideRegion`] and
    /// [`OverrideRegion::blend_factor`] result the caller already picked
    /// this override for (see
    /// [`TerrainOverrides::climate_tree_density_and_damage_at`]) -- this
    /// never re-scans `active` itself.
    fn effects_at(&self, region: &OverrideRegion, wpos: Vec2<i32>, blend: f32) -> DamageEffectsAt {
        let heal = self.heal_progress.clamp(0.0, 1.0);
        let remaining = 1.0 - heal;

        let norm_dist = region.normalized_distance(wpos);
        // Inside the bowl radius (`t <= 1.0`): a smooth `(1 - t^2)` profile,
        // full depth at the center, zero right at `radius`.
        let bowl_t = norm_dist.filter(|t| *t <= 1.0);
        // At or outside the bowl radius but still inside the falloff band
        // (`t >= 1.0`): `blend_factor` is already exactly the "how much
        // rim" fraction here (`1.0` right at `radius`, tapering to `0.0` by
        // the far edge) -- reused rather than re-derived. `t == 1.0` (the
        // rim's own starting edge) is included here rather than in
        // `bowl_t` -- harmless either way for `depth` (the bowl profile is
        // already exactly `0.0` at `t == 1.0`), but required for `rim` to
        // read its full `rim_height` right at `radius`, not just past it.
        let rim_frac = if norm_dist.is_some_and(|t| t >= 1.0) {
            region.blend_factor(wpos)
        } else {
            0.0
        };

        let mut depth = 0.0f32;
        let mut rim = 0.0f32;
        for shape in &self.shapes {
            if let DamageShape::Crater {
                max_depth,
                rim_height,
            } = shape
            {
                if let Some(t) = bowl_t {
                    depth += max_depth * remaining * (1.0 - t * t).max(0.0);
                }
                rim += rim_height * remaining * rim_frac;
            }
        }

        // Scorch/vegetation effects apply to every shape kind (a debris
        // field is scorched too, not just a crater), scaled by both the
        // radial blend and how much healing remains.
        let fade = blend * remaining;
        let scorch = self.scorch * fade;

        DamageEffectsAt {
            depth,
            rim,
            scorch,
            vegetation_mul: lerp(1.0, self.vegetation_mul, fade),
            force_no_snow: scorch > 0.0,
        }
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

    /// `governing_climate_override`'s tie-break rule (`Ordering::Equal`
    /// remapped to `Ordering::Greater` in its `max_by` comparator) isn't
    /// obvious from reading the priority field alone -- pin down that an
    /// exact priority tie deterministically favors the first-registered
    /// override (earlier position in `active`), not the last one, and not
    /// something arbitrary/unstable.
    #[test]
    fn equal_priority_override_ties_are_broken_by_insertion_order() {
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
                    0,
                ),
            ],
        };
        let (temp, _) = overrides.climate_at(Vec2::zero(), 20.0, 0.5);
        assert_eq!(
            temp, 5.0,
            "on an exact priority tie the first-registered override (index 0 in `active`) must \
             win deterministically"
        );
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

    #[test]
    fn normalized_distance_is_zero_at_center_one_at_radius_and_none_past_the_falloff_band() {
        let region = OverrideRegion::Circle {
            center: Vec2::zero(),
            radius: 50.0,
            edge: 10.0,
        };
        assert_eq!(region.normalized_distance(Vec2::zero()), Some(0.0));
        assert_eq!(region.normalized_distance(Vec2::new(50, 0)), Some(1.0));
        assert!(region.normalized_distance(Vec2::new(55, 0)).unwrap() > 1.0);
        assert_eq!(region.normalized_distance(Vec2::new(61, 0)), None);
    }

    fn damage_override(
        id: u64,
        center: Vec2<i32>,
        radius: f32,
        edge: f32,
        shapes: Vec<DamageShape>,
        scorch: f32,
        vegetation_mul: f32,
        heal_progress: f32,
    ) -> RegionalTerrainOverride {
        RegionalTerrainOverride {
            id: TerrainOverrideId(id),
            region: OverrideRegion::Circle {
                center,
                radius,
                edge,
            },
            payload: TerrainOverridePayload::Damage(DamageOverride {
                shapes,
                scorch,
                vegetation_mul,
                heal_progress,
                heal_stages: 4,
                heal_interval: 600.0,
                next_heal_at: 600.0,
            }),
            priority: 0,
            activated_at: 0.0,
            wipe_player_edits: false,
            ephemeral: true,
        }
    }

    #[test]
    fn a_fresh_crater_is_deepest_at_its_center_and_zero_at_the_rim() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![damage_override(
                1,
                Vec2::zero(),
                50.0,
                10.0,
                vec![DamageShape::Crater {
                    max_depth: 20.0,
                    rim_height: 0.0,
                }],
                0.0,
                1.0,
                0.0,
            )],
        };

        let center = overrides.damage_effects_at(Vec2::zero());
        assert!(
            (center.depth - 20.0).abs() < 0.001,
            "a fresh crater must be at full `max_depth` at its own center, got {}",
            center.depth
        );

        let rim = overrides.damage_effects_at(Vec2::new(50, 0));
        assert!(
            rim.depth.abs() < 0.001,
            "the crater bowl must taper to zero depth exactly at `radius`, got {}",
            rim.depth
        );

        let outside = overrides.damage_effects_at(Vec2::new(1000, 0));
        assert_eq!(outside, DamageEffectsAt::neutral());
    }

    #[test]
    fn healing_progress_shrinks_crater_depth_and_lerps_vegetation_back_to_noop() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![damage_override(
                1,
                Vec2::zero(),
                50.0,
                10.0,
                vec![DamageShape::Crater {
                    max_depth: 20.0,
                    rim_height: 0.0,
                }],
                0.5,
                0.1,
                0.5,
            )],
        };

        let effects = overrides.damage_effects_at(Vec2::zero());
        assert!(
            (effects.depth - 10.0).abs() < 0.001,
            "50% healed must halve the fresh depth, got {}",
            effects.depth
        );
        assert!(
            (effects.vegetation_mul - 0.55).abs() < 0.001,
            "50% healed must halve the remaining distance from vegetation_mul back to 1.0, got {}",
            effects.vegetation_mul
        );
        assert!(effects.scorch > 0.0 && effects.scorch < 0.5);
        assert!(effects.force_no_snow);
    }

    #[test]
    fn a_fully_healed_override_has_no_effect_at_all() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![damage_override(
                1,
                Vec2::zero(),
                50.0,
                10.0,
                vec![DamageShape::Crater {
                    max_depth: 20.0,
                    rim_height: 5.0,
                }],
                1.0,
                0.1,
                1.0,
            )],
        };

        assert_eq!(
            overrides.damage_effects_at(Vec2::zero()),
            DamageEffectsAt::neutral()
        );
        assert_eq!(
            overrides.damage_effects_at(Vec2::new(55, 0)),
            DamageEffectsAt::neutral()
        );
    }

    #[test]
    fn a_rim_rises_just_outside_radius_and_tapers_to_zero_by_the_far_edge() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![damage_override(
                1,
                Vec2::zero(),
                50.0,
                10.0,
                vec![DamageShape::Crater {
                    max_depth: 20.0,
                    rim_height: 8.0,
                }],
                0.0,
                1.0,
                0.0,
            )],
        };

        let at_radius = overrides.damage_effects_at(Vec2::new(50, 0));
        assert!(
            (at_radius.rim - 8.0).abs() < 0.001,
            "the rim must be at full `rim_height` right at `radius`, got {}",
            at_radius.rim
        );
        let mid_band = overrides.damage_effects_at(Vec2::new(55, 0));
        assert!(mid_band.rim > 0.0 && mid_band.rim < 8.0);
        let far_edge = overrides.damage_effects_at(Vec2::new(60, 0));
        assert!(far_edge.rim.abs() < 0.001);
        let inside = overrides.damage_effects_at(Vec2::new(10, 0));
        assert_eq!(
            inside.rim, 0.0,
            "the rim must not apply inside the crater bowl itself"
        );
    }

    #[test]
    fn a_debris_field_scorches_and_reduces_vegetation_without_any_depth_change() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![damage_override(
                1,
                Vec2::zero(),
                50.0,
                10.0,
                vec![DamageShape::Debris {
                    rubble_density: 0.2,
                    felled_tree_chance: 0.05,
                }],
                0.6,
                0.3,
                0.0,
            )],
        };

        let effects = overrides.damage_effects_at(Vec2::zero());
        assert_eq!(effects.depth, 0.0);
        assert_eq!(effects.rim, 0.0);
        assert!(effects.scorch > 0.0);
        assert!(effects.vegetation_mul < 1.0);
        assert!(effects.force_no_snow);
    }

    #[test]
    fn climate_and_damage_overrides_are_governed_independently() {
        let overrides = TerrainOverrides {
            version: 1,
            active: vec![
                climate_override(
                    1,
                    Vec2::zero(),
                    50.0,
                    0.0,
                    Some(ClimateValue::Set(-10.0)),
                    0,
                ),
                damage_override(
                    2,
                    Vec2::zero(),
                    50.0,
                    10.0,
                    vec![DamageShape::Crater {
                        max_depth: 20.0,
                        rim_height: 0.0,
                    }],
                    0.0,
                    1.0,
                    0.0,
                ),
            ],
        };

        let (temp, _) = overrides.climate_at(Vec2::zero(), 20.0, 0.5);
        assert_eq!(
            temp, -10.0,
            "a Climate override must still govern climate even with a Damage override also active \
             over the same region"
        );
        let damage = overrides.damage_effects_at(Vec2::zero());
        assert!(
            (damage.depth - 20.0).abs() < 0.001,
            "a Damage override must still govern damage even with a Climate override also active \
             over the same region"
        );
    }
}
