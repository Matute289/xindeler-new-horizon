use crate::{
    CONFIG, IndexRef,
    column::ColumnSample,
    sim::{AuthoredGroundCoverProfile, GroundCoverBand, RiverKind, WorldSim},
    site::SiteKind,
};
use common::{
    terrain::{
        CoordinateConversions, NEIGHBOR_DELTA, TerrainChunkSize,
        map::{Connection, ConnectionKind, MapConfig, MapSample},
        vec2_as_uniform_idx,
    },
    vol::RectVolSize,
};
use noise::NoiseFn;
use std::{f64, ops::Div};
use vek::*;

fn blend_rgb(base: Rgb<u8>, overlay: Rgb<u8>, blend: f64) -> Rgb<u8> {
    let blend = blend.clamp(0.0, 1.0);
    Rgb::new(
        (base.r as f64 * (1.0 - blend) + overlay.r as f64 * blend) as u8,
        (base.g as f64 * (1.0 - blend) + overlay.g as f64 * blend) as u8,
        (base.b as f64 * (1.0 - blend) + overlay.b as f64 * blend) as u8,
    )
}

fn shade_rgb(base: Rgb<u8>, factor: f64) -> Rgb<u8> {
    let factor = factor.clamp(0.35, 1.35);
    Rgb::new(
        (base.r as f64 * factor).clamp(0.0, 255.0) as u8,
        (base.g as f64 * factor).clamp(0.0, 255.0) as u8,
        (base.b as f64 * factor).clamp(0.0, 255.0) as u8,
    )
}

/// Whether authored land-only post-processing may run after the base map color
/// has been selected. This guard encloses texture, hillshade, contour, and
/// ridge work as well as the ground-cover tint, so physical water retains its
/// dedicated base treatment.
fn authored_map_post_processing_applies(
    river_kind: Option<RiverKind>,
    true_alt: f64,
    true_sea_level: f64,
) -> bool {
    !matches!(river_kind, Some(RiverKind::Lake { .. } | RiverKind::Ocean))
        && true_alt >= true_sea_level
}

