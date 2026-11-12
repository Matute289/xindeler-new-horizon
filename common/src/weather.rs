use std::fmt;

use serde::{Deserialize, Serialize};
use vek::{Lerp, Vec2, Vec3};

use crate::{
    assets::{AssetExt, Ron},
    grid::Grid,
    terrain::{SNOW_TEMP, TerrainChunkSize, TerrainGrid},
    vol::RectVolSize,
};

/// Fall speed of rain, in blocks per second.
const RAIN_FALL_RATE: f32 = 30.0;
/// Fall speed of snow, in blocks per second. An order of magnitude slower than
/// rain — this is what makes falling snow read as snow rather than as
/// white-tinted rain.
const SNOW_FALL_RATE: f32 = 3.0;
/// Extra horizontal drift snow picks up from the wind relative to rain, which
/// falls close to straight down. Snowflakes have far more drag per unit mass.
const SNOW_WIND_DRIFT: f32 = 2.5;

/// Designer-facing tuning for how snow and fog form and how much they obscure.
///
/// Loaded from `assets/common/weather_tuning.ron`, mirroring how `CombatTuning`
/// is read in `Attack::apply_attack`. `#[serde(default)]` keeps older and newer
/// copies of the asset mutually loadable.
///
/// Deliberately does *not* hold the precipitation fall rates
/// ([`RAIN_FALL_RATE`] and friends). They are look constants rather than
/// balance numbers, and the voxygen client re-derives the precipitation axis
/// from them at three separate points in a frame — `Scene::maintain`, which
/// renders the rain-occlusion map, and `FigureMgr`/`Terrain`, which sample it —
/// so keeping them out of a hot-reloadable asset removes one way those three
/// could disagree within a frame.
///
/// Be precise about what that does and does not buy, though: `snow_temp_band`
/// below *is* hot-reloadable and also feeds that same axis, so the axis is not
/// actually pinned today. Resolving it once per frame and threading it through
/// `SceneData` (as `wind_vel` already is) would pin it properly, and would then
/// make the fall rates perfectly safe to move here. Until that happens this
/// split is a reasonable default, not a guarantee.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default)]
pub struct WeatherTuning {
    /// Width, in abstract world-gen temperature units, of the rain→snow
    /// transition band above `SNOW_TEMP`. Precipitation is fully snow at
    /// `SNOW_TEMP` and below, fully rain at `SNOW_TEMP + snow_temp_band` and
    /// above, and sleet between.
    ///
    /// Non-zero so that crossing a biome boundary sleets rather than snapping
    /// between two completely different visuals at one chunk border.
    pub snow_temp_band: f32,
    /// Cloud cover below which no fog forms at all.
    pub fog_cloud_min: f32,
    /// Cloud cover at and above which fog is at full thickness.
    pub fog_cloud_max: f32,
    /// Precipitation above which rain starts clearing fog out.
    pub fog_rain_min: f32,
    /// Precipitation at and above which rain has cleared fog entirely.
    pub fog_rain_max: f32,
    /// Sight distance in the thickest possible snowfall, as a proportion of
    /// the clear-air value.
    pub snow_min_visibility: f32,
    /// Sight distance in the thickest possible fog, likewise.
    pub fog_min_visibility: f32,
}

impl Default for WeatherTuning {
    fn default() -> Self {
        Self {
            snow_temp_band: 0.2,
            fog_cloud_min: 0.45,
            fog_cloud_max: 0.9,
            fog_rain_min: 0.05,
            fog_rain_max: 0.35,
            snow_min_visibility: 0.5,
            fog_min_visibility: 0.35,
        }
    }
}

impl WeatherTuning {
    /// Reads the cached tuning asset, with the visibility floors clamped into
    /// the range the rest of this module assumes.
    ///
    /// Cheap but not free (a keyed asset-cache lookup), so callers that use it
    /// across many weather cells or many entities should hoist this out of
    /// their loop rather than calling it per item.
    ///
    /// The clamp is not belt-and-braces: this asset **hot-reloads**, so a typo
    /// reaches a running server without passing any test. A negative
    /// `snow_min_visibility` would make [`Weather::visibility_factor`] return a
    /// negative multiplier, and since every sight check squares the resulting
    /// distance (`dist_sqrd < sight_dist.powi(2)`) the sign is thrown away —
    /// a blizzard would silently make NPCs see *further*.
    pub fn load() -> Self {
        let mut tuning = Ron::<Self>::load_expect("common.weather_tuning").read().0;
        tuning.snow_min_visibility = clamp_unit(tuning.snow_min_visibility);
        tuning.fog_min_visibility = clamp_unit(tuning.fog_min_visibility);
        tuning
    }
}

