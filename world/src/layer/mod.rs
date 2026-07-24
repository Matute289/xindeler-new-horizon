pub mod cave;
pub mod rock;
pub mod scatter;
pub mod shrub;
pub mod spot;
pub mod tree;
pub mod wildlife;

pub use self::{
    cave::apply_caves_to, rock::apply_rocks_to, scatter::apply_scatter_to, shrub::apply_shrubs_to,
    spot::apply_spots_to, tree::apply_trees_to,
};

use crate::{
    Canvas, CanvasInfo,
    column::ColumnSample,
    config::CONFIG,
    sim,
    util::{FastNoise, RandomPerm, Sampler},
};
use common::{
    terrain::{Block, BlockKind, SpriteKind, TerrainChunkSize},
    vol::RectVolSize,
};
use hashbrown::HashMap;
use noise::NoiseFn;
use rand::{prelude::*, seq::IndexedRandom};
use serde::Deserialize;
use std::{
    f32,
    ops::{Add, Mul, Range, Sub},
};
use vek::*;

#[derive(Deserialize)]
pub struct Colors {
    pub bridge: (u8, u8, u8),
}

const EMPTY_AIR: Block = Block::empty();
pub(crate) const CROMATOLIS_AUTHORED_PATH_WIDTH: f32 = 6.25;
const CROMATOLIS_PATH_BASE_CUT: f32 = 5.0;
const CROMATOLIS_PATH_BASE_FILL: f32 = 4.0;
const CROMATOLIS_PATH_SIDE_CUT: f32 = 14.0;
const CROMATOLIS_PATH_SIDE_FILL: f32 = 10.0;
const CROMATOLIS_PATH_BRIDGE_START_DELTA: f32 = 16.0;
const CROMATOLIS_PATH_BRIDGE_FULL_DELTA: f32 = 36.0;
const CROMATOLIS_PATH_BRIDGE_FILL: f32 = 24.0;
const CROMATOLIS_PATH_STEEP_START_GRADIENT: f32 = 0.9;
const CROMATOLIS_PATH_STEEP_FULL_GRADIENT: f32 = 1.8;
const CROMATOLIS_PATH_STEEP_CENTER_CUT: f32 = 5.0;
const CROMATOLIS_CHARRAN_APPROACH_DECK_ALT: f32 = 205.0;
const CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT: f32 = 276.0;
const CROMATOLIS_CHARRAN_BRIDGE_DECK_THICKNESS: i32 = 3;
const CROMATOLIS_CHARRAN_APPROACH_HEAD_SPACE: i32 = 20;
const CROMATOLIS_SOURCE_MAP_SIZE: Vec2<f32> = Vec2::new(2048.0, 1536.0);
const CROMATOLIS_CHARRAN_APPROACH_MIN_SOURCE_PX: Vec2<i32> = Vec2::new(1580, 990);
const CROMATOLIS_CHARRAN_APPROACH_MAX_SOURCE_PX: Vec2<i32> = Vec2::new(1628, 1025);
const CROMATOLIS_CHARRAN_BRIDGE_MIN_SOURCE_PX: Vec2<i32> = Vec2::new(1628, 1008);
const CROMATOLIS_CHARRAN_BRIDGE_MAX_SOURCE_PX: Vec2<i32> = Vec2::new(1660, 1034);

#[derive(Copy, Clone)]
struct AuthoredPathProfile {
    riverless_alt: f32,
    depth: i32,
    head_space_bonus: i32,
}

#[derive(Copy, Clone)]
enum CharranRoadOverride {
    BridgeDeck(f32),
    ApproachRamp(f32),
}