/// Applies the authored ground-cover band's map-preview tint. Physical map
/// layers deliberately bypass this stage: their water/mountain rendering is
/// applied by `sample_pos` and must not inherit a vegetation tint.
fn authored_ground_cover_preview_tint(
    base: Rgb<u8>,
    profile: Option<&AuthoredGroundCoverProfile>,
    density: f32,
    is_water: bool,
    is_physical_mountain: bool,
) -> (Option<GroundCoverBand>, Rgb<u8>) {
    let Some(profile) = profile.filter(|_| !is_water && !is_physical_mountain) else {
        return (None, base);
    };
    let band = profile.classify(density);
    let definition = profile
        .bands
        .iter()
        .find(|definition| definition.band == band)
        .expect("validated ground-cover profile contains its classified band");
    let tint = Rgb::new(
        (definition.map_tint.0 * 255.0) as u8,
        (definition.map_tint.1 * 255.0) as u8,
        (definition.map_tint.2 * 255.0) as u8,
    );
    (
        Some(band),
        blend_rgb(base, tint, definition.map_blend as f64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        index::{Index, IndexOwned},
        sim::{AuthoredGroundCoverProfile, GroundCoverBand},
    };
    use common::assets::AssetExt;

    fn cromatolis_profile() -> AuthoredGroundCoverProfile {
        AuthoredGroundCoverProfile::load_owned("world.map.cromatolis_v0_ground_cover")
            .expect("the shipped Cromatolis ground-cover profile must load")
    }

    #[test]
    fn hot_authored_forest_preview_uses_its_profile_band_tint() {
        let profile = cromatolis_profile();
        let dry_beige = Rgb::new(0xbd, 0xad, 0x5a);

        let (band, color) =
            authored_ground_cover_preview_tint(dry_beige, Some(&profile), 0.50, false, false);

        assert_eq!(band, Some(GroundCoverBand::Forest));
        assert_eq!(color, Rgb::new(0x1e, 0x61, 0x32));
        assert_ne!(color, dry_beige);
    }

    #[test]
    fn preview_classification_matches_runtime_profile_and_preserves_physical_layers() {
        let profile = cromatolis_profile();
        let original = Rgb::new(0x51, 0x62, 0x73);

        let (band, _) =
            authored_ground_cover_preview_tint(original, Some(&profile), 0.60, false, false);
        assert_eq!(band, Some(profile.classify(0.60)));
        assert_eq!(band, Some(GroundCoverBand::Jungle));

        for (is_water, is_physical_mountain) in [(true, false), (false, true)] {
            assert_eq!(
                authored_ground_cover_preview_tint(
                    original,
                    Some(&profile),
                    0.90,
                    is_water,
                    is_physical_mountain,
                ),
                (None, original),
                "physical water and mountains keep their map treatment",
            );
        }

        assert_eq!(
            authored_ground_cover_preview_tint(original, None, 0.90, false, false),
            (None, original),
            "procedural maps stay on the legacy rendering path",
        );
    }

    #[test]
    fn physical_water_bypasses_the_complete_authored_post_processing_branch() {
        let sea_level = 0.5;

        assert!(!authored_map_post_processing_applies(
            Some(RiverKind::Ocean),
            0.9,
            sea_level,
        ));
        assert!(!authored_map_post_processing_applies(
            Some(RiverKind::Lake {
                neighbor_pass_pos: Vec2::zero(),
            }),
            0.9,
            sea_level,
        ));
        assert!(!authored_map_post_processing_applies(None, 0.49, sea_level));
        assert!(authored_map_post_processing_applies(None, 0.5, sea_level));
    }

    #[test]
    fn sample_pos_keeps_authored_ocean_color_outside_land_post_processing() {
        let mut sampler = WorldSim::empty();
        let sample = &mut sampler.chunks[0];
        sample.authored_cromatolis_v0 = true;
        sample.river.river_kind = Some(RiverKind::Ocean);
        sample.water_alt = 100.0;
        sample.temp = 1.0;
        sample.tree_density = 1.0;
        sampler.authored_ground_cover_profile = Some(cromatolis_profile());

        let config = MapConfig::orthographic(sampler.map_size_lg(), 0.0..=1_000.0);
        let index = IndexOwned::new(Index::new(0));

        let result = sample_pos(&config, &sampler, index.as_index_ref(), None, Vec2::zero());

        assert_eq!(result.rgb, Rgb::new(0x00, 0x4b, 0x82));
    }
}

/// A sample function that grabs the connections at a chunk.
///
/// Currently this just supports rivers, but ideally it can be extended past
/// that.
///
/// A sample function that grabs surface altitude at a column.
/// (correctly reflecting settings like is_basement and is_water).
///
/// The altitude produced by this function at a column corresponding to a
/// particular chunk should be identical to the altitude produced by
/// sample_pos at that chunk.
///
/// You should generally pass a closure over this function into generate
/// when constructing a map for the first time.
/// However, if repeated construction is needed, or alternate base colors
/// are to be used for some reason, one should pass a custom function to
/// generate instead (e.g. one that just looks up the height in a cached
/// array).
pub fn sample_wpos(config: &MapConfig, sampler: &WorldSim, wpos: Vec2<i32>) -> f32 {
    let MapConfig {
        focus,
        gain,

        is_basement,
        is_water,
        ..
    } = *config;

    (sampler
        .get_wpos(wpos)
        .map(|s| {
            if is_basement { s.basement } else { s.alt }.max(if is_water {
                s.water_alt
            } else {
                -f32::INFINITY
            })
        })
        .unwrap_or(CONFIG.sea_level)
        - focus.z as f32)
        / gain
}

/// Samples a MapSample at a chunk.
///
/// You should generally pass a closure over this function into generate
/// when constructing a map for the first time.
/// However, if repeated construction is needed, or alternate base colors
/// are to be used for some reason, one should pass a custom function to
/// generate instead (e.g. one that just looks up the color in a cached
/// array).
// NOTE: Deliberately not putting Rgb colors here in the config file; they
// aren't hot reloaded anyway, and for various reasons they're probably not a
// good idea to update in that way (for example, we currently want water colors
// to match voxygen's).  Eventually we'll fix these sorts of issues in some
// other way.
pub fn sample_pos(
    config: &MapConfig,
    sampler: &WorldSim,
    index: IndexRef,
    samples: Option<&[Option<ColumnSample>]>,
    pos: Vec2<i32>,
) -> MapSample {
    let map_size_lg = config.map_size_lg();
    let MapConfig {
        focus,
        gain,

        is_basement,
        is_water,
        is_ice,
        is_shaded,
        is_temperature,
        is_humidity,
        // is_debug,
        ..
    } = *config;

    let true_sea_level = (CONFIG.sea_level as f64 - focus.z) / gain as f64;

    let (
        chunk_idx,
        alt,
        basement,
        water_alt,
        humidity,
        temperature,
        downhill,
        river_kind,
        spline_derivative,
        is_path,
        is_bridge,
    ) = sampler
        .get(pos)
        .map(|sample| {
            (
                Some(vec2_as_uniform_idx(map_size_lg, pos)),
                sample.alt,
                sample.basement,
                sample.water_alt,
                sample.humidity,
                sample.temp,
                sample.downhill,
                sample.river.river_kind,
                sample.river.spline_derivative,
                sample.path.0.is_way(),
                sample.sites.iter().any(|site| {
                    let site = &index.sites.get(*site);
                    match site.kind {
                        Some(SiteKind::Bridge(_, _)) => {
                            if let Some(plot) =
                                site.wpos_tile(TerrainChunkSize::center_wpos(pos)).plot
                            {
                                matches!(site.plot(plot).kind, crate::site::PlotKind::Bridge(_))
                            } else {
                                false
                            }
                        },
                        _ => false,
                    }
                }),
            )
        })
        .unwrap_or((
            None,
            CONFIG.sea_level,
            CONFIG.sea_level,
            CONFIG.sea_level,
            0.0,
            0.0,
            None,
            None,
            Vec2::zero(),
            false,
            false,
        ));

    let humidity = humidity.clamp(0.0, 1.0);
    let temperature = temperature.clamp(-1.0, 1.0) * 0.5 + 0.5;
    let wpos = pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
    let column_data = samples
        .and_then(|samples| {
            chunk_idx
                .and_then(|chunk_idx| samples.get(chunk_idx))
                .and_then(Option::as_ref)
        })
        .map(|sample| {
            // TODO: Eliminate the redundancy between this and the block renderer.
            let alt = sample.alt;
            let basement = sample.basement;
            let grass_depth = (1.5 + 2.0 * sample.chaos).min(alt - basement);
            let wposz = if is_basement { basement } else { alt };
            let rgb = if is_basement && wposz < alt - grass_depth {
                Lerp::lerp(
                    sample.sub_surface_color,
                    sample.stone_col.map(|e| e as f32 / 255.0),
                    (alt - grass_depth - wposz) * 0.15,
                )
                .map(|e| e as f64)
            } else {
                Lerp::lerp(
                    sample.sub_surface_color,
                    sample.surface_color,
                    ((wposz - (alt - grass_depth)) / grass_depth).sqrt(),
                )
                .map(|e| e as f64)
            };

            (rgb, alt, sample.ice_depth)
        });

    let downhill_wpos = downhill.unwrap_or(wpos + TerrainChunkSize::RECT_SIZE.map(|e| e as i32));
    let alt = if is_basement {
        basement
    } else {
        column_data.map_or(alt, |(_, alt, _)| alt)
    };

    let depth_m = (alt.max(water_alt) - alt).max(0.0) as f64;
    let true_water_alt = (alt.max(water_alt) as f64 - focus.z) / gain as f64;
    let true_alt = (alt as f64 - focus.z) / gain as f64;
    let alt = true_alt.clamp(0.0, 1.0);

    let default_rgb = Rgb::new(
        if is_shaded || is_temperature {
            1.0
        } else {
            0.0
        },
        if is_shaded { 1.0 } else { alt },
        if is_shaded || is_humidity { 1.0 } else { 0.0 },
    );
    let column_rgb = column_data.map(|(rgb, _, _)| rgb).unwrap_or(default_rgb);
    let mut connections = [None; 8];
    let mut has_connections = false;
    // TODO: Support non-river connections.
    // TODO: Support multiple connections.
    let river_width = river_kind.map(|river| match river {
        RiverKind::River { cross_section } => cross_section.x,
        RiverKind::Lake { .. } | RiverKind::Ocean => TerrainChunkSize::RECT_SIZE.x as f32,
    });
    if let (Some(river_width), true) = (river_width, is_water) {
        let downhill_pos = downhill_wpos.wpos_to_cpos();
        NEIGHBOR_DELTA
            .iter()
            .zip(connections.iter_mut())
            .filter(|&(&offset, _)| downhill_pos - pos == Vec2::from(offset))
            .for_each(|(_, connection)| {
                has_connections = true;
                *connection = Some(Connection {
                    kind: ConnectionKind::River,
                    spline_derivative,
                    width: river_width,
                });
            });
    };
    let rgb = if is_water && is_ice && column_data.is_some_and(|(_, _, ice_depth)| ice_depth > 0.0)
    {
        CONFIG.ice_color
    } else {
        match (river_kind, (is_water, true_alt >= true_sea_level)) {
            (_, (false, _)) | (None, (_, true)) | (Some(RiverKind::River { .. }), _) => {
                let (r, g, b) = (
                    (column_rgb.r
                        * if is_temperature {
                            temperature as f64
                        } else {
                            column_rgb.r
                        })
                    .sqrt(),
                    column_rgb.g,
                    (column_rgb.b
                        * if is_humidity {
                            humidity as f64
                        } else {
                            column_rgb.b
                        })
                    .sqrt(),
                );
                Rgb::new((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
            },
            (None | Some(RiverKind::Lake { .. } | RiverKind::Ocean), _) => match depth_m {
                depth if depth < 5.0 => Rgb::new(0, 0xa8, 0xc9),
                depth if depth < 15.0 => Rgb::new(0, 0x91, 0xbd),
                depth if depth < 30.0 => Rgb::new(0, 0x78, 0xab),
                depth if depth < 70.0 => Rgb::new(0, 0x61, 0x99),
                depth if depth < 150.0 => Rgb::new(0, 0x4b, 0x82),
                depth if depth < 300.0 => Rgb::new(0, 0x36, 0x6b),
                _ => Rgb::new(0, 0x22, 0x52),
            },
        }
    };
    let rgb = if let Some(sample) = sampler
        .get(pos)
        .filter(|sample| sample.authored_cromatolis_v0)
        .filter(|_| authored_map_post_processing_applies(river_kind, true_alt, true_sea_level))
    {
        let altitude = ((sample.alt - CONFIG.sea_level) as f64 / 1050.0).clamp(0.0, 1.0);
        let vegetation = sample.tree_density.clamp(0.0, 1.0) as f64;
        let is_physical_water = is_water
            || matches!(river_kind, Some(RiverKind::Lake { .. } | RiverKind::Ocean))
            || true_alt < true_sea_level;
        let profile = sampler.authored_ground_cover_profile.as_ref();

        let mut out = rgb;
        if sample.temp >= 0.0 {
            if profile.is_some() {
                (_, out) = authored_ground_cover_preview_tint(
                    out,
                    profile,
                    sample.tree_density,
                    is_physical_water,
                    altitude > 0.28,
                );
                if !is_physical_water && altitude > 0.28 {
                    let mountain_t = ((altitude - 0.28) / 0.46).clamp(0.0, 1.0);
                    let mountain = if mountain_t > 0.6 {
                        Rgb::new(0x3d, 0x28, 0x1a)
                    } else {
                        Rgb::new(0x78, 0x55, 0x32)
                    };
                    let mountain_blend = mountain_t * (1.0 - vegetation * 0.42) * 0.95;
                    out = blend_rgb(out, mountain, mountain_blend);
                }
            }
        }
        let neighbor_alt = |offset: Vec2<i32>| {
            sampler
                .get(pos + offset)
                .map(|sample| sample.alt)
                .unwrap_or(sample.alt) as f64
        };
        let west = neighbor_alt(Vec2::new(-1, 0));
        let east = neighbor_alt(Vec2::new(1, 0));
        let north = neighbor_alt(Vec2::new(0, -1));
        let south = neighbor_alt(Vec2::new(0, 1));
        let slope =
            (((east - west).powi(2) + (south - north).powi(2)).sqrt() / 190.0).clamp(0.0, 1.0);
        let light = ((west + north) - (east + south)) / 260.0;

        let wposf = (pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32)).map(|e| e as f64);
        let large_noise = sampler
            .gen_ctx
            .hill_nz
            .get((wposf.div(900.0)).into_array())
            .clamp(-1.0, 1.0);
        let fine_noise = sampler
            .gen_ctx
            .small_nz
            .get((wposf.div(180.0)).into_array())
            .clamp(-1.0, 1.0);
        let rock_noise = sampler
            .gen_ctx
            .rock_nz
            .get((wposf.div(420.0)).into_array())
            .clamp(-1.0, 1.0);

        let texture_strength = (0.05 + altitude * 0.07 + vegetation * 0.035).clamp(0.04, 0.14);
        let texture = 1.0
            + large_noise * texture_strength
            + fine_noise * texture_strength * 0.55
            + rock_noise * slope * 0.11;
        out = shade_rgb(out, texture);

        let hillshade = (1.0 + light.clamp(-0.18, 0.18) + slope * 0.10).clamp(0.72, 1.24);
        out = shade_rgb(out, hillshade);

        if altitude > 0.18 {
            let contour_phase = ((sample.alt - CONFIG.sea_level) as f64 / 58.0).rem_euclid(1.0);
            let contour =
                (0.055 - contour_phase.min(1.0 - contour_phase)).clamp(0.0, 0.055) / 0.055;
            let contour_strength = contour * (0.12 + altitude * 0.18).min(0.28);
            out = shade_rgb(out, 1.0 - contour_strength);
        }

        if slope > 0.22 {
            let ridge = ((slope - 0.22) / 0.78).clamp(0.0, 1.0) * (0.16 + altitude * 0.20);
            out = blend_rgb(out, Rgb::new(0x24, 0x1c, 0x15), ridge);
        }

        out
    } else {
        rgb
    };
    let rgb = if is_bridge {
        Rgb::new(0x80, 0x80, 0x80)
    } else if is_path {
        Rgb::new(0x37, 0x29, 0x23)
    } else {
        rgb
    };

    MapSample {
        rgb: Rgb::new(rgb.r, rgb.g, rgb.b),
        alt: if is_water {
            true_alt.max(true_water_alt)
        } else {
            true_alt
        },
        downhill_wpos,
        connections: if has_connections {
            Some(connections)
        } else {
            None
        },
    }
}