/// Clamps into `0.0..=1.0`, mapping NaN to `1.0` (the no-effect value) rather
/// than letting it propagate — `f32::clamp` passes NaN straight through.
fn clamp_unit(v: f32) -> f32 { if v.is_nan() { 1.0 } else { v.clamp(0.0, 1.0) } }

/// Fraction of precipitation that falls as snow rather than rain at a given
/// abstract world-gen temperature: `1.0` is all snow, `0.0` all rain.
///
/// This is a property of *place*, not of the weather cell — `TerrainChunkMeta`
/// already carries `temp` at 32-block resolution and already ships to the
/// client with every terrain chunk, so rain-vs-snow needs no new networked
/// field and no change to [`CompressedWeather`].
pub fn snow_factor_at_temp(temp: f32, tuning: &WeatherTuning) -> f32 {
    let band = if tuning.snow_temp_band > f32::EPSILON {
        tuning.snow_temp_band
    } else {
        // A zero or negative band would divide by zero or invert the ramp.
        f32::EPSILON
    };
    if !temp.is_finite() {
        return 0.0;
    }
    (1.0 - (temp - SNOW_TEMP) / band).clamp(0.0, 1.0)
}

/// [`snow_factor_at_temp`] for a world position, reading the baked per-chunk
/// temperature out of loaded terrain.
///
/// Returns `None` when the chunk covering `wpos` is not loaded *or* lies
/// outside the map — the caller decides what to do about it (the client falls
/// back to the player's own value so distant, unloaded cells don't render a
/// hard snow/rain seam). Uses the `_real` lookup on purpose: the ordinary one
/// answers out-of-map keys with the void default chunk, whose `temp` of `0.0`
/// is *hot* on this scale and would put a rain seam along the world border.
pub fn snow_factor_at(
    terrain: &TerrainGrid,
    wpos: Vec2<f32>,
    tuning: &WeatherTuning,
) -> Option<f32> {
    let chunk_pos = wpos.map2(TerrainChunkSize::RECT_SIZE, |e, sz| {
        (e / sz as f32).floor() as i32
    });
    terrain
        .get_key_real(chunk_pos)
        .map(|chunk| snow_factor_at_temp(chunk.meta().temp(), tuning))
}

/// Hermite interpolation between two edges, matching GLSL's `smoothstep` so
/// the Rust and shader sides of a weather term can't drift apart.
///
/// Degenerate and non-finite edges collapse to a hard step instead of dividing
/// by zero. This matters because both edge pairs come from a **hot-reloadable**
/// asset: `fog_cloud_min == fog_cloud_max` would otherwise yield `NaN`, which
/// `f32::clamp` propagates rather than pins, and that `NaN` flows through
/// `fog_density` → `visibility_factor` → `psyche.sight_dist`, where
/// `dist_sqrd < NaN` is always `false` — every NPC in the world would go
/// permanently blind with no panic and no log line.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let denom = edge1 - edge0;
    if !denom.is_finite() || denom.abs() <= f32::EPSILON {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / denom).clamp(0.0, 1.0);
    if t.is_nan() {
        return 0.0;
    }
    t * t * (3.0 - 2.0 * t)
}

/// Weather::default is Clear, 0 degrees C and no wind
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Weather {
    /// Clouds currently in the area between 0 and 1
    pub cloud: f32,
    /// Rain per time, between 0 and 1
    pub rain: f32,
    /// Wind velocity in block / second
    pub wind: Vec2<f32>,
}

impl Weather {
    pub fn new(cloud: f32, rain: f32, wind: Vec2<f32>) -> Self { Self { cloud, rain, wind } }

    pub fn get_kind(&self) -> WeatherKind {
        // Over 24.5 m/s wind is a storm
        if self.wind.magnitude_squared() >= 24.5f32.powi(2) {
            WeatherKind::Storm
        } else if (0.1..=1.0).contains(&self.rain) {
            WeatherKind::Rain
        } else if (0.2..=1.0).contains(&self.cloud) {
            WeatherKind::Cloudy
        } else {
            WeatherKind::Clear
        }
    }