fn authored_cromatolis_path_profile(
    path_dist: f32,
    path_width: f32,
    local_riverless_alt: f32,
    center_riverless_alt: f32,
    local_gradient: Option<f32>,
) -> AuthoredPathProfile {
    let path_t = (1.0 - path_dist / path_width).clamped(0.0, 1.0);
    let bench_t = path_t * path_t * (3.0 - 2.0 * path_t);
    let target_delta = center_riverless_alt - local_riverless_alt;
    let side_strength = (target_delta.abs() / 8.0).clamped(0.0, 1.0);
    let bridge_strength = if target_delta > 0.0 {
        ((target_delta - CROMATOLIS_PATH_BRIDGE_START_DELTA)
            / (CROMATOLIS_PATH_BRIDGE_FULL_DELTA - CROMATOLIS_PATH_BRIDGE_START_DELTA))
            .clamped(0.0, 1.0)
            * bench_t
    } else {
        0.0
    };
    let steep_center_strength = ((local_gradient.unwrap_or(0.0)
        - CROMATOLIS_PATH_STEEP_START_GRADIENT)
        / (CROMATOLIS_PATH_STEEP_FULL_GRADIENT - CROMATOLIS_PATH_STEEP_START_GRADIENT))
        .clamped(0.0, 1.0)
        * bench_t
        * (1.0 - bridge_strength);
    let center_riverless_alt =
        center_riverless_alt - CROMATOLIS_PATH_STEEP_CENTER_CUT * steep_center_strength;
    let target_delta = center_riverless_alt - local_riverless_alt;
    let cut = Lerp::lerp(
        CROMATOLIS_PATH_BASE_CUT,
        CROMATOLIS_PATH_SIDE_CUT,
        side_strength,
    ) * bench_t;
    let fill = Lerp::lerp(
        Lerp::lerp(
            CROMATOLIS_PATH_BASE_FILL,
            CROMATOLIS_PATH_SIDE_FILL,
            side_strength,
        ),
        CROMATOLIS_PATH_BRIDGE_FILL,
        bridge_strength,
    ) * bench_t;
    let riverless_alt = local_riverless_alt + target_delta.clamped(-cut, fill);
    let max_fill_depth = if bridge_strength > 0.0 { 28 } else { 14 };
    let fill_depth = (riverless_alt.floor() as i32 - local_riverless_alt.floor() as i32)
        .max(0)
        .min(max_fill_depth);

    AuthoredPathProfile {
        riverless_alt,
        depth: fill_depth + 8,
        head_space_bonus: if side_strength > 0.45 || steep_center_strength > 0.45 {
            4
        } else {
            1
        },
    }
}

fn cromatolis_source_pixel_for_wpos(info: &CanvasInfo, wpos2d: Vec2<i32>) -> Vec2<i32> {
    let world_size =
        info.chunks().get_size().map(|e| e as f32) * TerrainChunkSize::RECT_SIZE.map(|e| e as f32);
    Vec2::new(
        (wpos2d.x as f32 / world_size.x * (CROMATOLIS_SOURCE_MAP_SIZE.x - 1.0))
            .round()
            .clamped(0.0, CROMATOLIS_SOURCE_MAP_SIZE.x - 1.0) as i32,
        ((1.0 - wpos2d.y as f32 / world_size.y) * (CROMATOLIS_SOURCE_MAP_SIZE.y - 1.0))
            .round()
            .clamped(0.0, CROMATOLIS_SOURCE_MAP_SIZE.y - 1.0) as i32,
    )
}

fn cromatolis_charran_road_override_for_source_pixel(
    source_px: Vec2<i32>,
) -> Option<CharranRoadOverride> {
    if source_px.x >= CROMATOLIS_CHARRAN_BRIDGE_MIN_SOURCE_PX.x
        && source_px.y >= CROMATOLIS_CHARRAN_BRIDGE_MIN_SOURCE_PX.y
        && source_px.x <= CROMATOLIS_CHARRAN_BRIDGE_MAX_SOURCE_PX.x
        && source_px.y <= CROMATOLIS_CHARRAN_BRIDGE_MAX_SOURCE_PX.y
    {
        Some(CharranRoadOverride::BridgeDeck(
            CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT,
        ))
    } else if source_px.x >= CROMATOLIS_CHARRAN_APPROACH_MIN_SOURCE_PX.x
        && source_px.y >= CROMATOLIS_CHARRAN_APPROACH_MIN_SOURCE_PX.y
        && source_px.x <= CROMATOLIS_CHARRAN_APPROACH_MAX_SOURCE_PX.x
        && source_px.y <= CROMATOLIS_CHARRAN_APPROACH_MAX_SOURCE_PX.y
    {
        let t = ((source_px.x - CROMATOLIS_CHARRAN_APPROACH_MIN_SOURCE_PX.x) as f32
            / (CROMATOLIS_CHARRAN_APPROACH_MAX_SOURCE_PX.x
                - CROMATOLIS_CHARRAN_APPROACH_MIN_SOURCE_PX.x) as f32)
            .clamped(0.0, 1.0);
        Some(CharranRoadOverride::ApproachRamp(Lerp::lerp(
            CROMATOLIS_CHARRAN_APPROACH_DECK_ALT,
            CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT,
            t,
        )))
    } else {
        None
    }
}

fn apply_cromatolis_charran_road_override(
    mut profile: AuthoredPathProfile,
    local_riverless_alt: f32,
    source_px: Vec2<i32>,
) -> AuthoredPathProfile {
    match cromatolis_charran_road_override_for_source_pixel(source_px) {
        Some(CharranRoadOverride::BridgeDeck(deck_alt)) => {
            let cut_head_space =
                (local_riverless_alt.floor() as i32 - deck_alt.floor() as i32).max(0);
            AuthoredPathProfile {
                riverless_alt: deck_alt,
                depth: CROMATOLIS_CHARRAN_BRIDGE_DECK_THICKNESS,
                head_space_bonus: (cut_head_space + 8).min(96),
            }
        },
        Some(CharranRoadOverride::ApproachRamp(ramp_alt)) => {
            let old_alt = profile.riverless_alt;
            profile.riverless_alt = profile.riverless_alt.max(ramp_alt);
            let fill_depth = (profile.riverless_alt.floor() as i32
                - local_riverless_alt.floor() as i32)
                .max(0)
                .min(14);
            profile.depth = profile.depth.max(fill_depth + 8);

            let cut_head_space =
                (local_riverless_alt.floor() as i32 - profile.riverless_alt.floor() as i32).max(0);
            profile.head_space_bonus = profile
                .head_space_bonus
                .max((cut_head_space + CROMATOLIS_CHARRAN_APPROACH_HEAD_SPACE).min(64));
            debug_assert!(profile.riverless_alt >= old_alt);
            profile
        },
        None => profile,
    }
}

