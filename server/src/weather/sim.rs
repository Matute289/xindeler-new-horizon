use common::{
    grid::Grid,
    resources::TimeOfDay,
    weather::{CELL_SIZE, CHUNKS_PER_CELL, Weather, WeatherGrid},
};
use noise::{NoiseFn, Perlin, SuperSimplex, Turbulence};
use vek::*;
use world::World;

use crate::weather::WEATHER_DT;

fn cell_to_wpos_center(p: Vec2<i32>) -> Vec2<i32> { p * CELL_SIZE as i32 + CELL_SIZE as i32 / 2 }

/// Derives a cell's cloud cover and rain from its pressure and (already
/// `powf(0.2)`-compressed) humidity constant.
///
/// Both outputs are clamped to `0.0..=1.0` to match the documented range of
/// [`Weather::cloud`] and [`Weather::rain`] -- neither formula below is
/// naturally bounded to that range. At low pressure combined with humidity
/// approaching `1.0` (a real combination: any chunk with non-trivial
/// procedural humidity gets pushed close to `1.0` by the `powf(0.2)` in
/// [`WeatherSim::new`]), `cloud` can exceed `4.0` and `rain` can exceed `1.6`
/// before this clamp.
///
/// This was never reachable as *visibly* broken before Cromatolis grew real,
/// spatially-varying humidity (see the `climate_zone` work in
/// `world/src/sim/mod.rs`): a flat, saturating baseline temperature had been
/// silently zeroing procedural humidity almost everywhere, so `humidity`
/// rarely got anywhere near the values that overflow this formula. Once
/// humidity is realistic -- persistently high near water, as at Rios Port --
/// the unclamped `cloud`/`rain` regularly overshoot `1.0` and get silently
/// saturated by an `f32 as u8` cast, which pins the area to permanent max
/// fog/cloud cover instead of the intended smooth falloff, rather than the
/// occasional overcast patch the formula's constants were tuned for. There
/// are two such casts downstream, both fixed uniformly by clamping at the
/// source here instead of at either of them individually: the LoD weather
/// texture upload (`voxygen/src/scene/lod.rs`), and
/// `CompressedWeather::from` (`common/src/weather.rs`), the network-sync
/// path every client (including singleplayer) actually reads
/// `Weather::cloud`/`rain` back through. The LoD-texture path is what the
/// screen-space fog raymarch in `cloud/regular.glsl` renders as a dense,
/// near-permanent haze over terrain: a pink-tinted sky and step-banded
/// ("wood-grain") terrain shading wherever the raymarch's step size is
/// coarse relative to how sharply that saturated fog cuts in.
fn cloud_and_rain(pressure: f32, humidity: f32) -> (f32, f32) {
    const RAIN_CLOUD_THRESHOLD: f32 = 0.25;
    let cloud = ((1.0 - pressure).max(0.0).powi(2) * 4.0).clamp(0.0, 1.0);
    let rain = (((1.0 - pressure - RAIN_CLOUD_THRESHOLD).max(0.0) * humidity * 2.5).powf(0.75))
        .clamp(0.0, 1.0);
    (cloud, rain)
}

#[derive(Clone)]
struct WeatherZone {
    weather: Weather,
    /// Time, in seconds this zone lives.
    time_to_live: f32,
}

struct CellConsts {
    humidity: f32,
}

pub struct WeatherSim {
    size: Vec2<u32>,
    consts: Grid<CellConsts>,
    zones: Grid<Option<WeatherZone>>,
}

/// A list of weather cells where lightning has a chance to strike.
#[derive(Default)]
pub struct LightningCells {
    pub cells: Vec<Vec2<i32>>,
}