    /// [`Self::get_kind`], but aware of where the weather is happening:
    /// precipitation over cold ground is [`WeatherKind::Snow`] rather than
    /// [`WeatherKind::Rain`].
    ///
    /// `snow_factor` comes from [`snow_factor_at_temp`]. Kept separate from
    /// `get_kind` rather than replacing it so that call sites with no position
    /// to hand (and the `Weather`-only wire type) keep their existing
    /// behaviour.
    ///
    /// Only `Rain` is rewritten, deliberately. A cold `Storm` is a blizzard,
    /// and `Storm` is the more useful label for it — the wind is what defines
    /// the experience, and content keyed on `Storm` should keep matching.
    /// Likewise precipitation too light to register as `Rain` stays `Cloudy`;
    /// flurries under overcast are not a snow *event*.
    ///
    /// The `0.5` below is not an independent knob and deliberately does not
    /// live in [`WeatherTuning`]: [`snow_factor_at_temp`] ramps linearly from
    /// `1.0` at `SNOW_TEMP` to `0.0` at `SNOW_TEMP + snow_temp_band`, so `0.5`
    /// is by construction the midpoint of the sleet band. Naming the sleet
    /// band is what a designer retunes; where its middle is, is arithmetic.
    pub fn get_kind_at(&self, snow_factor: f32) -> WeatherKind {
        match self.get_kind() {
            WeatherKind::Rain if snow_factor >= 0.5 => WeatherKind::Snow,
            kind => kind,
        }
    }

    /// The portion of [`Self::rain`] falling as snow.
    pub fn snow(&self, snow_factor: f32) -> f32 { self.rain * snow_factor.clamp(0.0, 1.0) }

    /// The portion of [`Self::rain`] falling as liquid rain.
    ///
    /// `rain` itself remains "precipitation per time" regardless of form, and
    /// consumers split on which meaning they want. Things that care how *much*
    /// falls — the rain-occlusion pass and rtsim's [`WeatherGrid::is_raining`]
    /// — keep using `rain` and are correct for snow as they stand. Things that
    /// care about *water* specifically — wet-surface sheen, puddle ripples,
    /// footstep splashes, the rain ambience loop — must use this instead, or
    /// they will treat a blizzard as a downpour.
    pub fn liquid_rain(&self, snow_factor: f32) -> f32 {
        self.rain * (1.0 - snow_factor.clamp(0.0, 1.0))
    }

    /// How thick ground-hugging fog is here, `0.0` to `1.0`.
    ///
    /// Derived from the two values [`CompressedWeather`] already carries, so
    /// it is identical on client and server and costs no wire format change:
    /// fog is the overcast-but-not-raining case (stratus/radiation fog), and
    /// heavy convective rain clears it out.
    pub fn fog_density(&self, tuning: &WeatherTuning) -> f32 {
        smoothstep(tuning.fog_cloud_min, tuning.fog_cloud_max, self.cloud)
            * (1.0 - smoothstep(tuning.fog_rain_min, tuning.fog_rain_max, self.rain))
    }

    /// Multiplier on how far an observer can see through the current weather,
    /// `1.0` in clear air. Applied to NPC sight distance so reduced visibility
    /// is a real, server-authoritative mechanic rather than only a client-side
    /// visual.
    ///
    /// Always in `0.0..=1.0`. The final clamp is load-bearing rather than
    /// defensive: every sight check squares this into a distance, so a value
    /// outside the unit range does not degrade gracefully — above `1.0` an NPC
    /// sees *further* in a blizzard than in clear air, and a `NaN` makes every
    /// `dist_sqrd < sight_dist.powi(2)` comparison `false`, blinding it
    /// entirely. `WeatherTuning::load` clamps the two floors and `smoothstep`
    /// refuses to divide by zero, so this should be unreachable — it is here
    /// because the failure is silent and world-wide if it ever is not.
    pub fn visibility_factor(&self, snow_factor: f32, tuning: &WeatherTuning) -> f32 {
        let snow = self.snow(snow_factor).clamp(0.0, 1.0);
        let fog = self.fog_density(tuning).clamp(0.0, 1.0);
        // Multiplicative so snowfall inside fog is worse than either alone,
        // but neither can drive sight to zero.
        let factor = (1.0 - snow * (1.0 - tuning.snow_min_visibility))
            * (1.0 - fog * (1.0 - tuning.fog_min_visibility));
        clamp_unit(factor)
    }