pub struct PathLocals {
    pub riverless_alt: f32,
    pub alt: f32,
    pub water_dist: f32,
    pub bridge_offset: f32,
    pub depth: i32,
}

impl PathLocals {
    pub fn new(info: &CanvasInfo, col: &ColumnSample, path_nearest: Vec2<f32>) -> PathLocals {
        // Try to use the column at the centre of the path for sampling to make them
        // flatter
        let col_pos = -info.wpos().map(|e| e as f32) + path_nearest;
        let col00 = info.col(info.wpos() + col_pos.map(|e| e.floor() as i32) + Vec2::new(0, 0));
        let col10 = info.col(info.wpos() + col_pos.map(|e| e.floor() as i32) + Vec2::new(1, 0));
        let col01 = info.col(info.wpos() + col_pos.map(|e| e.floor() as i32) + Vec2::new(0, 1));
        let col11 = info.col(info.wpos() + col_pos.map(|e| e.floor() as i32) + Vec2::new(1, 1));
        let col_attr = |col: &ColumnSample| {
            Vec3::new(col.riverless_alt, col.alt, col.water_dist.unwrap_or(1000.0))
        };
        let [riverless_alt, alt, water_dist] = match (col00, col10, col01, col11) {
            (Some(col00), Some(col10), Some(col01), Some(col11)) => Lerp::lerp(
                Lerp::lerp(col_attr(col00), col_attr(col10), path_nearest.x.fract()),
                Lerp::lerp(col_attr(col01), col_attr(col11), path_nearest.x.fract()),
                path_nearest.y.fract(),
            ),
            _ => col_attr(col),
        }
        .into_array();
        let (bridge_offset, depth) = (
            ((water_dist.max(0.0) * 0.2).min(f32::consts::PI).cos() + 1.0) * 5.0,
            ((1.0 - ((water_dist + 2.0) * 0.3).min(0.0).cos().abs())
                * (riverless_alt + 5.0 - alt).max(0.0)
                * 1.75
                + 3.0) as i32,
        );
        PathLocals {
            riverless_alt,
            alt,
            water_dist,
            bridge_offset,
            depth,
        }
    }
}

