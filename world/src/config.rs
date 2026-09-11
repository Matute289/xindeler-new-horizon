use common::assets::{BoxedError, FileAsset, load_ron};
use serde::Deserialize;
use std::borrow::Cow;
use vek::*;

pub struct Config {
    pub sea_level: f32,
    pub mountain_scale: f32,
    /// Abstract engine temperature scale (not a real unit -- see
    /// `abstract_temp_to_celsius` for its documented, real-Celsius
    /// equivalent).
    pub snow_temp: f32,
    /// See `snow_temp`'s doc comment.
    pub temperate_temp: f32,
    /// See `snow_temp`'s doc comment.
    pub tropical_temp: f32,
    /// See `snow_temp`'s doc comment.
    pub desert_temp: f32,
    pub desert_hum: f32,
    pub forest_hum: f32,
    pub jungle_hum: f32,
    /// Rainfall (in meters) per m² of surface per minute.  Default is set to
    /// make it approximately 1 m rainfall / year uniformly across the whole
    /// land area, which is the average rainfall on Earth.
    pub rainfall_chunk_rate: f32,
    /// Roughness coefficient is an empirical value that controls the rate of
    /// energy loss of water in a river.  The higher it is, the more water
    /// slows down as it flows downhill, which consequently leads to lower
    /// velocities and higher river area for the same flow rate.
    ///
    /// See <https://wwwrcamnl.wr.usgs.gov/sws/fieldmethods/Indirects/nvalues/index.htm>.
    ///
    /// The default is set to over 0.06, which is pretty high but still within a
    /// reasonable range for rivers.  The higher this is, the quicker rivers
    /// appear, and since we often will have high slopes we want to give
    /// rivers as much of a chance as possible.  In the future we can set
    /// this dynamically.
    ///
    /// NOTE: The values in the link are in seconds / (m^(-1/3)), but we use
    /// them without conversion as though they are in minutes / (m^(-1/3)).
    /// The idea here is that our clock speed has time go by at
    /// approximately 1 minute per second, but since velocity depends on
    /// this parameter, we want flow rates to still "look" natural at the second
    /// level.  The way we are cheating is that we still allow the refill
    /// rate (via rainfall) of rivers and lakes to be specified as though
    /// minutes are *really* minutes.  This reduces the amount of water
    /// needed to form a river of a given area by 60, but hopefully this should
    /// not feel too unnatural since the refill rate is still below what
    /// people should be able to perceive.
    pub river_roughness: f32,
    /// Maximum width of rivers, in terms of a multiple of the horizontal chunk
    /// size.
    ///
    /// Currently, not known whether setting this above 1.0 will work properly.
    /// Please use with care!
    pub river_max_width: f32,
    /// Minimum height at which rivers display.
    pub river_min_height: f32,
    /// Rough desired river width-to-depth ratio (in terms of horizontal chunk
    /// width / m, for some reason).  Not exact.
    pub river_width_to_depth: f32,
    /// TODO: Move to colors.ron when blockgen can access it
    pub ice_color: Rgb<u8>,
}

pub const CONFIG: Config = Config {
    sea_level: 140.0,
    mountain_scale: 2048.0,
    // temperature
    snow_temp: -0.8,
    temperate_temp: -0.4,
    tropical_temp: 0.4,
    desert_temp: 0.8,
    // humidity
    desert_hum: 0.15,
    forest_hum: 0.5,
    jungle_hum: 0.75,
    // water
    rainfall_chunk_rate: 1.0 / (512.0 * 32.0 * 32.0),
    river_roughness: 0.06125,
    river_max_width: 2.0,
    river_min_height: 0.25,
    river_width_to_depth: 8.0,
    ice_color: Rgb::new(140, 175, 255),
};

/// Real-Celsius equivalent of one abstract-scale unit, i.e. `celsius =
/// ABSTRACT_TEMP_CELSIUS_SLOPE * abstract_temp +
/// ABSTRACT_TEMP_CELSIUS_INTERCEPT`. See `abstract_temp_to_celsius`'s doc
/// comment for the reasoning behind these two constants.
pub const ABSTRACT_TEMP_CELSIUS_SLOPE: f32 = 15.0;
/// See `ABSTRACT_TEMP_CELSIUS_SLOPE`'s doc comment.
pub const ABSTRACT_TEMP_CELSIUS_INTERCEPT: f32 = 20.0;

/// Lower bound of the real-Celsius range `abstract_temp_to_celsius` and
/// `celsius_to_abstract_temp` support. Chosen to be far colder than any
/// terrestrial biome currently produces, reserved for a possible future
/// arctic/frozen-wastes biome.
pub const REAL_TEMP_MIN_CELSIUS: f32 = -60.0;
/// Upper bound of the same range. Reserved for a possible future
/// hell/infernal or lava-adjacent biome.
pub const REAL_TEMP_MAX_CELSIUS: f32 = 100.0;