    pub fn lerp_unclamped(&self, to: &Self, t: f32) -> Self {
        Self {
            cloud: f32::lerp_unclamped(self.cloud, to.cloud, t),
            rain: f32::lerp_unclamped(self.rain, to.rain, t),
            wind: Vec2::<f32>::lerp_unclamped(self.wind, to.wind, t),
        }
    }

    // Get the rain velocity for this weather
    pub fn rain_vel(&self) -> Vec3<f32> { self.wind.with_z(-RAIN_FALL_RATE) }

    /// Velocity of falling precipitation, blending between rain and snow by
    /// `snow_factor` (see [`snow_factor_at_temp`]).
    ///
    /// Snow falls far slower and is pushed around much more by wind, which is
    /// what the falling-precipitation visual keys off to read as snow rather
    /// than as pale rain.
    pub fn precip_vel(&self, snow_factor: f32) -> Vec3<f32> {
        let snow_factor = snow_factor.clamp(0.0, 1.0);
        let fall_rate = f32::lerp_unclamped(RAIN_FALL_RATE, SNOW_FALL_RATE, snow_factor);
        let drift = f32::lerp_unclamped(1.0, SNOW_WIND_DRIFT, snow_factor);
        (self.wind * drift).with_z(-fall_rate)
    }

    // Get the wind velocity for this weather
    pub fn wind_vel(&self) -> Vec2<f32> { self.wind }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WeatherKind {
    Clear,
    Cloudy,
    Rain,
    Storm,
    /// Precipitation over ground cold enough for it to fall as snow. Only ever
    /// produced by [`Weather::get_kind_at`], which knows where the weather is
    /// happening; [`Weather::get_kind`] cannot tell rain from snow and never
    /// returns this.
    ///
    /// Appended rather than inserted next to `Rain` so the variant ordering
    /// every existing serialized form depends on stays untouched.
    Snow,
}

impl fmt::Display for WeatherKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WeatherKind::Clear => write!(f, "Clear"),
            WeatherKind::Cloudy => write!(f, "Cloudy"),
            WeatherKind::Rain => write!(f, "Rain"),
            WeatherKind::Storm => write!(f, "Storm"),
            WeatherKind::Snow => write!(f, "Snow"),
        }
    }
}

// How many chunks wide a weather cell is.
// So one weather cell has (CHUNKS_PER_CELL * CHUNKS_PER_CELL) chunks.
pub const CHUNKS_PER_CELL: u32 = 16;

pub const CELL_SIZE: u32 = CHUNKS_PER_CELL * TerrainChunkSize::RECT_SIZE.x;

#[derive(Debug, Clone)]
pub struct WeatherGrid {
    weather: Grid<Weather>,
}

/// Weather that's compressed in order to send it to the client.
#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompressedWeather {
    cloud: u8,
    rain: u8,
}

impl CompressedWeather {
    pub fn lerp_unclamped(&self, to: &CompressedWeather, t: f32) -> Weather {
        Weather {
            cloud: f32::lerp_unclamped(self.cloud as f32, to.cloud as f32, t) / 255.0,
            rain: f32::lerp_unclamped(self.rain as f32, to.rain as f32, t) / 255.0,
            wind: Vec2::zero(),
        }
    }
}

impl From<Weather> for CompressedWeather {
    fn from(weather: Weather) -> Self {
        Self {
            cloud: (weather.cloud * 255.0).round() as u8,
            rain: (weather.rain * 255.0).round() as u8,
        }
    }
}