pub fn apply_paths_to(canvas: &mut Canvas) {
    canvas.foreach_col(|canvas, wpos2d, col| {
        if let Some((path_dist, path_nearest, path, _)) =
            col.path.filter(|(dist, _, path, _)| *dist < path.width)
        {
            let inset = 0;

            let authored_path_profile = if canvas.info().chunk.authored_cromatolis_v0 {
                let info = canvas.info();
                let center_alt = PathLocals::new(&info, col, path_nearest).riverless_alt;
                let mut profile = authored_cromatolis_path_profile(
                    path_dist,
                    path.width,
                    col.riverless_alt,
                    center_alt,
                    col.gradient,
                );
                profile = apply_cromatolis_charran_road_override(
                    profile,
                    col.riverless_alt,
                    cromatolis_source_pixel_for_wpos(&info, wpos2d),
                );
                Some(profile)
            } else {
                None
            };
            let riverless_alt = authored_path_profile
                .map(|profile| profile.riverless_alt)
                .unwrap_or_else(|| {
                    PathLocals::new(&canvas.info(), col, path_nearest).riverless_alt
                });

            let surface_z = riverless_alt.floor() as i32;
            let depth = authored_path_profile
                .map(|profile| profile.depth)
                .unwrap_or(4);

            for z in inset - depth..inset {
                let wpos = Vec3::new(wpos2d.x, wpos2d.y, surface_z + z);
                let path_color =
                    path.surface_color(col.sub_surface_color.map(|e| (e * 255.0) as u8), wpos);
                canvas.set(wpos, Block::new(BlockKind::Earth, path_color));
            }
            let head_space = path.head_space(path_dist)
                + authored_path_profile
                    .map(|profile| profile.head_space_bonus)
                    .unwrap_or(0);
            for z in inset..inset + head_space {
                let pos = Vec3::new(wpos2d.x, wpos2d.y, surface_z + z);
                if canvas.get(pos).kind() != BlockKind::Water {
                    canvas.set(pos, EMPTY_AIR);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_cromatolis_sidehill_path_cuts_a_deeper_bench() {
        let flat = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            98.0,
            None,
        );
        let sidehill = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            86.0,
            None,
        );

        assert!(100.0 - sidehill.riverless_alt > 100.0 - flat.riverless_alt);
        assert!(sidehill.head_space_bonus > flat.head_space_bonus);
    }

    #[test]
    fn authored_cromatolis_normal_path_keeps_low_enclosure() {
        let profile = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            100.75,
            None,
        );

        assert!(profile.riverless_alt <= 100.75);
        assert_eq!(profile.head_space_bonus, 1);
        assert_eq!(profile.depth, 8);
    }

    #[test]
    fn authored_cromatolis_filled_path_gets_deeper_support() {
        let profile = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            112.0,
            None,
        );

        assert!(profile.riverless_alt > 100.0);
        assert!(profile.depth > 7);
    }

    #[test]
    fn authored_cromatolis_profile_does_not_fake_tunnels_locally() {
        let profile = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            120.0,
            121.0,
            None,
        );

        assert!(profile.riverless_alt >= 120.0);
        assert_eq!(profile.head_space_bonus, 1);
    }

    #[test]
    fn authored_cromatolis_bridge_gap_gets_high_support() {
        let normal_fill = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            112.0,
            None,
        );
        let bridge_fill = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            100.0,
            140.0,
            None,
        );

        assert!(bridge_fill.riverless_alt - 100.0 > normal_fill.riverless_alt - 100.0);
        assert!(bridge_fill.depth > normal_fill.depth);
    }

    #[test]
    fn authored_cromatolis_steep_center_gets_modest_smoothing() {
        let normal = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            120.0,
            120.0,
            None,
        );
        let steep = authored_cromatolis_path_profile(
            0.0,
            CROMATOLIS_AUTHORED_PATH_WIDTH,
            120.0,
            120.0,
            Some(2.0),
        );

        assert!(steep.riverless_alt < normal.riverless_alt);
        assert!(normal.riverless_alt - steep.riverless_alt <= CROMATOLIS_PATH_STEEP_CENTER_CUT);
    }

    #[test]
    fn authored_cromatolis_charran_bridge_has_local_deck_override() {
        let early_approach_alt =
            match cromatolis_charran_road_override_for_source_pixel(Vec2::new(1607, 1005))
                .expect("the 1607,1005 bridge approach must not fall back to the generic road")
            {
                CharranRoadOverride::ApproachRamp(alt) => alt,
                CharranRoadOverride::BridgeDeck(_) => panic!("1607,1005 must be an approach ramp"),
            };
        assert!(early_approach_alt > CROMATOLIS_CHARRAN_APPROACH_DECK_ALT);
        assert!(early_approach_alt < CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT);

        let approach_alt =
            match cromatolis_charran_road_override_for_source_pixel(Vec2::new(1626, 1011))
                .expect("the 1626,1011 bridge approach must be treated as a carved route segment")
            {
                CharranRoadOverride::ApproachRamp(alt) => alt,
                CharranRoadOverride::BridgeDeck(_) => panic!("1626,1011 must be an approach ramp"),
            };
        assert!(approach_alt > early_approach_alt);
        assert!(approach_alt > CROMATOLIS_CHARRAN_APPROACH_DECK_ALT);
        assert!(approach_alt < CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT);
        match cromatolis_charran_road_override_for_source_pixel(Vec2::new(1642, 1021)) {
            Some(CharranRoadOverride::BridgeDeck(alt)) => {
                assert_eq!(alt, CROMATOLIS_CHARRAN_BRIDGE_DECK_ALT)
            },
            _ => panic!("1642,1021 must be the bridge deck"),
        }
        assert_eq!(CROMATOLIS_CHARRAN_BRIDGE_DECK_THICKNESS, 3);
        assert_eq!(CROMATOLIS_CHARRAN_APPROACH_HEAD_SPACE, 20);
        assert_eq!(
            cromatolis_charran_road_override_for_source_pixel(Vec2::new(1570, 1021)).map(|_| ()),
            None
        );
    }

    #[test]
    fn authored_cromatolis_charran_approach_never_buries_normal_road() {
        let profile = AuthoredPathProfile {
            riverless_alt: 260.0,
            depth: 8,
            head_space_bonus: 1,
        };

        let adjusted =
            apply_cromatolis_charran_road_override(profile, 260.0, Vec2::new(1607, 1005));

        assert_eq!(adjusted.riverless_alt, profile.riverless_alt);
        assert!(adjusted.depth >= profile.depth);
        assert!(adjusted.head_space_bonus >= CROMATOLIS_CHARRAN_APPROACH_HEAD_SPACE);
    }
}