/// Converts the engine's abstract world-gen temperature scale (the unit
/// `SimChunk::temp` and the four threshold fields on `CONFIG` above are
/// still expressed in) into real degrees Celsius.
///
/// This is a documentation and tooling convenience, not (yet) load-bearing
/// for any gameplay behavior beyond the authored Cromatolis baseline curve
/// in `sim::SimChunk::generate`: `SimChunk::temp` itself keeps its existing
/// abstract representation, because dozens of hand-tuned call sites across
/// `world/src/layer/{scatter,wildlife,rock}.rs`, `world/src/civ/mod.rs`,
/// `world/src/site/**`, and `rtsim/src/rule/architect.rs` compare it
/// against `CONFIG`'s thresholds with small, empirically-tuned epsilon
/// widths (e.g. `close(chunk.temp, CONFIG.snow_temp, 0.15)`) calibrated
/// against the current `[-1.0, 1.0]` range for the entire procedural
/// world, not just Cromatolis. Rescaling the stored unit would require
/// proportionally rescaling every one of those, which is out of scope
/// here.
///
/// The mapping is a single affine transform (see
/// `ABSTRACT_TEMP_CELSIUS_SLOPE`/`_INTERCEPT`), calibrated so:
///   - `-1.0` abstract == `5.0`  °C (a cool, non-freezing climate --
///     deliberately kept above freezing, since none of the terrestrial biomes
///     that use this scale today are meant to have real snow/ice)
///   - `1.0`  abstract == `35.0` °C (a hot tropical extreme)
///
/// and extended linearly to cover the wider `[-60.0, 100.0]` real-Celsius
/// range for values outside `[-1.0, 1.0]`, reserved for future biomes far
/// more extreme than anything the engine currently generates.
///
/// Clamped to `[REAL_TEMP_MIN_CELSIUS, REAL_TEMP_MAX_CELSIUS]`, so an
/// out-of-range or non-finite `abstract_temp` can never produce an
/// unbounded or NaN-propagating result.
pub fn abstract_temp_to_celsius(abstract_temp: f32) -> f32 {
    let abstract_temp = if abstract_temp.is_finite() {
        abstract_temp
    } else {
        0.0
    };
    (ABSTRACT_TEMP_CELSIUS_SLOPE * abstract_temp + ABSTRACT_TEMP_CELSIUS_INTERCEPT)
        .clamp(REAL_TEMP_MIN_CELSIUS, REAL_TEMP_MAX_CELSIUS)
}

/// Inverse of `abstract_temp_to_celsius`: converts a real-Celsius value
/// into the engine's abstract world-gen temperature scale.
///
/// Does NOT clamp its output to `[-1.0, 1.0]` -- most existing world-gen
/// code implicitly assumes abstract temperatures stay within that range
/// (it's what `cdf_irwin_hall`-driven procedural generation always
/// produces), so a caller that needs to preserve that assumption (as the
/// authored Cromatolis curve in `sim::SimChunk::generate` does) must clamp
/// the result itself. A caller building a deliberately extreme future
/// biome may want the wider range this function can actually produce.
pub fn celsius_to_abstract_temp(celsius: f32) -> f32 {
    let celsius = if celsius.is_finite() {
        celsius
    } else {
        ABSTRACT_TEMP_CELSIUS_INTERCEPT
    };
    (celsius.clamp(REAL_TEMP_MIN_CELSIUS, REAL_TEMP_MAX_CELSIUS) - ABSTRACT_TEMP_CELSIUS_INTERCEPT)
        / ABSTRACT_TEMP_CELSIUS_SLOPE
}

#[derive(Deserialize)]
pub struct Features {
    pub caverns: bool,
    pub caves: bool,
    pub rocks: bool,
    pub shrubs: bool,
    pub trees: bool,
    pub scatter: bool,
    pub paths: bool,
    pub spots: bool,
    // 1.0 is the default wildlife density
    pub wildlife_density: f32,
    pub peak_naming: bool,
    pub biome_naming: bool,
    pub train_tracks: bool,
}

impl FileAsset for Features {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celsius_conversion_round_trips_within_the_normal_abstract_range() {
        for &abstract_temp in &[-1.0, -0.5, 0.0, 0.5, 1.0] {
            let celsius = abstract_temp_to_celsius(abstract_temp);
            let round_tripped = celsius_to_abstract_temp(celsius);
            assert!(
                (round_tripped - abstract_temp).abs() < 1e-4,
                "round-trip mismatch: {abstract_temp} -> {celsius}C -> {round_tripped}"
            );
        }
    }

    #[test]
    fn celsius_conversion_matches_documented_reference_points() {
        assert!((abstract_temp_to_celsius(-1.0) - 5.0).abs() < 1e-4);
        assert!((abstract_temp_to_celsius(1.0) - 35.0).abs() < 1e-4);
    }

    #[test]
    fn abstract_temp_to_celsius_never_panics_and_stays_in_range() {
        for &abstract_temp in &[
            f32::MIN,
            f32::MAX,
            -1000.0,
            1000.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let celsius = abstract_temp_to_celsius(abstract_temp);
            assert!(celsius.is_finite());
            assert!((REAL_TEMP_MIN_CELSIUS..=REAL_TEMP_MAX_CELSIUS).contains(&celsius));
        }
    }

    #[test]
    fn celsius_to_abstract_temp_never_panics_and_stays_finite() {
        for &celsius in &[
            f32::MIN,
            f32::MAX,
            -1000.0,
            1000.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let abstract_temp = celsius_to_abstract_temp(celsius);
            assert!(abstract_temp.is_finite());
        }
    }

    #[test]
    fn celsius_conversion_is_monotonically_increasing() {
        let mut prev = abstract_temp_to_celsius(-2.0);
        for i in -19..=20 {
            let t = i as f32 / 10.0;
            let celsius = abstract_temp_to_celsius(t);
            assert!(
                celsius >= prev,
                "conversion must never decrease as abstract_temp increases"
            );
            prev = celsius;
        }
    }
}