impl From<CompressedWeather> for Weather {
    fn from(weather: CompressedWeather) -> Self {
        Self {
            cloud: weather.cloud as f32 / 255.0,
            rain: weather.rain as f32 / 255.0,
            wind: Vec2::zero(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedWeatherGrid {
    weather: Grid<CompressedWeather>,
}

impl From<&WeatherGrid> for SharedWeatherGrid {
    fn from(value: &WeatherGrid) -> Self {
        Self {
            weather: Grid::from_raw(
                value.weather.size(),
                value
                    .weather
                    .raw()
                    .iter()
                    .copied()
                    .map(CompressedWeather::from)
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

impl From<&SharedWeatherGrid> for WeatherGrid {
    fn from(value: &SharedWeatherGrid) -> Self {
        Self {
            weather: Grid::from_raw(
                value.weather.size(),
                value
                    .weather
                    .raw()
                    .iter()
                    .copied()
                    .map(Weather::from)
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

impl SharedWeatherGrid {
    pub fn new(size: Vec2<u32>) -> Self {
        size.map(|e| debug_assert!(i32::try_from(e).is_ok()));
        Self {
            weather: Grid::new(size.as_(), CompressedWeather::default()),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Vec2<i32>, &CompressedWeather)> {
        self.weather.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Vec2<i32>, &mut CompressedWeather)> {
        self.weather.iter_mut()
    }

    pub fn size(&self) -> Vec2<u32> { self.weather.size().as_() }
}

/// Transforms a world position to cell coordinates. Where (0.0, 0.0) in cell
/// coordinates is the center of the weather cell located at (0, 0) in the grid.
fn to_cell_pos(wpos: Vec2<f32>) -> Vec2<f32> { wpos / CELL_SIZE as f32 - 0.5 }

// TODO: Move consts from world to common to avoid duplication
const LOCALITY: [Vec2<i32>; 9] = [
    Vec2::new(0, 0),
    Vec2::new(0, 1),
    Vec2::new(1, 0),
    Vec2::new(0, -1),
    Vec2::new(-1, 0),
    Vec2::new(1, 1),
    Vec2::new(1, -1),
    Vec2::new(-1, 1),
    Vec2::new(-1, -1),
];

impl WeatherGrid {
    pub fn new(size: Vec2<u32>) -> Self {
        size.map(|e| debug_assert!(i32::try_from(e).is_ok()));
        Self {
            weather: Grid::new(size.as_(), Weather::default()),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Vec2<i32>, &Weather)> { self.weather.iter() }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Vec2<i32>, &mut Weather)> {
        self.weather.iter_mut()
    }

    pub fn size(&self) -> Vec2<u32> { self.weather.size().as_() }

    pub fn get(&self, cell_pos: Vec2<u32>) -> Weather {
        self.weather
            .get(cell_pos.as_())
            .copied()
            .unwrap_or_default()
    }

    /// Get the weather at a given world position by doing bilinear
    /// interpolation between four cells.
    pub fn get_interpolated(&self, wpos: Vec2<f32>) -> Weather {
        let cell_pos = to_cell_pos(wpos);
        let rpos = cell_pos.map(|e| e.fract() + (1.0 - e.signum()) / 2.0);
        let cell_pos = cell_pos.map(|e| e.floor());

        let cpos = cell_pos.as_::<i32>();
        Weather::lerp_unclamped(
            &Weather::lerp_unclamped(
                self.weather.get(cpos).unwrap_or(&Weather::default()),
                self.weather
                    .get(cpos + Vec2::unit_x())
                    .unwrap_or(&Weather::default()),
                rpos.x,
            ),
            &Weather::lerp_unclamped(
                self.weather
                    .get(cpos + Vec2::unit_y())
                    .unwrap_or(&Weather::default()),
                self.weather
                    .get(cpos + Vec2::one())
                    .unwrap_or(&Weather::default()),
                rpos.x,
            ),
            rpos.y,
        )
    }

    /// Get the max weather near a position
    pub fn get_max_near(&self, wpos: Vec2<f32>) -> Weather {
        let cell_pos: Vec2<i32> = to_cell_pos(wpos).as_();
        LOCALITY
            .iter()
            .map(|l| {
                self.weather
                    .get(cell_pos + l)
                    .cloned()
                    .unwrap_or_default()
            })
            .reduce(|a, b| Weather {
                cloud: a.cloud.max(b.cloud),
                rain: a.rain.max(b.rain),
                wind: a.wind.map2(b.wind, |a, b| a.max(b)),
            })
            // There will always be 9 elements in locality
            .unwrap()
    }

    pub fn is_raining(&self, wpos: Vec2<f32>) -> bool {
        // offset to convert rtsim wpos to weather grid
        let offset = Vec2::new(512.0, 512.0);
        let weather = self.get_max_near(wpos + offset);
        let weather_kind = weather.get_kind();
        weather_kind == WeatherKind::Rain || weather_kind == WeatherKind::Storm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snow_factor_spans_the_transition_band() {
        let t = WeatherTuning::default();
        let band = t.snow_temp_band;
        assert_eq!(snow_factor_at_temp(SNOW_TEMP, &t), 1.0);
        assert_eq!(snow_factor_at_temp(SNOW_TEMP - 5.0, &t), 1.0);
        assert!(snow_factor_at_temp(SNOW_TEMP + band, &t) < 1e-5);
        assert_eq!(snow_factor_at_temp(SNOW_TEMP + 5.0, &t), 0.0);
        let mid = snow_factor_at_temp(SNOW_TEMP + band / 2.0, &t);
        assert!(
            (mid - 0.5).abs() < 1e-5,
            "expected sleet midpoint, got {mid}"
        );
    }

    #[test]
    fn snow_factor_never_panics_or_leaves_unit_range() {
        // Includes tuning a designer could plausibly get wrong: a zero or
        // negative transition band must not divide by zero or invert the ramp.
        for band in [0.2, 0.0, -1.0, f32::NAN] {
            let t = WeatherTuning {
                snow_temp_band: band,
                ..WeatherTuning::default()
            };
            for temp in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1e30, 1e30] {
                let f = snow_factor_at_temp(temp, &t);
                assert!(
                    (0.0..=1.0).contains(&f),
                    "snow factor out of range for temp {temp}, band {band}: {f}"
                );
            }
        }
    }

    #[test]
    fn shipped_weather_tuning_asset_loads_and_is_sane() {
        let t = WeatherTuning::load();
        assert!(t.snow_temp_band > 0.0);
        assert!(t.fog_cloud_min < t.fog_cloud_max);
        assert!(t.fog_rain_min < t.fog_rain_max);
        assert!((0.0..1.0).contains(&t.snow_min_visibility));
        assert!((0.0..1.0).contains(&t.fog_min_visibility));
    }

    #[test]
    fn precipitation_splits_into_rain_and_snow_without_loss() {
        let w = Weather::new(0.8, 0.6, Vec2::zero());
        for snow_factor in [0.0, 0.25, 0.5, 1.0] {
            let total = w.snow(snow_factor) + w.liquid_rain(snow_factor);
            assert!(
                (total - w.rain).abs() < 1e-5,
                "rain+snow should sum to precipitation, got {total} vs {}",
                w.rain
            );
        }
    }

    #[test]
    fn cold_precipitation_reads_as_snow() {
        let w = Weather::new(0.9, 0.5, Vec2::zero());
        assert_eq!(w.get_kind(), WeatherKind::Rain);
        assert_eq!(w.get_kind_at(0.0), WeatherKind::Rain);
        assert_eq!(w.get_kind_at(1.0), WeatherKind::Snow);
        // Non-precipitating weather is unaffected by how cold it is.
        let clear = Weather::default();
        assert_eq!(clear.get_kind_at(1.0), WeatherKind::Clear);
        // A cold storm stays a Storm: the wind, not the form of the
        // precipitation, is what defines a blizzard.
        let blizzard = Weather::new(1.0, 0.8, Vec2::new(30.0, 0.0));
        assert_eq!(blizzard.get_kind_at(1.0), WeatherKind::Storm);
    }

    #[test]
    fn snow_falls_slower_than_rain_and_drifts_further() {
        let w = Weather::new(0.9, 0.5, Vec2::new(6.0, 0.0));
        let rain = w.precip_vel(0.0);
        let snow = w.precip_vel(1.0);
        assert_eq!(rain, w.rain_vel());
        assert!(snow.z > rain.z, "snow should fall slower than rain");
        assert!(snow.xy().magnitude() > rain.xy().magnitude());
    }

    #[test]
    fn fog_is_thickest_when_overcast_and_dry() {
        let t = WeatherTuning::default();
        let overcast_dry = Weather::new(1.0, 0.0, Vec2::zero());
        let overcast_wet = Weather::new(1.0, 1.0, Vec2::zero());
        let clear = Weather::new(0.0, 0.0, Vec2::zero());
        assert!(overcast_dry.fog_density(&t) > 0.9);
        assert!(overcast_wet.fog_density(&t) < 0.05);
        assert_eq!(clear.fog_density(&t), 0.0);
    }

    #[test]
    fn visibility_degrades_but_never_reaches_zero() {
        let t = WeatherTuning::default();
        let clear = Weather::new(0.0, 0.0, Vec2::zero());
        assert_eq!(clear.visibility_factor(0.0, &t), 1.0);

        let blizzard = Weather::new(1.0, 1.0, Vec2::zero());
        let v = blizzard.visibility_factor(1.0, &t);
        assert!(
            v > 0.0 && v < 1.0,
            "expected reduced but non-zero sight: {v}"
        );

        // Snow over cold ground must obscure more than the same cell's rain.
        let precip = Weather::new(0.6, 0.8, Vec2::zero());
        assert!(precip.visibility_factor(1.0, &t) < precip.visibility_factor(0.0, &t));
    }

    /// `weather_tuning.ron` hot-reloads, so a typo in it reaches a running
    /// server without passing a single test. These two failure modes are
    /// silent and world-wide: a `NaN` sight distance makes every
    /// `dist_sqrd < sight_dist.powi(2)` false (every NPC blind), and a
    /// negative one is squared back to positive (every NPC sees *further* in a
    /// blizzard). Neither logs anything.
    #[test]
    fn broken_tuning_cannot_blind_or_super_sight_every_npc() {
        let weathers = [
            Weather::new(0.0, 0.0, Vec2::zero()),
            Weather::new(1.0, 0.0, Vec2::zero()),
            Weather::new(0.45, 0.05, Vec2::zero()),
            Weather::new(1.0, 1.0, Vec2::new(30.0, 0.0)),
            // Sitting exactly ON the degenerate edges below. This is the case
            // that actually produced `NaN` (0.0 / 0.0) rather than an infinity
            // that `clamp` would have pinned, so without it this test passes
            // against the broken implementation too.
            Weather::new(0.5, 0.2, Vec2::zero()),
        ];

        // Degenerate fog edges (min == max) used to divide by zero; NaN and
        // inverted edges are the other plausible hand-edit mistakes.
        let mut broken = vec![
            WeatherTuning {
                fog_cloud_min: 0.5,
                fog_cloud_max: 0.5,
                ..WeatherTuning::default()
            },
            WeatherTuning {
                fog_rain_min: 0.2,
                fog_rain_max: 0.2,
                ..WeatherTuning::default()
            },
            WeatherTuning {
                fog_cloud_min: 0.9,
                fog_cloud_max: 0.1,
                ..WeatherTuning::default()
            },
            WeatherTuning {
                fog_cloud_min: f32::NAN,
                ..WeatherTuning::default()
            },
        ];
        // `load()` clamps the two floors, so out-of-range values are only
        // reachable here by constructing the struct directly — which is exactly
        // why `visibility_factor` clamps its own result too.
        for bad in [-1.0, 2.0, f32::NAN, f32::INFINITY] {
            broken.push(WeatherTuning {
                snow_min_visibility: bad,
                ..WeatherTuning::default()
            });
            broken.push(WeatherTuning {
                fog_min_visibility: bad,
                ..WeatherTuning::default()
            });
        }

        for tuning in &broken {
            for weather in &weathers {
                for snow_factor in [0.0, 0.5, 1.0] {
                    let v = weather.visibility_factor(snow_factor, tuning);
                    assert!(
                        v.is_finite() && (0.0..=1.0).contains(&v),
                        "visibility must stay a finite 0..=1 multiplier, got {v} for {weather:?} \
                         / snow {snow_factor} / {tuning:?}"
                    );
                    let fog = weather.fog_density(tuning);
                    assert!(
                        fog.is_finite() && (0.0..=1.0).contains(&fog),
                        "fog density must stay a finite 0..=1 value, got {fog}"
                    );
                }
            }
        }
    }

    #[test]
    fn shipped_tuning_survives_the_load_clamp_unchanged() {
        // If this ever fails, the shipped asset is out of range and `load()`
        // has been quietly correcting it — fix the asset, not the test.
        let loaded = WeatherTuning::load();
        assert_eq!(
            loaded.snow_min_visibility,
            clamp_unit(loaded.snow_min_visibility)
        );
        assert_eq!(
            loaded.fog_min_visibility,
            clamp_unit(loaded.fog_min_visibility)
        );
    }
}