pub fn apply_trains_to(
    canvas: &mut Canvas,
    sim: &sim::WorldSim,
    sim_chunk: &sim::SimChunk,
    chunk_center_wpos2d: Vec2<i32>,
) {
    let mut splines = Vec::new();
    let g = |v: Vec2<f32>| -> Vec3<f32> {
        let path_nearest = sim
            .get_nearest_path(v.as_::<i32>())
            .map(|x| x.1)
            .unwrap_or(v.as_::<f32>());
        let alt = if let Some(c) = canvas.col_or_gen(v.as_::<i32>()) {
            let pl = PathLocals::new(canvas, &c, path_nearest);
            pl.riverless_alt + pl.bridge_offset + 0.75
        } else {
            sim_chunk.alt
        };
        v.with_z(alt)
    };
    fn hermite_to_bezier(
        p0: Vec3<f32>,
        m0: Vec3<f32>,
        p3: Vec3<f32>,
        m3: Vec3<f32>,
    ) -> CubicBezier3<f32> {
        let hermite = Vec4::new(p0, p3, m0, m3);
        let hermite = hermite.map(|v| v.with_w(0.0));
        let hermite: [[f32; 4]; 4] = hermite.map(|v: Vec4<f32>| v.into_array()).into_array();
        // https://courses.engr.illinois.edu/cs418/sp2009/notes/12-MoreSplines.pdf
        let mut m = Mat4::from_row_arrays([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [-3.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, -3.0, 3.0],
        ]);
        m.invert();
        let bezier = m * Mat4::from_row_arrays(hermite);
        let bezier: Vec4<Vec4<f32>> =
            Vec4::<[f32; 4]>::from(bezier.into_row_arrays()).map(Vec4::from);
        let bezier = bezier.map(Vec3::from);
        CubicBezier3::from(bezier)
    }
    for sim::NearestWaysData { bezier: bez, .. } in
        sim.get_nearest_ways(chunk_center_wpos2d, &|chunk| Some(chunk.path))
    {
        if bez.length_by_discretization(16) < 0.125 {
            continue;
        }
        let a = 0.0;
        let b = 1.0;
        for bez in bez.split((a + b) / 2.0) {
            let p0 = g(bez.evaluate(a));
            let p1 = g(bez.evaluate(a + (b - a) / 3.0));
            let p2 = g(bez.evaluate(a + 2.0 * (b - a) / 3.0));
            let p3 = g(bez.evaluate(b));
            splines.push(hermite_to_bezier(p0, 3.0 * (p1 - p0), p3, 3.0 * (p3 - p2)));
        }
    }
    for spline in splines.into_iter() {
        canvas.chunk.meta_mut().add_track(spline);
    }
}

pub fn apply_coral_to(canvas: &mut Canvas) {
    let info = canvas.info();

    if !info.chunk.river.near_water() {
        return; // Don't bother with coral for a chunk nowhere near water
    }

    canvas.foreach_col(|canvas, wpos2d, col| {
        const CORAL_DEPTH: Range<f32> = 14.0..32.0;
        const CORAL_HEIGHT: f32 = 14.0;
        const CORAL_DEPTH_FADEOUT: f32 = 5.0;
        const CORAL_SCALE: f32 = 10.0;

        let water_depth = col.water_level - col.alt;

        if !CORAL_DEPTH.contains(&water_depth) {
            return; // Avoid coral entirely for this column if we're outside coral depths
        }

        for z in col.alt.floor() as i32..(col.alt + CORAL_HEIGHT) as i32 {
            let wpos = Vec3::new(wpos2d.x, wpos2d.y, z);

            let coral_factor = Lerp::lerp(
                1.0,
                0.0,
                // Fade coral out due to incorrect depth
                ((water_depth.clamped(CORAL_DEPTH.start, CORAL_DEPTH.end) - water_depth).abs()
                    / CORAL_DEPTH_FADEOUT)
                    .min(1.0),
            ) * Lerp::lerp(
                1.0,
                0.0,
                // Fade coral out due to incorrect altitude above the seabed
                ((z as f32 - col.alt) / CORAL_HEIGHT).powi(2),
            ) * FastNoise::new(info.index.seed + 7)
                .get(wpos.map(|e| e as f64) / 32.0)
                .sub(0.2)
                .mul(100.0)
                .clamped(0.0, 1.0);

            let nz = Vec3::iota().map(|e: u32| FastNoise::new(info.index.seed + e * 177));

            let wpos_warped = wpos.map(|e| e as f32)
                + nz.map(|nz| {
                    nz.get(wpos.map(|e| e as f64) / CORAL_SCALE as f64) * CORAL_SCALE * 0.3
                });

            // let is_coral = FastNoise2d::new(info.index.seed + 17)
            //     .get(wpos_warped.xy().map(|e| e as f64) / CORAL_SCALE)
            //     .sub(1.0 - coral_factor)
            //     .max(0.0)
            //     .div(coral_factor) > 0.5;

            let is_coral = [
                FastNoise::new(info.index.seed),
                FastNoise::new(info.index.seed + 177),
            ]
            .iter()
            .all(|nz| {
                nz.get(wpos_warped.map(|e| e as f64) / CORAL_SCALE as f64)
                    .abs()
                    < coral_factor * 0.3
            });

            if is_coral {
                canvas.set(wpos, Block::new(BlockKind::Rock, Rgb::new(170, 220, 210)));
            }
        }
    });
}

