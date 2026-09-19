use crate::{
    CONFIG, IndexRef,
    column::ColumnSample,
    sim::{CROMATOLIS_V0_REGION_ID, SimChunk, WaterBodyKind},
    util::close,
};
use common::{
    assets::{AssetExt, Ron},
    calendar::{Calendar, CalendarEvent},
    generation::{ChunkSupplement, EntityInfo, EntitySpawn},
    resources::TimeOfDay,
    terrain::{BiomeKind, Block},
    time::DayPeriod,
    vol::{ReadVol, RectSizedVol, WriteVol},
};
use rand::prelude::*;
use serde::Deserialize;
use std::{f32, iter};
use vek::*;

type Weight = u32;
type Min = u8;
type Max = u8;

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnEntry {
    /// User-facing info for wiki, statistical tools, etc.
    pub name: String,
    pub note: String,
    /// Rules describing what and when to spawn
    pub rules: Vec<Pack>,
}

impl SpawnEntry {
    pub fn from(asset_specifier: &str) -> Self {
        Ron::load_expect_cloned(asset_specifier).into_inner()
    }

    pub fn request(
        &self,
        requested_period: DayPeriod,
        calendar: Option<&Calendar>,
        is_underwater: bool,
        is_ice: bool,
    ) -> Option<Pack> {
        self.rules
            .iter()
            .find(|pack| {
                let time_match = pack.day_period.contains(&requested_period);
                let calendar_match = if let Some(calendar) = calendar {
                    pack.calendar_events
                        .as_ref()
                        .is_none_or(|events| events.iter().any(|event| calendar.is_event(*event)))
                } else {
                    false
                };
                let mode_match = match pack.spawn_mode {
                    SpawnMode::Land => !is_underwater,
                    SpawnMode::Ice => is_ice,
                    SpawnMode::Water | SpawnMode::Underwater => is_underwater,
                    SpawnMode::Air(_) => true,
                };
                time_match && calendar_match && mode_match
            })
            .cloned()
    }
}

/// Dataset of animals to spawn
///
/// Example:
/// ```text
///        Pack(
///            groups: [
///                (3, (1, 2, "common.entity.wild.aggressive.frostfang")),
///                (1, (1, 1, "common.entity.wild.aggressive.snow_leopard")),
///                (1, (1, 1, "common.entity.wild.aggressive.yale")),
///                (1, (1, 1, "common.entity.wild.aggressive.grolgar")),
///            ],
///            spawn_mode: Land,
///            day_period: [Night, Morning, Noon, Evening],
///        ),
/// ```
/// Groups:
/// ```text
///                (3, (1, 2, "common.entity.wild.aggressive.frostfang")),
/// ```
/// (3, ...) means that it has x3 chance to spawn (3/6 when every other has
/// 1/6).
///
/// (.., (1, 2, ...)) is `1..=2` group size which means that it has
/// chance to spawn as single mob or in pair
///
/// (..., (..., "common.entity.wild.aggressive.frostfang")) corresponds
/// to `assets/common/entity/wild/aggressive/frostfang.ron` file with
/// EntityConfig
///
/// Spawn mode:
/// `spawn_mode: Land` means mobs spawn on land at the surface (i.e: cows)
/// `spawn_mode: means mobs spawn on the surface of water ice
/// `spawn_mode: Water` means mobs spawn *in* water at a random depth (i.e:
/// fish) `spawn_mode: Underwater` means mobs spawn at the bottom of a body of
/// water (i.e: crabs) `spawn_mode: Air(32)` means mobs spawn in the air above
/// either land or water, with a maximum altitude of 32
///
/// Day period:
/// `day_period: [Night, Morning, Noon, Evening]`
/// means that mobs from this pack may be spawned in any day period without
/// exception
#[derive(Clone, Debug, Deserialize)]
pub struct Pack {
    pub groups: Vec<(Weight, (Min, Max, String))>,
    pub spawn_mode: SpawnMode,
    pub day_period: Vec<DayPeriod>,
    #[serde(default)]
    pub calendar_events: Option<Vec<CalendarEvent>>, /* None implies that the group isn't
                                                      * limited by calendar events */
}

#[derive(Copy, Clone, Debug, Deserialize)]
pub enum SpawnMode {
    Land,
    Ice,
    Water,
    Underwater,
    Air(f32),
}

impl Pack {
    pub fn generate(&self, pos: Vec3<f32>, dynamic_rng: &mut impl Rng) -> EntitySpawn {
        let (_, (from, to, entity_asset)) = self
            .groups
            .choose_weighted(dynamic_rng, |(p, _group)| *p)
            .expect("Failed to choose group");
        let entity = EntityInfo::at(pos).with_asset_expect(entity_asset, dynamic_rng, None);
        let group_size = dynamic_rng.random_range(*from..=*to);

        if group_size > 1 {
            let group = iter::repeat_n(entity, group_size as usize).collect::<Vec<_>>();

            EntitySpawn::Group(group)
        } else {
            EntitySpawn::Entity(Box::new(entity))
        }
    }
}

pub type DensityFn = fn(&SimChunk, &ColumnSample) -> f32;