impl WeatherSim {
    pub fn new(size: Vec2<u32>, world: &World) -> Self {
        Self {
            size,
            consts: Grid::from_raw(
                size.as_(),
                (0..size.x * size.y)
                    .map(|i| Vec2::new(i % size.x, i / size.x))
                    .map(|p| {
                        let mut humid_sum = 0.0;

                        for y in 0..CHUNKS_PER_CELL {
                            for x in 0..CHUNKS_PER_CELL {
                                let chunk_pos = p * CHUNKS_PER_CELL + Vec2::new(x, y);
                                if let Some(chunk) = world.sim().get(chunk_pos.as_()) {
                                    let env = chunk.get_environment();
                                    humid_sum += env.humid;
                                }
                            }
                        }
                        let average_humid = humid_sum / (CHUNKS_PER_CELL * CHUNKS_PER_CELL) as f32;
                        CellConsts {
                            humidity: average_humid.powf(0.2).min(1.0),
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
            zones: Grid::new(size.as_(), None),
        }
    }

    /// Adds a weather zone as a circle at a position, with a given radius. Both
    /// of which should be in weather cell units
    pub fn add_zone(&mut self, weather: Weather, pos: Vec2<f32>, radius: f32, time: f32) {
        let min: Vec2<i32> = (pos - radius).as_::<i32>().map(|e| e.max(0));
        let max: Vec2<i32> = (pos + radius)
            .ceil()
            .as_::<i32>()
            .map2(self.size.as_::<i32>(), |a, b| a.min(b));
        for y in min.y..max.y {
            for x in min.x..max.x {
                let ipos = Vec2::new(x, y);
                let p = ipos.as_::<f32>();

                if p.distance_squared(pos) < radius.powi(2) {
                    self.zones[ipos] = Some(WeatherZone {
                        weather,
                        time_to_live: time,
                    });
                }
            }
        }
    }

    // Time step is cell size / maximum wind speed.
    pub fn tick(&mut self, time_of_day: TimeOfDay, out: &mut WeatherGrid) -> LightningCells {
        let time = time_of_day.0;

        let base_nz: Turbulence<Turbulence<SuperSimplex, Perlin>, Perlin> = Turbulence::new(
            Turbulence::new(SuperSimplex::new(0))
                .set_frequency(0.2)
                .set_power(1.5),
        )
        .set_frequency(2.0)
        .set_power(0.2);

        let rain_nz = SuperSimplex::new(0);

        let mut lightning_cells = Vec::new();
        for (point, cell) in out.iter_mut() {
            if let Some(zone) = &mut self.zones[point] {
                *cell = zone.weather;
                zone.time_to_live -= WEATHER_DT;
                if zone.time_to_live <= 0.0 {
                    self.zones[point] = None;
                }
            } else {
                let wpos = cell_to_wpos_center(point);

                let pos = wpos.as_::<f64>() + time * 0.1;

                let space_scale = 7_500.0;
                let time_scale = 100_000.0;
                let spos = (pos / space_scale).with_z(time / time_scale);

                let avg_scale = 30_000.0;
                let avg_delay = 250_000.0;
                let pressure = ((base_nz
                    .get((pos / avg_scale).with_z(time / avg_delay).into_array())
                    + base_nz.get(
                        (pos / (avg_scale * 0.25))
                            .with_z(time / (avg_delay * 0.25))
                            .into_array(),
                    ) * 0.5)
                    * 0.5
                    + 1.0)
                    .clamped(0.0, 1.0) as f32
                    + 0.55
                    - self.consts[point].humidity * 0.6;

                let (cloud, rain) = cloud_and_rain(pressure, self.consts[point].humidity);
                cell.cloud = cloud;
                cell.rain = rain;
                cell.wind = Vec2::new(
                    rain_nz.get(spos.into_array()).powi(3) as f32,
                    rain_nz.get((spos + 1.0).into_array()).powi(3) as f32,
                ) * 200.0
                    * (1.0 - pressure);
            }

            if cell.rain > 0.2 && cell.cloud > 0.15 {
                lightning_cells.push(point);
            }
        }
        LightningCells {
            cells: lightning_cells,
        }
    }

    pub fn size(&self) -> Vec2<u32> { self.size }
}

#[cfg(test)]
mod tests {
    use super::cloud_and_rain;

    /// Without the clamp, high near-water humidity (pushed close to `1.0` by
    /// `powf(0.2)` in [`super::WeatherSim::new`]) combined with a
    /// low-pressure cell drives the raw formula's `cloud` well past `4.0` and
    /// `rain` past `1.6` -- see e.g. Rios Port on Cromatolis. Both must land
    /// in the range [`Weather::cloud`]/[`Weather::rain`] document.
    #[test]
    fn cloud_and_rain_stay_in_unit_range_under_high_humidity_low_pressure() {
        // Reproduces the reachable extreme: humidity saturated near 1.0 (as
        // `average_humid.powf(0.2)` yields for almost any nonzero input) and
        // pressure pushed slightly negative by that same humidity term.
        let (cloud, rain) = cloud_and_rain(-0.05, 1.0);
        assert!(
            (0.0..=1.0).contains(&cloud),
            "cloud must stay in 0.0..=1.0, got {cloud}"
        );
        assert!(
            (0.0..=1.0).contains(&rain),
            "rain must stay in 0.0..=1.0, got {rain}"
        );
        // Confirm this case genuinely would have overflowed pre-clamp,
        // so the test is exercising the bug and not a case that was already
        // in range.
        let raw_cloud = (1.0_f32 - -0.05_f32).max(0.0).powi(2) * 4.0;
        let raw_rain = ((1.0_f32 - -0.05_f32 - 0.25).max(0.0) * 1.0 * 2.5).powf(0.75);
        assert!(raw_cloud > 1.0, "test setup should overflow cloud");
        assert!(raw_rain > 1.0, "test setup should overflow rain");
    }

    #[test]
    fn cloud_and_rain_never_negative() {
        // High pressure, zero humidity: both terms clamp at their `max(0.0)`
        // floor already, but the clamp must not turn that into something
        // negative either.
        let (cloud, rain) = cloud_and_rain(2.0, 0.0);
        assert!((0.0..=1.0).contains(&cloud));
        assert!((0.0..=1.0).contains(&rain));
    }

    #[test]
    fn cloud_and_rain_match_unclamped_formula_in_normal_range() {
        // A moderate case that was already within range should be
        // unaffected by the clamp.
        let pressure = 0.6;
        let humidity = 0.3;
        let (cloud, rain) = cloud_and_rain(pressure, humidity);
        let raw_cloud = (1.0 - pressure).max(0.0).powi(2) * 4.0;
        let raw_rain = ((1.0 - pressure - 0.25).max(0.0) * humidity * 2.5).powf(0.75);
        assert!(raw_cloud <= 1.0, "test setup should stay in range");
        assert!(raw_rain <= 1.0, "test setup should stay in range");
        assert!((cloud - raw_cloud).abs() < 1e-6);
        assert!((rain - raw_rain).abs() < 1e-6);
    }
}