pub fn apply_caverns_to<R: Rng>(canvas: &mut Canvas, dynamic_rng: &mut R) {
    let info = canvas.info();

    let canvern_nz_at = |wpos2d: Vec2<i32>| {
        // Horizontal average scale of caverns
        let scale = 2048.0;
        // How common should they be? (0.0 - 1.0)
        let common = 0.15;

        let cavern_nz = info
            .index()
            .noise
            .cave_nz
            .get((wpos2d.map(|e| e as f64) / scale).into_array()) as f32;
        ((cavern_nz * 0.5 + 0.5 - (1.0 - common)).max(0.0) / common).powf(common * 2.0)
    };

    // Get cavern attributes at a position
    let cavern_at = |wpos2d| {
        let alt = info.land().get_alt_approx(wpos2d);

        // Range of heights for the caverns
        let height_range = 16.0..250.0;
        // Minimum distance below the surface
        let surface_clearance = 64.0;

        let cavern_avg_height = Lerp::lerp(
            height_range.start,
            height_range.end,
            info.index()
                .noise
                .cave_nz
                .get((wpos2d.map(|e| e as f64) / 300.0).into_array()) as f32
                * 0.5
                + 0.5,
        );

        let cavern_avg_alt =
            CONFIG.sea_level.min(alt * 0.25) - height_range.end - surface_clearance;

        let cavern = canvern_nz_at(wpos2d);
        let cavern_height = cavern * cavern_avg_height;

        // Stalagtites
        let stalactite = info
            .index()
            .noise
            .cave_nz
            .get(wpos2d.map(|e| e as f64 * 0.015).into_array())
            .sub(0.5)
            .max(0.0)
            .mul((cavern_height as f64 - 5.0).mul(0.15).clamped(0.0, 1.0))
            .mul(32.0 + cavern_avg_height as f64);

        let hill = info
            .index()
            .noise
            .cave_nz
            .get((wpos2d.map(|e| e as f64) / 96.0).into_array()) as f32
            * cavern
            * 24.0;
        let rugged = 0.4; // How bumpy should the floor be relative to the ceiling?
        let cavern_bottom = (cavern_avg_alt - cavern_height * rugged + hill) as i32;
        let cavern_avg_bottom =
            (cavern_avg_alt - ((height_range.start + height_range.end) * 0.5) * rugged) as i32;
        let cavern_top = (cavern_avg_alt + cavern_height) as i32;
        let cavern_avg_top = (cavern_avg_alt + cavern_avg_height) as i32;

        // Stalagmites rise up to meet stalactites
        let stalagmite = stalactite;

        let floor = stalagmite as i32;

        (
            cavern_bottom,
            cavern_top,
            cavern_avg_bottom,
            cavern_avg_top,
            floor,
            stalactite,
            cavern_avg_bottom + 16, // Water level
        )
    };

    let mut mushroom_cache = HashMap::new();

    struct Mushroom {
        pos: Vec3<i32>,
        stalk: f32,
        head_color: Rgb<u8>,
    }

    // Get mushroom block, if any, at a position
    let mut get_mushroom = |wpos: Vec3<i32>, dynamic_rng: &mut R| {
        for (wpos2d, seed) in info.chunks().gen_ctx.structure_gen.get(wpos.xy()) {
            let mushroom = if let Some(mushroom) =
                mushroom_cache.entry(wpos2d).or_insert_with(|| {
                    let mut rng = RandomPerm::new(seed);
                    let (cavern_bottom, cavern_top, _, _, floor, _, water_level) =
                        cavern_at(wpos2d);
                    let pos = wpos2d.with_z(cavern_bottom + floor);
                    if rng.random_bool(0.15)
                        && cavern_top - cavern_bottom > 32
                        && pos.z > water_level - 2
                    {
                        Some(Mushroom {
                            pos,
                            stalk: 12.0 + rng.random::<f32>().powf(2.0) * 35.0,
                            head_color: Rgb::new(
                                50,
                                rng.random_range(70..110),
                                rng.random_range(100..200),
                            ),
                        })
                    } else {
                        None
                    }
                }) {
                mushroom
            } else {
                continue;
            };

            let wposf = wpos.map(|e| e as f64);
            let warp_freq = 1.0 / 32.0;
            let warp_amp = Vec3::new(12.0, 12.0, 12.0);
            let wposf_warped = wposf.map(|e| e as f32)
                + Vec3::new(
                    FastNoise::new(seed).get(wposf * warp_freq),
                    FastNoise::new(seed + 1).get(wposf * warp_freq),
                    FastNoise::new(seed + 2).get(wposf * warp_freq),
                ) * warp_amp
                    * (wposf.z as f32 - mushroom.pos.z as f32)
                        .mul(0.1)
                        .clamped(0.0, 1.0);

            let rpos = wposf_warped - mushroom.pos.map(|e| e as f32);

            let stalk_radius = 2.5f32;
            let head_radius = 18.0f32;
            let head_height = 16.0;

            let dist_sq = rpos.xy().magnitude_squared();
            if dist_sq < head_radius.powi(2) {
                let dist = dist_sq.sqrt();
                let head_dist = ((rpos - Vec3::unit_z() * mushroom.stalk)
                    / Vec2::broadcast(head_radius).with_z(head_height))
                .magnitude();

                let stalk = mushroom.stalk + Lerp::lerp(head_height * 0.5, 0.0, dist / head_radius);

                // Head
                if rpos.z > stalk
                    && rpos.z <= mushroom.stalk + head_height
                    && dist
                        < head_radius * (1.0 - (rpos.z - mushroom.stalk) / head_height).powf(0.125)
                {
                    if head_dist < 0.85 {
                        let radial = (rpos.x.atan2(rpos.y) * 10.0).sin() * 0.5 + 0.5;
                        return Some(Block::new(
                            BlockKind::GlowingMushroom,
                            Rgb::new(30, 50 + (radial * 100.0) as u8, 100 - (radial * 50.0) as u8),
                        ));
                    } else if head_dist < 1.0 {
                        return Some(Block::new(BlockKind::Wood, mushroom.head_color));
                    }
                }

                if rpos.z <= mushroom.stalk + head_height - 1.0
                    && dist_sq
                        < (stalk_radius * Lerp::lerp(1.5, 0.75, rpos.z / mushroom.stalk)).powi(2)
                {
                    // Stalk
                    return Some(Block::new(BlockKind::Wood, Rgb::new(25, 60, 90)));
                } else if ((mushroom.stalk - 0.1)..(mushroom.stalk + 0.9)).contains(&rpos.z) // Hanging orbs
                    && dist > head_radius * 0.85
                    && dynamic_rng.random_bool(0.1)
                {
                    use SpriteKind::*;
                    let sprites = if dynamic_rng.random_bool(0.1) {
                        &[Beehive, Lantern] as &[_]
                    } else {
                        &[Orb, MycelBlue, MycelBlue] as &[_]
                    };
                    return Some(Block::air(*sprites.choose(dynamic_rng).unwrap()));
                }
            }
        }

        None
    };

    canvas.foreach_col(|canvas, wpos2d, _col| {
        if canvern_nz_at(wpos2d) <= 0.0 {
            return;
        }

        let (
            cavern_bottom,
            cavern_top,
            cavern_avg_bottom,
            cavern_avg_top,
            floor,
            stalactite,
            water_level,
        ) = cavern_at(wpos2d);

        let mini_stalactite = info
            .index()
            .noise
            .cave_nz
            .get(wpos2d.map(|e| e as f64 * 0.08).into_array())
            .sub(0.5)
            .max(0.0)
            .mul(
                ((cavern_top - cavern_bottom) as f64 - 5.0)
                    .mul(0.15)
                    .clamped(0.0, 1.0),
            )
            .mul(24.0 + (cavern_avg_top - cavern_avg_bottom) as f64 * 0.2);
        let stalactite_height = (stalactite + mini_stalactite) as i32;

        let moss_common = 1.5;
        let moss = info
            .index()
            .noise
            .cave_nz
            .get(wpos2d.map(|e| e as f64 * 0.035).into_array())
            .sub(1.0 - moss_common)
            .max(0.0)
            .mul(1.0 / moss_common)
            .powf(8.0 * moss_common)
            .mul(
                ((cavern_top - cavern_bottom) as f64)
                    .mul(0.15)
                    .clamped(0.0, 1.0),
            )
            .mul(16.0 + (cavern_avg_top - cavern_avg_bottom) as f64 * 0.35);

        let plant_factor = info
            .index()
            .noise
            .cave_nz
            .get(wpos2d.map(|e| e as f64 * 0.015).into_array())
            .add(1.0)
            .mul(0.5)
            .powf(2.0);

        let is_vine = |wpos: Vec3<f32>, dynamic_rng: &mut R| {
            let wpos = wpos + wpos.xy().yx().with_z(0.0) * 0.2; // A little twist
            let dims = Vec2::new(7.0, 256.0); // Long and thin
            let vine_posf = (wpos + Vec2::new(0.0, (wpos.x / dims.x).floor() * 733.0)) / dims; // ~Random offset
            let vine_pos = vine_posf.map(|e| e.floor() as i32);
            let mut rng = RandomPerm::new(((vine_pos.x << 16) | vine_pos.y) as u32); // Rng for vine attributes
            if rng.random_bool(0.2) {
                let vine_height = (cavern_avg_top - cavern_avg_bottom).max(64) as f32;
                let vine_base = cavern_avg_bottom as f32 + rng.random_range(48.0..vine_height);
                let vine_y = (vine_posf.y.fract() - 0.5).abs() * 2.0 * dims.y;
                let vine_reach = (vine_y * 0.05).powf(2.0).min(1024.0);
                let vine_z = vine_base + vine_reach;
                if Vec2::new(vine_posf.x.fract() * 2.0 - 1.0, (wpos.z - vine_z) / 5.0)
                    .magnitude_squared()
                    < 1.0f32
                {
                    let kind = if dynamic_rng.random_bool(0.025) {
                        BlockKind::GlowingRock
                    } else {
                        BlockKind::Leaves
                    };
                    Some(Block::new(
                        kind,
                        Rgb::new(
                            85,
                            (vine_y + vine_reach).mul(0.05).sin().mul(35.0).add(85.0) as u8,
                            20,
                        ),
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        };

        let mut last_kind = BlockKind::Rock;
        for z in cavern_bottom - 1..cavern_top {
            use SpriteKind::*;

            let wpos = wpos2d.with_z(z);
            let wposf = wpos.map(|e| e as f32);

            let block = if z < cavern_bottom {
                if z > water_level + dynamic_rng.random_range(4..16) {
                    Block::new(BlockKind::Grass, Rgb::new(10, 75, 90))
                } else {
                    Block::new(BlockKind::Rock, Rgb::new(50, 40, 10))
                }
            } else if z < cavern_bottom + floor {
                Block::new(BlockKind::WeakRock, Rgb::new(110, 120, 150))
            } else if z > cavern_top - stalactite_height {
                if dynamic_rng.random_bool(0.0035) {
                    // Glowing rock in stalactites
                    Block::new(BlockKind::GlowingRock, Rgb::new(30, 150, 120))
                } else {
                    Block::new(BlockKind::WeakRock, Rgb::new(110, 120, 150))
                }
            } else if let Some(mushroom_block) = get_mushroom(wpos, dynamic_rng) {
                mushroom_block
            } else if z > cavern_top - moss as i32 {
                let kind = if dynamic_rng
                    .random_bool(0.05 / (1.0 + ((cavern_top - z).max(0) as f64).mul(0.1)))
                {
                    BlockKind::GlowingMushroom
                } else {
                    BlockKind::Leaves
                };
                Block::new(kind, Rgb::new(50, 120, 160))
            } else if z < water_level {
                Block::water(Empty).with_sprite(
                    if z == cavern_bottom + floor && dynamic_rng.random_bool(0.01) {
                        *[Seagrass, SeaGrapes, SeaweedTemperate, StonyCoral]
                            .choose(dynamic_rng)
                            .unwrap()
                    } else {
                        Empty
                    },
                )
            } else if z == water_level
                && dynamic_rng.random_bool(Lerp::lerp(0.0, 0.05, plant_factor))
                && last_kind == BlockKind::Water
            {
                Block::air(CavernLillypadBlue)
            } else if z == cavern_bottom + floor
                && dynamic_rng.random_bool(Lerp::lerp(0.0, 0.5, plant_factor))
                && last_kind == BlockKind::Grass
            {
                Block::air(
                    *if dynamic_rng.random_bool(0.9) {
                        // High density
                        &[GrassBlueShort, GrassBlueMedium, GrassBlueLong] as &[_]
                    } else if dynamic_rng.random_bool(0.5) {
                        // Medium density
                        &[CaveMushroom] as &[_]
                    } else {
                        // Low density
                        &[LeafyPlant, Fern, Pyrebloom, Moonbell, Welwitch, GrassBlue] as &[_]
                    }
                    .choose(dynamic_rng)
                    .unwrap(),
                )
            } else if z == cavern_top - 1 && dynamic_rng.random_bool(0.001) {
                Block::air(
                    *[CrystalHigh, CeilingMushroom, Orb, MycelBlue]
                        .choose(dynamic_rng)
                        .unwrap(),
                )
            } else if let Some(vine) = is_vine(wposf, dynamic_rng)
                .or_else(|| is_vine(wposf.xy().yx().with_z(wposf.z), dynamic_rng))
            {
                vine
            } else {
                Block::empty()
            };

            last_kind = block.kind();

            let block = if block.is_filled() {
                Block::new(
                    block.kind(),
                    block.get_color().unwrap_or_default().map(|e| {
                        (e as f32 * dynamic_rng.random_range(0.95..1.05)).clamped(0.0, 255.0) as u8
                    }),
                )
            } else {
                block
            };

            canvas.set(wpos, block);
        }
    });
}