pub fn spawn_manifest() -> Vec<(&'static str, DensityFn)> {
    const BASE_DENSITY: f32 = 1.0e-5; // Base wildlife density
    // NOTE: Order matters.
    // Entries with more specific requirements
    // and overall scarcity should come first, where possible.
    vec![
        // **Tundra**
        // Rock animals
        ("world.wildlife.spawn.tundra.rock", |c, col| {
            close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * col.rock_density * 1.0
        }),
        // Core animals
        ("world.wildlife.spawn.tundra.core", |c, _col| {
            close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * 0.5
        }),
        // Core animals events
        (
            "world.wildlife.spawn.calendar.christmas.tundra.core",
            |c, _col| close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * 0.5,
        ),
        (
            "world.wildlife.spawn.calendar.halloween.tundra.core",
            |c, _col| close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * 1.0,
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.tundra.core",
            |c, _col| close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * 0.5,
        ),
        (
            "world.wildlife.spawn.calendar.easter.tundra.core",
            |c, _col| close(c.temp, CONFIG.snow_temp, 0.15) * BASE_DENSITY * 0.5,
        ),
        // Snowy animals
        ("world.wildlife.spawn.tundra.snow", |c, col| {
            close(c.temp, CONFIG.snow_temp, 0.3) * BASE_DENSITY * col.snow_cover as i32 as f32 * 1.0
        }),
        // Snowy animals event
        (
            "world.wildlife.spawn.calendar.christmas.tundra.snow",
            |c, col| {
                close(c.temp, CONFIG.snow_temp, 0.3)
                    * BASE_DENSITY
                    * col.snow_cover as i32 as f32
                    * 1.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.halloween.tundra.snow",
            |c, col| {
                close(c.temp, CONFIG.snow_temp, 0.3)
                    * BASE_DENSITY
                    * col.snow_cover as i32 as f32
                    * 1.5
            },
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.tundra.snow",
            |c, col| {
                close(c.temp, CONFIG.snow_temp, 0.3)
                    * BASE_DENSITY
                    * col.snow_cover as i32 as f32
                    * 1.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.easter.tundra.snow",
            |c, col| {
                close(c.temp, CONFIG.snow_temp, 0.3)
                    * BASE_DENSITY
                    * col.snow_cover as i32 as f32
                    * 1.0
            },
        ),
        // Forest animals
        ("world.wildlife.spawn.tundra.forest", |c, col| {
            close(c.temp, CONFIG.snow_temp, 0.3) * col.tree_density * BASE_DENSITY * 1.4
        }),
        // River wildlife
        ("world.wildlife.spawn.tundra.river", |c, col| {
            close(col.temp, CONFIG.snow_temp, 0.3)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt > CONFIG.sea_level + 20.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Forest animals event
        (
            "world.wildlife.spawn.calendar.christmas.tundra.forest",
            |c, col| close(c.temp, CONFIG.snow_temp, 0.3) * col.tree_density * BASE_DENSITY * 1.4,
        ),
        (
            "world.wildlife.spawn.calendar.halloween.tundra.forest",
            |c, col| close(c.temp, CONFIG.snow_temp, 0.3) * col.tree_density * BASE_DENSITY * 2.0,
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.tundra.forest",
            |c, col| close(c.temp, CONFIG.snow_temp, 0.3) * col.tree_density * BASE_DENSITY * 1.4,
        ),
        (
            "world.wildlife.spawn.calendar.easter.tundra.forest",
            |c, col| close(c.temp, CONFIG.snow_temp, 0.3) * col.tree_density * BASE_DENSITY * 1.4,
        ),
        // **Taiga**
        // Forest core animals
        ("world.wildlife.spawn.taiga.core_forest", |c, col| {
            close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * col.tree_density * BASE_DENSITY * 0.4
        }),
        // Forest core animals event
        (
            "world.wildlife.spawn.calendar.christmas.taiga.core_forest",
            |c, col| {
                close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * col.tree_density * BASE_DENSITY * 0.4
            },
        ),
        (
            "world.wildlife.spawn.calendar.halloween.taiga.core",
            |c, col| {
                close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * col.tree_density * BASE_DENSITY * 0.8
            },
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.taiga.core",
            |c, col| {
                close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * col.tree_density * BASE_DENSITY * 0.4
            },
        ),
        (
            "world.wildlife.spawn.calendar.easter.taiga.core",
            |c, col| {
                close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * col.tree_density * BASE_DENSITY * 0.4
            },
        ),
        // Core animals
        ("world.wildlife.spawn.taiga.core", |c, _col| {
            close(c.temp, CONFIG.snow_temp + 0.2, 0.2) * BASE_DENSITY * 1.0
        }),
        // Forest area animals
        ("world.wildlife.spawn.taiga.forest", |c, col| {
            close(c.temp, CONFIG.snow_temp + 0.2, 0.6) * col.tree_density * BASE_DENSITY * 0.9
        }),
        // Area animals
        ("world.wildlife.spawn.taiga.area", |c, _col| {
            close(c.temp, CONFIG.snow_temp + 0.2, 0.6) * BASE_DENSITY * 5.0
        }),
        // Water animals
        ("world.wildlife.spawn.taiga.water", |c, col| {
            close(c.temp, CONFIG.snow_temp, 0.15) * col.tree_density * BASE_DENSITY * 5.0
        }),
        // River wildlife
        ("world.wildlife.spawn.taiga.river", |c, col| {
            close(col.temp, CONFIG.snow_temp + 0.2, 0.6)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt > CONFIG.sea_level + 20.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // **Temperate**
        // Area rare
        ("world.wildlife.spawn.temperate.rare", |c, _col| {
            close(c.temp, CONFIG.temperate_temp, 0.8) * BASE_DENSITY * 0.08
        }),
        // Plains
        ("world.wildlife.spawn.temperate.plains", |c, _col| {
            close(c.temp, CONFIG.temperate_temp, 0.8)
                * close(c.tree_density, 0.0, 0.1)
                * BASE_DENSITY
                * 5.0
        }),
        // River wildlife
        ("world.wildlife.spawn.temperate.river", |c, col| {
            close(col.temp, CONFIG.temperate_temp, 0.6)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt > CONFIG.sea_level + 20.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Forest animals
        ("world.wildlife.spawn.temperate.wood", |c, col| {
            close(c.temp, CONFIG.temperate_temp + 0.1, 0.5) * col.tree_density * BASE_DENSITY * 5.0
        }),
        // Rainforest animals
        ("world.wildlife.spawn.temperate.rainforest", |c, _col| {
            close(c.temp, CONFIG.temperate_temp + 0.1, 0.6)
                * close(c.humidity, CONFIG.forest_hum, 0.6)
                * BASE_DENSITY
                * 5.0
        }),
        // Temperate Rainforest animals event
        (
            "world.wildlife.spawn.calendar.halloween.temperate.rainforest",
            |c, _col| {
                close(c.temp, CONFIG.temperate_temp + 0.1, 0.6)
                    * close(c.humidity, CONFIG.forest_hum, 0.6)
                    * BASE_DENSITY
                    * 5.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.temperate.rainforest",
            |c, _col| {
                close(c.temp, CONFIG.temperate_temp + 0.1, 0.6)
                    * close(c.humidity, CONFIG.forest_hum, 0.6)
                    * BASE_DENSITY
                    * 4.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.easter.temperate.rainforest",
            |c, _col| {
                close(c.temp, CONFIG.temperate_temp + 0.1, 0.6)
                    * close(c.humidity, CONFIG.forest_hum, 0.6)
                    * BASE_DENSITY
                    * 4.0
            },
        ),
        // Ocean animals
        ("world.wildlife.spawn.temperate.ocean", |c, col| {
            not_cromatolis(c) * close(col.temp, CONFIG.temperate_temp, 1.0) / 10.0
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Ocean beach animals
        ("world.wildlife.spawn.temperate.beach", |c, col| {
            close(col.temp, CONFIG.temperate_temp, 1.0) / 10.0
                * if col.water_dist.map(|d| d < 30.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt < CONFIG.sea_level + 2.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // **Jungle**
        // Rainforest animals
        ("world.wildlife.spawn.jungle.rainforest", |c, _col| {
            close(c.temp, CONFIG.tropical_temp + 0.2, 0.2)
                * close(c.humidity, CONFIG.jungle_hum, 0.2)
                * BASE_DENSITY
                * 2.8
        }),
        // Rainforest area animals
        ("world.wildlife.spawn.jungle.rainforest_area", |c, _col| {
            close(c.temp, CONFIG.tropical_temp + 0.2, 0.3)
                * close(c.humidity, CONFIG.jungle_hum, 0.2)
                * BASE_DENSITY
                * 8.0
        }),
        // Jungle animals event
        (
            "world.wildlife.spawn.calendar.halloween.jungle.area",
            |c, _col| {
                close(c.temp, CONFIG.tropical_temp + 0.2, 0.3)
                    * close(c.humidity, CONFIG.jungle_hum, 0.2)
                    * BASE_DENSITY
                    * 10.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.jungle.area",
            |c, _col| {
                close(c.temp, CONFIG.tropical_temp + 0.2, 0.3)
                    * close(c.humidity, CONFIG.jungle_hum, 0.2)
                    * BASE_DENSITY
                    * 8.0
            },
        ),
        (
            "world.wildlife.spawn.calendar.easter.jungle.area",
            |c, _col| {
                close(c.temp, CONFIG.tropical_temp + 0.2, 0.3)
                    * close(c.humidity, CONFIG.jungle_hum, 0.2)
                    * BASE_DENSITY
                    * 8.0
            },
        ),
        // **Tropical**
        // River animals
        ("world.wildlife.spawn.tropical.river", |c, col| {
            not_cromatolis(c)
                * close(col.temp, CONFIG.tropical_temp, 0.5)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt > CONFIG.sea_level + 20.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Ocean animals
        ("world.wildlife.spawn.tropical.ocean", |c, col| {
            not_cromatolis(c) * close(col.temp, CONFIG.tropical_temp, 0.1) / 10.0
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Ocean beach animals
        ("world.wildlife.spawn.tropical.beach", |c, col| {
            close(col.temp, CONFIG.tropical_temp, 1.0) / 10.0
                * if col.water_dist.map(|d| d < 30.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt < CONFIG.sea_level + 2.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Arctic ocean animals
        ("world.wildlife.spawn.arctic.ocean", |c, col| {
            not_cromatolis(c) * close(col.temp, CONFIG.snow_temp, 0.25) / 10.0
                * if matches!(col.chunk.get_biome(), BiomeKind::Ocean) {
                    0.001
                } else {
                    0.0
                }
        }),
        // Cromatolis ocean animals -- scoped to the authored region rather
        // than widening an existing window (same pattern as `not_cromatolis`
        // below, just the positive case). The region's authored climate puts
        // its water outside every general ocean window: while the baseline was
        // one flat hot value, real ocean columns measured 0.792 ..= 0.920
        // against temperate.ocean's (-1.4, 0.6), tropical.ocean's (0.3, 0.5)
        // and arctic.ocean's far colder band, and 0 of 328,231 sampled columns
        // got density from any of the three. Per-zone temperatures moved that
        // range to -0.270 ..= 0.570 -- now overlapping two of the three, which
        // is why the `not_cromatolis` gates on them are load-bearing rather
        // than defensive. Widening a shared entry instead would change
        // ocean-fauna density for every other world using this manifest, not
        // just Cromatolis. The three general entries above
        // (and `tropical.river` below, which also matches Lake columns) are
        // now gated with `not_cromatolis(c)`, which is what keeps that
        // non-overlap structural rather than an accident of where the
        // temperature happens to sit -- a retune on either side can no longer
        // silently double-count density on Cromatolis ocean/lake columns. Same
        // defensive pattern the desert entries below use for the reverse
        // direction.
        ("world.wildlife.spawn.cromatolis.ocean", |c, col| {
            f32::from(c.authored_region_id == Some(CROMATOLIS_V0_REGION_ID))
                * cromatolis_aquatic_temp_window(col.temp)
                / 10.0
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Cromatolis lake animals -- two independent gaps stacked on top of
        // each other, both found by sampling the real generated world (only
        // 0.55% of 35,138 sampled real lake columns got any nonzero density
        // from any existing river/lake entry):
        // 1) The same hot-climate temp-window gap as the ocean entry above (real sampled lake temp
        //    is 0.84-0.90).
        // 2) Every generic `*.river` entry (the only ones that also match Lake columns, via their
        //    `!= Ocean` check) additionally gates on `c.alt > CONFIG.sea_level + 20.0`. That gate
        //    is meant to keep river fauna out of brackish estuary mouths near the coast, but it
        //    isn't meaningful for lakes (an enclosed body, never brackish the way a river mouth
        //    is) -- and it's actively wrong for Cromatolis: real sampled lake chunk `alt` is
        //    134-136, i.e. *below* the engine's abstract `CONFIG.sea_level` (140.0), despite these
        //    being lore-"elevated" lakes -- so the gate passed for only 221 of 35,138 sampled real
        //    lake columns (0.63%), matching the pre-fix nonzero-density count almost exactly and
        //    confirming it was the actual bottleneck, not temperature. Uses `river.is_lake()`
        //    directly instead (precise, and doesn't need an altitude proxy at all).
        // 3) COW-22 `C22-1b` made `RiverKind::River` reachable on this map for the first time, so
        //    the gate is `cromatolis_freshwater` (lake *or* river) rather than `is_lake()` -- see
        //    that function's doc comment.
        ("world.wildlife.spawn.cromatolis.lake", |c, col| {
            f32::from(c.authored_region_id == Some(CROMATOLIS_V0_REGION_ID))
                * cromatolis_aquatic_temp_window(col.temp)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && cromatolis_freshwater(col.chunk)
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Rainforest area animals
        ("world.wildlife.spawn.tropical.rainforest", |c, _col| {
            close(c.temp, CONFIG.tropical_temp + 0.1, 0.4)
                * close(c.humidity, CONFIG.jungle_hum, 0.4)
                * BASE_DENSITY
                * 2.0
        }),
        // Tropical Rainforest animals event
        (
            "world.wildlife.spawn.calendar.halloween.tropical.rainforest",
            |c, _col| {
                close(c.temp, CONFIG.tropical_temp + 0.1, 0.4)
                    * close(c.humidity, CONFIG.jungle_hum, 0.4)
                    * BASE_DENSITY
                    * 3.5
            },
        ),
        (
            "world.wildlife.spawn.calendar.april_fools.tropical.rainforest",
            |c, _col| {
                close(c.temp, CONFIG.tropical_temp + 0.1, 0.4)
                    * close(c.humidity, CONFIG.jungle_hum, 0.4)
                    * BASE_DENSITY
                    * 2.0
            },
        ),
        // Rock animals
        ("world.wildlife.spawn.tropical.rock", |c, col| {
            close(c.temp, CONFIG.tropical_temp + 0.1, 0.5) * col.rock_density * BASE_DENSITY * 5.0
        }),
        // **Desert** -- gated off in Cromatolis (see `not_cromatolis`'s doc
        // comment): its authored baseline temperature curve
        // (`cromatolis_baseline_temp`) needs some real coastal chunks to
        // reach the hot end of the abstract scale (several dungeon-site
        // predicates require it), which would otherwise satisfy these
        // temp-only/loosely-humidity-gated density formulas across the
        // large majority of the map -- verified directly against real
        // generated Cromatolis terrain: `close(chunk.temp, CONFIG.desert_temp
        // + 0.2, 0.3)` alone (the ungated `desert.hot` formula below) is
        // nonzero for ~86% of all chunks. Same scoping pattern as
        // `SimChunk::get_biome`'s `Snowland`/`Desert` checks.
        // Area animals
        ("world.wildlife.spawn.desert.area", |c, _col| {
            not_cromatolis(c)
                * close(c.temp, CONFIG.desert_temp + 0.1, 0.4)
                * close(c.humidity, CONFIG.desert_hum, 0.4)
                * BASE_DENSITY
                * 0.8
        }),
        // Wasteland animals
        ("world.wildlife.spawn.desert.wasteland", |c, _col| {
            not_cromatolis(c)
                * close(c.temp, CONFIG.desert_temp + 0.2, 0.3)
                * close(c.humidity, CONFIG.desert_hum, 0.5)
                * BASE_DENSITY
                * 1.3
        }),
        // River animals
        ("world.wildlife.spawn.desert.river", |c, col| {
            not_cromatolis(c)
                * close(col.temp, CONFIG.desert_temp + 0.2, 0.3)
                * if col.water_dist.map(|d| d < 1.0).unwrap_or(false)
                    && !matches!(col.chunk.get_biome(), BiomeKind::Ocean)
                    && c.alt > CONFIG.sea_level + 20.0
                {
                    0.001
                } else {
                    0.0
                }
        }),
        // Hot area desert
        ("world.wildlife.spawn.desert.hot", |c, _col| {
            not_cromatolis(c) * close(c.temp, CONFIG.desert_temp + 0.2, 0.3) * BASE_DENSITY * 3.8
        }),
        // Rock animals
        ("world.wildlife.spawn.desert.rock", |c, col| {
            not_cromatolis(c)
                * close(c.temp, CONFIG.desert_temp + 0.2, 0.05)
                * col.rock_density
                * BASE_DENSITY
                * 4.0
        }),
    ]
}

/// `1.0` outside the authored Cromatolis region, `0.0` inside it. Used to
/// gate density/feature formulas that key off `chunk.temp`/`chunk.humidity`
/// crossing `CONFIG`'s desert thresholds, so that literal desert wildlife
/// never appears on lore-authored tropical/Caribbean ground. Same scoping
/// pattern as `SimChunk::get_biome`'s `Snowland`/`Desert` checks, extracted
/// here since several density formulas need it.
///
/// What it protects against changed with the per-zone climate, but it is
/// still load-bearing. It used to hold back the whole coastline: one flat hot
/// sea-level baseline put ~86% of the grid inside the ungated
/// `world.wildlife.spawn.desert.hot` window. No *climatic* zone reaches
/// `CONFIG.desert_temp` any more (the warmest anchor lands at abstract 0.533),
/// but an authored microclimate pocket does, by design -- so without this gate
/// the magically-warmed ground around a dungeon would spawn desert fauna.
pub(crate) fn not_cromatolis(c: &SimChunk) -> f32 {
    f32::from(c.authored_region_id != Some(CROMATOLIS_V0_REGION_ID))
}

/// Temperature factor shared by the two Cromatolis aquatic spawn entries: the
/// whole abstract range a water column on this map can occupy.
///
/// **This is a placeholder for a per-water-body ecology profile, not a
/// habitat model.** A real one selects fauna by water kind, salinity, depth
/// *and* a narrow temperature band; this deliberately selects none of that,
/// because the alternative -- a narrow window tuned to today's numbers -- is
/// exactly what broke last time.
///
/// The two entries previously used `close(col.temp, CONFIG.desert_temp + 0.1,
/// 0.2)`, i.e. a window of `[0.7, 1.1]`. That fitted a map whose every water
/// column sat at a flat, artificially hot sea-level baseline. Measured against
/// real generated columns after the climate-zone rework, ocean runs
/// `-0.270 ..= 0.570` and freshwater `-0.653 ..= 0.569`; the old window covers
/// **none** of that, so both entries would have gone to hard zero and the map
/// would be fishless again.
///
/// `close(t, 0.0, 1.0)` is nonzero across `(-1.0, 1.0)` -- the entire abstract
/// scale -- and its 0.125-power falloff keeps the factor between 0.87 and 1.0
/// over the range actually observed, against the ~0.917 the old window
/// produced on ocean. So coverage goes to 100% of wet ocean and freshwater
/// columns without inflating density.
fn cromatolis_aquatic_temp_window(temp: f32) -> f32 { close(temp, 0.0, 1.0) }

/// Chunk-level gate for the `cromatolis.lake` spawn entry: the authored map's
/// *freshwater* -- everything that is water but not sea.
///
/// Deliberately reads `SimChunk::water_body` rather than `river.is_lake()`.
/// Until COW-22 `C22-1b` no chunk on the Cromatolis map could be
/// `RiverKind::River` at all (every authored river corridor lost to the
/// broader `water` mask and came out `RiverKind::Lake`), so `is_lake()` alone
/// happened to cover the whole freshwater network. With `RiverKind::River`
/// reachable it no longer does, and the carveable river chunks would silently
/// drop to *zero* wildlife density -- every generic `*.river` manifest entry
/// is `not_cromatolis`-gated, so nothing else would pick them up. Asking the
/// ecological classification directly also survives the next change to how
/// wide corridors are carved, which the physical `RiverKind` would not.
pub(crate) fn cromatolis_freshwater(c: &SimChunk) -> bool {
    matches!(
        c.water_body,
        Some(WaterBodyKind::River | WaterBodyKind::Lake | WaterBodyKind::Lagoon)
    )
}

/// Cromatolis-only density multiplier, loaded from `cromatolis_v0_density_
/// boost.ron` (see `CromatolisWildlifeDensityBoost`) and applied once per
/// column in `apply_wildlife_supplement` on top of every manifest entry's
/// own density formula, rather than editing each affected land/jungle/
/// tropical closure individually (which risks silently missing one as new
/// entries are added). `BASE_DENSITY` (`spawn_manifest`) and the general,
/// non-Cromatolis manifest entries are shared by every world this engine
/// can generate, so they can't be raised without rebalancing every other
/// world using this engine -- same constraint `not_cromatolis` above
/// already documents, just the boost-instead-of-gate case.
///
/// Land and open ocean were both measured, via a real
/// `World::generate_chunk`-instrumented audit (not just the column-density
/// formula, which can be misleadingly optimistic vs. the real
/// roll+gradient+footprint-clearance pipeline), as reading far too sparse
/// during normal exploration; lake (whose presence bug, a separate root
/// cause, is already fixed above by the `cromatolis.lake` manifest entry)
/// measured already dense, so it's left alone (`1.0`). See the commit that
/// introduced this constant for the full real before/after audit numbers
/// and the exact tuning iteration.
///
/// `1.0` for every column outside the authored Cromatolis region (a no-op
/// for every other world using this manifest) and for lake/river columns
/// inside it (left unboosted -- already dense enough, see above).
///
/// Takes only the primitives it needs (rather than `&SimChunk`/
/// `&ColumnSample`) so it stays trivially unit-testable without hand-
/// constructing either of those large, many-field structs.
fn cromatolis_wildlife_boost(
    authored_region_id: Option<&'static str>,
    is_underwater: bool,
    is_ocean: bool,
    boost: CromatolisWildlifeDensityBoost,
) -> f32 {
    if authored_region_id != Some(CROMATOLIS_V0_REGION_ID) {
        return 1.0;
    }
    if !is_underwater {
        boost.land
    } else if is_ocean {
        boost.ocean
    } else {
        1.0
    }
}

/// Cromatolis-only wildlife-density tuning knob (see
/// `cromatolis_wildlife_boost`), loaded from `assets/world/wildlife/
/// cromatolis_v0_density_boost.ron` -- kept as data, alongside
/// `AuthoredCromatolisClimate`'s `cromatolis_v0_climate.ron`, rather than a
/// Rust constant, since it's a designer-tunable balance number that has
/// already needed more than one measure-and-adjust iteration.
#[derive(Clone, Copy, Debug, Deserialize)]
struct CromatolisWildlifeDensityBoost {
    land: f32,
    ocean: f32,
}

impl CromatolisWildlifeDensityBoost {
    fn load() -> Self {
        Ron::load_expect_cloned("world.wildlife.cromatolis_v0_density_boost").into_inner()
    }
}

pub fn apply_wildlife_supplement<'a, R: Rng>(
    // NOTE: Used only for dynamic elements like chests and entities!
    dynamic_rng: &mut R,
    wpos2d: Vec2<i32>,
    mut get_column: impl FnMut(Vec2<i32>) -> Option<&'a ColumnSample<'a>>,
    vol: &(impl RectSizedVol<Vox = Block> + ReadVol + WriteVol),
    index: IndexRef,
    chunk: &SimChunk,
    supplement: &mut ChunkSupplement,
    time: Option<&(TimeOfDay, Calendar)>,
) {
    let scatter = &index.wildlife_spawns;
    // Configurable density multiplier
    let wildlife_density_modifier = index.features.wildlife_density;
    // Loaded once per chunk (not per column) and only when actually inside
    // Cromatolis -- every other world never touches this asset at all. See
    // `cromatolis_wildlife_boost`'s doc comment.
    let cromatolis_density_boost = (chunk.authored_region_id == Some(CROMATOLIS_V0_REGION_ID))
        .then(CromatolisWildlifeDensityBoost::load)
        .unwrap_or(CromatolisWildlifeDensityBoost {
            land: 1.0,
            ocean: 1.0,
        });

    for y in 0..vol.size_xy().y as i32 {
        for x in 0..vol.size_xy().x as i32 {
            let offs = Vec2::new(x, y);

            let wpos2d = wpos2d + offs;

            // Sample terrain
            let col_sample = if let Some(col_sample) = get_column(offs) {
                col_sample
            } else {
                continue;
            };

            let is_underwater = col_sample.water_level > col_sample.alt;
            let is_ice = col_sample.ice_depth > 0.5 && is_underwater;
            let (current_day_period, calendar) = if let Some((time, calendar)) = time {
                (DayPeriod::from(time.0), Some(calendar))
            } else {
                (DayPeriod::Noon, None)
            };

            // Cromatolis-scoped wildlife-density boost (see
            // `cromatolis_wildlife_boost`'s doc comment). `1.0` everywhere
            // outside the authored Cromatolis region, so this is a no-op for
            // every other world using this manifest.
            let cromatolis_boost = cromatolis_wildlife_boost(
                chunk.authored_region_id,
                is_underwater,
                matches!(col_sample.chunk.get_biome(), BiomeKind::Ocean),
                cromatolis_density_boost,
            );

            let entity_group = scatter
                .iter()
                .filter_map(|(entry, get_density)| {
                    let density =
                        get_density(chunk, col_sample) * wildlife_density_modifier * cromatolis_boost;
                    (density > 0.0)
                        .then(|| {
                            entry
                                .read()
                                .0
                                .request(current_day_period, calendar, is_underwater, is_ice)
                                .and_then(|pack| {
                                    (dynamic_rng.random::<f32>() < density * col_sample.spawn_rate
                                        && col_sample.gradient < Some(1.3))
                                    .then_some(pack)
                                })
                        })
                        .flatten()
                })
                .collect::<Vec<_>>() // TODO: Don't allocate
                .choose_mut(dynamic_rng)
                .cloned();

            if let Some(pack) = entity_group {
                let desired_alt = match pack.spawn_mode {
                    SpawnMode::Land | SpawnMode::Underwater => col_sample.alt,
                    SpawnMode::Ice => col_sample.water_level + 1.0 + col_sample.ice_depth,
                    SpawnMode::Water => dynamic_rng.random_range(
                        col_sample.alt..col_sample.water_level.max(col_sample.alt + 0.1),
                    ),
                    SpawnMode::Air(height) => {
                        col_sample.alt.max(col_sample.water_level)
                            + dynamic_rng.random::<f32>() * height
                    },
                };

                // Checks that the entity's *entire* footprint (not just the column's center
                // point) is clear of solid terrain around the candidate column. A
                // center-only check lets an entity spawn with its body clipped into
                // anything solid immediately beside the column, e.g. flush against (or
                // inside) an adjacent tree trunk stamped into the terrain.
                let spawn_offset = |offs_wpos2d: Vec2<i32>, footprint_radius: i32| {
                    // Clamp position to chunk
                    let offs_wpos2d = (offs + offs_wpos2d)
                        .clamped(Vec2::zero(), vol.size_xy().map(|e| e as i32) - 1)
                        - offs;

                    // Find the intersection between ground and air, if there is one near the
                    // surface
                    let z_offset = (0..16)
                        .map(|z| if z % 2 == 0 { z } else { -z } / 2)
                        .find(|z| {
                            (-footprint_radius..=footprint_radius).all(|dx| {
                                (-footprint_radius..=footprint_radius).all(|dy| {
                                    (0..2).all(|z2| {
                                        vol.get(
                                            Vec3::new(offs.x, offs.y, desired_alt as i32)
                                                + (offs_wpos2d + Vec2::new(dx, dy)).with_z(z + z2),
                                        )
                                        .map(|b| !b.is_solid())
                                        .unwrap_or(true)
                                    })
                                })
                            })
                        });

                    z_offset.map(|z_offset| offs_wpos2d.with_z(z_offset).map(|e| e as f32))
                };

                // Bounded so the (2*radius+1)^2 cost of the footprint scan above can't blow
                // up for a handful of oversized wildlife bodies; this comfortably covers
                // ordinary wildlife (e.g. cattle sit well under it) while still being far
                // more accurate than the previous single-point check.
                const MAX_FOOTPRINT_RADIUS: i32 = 4;
                let footprint_radius_for = |body: &common::comp::Body, scale: f32| -> i32 {
                    let radius = (body.max_radius() * scale).ceil().max(0.0) as i32;
                    // If a future wildlife entry uses a body large enough to exceed the cap,
                    // it'll be silently clamped back to the old imprecise checking regime for
                    // that entry — make that loud instead of silent so it gets a wildlife.rs
                    // deep-dive when it happens, rather than reappearing as a "mob in a tree"
                    // report.
                    debug_assert!(
                        radius <= MAX_FOOTPRINT_RADIUS,
                        "wildlife body {body:?} (scale {scale}) has footprint radius {radius} > \
                         MAX_FOOTPRINT_RADIUS ({MAX_FOOTPRINT_RADIUS}); spawn overlap checks for \
                         it will be clamped and may under-check its true footprint"
                    );
                    radius.min(MAX_FOOTPRINT_RADIUS)
                };

                let mut entity_spawn = pack.generate(
                    (wpos2d.map(|e| e as f32) + 0.5).with_z(desired_alt),
                    dynamic_rng,
                );
                match entity_spawn {
                    EntitySpawn::Entity(ref mut entity) => {
                        // Choose a nearby position
                        let offs_wpos2d = (Vec2::new(0.0, 1.0)
                            * (5.0 + dynamic_rng.random::<f32>().powf(0.5) * 5.0))
                            .map(|e| e as i32);
                        let footprint_radius = footprint_radius_for(&entity.body, entity.scale);

                        if let Some(spawn_offset) = spawn_offset(offs_wpos2d, footprint_radius) {
                            entity.pos += spawn_offset;
                            supplement.add_entity_spawn(entity_spawn);
                        }
                    },
                    EntitySpawn::Group(ref mut group) => {
                        let group_size = group.len();
                        for e in (0..group.len()).rev() {
                            // Choose a nearby position
                            let offs_wpos2d = (Vec2::new(
                                (e as f32 / group_size as f32 * 2.0 * f32::consts::PI).sin(),
                                (e as f32 / group_size as f32 * 2.0 * f32::consts::PI).cos(),
                            ) * (5.0
                                + dynamic_rng.random::<f32>().powf(0.5) * 5.0))
                                .map(|e| e as i32);
                            let footprint_radius =
                                footprint_radius_for(&group[e].body, group[e].scale);

                            if let Some(spawn_offset) = spawn_offset(offs_wpos2d, footprint_radius)
                            {
                                group[e].pos += spawn_offset;
                            } else {
                                group.remove(e);
                            }
                        }

                        if !group.is_empty() {
                            supplement.add_entity_spawn(entity_spawn);
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashbrown::HashMap;

    // Regression: `cromatolis_wildlife_boost` must be a strict no-op
    // (`1.0`) for every chunk outside the authored Cromatolis region, for
    // every combination of land/underwater and ocean/non-ocean -- i.e. a
    // non-Cromatolis world's wildlife density is numerically unchanged by
    // the boost this function applies. Exhaustive over the function's whole
    // (tiny) input space, so this can't miss a case.
    #[test]
    fn cromatolis_wildlife_boost_is_noop_outside_cromatolis() {
        // Deliberately not `1.0`/`1.0` -- if the region check were ever
        // accidentally dropped or inverted, a boost this far from identity
        // would make the assertions below fail loudly instead of by luck.
        let boost = CromatolisWildlifeDensityBoost {
            land: 24.0,
            ocean: 3.2,
        };
        for authored_region_id in [None, Some("some_other_future_region")] {
            for is_underwater in [false, true] {
                for is_ocean in [false, true] {
                    assert_eq!(
                        cromatolis_wildlife_boost(
                            authored_region_id,
                            is_underwater,
                            is_ocean,
                            boost
                        ),
                        1.0,
                        "authored_region_id={authored_region_id:?} is_underwater={is_underwater} \
                         is_ocean={is_ocean} must not be boosted outside Cromatolis"
                    );
                }
            }
        }
    }

    // Regression: inside Cromatolis, land and open-ocean columns get the
    // passed-in boost, and lake/river columns (underwater, not ocean) are
    // deliberately left at `1.0` -- see `cromatolis_wildlife_boost`'s doc
    // comment for why.
    #[test]
    fn cromatolis_wildlife_boost_applies_inside_cromatolis() {
        let region = Some(CROMATOLIS_V0_REGION_ID);
        let boost = CromatolisWildlifeDensityBoost {
            land: 24.0,
            ocean: 3.2,
        };
        assert_eq!(
            cromatolis_wildlife_boost(region, false, false, boost),
            boost.land
        );
        assert_eq!(
            cromatolis_wildlife_boost(region, true, true, boost),
            boost.ocean
        );
        assert_eq!(
            cromatolis_wildlife_boost(region, true, false, boost),
            1.0,
            "lake/river columns must stay unboosted"
        );
    }

    // Checks that the real Cromatolis wildlife-density boost asset loads
    // and parses.
    #[test]
    fn cromatolis_wildlife_density_boost_asset_loads() {
        let boost = CromatolisWildlifeDensityBoost::load();
        assert!(boost.land > 0.0);
        assert!(boost.ocean > 0.0);
    }

    // Checks that each entry in spawn manifest is loadable
    #[test]
    fn test_load_entries() {
        let scatter = spawn_manifest();
        for (entry, _) in scatter.into_iter() {
            drop(SpawnEntry::from(entry));
        }
    }

    // Check that each spawn entry has unique name
    #[test]
    fn test_name_uniqueness() {
        let scatter = spawn_manifest();
        let mut names = HashMap::new();
        for (entry, _) in scatter.into_iter() {
            let SpawnEntry { name, .. } = SpawnEntry::from(entry);
            if let Some(old_entry) = names.insert(name, entry) {
                panic!("{}: Found name duplicate with {}", entry, old_entry);
            }
        }
    }

    // Checks that each entity is loadable
    #[test]
    fn test_load_entities() {
        let scatter = spawn_manifest();
        for (entry, _) in scatter.into_iter() {
            let SpawnEntry { rules, .. } = SpawnEntry::from(entry);
            for pack in rules {
                let Pack { groups, .. } = pack;
                for group in &groups {
                    println!("{}:", entry);
                    let (_, (_, _, asset)) = group;
                    let dummy_pos = Vec3::new(0.0, 0.0, 0.0);
                    let mut dummy_rng = rand::rng();
                    let entity =
                        EntityInfo::at(dummy_pos).with_asset_expect(asset, &mut dummy_rng, None);
                    drop(entity);
                }
            }
        }
    }

    // Checks that group distribution has valid form
    #[test]
    fn test_group_choose() {
        let scatter = spawn_manifest();
        for (entry, _) in scatter.into_iter() {
            let SpawnEntry { rules, .. } = SpawnEntry::from(entry);
            for pack in rules {
                let Pack { groups, .. } = pack;
                let dynamic_rng = &mut rand::rng();
                let _ = groups
                    .choose_weighted(dynamic_rng, |(p, _group)| *p)
                    .unwrap_or_else(|err| {
                        panic!("{}: Failed to choose random group. Err: {}", entry, err)
                    });
            }
        }
    }
}
