use super::*;
use crate::{Land, site::generation::PrimitiveTransform};
use common::{
    generation::EntityInfo,
    terrain::{BiomeKind, Block, BlockKind},
};
use num::integer::Roots;
use rand::prelude::*;
use vek::*;

enum RoofKind {
    Crenelated,
    Hipped,
}

struct HeightenedViaduct {
    slope_inv: i32,
    bridge_start_offset: i32,
    vault_spacing: i32,
    vault_size: (i32, i32),
    side_vault_size: (i32, i32),
    holes: bool,
}

impl HeightenedViaduct {
    fn random(rng: &mut impl Rng, height: i32) -> Self {
        let vault_spacing = *[3, 4, 5, 6].choose_mut(rng).unwrap();
        Self {
            slope_inv: rng.random_range(6..=8),
            bridge_start_offset: rng.random_range({
                let min = (5 - height / 3).max(0);
                min..=(12 - height).max(min)
            }),
            vault_spacing,
            vault_size: *[(3, 16), (1, 4), (1, 4), (1, 4), (5, 32), (5, 32)]
                .choose_mut(rng)
                .unwrap(),
            side_vault_size: *[(4, 5), (7, 10), (7, 10), (13, 20)]
                .choose_mut(rng)
                .unwrap(),
            holes: vault_spacing >= 4 && vault_spacing % 2 == 0 && rng.random_bool(0.8),
        }
    }
}

enum BridgeKind {
    Flat,
    Tower(RoofKind),
    Short,
    HeightenedViaduct(HeightenedViaduct),
    HangBridge,
    GrandStoneIron {
        deck_width: i32,
        clearance: i32,
        deck_thickness: i32,
    },
    StoneArch {
        deck_width: i32,
        clearance: i32,
        deck_thickness: i32,
    },
    TimberFootbridge {
        deck_width: i32,
        clearance: i32,
        deck_thickness: i32,
    },
    NaturalStoneEarth {
        deck_width: i32,
        clearance: i32,
        deck_thickness: i32,
    },
}

impl BridgeKind {
    fn random(
        rng: &mut impl Rng,
        start: Vec3<i32>,
        start_dist: i32,
        end: Vec3<i32>,
        end_dist: i32,
        water_alt: i32,
    ) -> BridgeKind {
        let len = (start.xy() - end.xy()).map(|e| e.abs()).reduce_max();
        let height = end.z - start.z;
        let down = start.z - water_alt;
        (0..=4)
            .filter_map(|bridge| match bridge {
                0 if height >= 16 => Some(BridgeKind::Tower(match rng.random_range(0..=2) {
                    0 => RoofKind::Crenelated,
                    _ => RoofKind::Hipped,
                })),
                1 if len < 60 => Some(BridgeKind::Short),
                2 if len >= 50
                    && height < 13
                    && down < 20
                    && ((start_dist > 13 && end_dist > 13)
                        || (start_dist - end_dist).abs() < 6) =>
                {
                    Some(BridgeKind::HeightenedViaduct(HeightenedViaduct::random(
                        rng, height,
                    )))
                },
                3 if height < 10 && down > 10 => Some(BridgeKind::HangBridge),
                4 if down > 8 => Some(BridgeKind::Flat),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .choose(rng)
            .unwrap_or(BridgeKind::Flat)
    }

    fn width(&self) -> i32 {
        match self {
            BridgeKind::HangBridge => 2,
            BridgeKind::GrandStoneIron { deck_width, .. }
            | BridgeKind::StoneArch { deck_width, .. }
            | BridgeKind::TimberFootbridge { deck_width, .. }
            | BridgeKind::NaturalStoneEarth { deck_width, .. } => *deck_width,
            _ => 8,
        }
    }
}

fn aabb(min: Vec3<i32>, max: Vec3<i32>) -> Aabb<i32> {
    let aabb = Aabb { min, max }.made_valid();
    Aabb {
        min: aabb.min,
        max: aabb.max + 1,
    }
}

fn render_short(bridge: &Bridge, painter: &Painter) {
    let (bridge_fill, edge_fill) = match bridge.biome {
        BiomeKind::Desert => (
            Fill::Block(Block::new(BlockKind::Rock, Rgb::new(212, 191, 142))),
            Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(190))),
        ),
        _ => (
            Fill::Brick(BlockKind::Rock, Rgb::gray(70), 25),
            Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(130))),
        ),
    };

    let bridge_width = 3;

    let orth_dir = bridge.dir.orthogonal();

    let orthogonal = orth_dir.to_vec2();
    let forward = bridge.dir.to_vec2();

    let len = (bridge.start.xy() - bridge.end.xy())
        .map(|e| e.abs())
        .reduce_max();
    let inset = 4;

    let top = bridge.end.z + (len / 5).max(8) - inset;

    let side = orthogonal * bridge_width;

    let remove = painter.vault(
        aabb(
            (bridge.start.xy() - side + forward * inset).with_z(bridge.start.z),
            (bridge.end.xy() + side - forward * inset).with_z(top - 2),
        ),
        orth_dir,
    );

    // let outset = 7;

    let up_ramp = |point: Vec3<i32>, dir: Dir2, side_len: i32| {
        let forward = dir.to_vec2();
        let side = dir.orthogonal().to_vec2() * side_len;
        let ramp_in = top - point.z;
        painter
            .ramp(
                aabb(
                    point - side,
                    (point.xy() + side + forward * ramp_in).with_z(top),
                ),
                dir,
            )
            .union(painter.aabb(aabb(
                (point - side).with_z(point.z - 4),
                point + side + forward * ramp_in,
            )))
    };

    let bridge_prim = |side_len: i32| {
        let side = orthogonal * side_len;
        painter
            .aabb(aabb(
                (bridge.start.xy() - side + forward * (top - bridge.start.z))
                    .with_z(bridge.start.z),
                (bridge.end.xy() + side - forward * (top - bridge.end.z)).with_z(top),
            ))
            .union(up_ramp(bridge.start, bridge.dir, side_len).union(up_ramp(
                bridge.end,
                -bridge.dir,
                side_len,
            )))
    };

    let b = bridge_prim(bridge_width);

    /*
    let t = 4;
    b.union(
        painter.aabb(aabb(
            (bridge.start.xy() - side - forward * (top - bridge.start.z))
                .with_z(bridge.start.z - t),
            (bridge.end.xy() + side + forward * (top - bridge.end.z))
                .with_z(bridge.start.z),
        )),
    )
    .translate(Vec3::new(0, 0, t))
    .without(b)
    .clear();
    */

    b.without(remove).fill(bridge_fill);

    let prim = bridge_prim(bridge_width + 1);

    prim.translate(Vec3::unit_z())
        .without(prim)
        .without(painter.aabb(aabb(
            bridge.start - side - forward,
            (bridge.end.xy() + side + forward).with_z(top + 1),
        )))
        .fill(edge_fill);
}

fn render_flat(bridge: &Bridge, painter: &Painter) {
    let light_rock_color = Rgb::gray(130);
    let surface_color = bridge.surface_color.map(|e| (e * 255.0) as u8);
    let gradient_center = Vec3::new(
        bridge.center.x as f32,
        bridge.center.y as f32,
        (bridge.center.z + 1) as f32,
    );

    let light_rock = Fill::GradientBrick(
        util::gradient::Gradient::new(
            gradient_center,
            8.0,
            util::gradient::Shape::plane(Vec3::unit_z()),
            (surface_color, light_rock_color),
        ),
        BlockKind::Rock,
        25,
    );
    let rock = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(50)));

    let orth_dir = bridge.dir.orthogonal();

    let orthogonal = orth_dir.to_vec2();
    let forward = bridge.dir.to_vec2();

    let height = bridge.end.z - bridge.start.z;

    let bridge_width = bridge.width();
    let side = orthogonal * bridge_width;

    let aabr = Aabr {
        min: bridge.start.xy() - side,
        max: bridge.end.xy() + side,
    }
    .made_valid();

    let [ramp_aabr, aabr] = bridge.dir.split_aabr_offset(aabr, height);

    let ramp_prim = |ramp_aabr: Aabr<i32>, offset: i32| {
        painter
            .aabb(aabb(
                ramp_aabr.min.with_z(bridge.start.z - 10 + offset),
                ramp_aabr.max.with_z(bridge.start.z - 1 + offset),
            ))
            .union(painter.ramp(
                aabb(
                    ramp_aabr.min.with_z(bridge.start.z + offset),
                    ramp_aabr.max.with_z(bridge.end.z + offset),
                ),
                bridge.dir,
            ))
    };

    ramp_prim(ramp_aabr, 1).fill(light_rock.clone());

    let ramp_aabr = orth_dir
        .opposite()
        .trim_aabr(orth_dir.trim_aabr(ramp_aabr, 1), 1);
    ramp_prim(ramp_aabr, 5).clear();
    ramp_prim(ramp_aabr, 0).fill(rock.clone());

    let vault_width = 12;
    let vault_offset = 5;
    let bridge_thickness = 4;

    let [vault, _] = bridge.dir.split_aabr_offset(aabr, vault_width);

    let len = bridge.dir.select(aabr.size());
    let true_offset = vault_width + vault_offset;
    let n = (len / true_offset).max(1);
    let p = len / n;

    let holes = painter
        .vault(
            aabb(
                vault.min.with_z(bridge.center.z - 20),
                vault.max.with_z(bridge.end.z - bridge_thickness - 1),
            ),
            orth_dir,
        )
        .repeat((forward * p).with_z(0), n as u32);

    painter
        .aabb(aabb(
            aabr.min.with_z(bridge.center.z - 10),
            aabr.max.with_z(bridge.end.z + 1),
        ))
        .without(holes)
        .fill(light_rock);

    let aabr = orth_dir
        .opposite()
        .trim_aabr(orth_dir.trim_aabr(aabr, 1), 1);
    painter
        .aabb(aabb(
            aabr.min.with_z(bridge.end.z + 1),
            aabr.max.with_z(bridge.end.z + 8),
        ))
        .clear();

    painter
        .aabb(aabb(
            aabr.min.with_z(bridge.end.z),
            aabr.max.with_z(bridge.end.z),
        ))
        .fill(rock);
}

fn render_grand_stone_iron(
    bridge: &Bridge,
    painter: &Painter,
    deck_width: i32,
    clearance: i32,
    deck_thickness: i32,
) {
    let start = bridge.start.xy();
    let end = bridge.end.xy();
    let spine = rasterized_bridge_spine(start, end);
    let half_width = (deck_width / 2).max(4);
    let delta = end - start;
    let width_axis = if delta.x.abs() >= delta.y.abs() {
        Vec2::new(0, half_width)
    } else {
        Vec2::new(half_width, 0)
    };

    // Authored crossings are freeform lines on the 2048 x 1536 source map.
    // `Dir2` only has four cardinal directions, so using a single AABB here
    // fills the entire diagonal bounding rectangle. Rasterise the deck along
    // the actual source segment instead: a long diagonal crossing remains a
    // narrow bridge rather than becoming a solid artificial island.
    let deck_z = (bridge.water_alt + clearance).max(bridge.start.z.max(bridge.end.z));
    let stone = Fill::Brick(BlockKind::Rock, Rgb::new(106, 103, 98), 14);
    let trim_stone = Fill::Brick(BlockKind::Rock, Rgb::new(72, 70, 68), 10);
    let road = Fill::Brick(BlockKind::Rock, Rgb::new(82, 75, 70), 8);
    let iron = Fill::Brick(BlockKind::Rock, Rgb::new(20, 23, 28), 5);
    let lamp = Fill::Block(Block::new(BlockKind::GlowingRock, Rgb::new(245, 189, 84)));

    // The central carriageway uses a gentle 1:4 stepped grade.  Pedestrians
    // get two clearly distinct stone stair lanes at the outside edges, so the
    // civic crossing reads as a bridge approach rather than a flat slab that
    // suddenly begins above the banks.
    for (bank, outward) in [
        (bridge.start, -bridge_step_direction(delta)),
        (bridge.end, bridge_step_direction(delta)),
    ] {
        render_grand_bridge_approach(
            painter,
            bank,
            outward,
            width_axis,
            deck_z,
            deck_thickness,
            road.clone(),
            trim_stone.clone(),
        );
    }

    for point in &spine {
        painter
            .aabb(aabb(
                (*point - width_axis).with_z(deck_z - deck_thickness + 1),
                (*point + width_axis).with_z(deck_z),
            ))
            .fill(road.clone());
        for curb_side in [width_axis, -width_axis] {
            painter
                .aabb(aabb(
                    (*point + curb_side).with_z(deck_z + 1),
                    (*point + curb_side).with_z(deck_z + 2),
                ))
                .fill(trim_stone.clone());
        }
    }

    // Pair each open span with a narrow masonry pier.  They are deliberately
    // sparse and never meet across the channel, so boats can pass between them.
    let pier_step = 36usize;
    for point in spine.iter().step_by(pier_step).chain(spine.last()) {
        painter
            .aabb(aabb(
                (*point - Vec2::broadcast(2)).with_z(bridge.water_alt),
                (*point + Vec2::broadcast(2)).with_z(deck_z - deck_thickness),
            ))
            .fill(stone.clone());
    }

    // Black-iron rails remain open: posts plus thin diagonal rails, never a
    // solid parapet.  Lamps mark the two civic approaches and the mid-spans.
    let post_step = 12usize;
    for (index, point) in spine.iter().enumerate() {
        if index % post_step != 0 && index + 1 != spine.len() {
            continue;
        }
        for rail_side in [width_axis, -width_axis] {
            let post = *point + rail_side;
            painter
                .aabb(aabb(post.with_z(deck_z + 1), post.with_z(deck_z + 5)))
                .fill(iron.clone());
        }
    }
    for rail_side in [width_axis, -width_axis] {
        let rail_start = start + rail_side;
        let rail_end = end + rail_side;
        painter
            .line(
                rail_start.with_z(deck_z + 3),
                rail_end.with_z(deck_z + 3),
                0.45,
            )
            .fill(iron.clone());
        painter
            .line(
                rail_start.with_z(deck_z + 5),
                rail_end.with_z(deck_z + 5),
                0.35,
            )
            .fill(iron.clone());
    }

    for index in [0, spine.len() / 3, spine.len() * 2 / 3, spine.len() - 1] {
        let point = spine[index];
        for rail_side in [width_axis, -width_axis] {
            painter
                .aabb(aabb(
                    (point + rail_side).with_z(deck_z + 6),
                    (point + rail_side).with_z(deck_z + 7),
                ))
                .fill(lamp.clone());
        }
    }
}

fn bridge_step_direction(delta: Vec2<i32>) -> Vec2<i32> {
    Vec2::new(delta.x.signum(), delta.y.signum())
}

#[allow(clippy::too_many_arguments)]
fn render_grand_bridge_approach(
    painter: &Painter,
    bank: Vec3<i32>,
    outward: Vec2<i32>,
    width_axis: Vec2<i32>,
    deck_z: i32,
    deck_thickness: i32,
    road: Fill,
    stair_stone: Fill,
) {
    let rise = (deck_z - bank.z).max(0);
    if rise == 0 || outward == Vec2::zero() {
        return;
    }

    // Four metres of run for each metre of rise keeps carts and NPCs on a
    // predictable, walkable grade. Clamp it to avoid sprawling through a
    // town if an authored bridge happens to sit far above its bank.
    let run = (rise * 4).clamp(12, 48);
    let approach_end = bank.xy() + outward * run;
    let mut spine = rasterized_bridge_spine(approach_end, bank.xy());
    if spine.len() < 2 {
        return;
    }

    let span = (spine.len() - 1) as i32;
    let stair_lane_width = 2;
    let inner_edge = if width_axis.x == 0 {
        Vec2::new(0, (width_axis.y.abs() - stair_lane_width).max(1))
    } else {
        Vec2::new((width_axis.x.abs() - stair_lane_width).max(1), 0)
    };

    for (index, point) in spine.drain(..).enumerate() {
        // Quantisation produces real one-block steps at a maximum 1:4 grade;
        // the wide middle strip remains the transport ramp.
        let deck_step = bank.z + (rise * index as i32 / span);
        painter
            .aabb(aabb(
                (point - width_axis).with_z(deck_step - deck_thickness + 1),
                (point + width_axis).with_z(deck_step),
            ))
            .fill(road.clone());

        for sign in [1, -1] {
            let outer = width_axis * sign;
            let inner = inner_edge * sign;
            painter
                .aabb(aabb(
                    (point + inner).with_z(deck_step + 1),
                    (point + outer).with_z(deck_step + 2),
                ))
                .fill(stair_stone.clone());
        }
    }
}

fn rasterized_bridge_spine(start: Vec2<i32>, end: Vec2<i32>) -> Vec<Vec2<i32>> {
    let (mut x, mut y) = (start.x, start.y);
    let (end_x, end_y) = (end.x, end.y);
    let dx = (end_x - x).abs();
    let dy = -(end_y - y).abs();
    let step_x = if x < end_x { 1 } else { -1 };
    let step_y = if y < end_y { 1 } else { -1 };
    let mut error = dx + dy;
    let mut spine = Vec::with_capacity((dx - dy) as usize + 1);

    loop {
        spine.push(Vec2::new(x, y));
        if x == end_x && y == end_y {
            return spine;
        }
        let twice_error = error * 2;
        if twice_error >= dy {
            error += dy;
            x += step_x;
        }
        if twice_error <= dx {
            error += dx;
            y += step_y;
        }
    }
}

fn render_authored_low_span(
    bridge: &Bridge,
    painter: &Painter,
    deck_width: i32,
    clearance: i32,
    deck_thickness: i32,
    deck: Fill,
    support: Fill,
    rail: Fill,
    rail_height: i32,
) {
    let forward = bridge.dir.to_vec2();
    let orthogonal = bridge.dir.orthogonal().to_vec2();
    let half_width = (deck_width / 2).max(2);
    let side = orthogonal * half_width;
    let deck_z = (bridge.water_alt + clearance).max(bridge.start.z.max(bridge.end.z));
    let length = (bridge.start.xy() - bridge.end.xy())
        .map(|component| component.abs())
        .reduce_max()
        .max(1);

    // A narrow, shallow approach joins the terrain to the deck.  Unlike the old
    // generic flat bridge this never fills the complete river corridor.
    for (point, direction) in [(bridge.start, bridge.dir), (bridge.end, -bridge.dir)] {
        let rise = (deck_z - point.z).max(0);
        if rise > 0 {
            let run = (rise * 3).clamp(4, 12);
            let ramp_end = point.xy() + direction.to_vec2() * run;
            painter
                .ramp(
                    aabb(
                        (point.xy() - side).with_z(point.z),
                        (ramp_end + side).with_z(deck_z),
                    ),
                    direction,
                )
                .fill(deck.clone());
        }
    }

    painter
        .aabb(aabb(
            (bridge.start.xy() - side).with_z(deck_z - deck_thickness + 1),
            (bridge.end.xy() + side).with_z(deck_z),
        ))
        .fill(deck.clone());

    // Supports are deliberately narrow and sparse: a bridge must span the
    // water, not become a solid dam or an underground wall.
    let support_count = (length / 28).clamp(1, 3);
    for index in 1..=support_count {
        let offset = length * index / (support_count + 1);
        let center = bridge.start.xy() + forward * offset;
        let pier_side = orthogonal;
        painter
            .aabb(aabb(
                (center - pier_side - forward).with_z(bridge.water_alt),
                (center + pier_side + forward).with_z(deck_z - deck_thickness),
            ))
            .fill(support.clone());
    }

    if rail_height > 0 {
        for rail_side in [side, -side] {
            painter
                .aabb(aabb(
                    (bridge.start.xy() + rail_side).with_z(deck_z + 1),
                    (bridge.end.xy() + rail_side).with_z(deck_z + rail_height),
                ))
                .fill(rail.clone());
        }
    }
}

fn render_stone_arch(
    bridge: &Bridge,
    painter: &Painter,
    deck_width: i32,
    clearance: i32,
    deck_thickness: i32,
) {
    let stone = Fill::Brick(BlockKind::Rock, Rgb::new(112, 108, 101), 18);
    let dark_stone = Fill::Brick(BlockKind::Rock, Rgb::new(70, 68, 66), 10);
    render_authored_low_span(
        bridge,
        painter,
        deck_width,
        clearance,
        deck_thickness,
        stone.clone(),
        dark_stone,
        stone,
        1,
    );
}

fn render_timber_footbridge(
    bridge: &Bridge,
    painter: &Painter,
    deck_width: i32,
    clearance: i32,
    deck_thickness: i32,
) {
    let timber = Fill::Brick(BlockKind::Wood, Rgb::new(92, 52, 25), 12);
    let dark_timber = Fill::Brick(BlockKind::Wood, Rgb::new(58, 33, 18), 8);
    render_authored_low_span(
        bridge,
        painter,
        deck_width,
        clearance,
        deck_thickness,
        timber.clone(),
        dark_timber.clone(),
        dark_timber,
        1,
    );
}

fn render_natural_stone_earth(
    bridge: &Bridge,
    painter: &Painter,
    deck_width: i32,
    clearance: i32,
    deck_thickness: i32,
) {
    let earth = Fill::Brick(BlockKind::Rock, Rgb::new(115, 87, 62), 14);
    let stone = Fill::Brick(BlockKind::Rock, Rgb::new(80, 78, 72), 12);
    render_authored_low_span(
        bridge,
        painter,
        deck_width,
        clearance,
        deck_thickness,
        earth,
        stone,
        Fill::Block(Block::new(BlockKind::Grass, Rgb::new(64, 112, 48))),
        0,
    );
}

fn render_heightened_viaduct(bridge: &Bridge, painter: &Painter, data: &HeightenedViaduct) {
    let rock = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(50)));
    let light_rock = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(130)));
    let orth_dir = bridge.dir.orthogonal();

    let orthogonal = orth_dir.to_vec2();
    let forward = bridge.dir.to_vec2();

    let slope_inv = data.slope_inv;

    let len = (bridge.start.xy() - bridge.end.xy())
        .map(|e| e.abs())
        .reduce_max();

    let bridge_start_z = bridge.end.z + data.bridge_start_offset;
    let bridge_top = bridge_start_z + len / slope_inv / 2;

    let bridge_width = bridge.width();
    let side = orthogonal * bridge_width;

    let aabr = Aabr {
        min: bridge.start.xy() - side,
        max: bridge.end.xy() + side,
    }
    .made_valid();

    let [_start_aabr, rest] = bridge
        .dir
        .split_aabr_offset(aabr, bridge_start_z - bridge.start.z);
    let [_end_aabr, bridge_aabr] =
        (-bridge.dir).split_aabr_offset(rest, bridge_start_z - bridge.end.z);
    let under = bridge.center.z - 15;

    let bridge_prim = |bridge_width: i32| {
        let side = orthogonal * bridge_width;

        let aabr = Aabr {
            min: bridge.start.xy() - side,
            max: bridge.end.xy() + side,
        }
        .made_valid();

        let [start_aabr, rest] = bridge
            .dir
            .split_aabr_offset(aabr, bridge_start_z - bridge.start.z);
        let [end_aabr, bridge_aabr] =
            (-bridge.dir).split_aabr_offset(rest, bridge_start_z - bridge.end.z);
        let [bridge_start, bridge_end] = bridge
            .dir
            .split_aabr_offset(bridge_aabr, bridge.dir.select(bridge_aabr.size()) / 2);

        let ramp_in_aabr = |aabr: Aabr<i32>, dir: Dir2, zmin, zmax| {
            let inset = dir.select(aabr.size());
            painter.ramp_inset(
                aabb(aabr.min.with_z(zmin), aabr.max.with_z(zmax)),
                inset,
                dir,
            )
        };

        ramp_in_aabr(start_aabr, bridge.dir, bridge.start.z, bridge_start_z)
            .union(
                ramp_in_aabr(end_aabr, -bridge.dir, bridge.end.z, bridge_start_z)
                    .union(ramp_in_aabr(
                        bridge_start,
                        bridge.dir,
                        bridge_start_z + 1,
                        bridge_top,
                    ))
                    .union(ramp_in_aabr(
                        bridge_end,
                        -bridge.dir,
                        bridge_start_z + 1,
                        bridge_top,
                    )),
            )
            .union(
                painter
                    .aabb(aabb(
                        start_aabr.min.with_z(under),
                        start_aabr.max.with_z(bridge.start.z - 1),
                    ))
                    .union(painter.aabb(aabb(
                        end_aabr.min.with_z(under),
                        end_aabr.max.with_z(bridge.end.z - 1),
                    ))),
            )
            .union(painter.aabb(aabb(
                bridge_aabr.min.with_z(under),
                bridge_aabr.max.with_z(bridge_start_z),
            )))
    };

    let br = bridge_prim(bridge_width - 1);
    let b = br.without(br.translate(-Vec3::unit_z()));

    let c = bridge_aabr.center();
    let len = bridge.dir.select(bridge_aabr.size());
    let vault_size = data.vault_size.0 * len / data.vault_size.1;
    let side_vault = data.side_vault_size.0 * vault_size / data.side_vault_size.1;
    let vertical = 5;
    let spacing = data.vault_spacing;
    let vault_top = bridge_top - vertical;
    let side_vault_top = vault_top - (vault_size + spacing + 1 + side_vault) / slope_inv;
    let side_vault_offset = vault_size + spacing + 1;

    let mut remove = painter.vault(
        aabb(
            (c - side - forward * vault_size).with_z(under),
            (c + side + forward * vault_size).with_z(vault_top),
        ),
        orth_dir,
    );

    if side_vault * 2 + side_vault_offset < len / 2 + 5 {
        remove = remove.union(
            painter
                .vault(
                    aabb(
                        (c - side + forward * side_vault_offset).with_z(under),
                        (c + side + forward * (side_vault * 2 + side_vault_offset))
                            .with_z(side_vault_top),
                    ),
                    orth_dir,
                )
                .union(
                    painter.vault(
                        aabb(
                            (c - side - forward * side_vault_offset).with_z(under),
                            (c + side - forward * (side_vault * 2 + side_vault_offset))
                                .with_z(side_vault_top),
                        ),
                        orth_dir,
                    ),
                ),
        );

        if data.holes {
            remove = remove.union(
                painter
                    .vault(
                        aabb(
                            (c - side + forward * (vault_size + 1)).with_z(side_vault_top - 4),
                            (c + side + forward * (vault_size + spacing))
                                .with_z(side_vault_top + 2),
                        ),
                        orth_dir,
                    )
                    .union(
                        painter.vault(
                            aabb(
                                (c - side - forward * (vault_size + 1)).with_z(side_vault_top - 4),
                                (c + side - forward * (vault_size + spacing))
                                    .with_z(side_vault_top + 2),
                            ),
                            orth_dir,
                        ),
                    ),
            );
        }
    }

    bridge_prim(bridge_width).without(remove).fill(rock);
    b.translate(-Vec3::unit_z()).fill(light_rock);

    br.translate(Vec3::unit_z() * 5)
        .without(br.translate(-Vec3::unit_z()))
        .clear();

    /*
    let place_lights = |center: Vec3<i32>| {
        painter.sprite(
            orth_dir
                .select_aabr_with(bridge_aabr, center.xy())
                .with_z(center.z),
            SpriteKind::FireBowlGround,
        );
        painter.sprite(
            (-orth_dir)
                .select_aabr_with(bridge_aabr, center.xy())
                .with_z(center.z),
            SpriteKind::FireBowlGround,
        );
    };

    place_lights(bridge_aabr.center().with_z(bridge_top + 1));

    let light_spacing = 1;
    let num_lights = (len - 1) / 2 / light_spacing;

    let place_lights = |i: i32| {
        let offset = i * light_spacing;
        let z =
            bridge_start_z + 1 + (offset + if len / 2 % 2 == 0 { 4 } else { 3 }) / (slope_inv - 1);

        place_lights(
            (bridge
                .dir
                .select_aabr_with(bridge_aabr, bridge_aabr.center())
                - forward * offset)
                .with_z(z),
        );
        place_lights(
            ((-bridge.dir).select_aabr_with(bridge_aabr, bridge_aabr.center()) + forward * offset)
                .with_z(z),
        );
    };
    for i in 0..num_lights {
        place_lights(i);
    }
    */

    // Small chance to spawn a troll.
    let mut rng = rand::rng();
    if rng.random_bool(0.1) {
        painter.spawn(
            EntityInfo::at(c.with_z(vault_top - 2).as_()).with_asset_expect(
                "common.entity.wild.aggressive.swamp_troll",
                &mut rng,
                None,
            ),
        );
    }
}

fn render_tower(bridge: &Bridge, painter: &Painter, roof_kind: &RoofKind) {
    let rock = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(50)));
    let wood = Fill::Block(Block::new(BlockKind::Wood, Rgb::new(40, 28, 20)));

    let tower_size = 5;

    let bridge_width = tower_size - 2;

    let orth_dir = bridge.dir.orthogonal();

    let orthogonal = orth_dir.to_vec2();
    let forward = bridge.dir.to_vec2();

    let tower_height_extend = 10;

    let tower_end = bridge.end.z + tower_height_extend;

    let tower_center = bridge.start.xy() + forward * tower_size;
    let tower_aabr = Aabr {
        min: tower_center - tower_size,
        max: tower_center + tower_size,
    };

    let len = (bridge.dir.select(bridge.end.xy()) - bridge.dir.select_aabr(tower_aabr)).abs() - 1;

    painter
        .aabb(aabb(
            tower_aabr.min.with_z(bridge.start.z - 5),
            tower_aabr.max.with_z(tower_end),
        ))
        .fill(rock.clone());

    painter
        .aabb(aabb(
            (tower_aabr.min + 1).with_z(bridge.start.z),
            (tower_aabr.max - 1).with_z(tower_end - 1),
        ))
        .clear();

    let c = (-bridge.dir).select_aabr_with(tower_aabr, tower_aabr.center());
    painter
        .aabb(aabb(
            (c - orthogonal).with_z(bridge.start.z),
            (c + orthogonal).with_z(bridge.start.z + 2),
        ))
        .clear();

    let ramp_height = 8;

    let ramp_aabb = aabb(
        (c - forward - orthogonal).with_z(bridge.start.z - 1),
        (c - forward * ramp_height + orthogonal).with_z(bridge.start.z + ramp_height - 2),
    );

    painter
        .aabb(ramp_aabb)
        .without(painter.ramp(ramp_aabb, -bridge.dir))
        .clear();

    let c = bridge.dir.select_aabr_with(tower_aabr, tower_aabr.center());
    painter
        .aabb(aabb(
            (c - orthogonal).with_z(bridge.end.z),
            (c + orthogonal).with_z(bridge.end.z + 2),
        ))
        .clear();

    let stair_thickness = 2;
    painter
        .staircase_in_aabb(
            aabb(
                (tower_aabr.min + 1).with_z(bridge.start.z),
                (tower_aabr.max - 1).with_z(bridge.end.z - 1),
            ),
            stair_thickness,
            bridge.dir.rotated_ccw(),
        )
        .fill(rock.clone());
    let aabr = bridge
        .dir
        .rotated_cw()
        .split_aabr_offset(tower_aabr, stair_thickness + 1)[1];

    painter
        .aabb(aabb(
            aabr.min.with_z(bridge.end.z - 1),
            aabr.max.with_z(bridge.end.z - 1),
        ))
        .fill(rock.clone());

    painter
        .aabb(aabb(
            tower_aabr.center().with_z(bridge.start.z),
            tower_aabr.center().with_z(bridge.end.z - 1),
        ))
        .fill(rock.clone());

    let offset = tower_size * 2 - 2;
    let d = 2;
    let n = (bridge.end.z - bridge.start.z - d) / offset;
    let p = (bridge.end.z - bridge.start.z - d) / n;

    for i in 1..=n {
        let c = tower_aabr.center().with_z(bridge.start.z + i * p);

        for dir in Dir2::ALL {
            painter.rotated_sprite(
                c + dir.to_vec2(),
                SpriteKind::WallSconce,
                dir.sprite_ori_legacy(),
            );
        }
    }

    painter.rotated_sprite(
        (tower_aabr.center() + bridge.dir.to_vec2() * (tower_size - 1))
            .with_z(bridge.end.z + tower_height_extend / 2),
        SpriteKind::WallLamp,
        (-bridge.dir).sprite_ori_legacy(),
    );

    match roof_kind {
        RoofKind::Crenelated => {
            painter
                .aabb(aabb(
                    (tower_aabr.min - 1).with_z(tower_end + 1),
                    (tower_aabr.max + 1).with_z(tower_end + 2),
                ))
                .fill(rock.clone());

            painter
                .aabbs_around_aabb(
                    aabb(
                        tower_aabr.min.with_z(tower_end + 3),
                        tower_aabr.max.with_z(tower_end + 3),
                    ),
                    1,
                    1,
                )
                .fill(rock.clone());

            painter
                .aabb(aabb(
                    tower_aabr.min.with_z(tower_end + 2),
                    tower_aabr.max.with_z(tower_end + 2),
                ))
                .clear();

            painter
                .aabbs_around_aabb(
                    aabb(
                        (tower_aabr.min + 1).with_z(tower_end + 2),
                        (tower_aabr.max - 1).with_z(tower_end + 2),
                    ),
                    1,
                    4,
                )
                .fill(Fill::sprite(SpriteKind::FireBowlGround));
        },
        RoofKind::Hipped => {
            painter
                .pyramid(aabb(
                    (tower_aabr.min - 1).with_z(tower_end + 1),
                    (tower_aabr.max + 1).with_z(tower_end + 2 + tower_size),
                ))
                .fill(wood);
        },
    }

    let offset = 15;
    let thickness = 3;

    let size = (offset - thickness) / 2;

    let n = len / offset;
    let p = len / n;

    let offset = forward * p;

    let size = bridge_width * orthogonal + forward * size;
    let start = bridge.dir.select_aabr_with(tower_aabr, tower_aabr.center()) + forward;
    painter
        .aabb(aabb(
            (start - orthogonal * bridge_width).with_z(bridge.center.z - 10),
            (bridge.end + orthogonal * bridge_width).with_z(bridge.end.z - 1),
        ))
        .without(
            painter
                .vault(
                    aabb(
                        (start + offset / 2 - size).with_z(bridge.center.z - 10),
                        (start + offset / 2 + size).with_z(bridge.end.z - 3),
                    ),
                    orth_dir,
                )
                .repeat(offset.with_z(0), n as u32),
        )
        .fill(rock);

    painter
        .aabb(aabb(
            (start - orthogonal * bridge_width).with_z(bridge.end.z),
            (bridge.end + orthogonal * bridge_width).with_z(bridge.end.z + 5),
        ))
        .clear();

    let light_spacing = 10;
    let n = len / light_spacing;
    let p = len / n;

    let start = bridge.end;
    let offset = -forward * p;
    for i in 1..=n {
        let c = start + i * offset;

        painter.sprite(c + orthogonal * bridge_width, SpriteKind::StreetLamp);
        painter.sprite(c - orthogonal * bridge_width, SpriteKind::StreetLamp);
    }
}

fn render_hang(bridge: &Bridge, painter: &Painter) {
    let orth_dir = bridge.dir.orthogonal();

    let orthogonal = orth_dir.to_vec2();
    let forward = bridge.dir.to_vec2();

    let rock = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(50)));
    let wood = Fill::Block(Block::new(BlockKind::Wood, Rgb::new(133, 94, 66)));

    let bridge_width = bridge.width();
    let side = orthogonal * bridge_width;

    let aabr = Aabr {
        min: bridge.start.xy() - side,
        max: bridge.end.xy() + side,
    }
    .made_valid();

    let top_offset = 4;
    let top = bridge.end.z + top_offset;

    let [ramp_f, aabr] = bridge.dir.split_aabr_offset(aabr, top - bridge.start.z + 1);

    painter
        .aabb(aabb(
            ramp_f.min.with_z(bridge.start.z - 10),
            ramp_f.max.with_z(bridge.start.z),
        ))
        .fill(rock.clone());
    painter
        .ramp_inset(
            aabb(ramp_f.min.with_z(bridge.start.z), ramp_f.max.with_z(top)),
            top - bridge.start.z + 1,
            bridge.dir,
        )
        .fill(rock.clone());

    let [ramp_b, aabr] = (-bridge.dir).split_aabr_offset(aabr, top_offset + 1);
    painter
        .aabb(aabb(
            ramp_b.min.with_z(bridge.end.z - 10),
            ramp_b.max.with_z(bridge.end.z),
        ))
        .fill(rock.clone());
    painter
        .ramp(
            aabb(ramp_b.min.with_z(bridge.end.z), ramp_b.max.with_z(top)),
            -bridge.dir,
        )
        .fill(rock.clone());

    let len = bridge.dir.select(aabr.size());

    let h = 3 * len.sqrt() / 4;

    let x = len / 2;

    let xsqr = (x * x) as f32;
    let hsqr = (h * h) as f32;
    let w = ((xsqr + (xsqr * (4.0 * hsqr + xsqr)).sqrt()) / 2.0)
        .sqrt()
        .ceil()
        + 1.0;

    let bottom = top - (h - (hsqr - hsqr * x as f32 / w).sqrt().ceil() as i32);

    let w = w as i32;
    let c = aabr.center();

    let cylinder = painter
        .horizontal_cylinder(
            aabb(
                (c - forward * w - side).with_z(bottom),
                (c + forward * w + side).with_z(bottom + h * 2),
            ),
            orth_dir,
        )
        .intersect(painter.aabb(aabb(
            aabr.min.with_z(bottom),
            aabr.max.with_z(bottom + h * 2),
        )));

    cylinder.fill(wood.clone());

    cylinder.translate(Vec3::unit_z()).clear();

    let edges = cylinder
        .without(cylinder.translate(Vec3::unit_z()))
        .without(painter.aabb(aabb(
            (c - forward * w - orthogonal * (bridge_width - 1)).with_z(bottom),
            (c + forward * w + orthogonal * (bridge_width - 1)).with_z(bottom + h * 2),
        )));

    edges
        .translate(Vec3::unit_z())
        .fill(Fill::sprite(SpriteKind::Rope));

    edges.translate(Vec3::unit_z() * 2).fill(wood);

    let column_height = 3;
    let column_range = top..=top + column_height;
    painter
        .column(
            bridge.dir.select_aabr_with(ramp_f, ramp_f.min),
            column_range.clone(),
        )
        .fill(rock.clone());
    painter
        .column(
            bridge.dir.select_aabr_with(ramp_f, ramp_f.max),
            column_range.clone(),
        )
        .fill(rock.clone());
    painter
        .column(
            (-bridge.dir).select_aabr_with(ramp_b, ramp_b.min),
            column_range.clone(),
        )
        .fill(rock.clone());
    painter
        .column(
            (-bridge.dir).select_aabr_with(ramp_b, ramp_b.max),
            column_range,
        )
        .fill(rock);
}

pub struct Bridge {
    /// The original start position in world coords.
    pub(crate) original_start: Vec2<i32>,
    /// The original end position in world coords.
    pub(crate) original_end: Vec2<i32>,

    pub(crate) start: Vec3<i32>,
    pub(crate) end: Vec3<i32>,
    pub(crate) dir: Dir2,
    center: Vec3<i32>,
    water_alt: i32,
    kind: BridgeKind,
    biome: BiomeKind,
    surface_color: Rgb<f32>,
}

impl Bridge {
    pub fn generate(
        land: &Land,
        index: IndexRef,
        rng: &mut impl Rng,
        site: &Site,
        start: Vec2<i32>,
        end: Vec2<i32>,
        authored_design: Option<AuthoredBridgeDesign>,
    ) -> Self {
        let original_start = site.tile_wpos(start);
        let original_end = site.tile_wpos(end);

        let min_water_dist = 5;
        let find_edge = |start: Vec2<i32>, end: Vec2<i32>| {
            let mut test_start = start;
            let dir = Dir2::from_vec2(end - start).to_vec2();
            let mut last_alt = if let Some(col) = land.column_sample(start, index) {
                col.alt as i32
            } else {
                return (
                    test_start.with_z(land.get_alt_approx(start) as i32),
                    i32::MAX,
                );
            };
            let mut step = 0;
            loop {
                if let Some(sample) = land.column_sample(test_start + step * dir, index) {
                    let alt = sample.alt as i32;
                    let water_dist = sample.water_dist.unwrap_or(16.0) as i32;
                    if last_alt - alt > 1 + (step + 2) / 3
                        || sample.riverless_alt - sample.alt > 2.0
                    {
                        break (test_start.with_z(last_alt), water_dist);
                    } else {
                        test_start += step * dir;

                        if water_dist <= min_water_dist {
                            break (test_start.with_z(alt), water_dist);
                        }

                        step = water_dist - min_water_dist;

                        last_alt = alt;
                    }
                } else {
                    break (test_start.with_z(last_alt), i32::MAX);
                }
            }
        };

        let (test_start, start_dist) = find_edge(original_start, original_end);

        let (test_end, end_dist) = find_edge(original_end, original_start);

        let (start, start_dist, end, end_dist) = if test_start.z < test_end.z {
            (test_start, start_dist, test_end, end_dist)
        } else {
            (test_end, end_dist, test_start, start_dist)
        };

        let center = (start.xy() + end.xy()) / 2;
        let col = land.column_sample(center, index).unwrap();
        let center = center.with_z(col.alt as i32);
        let surface_color = col.surface_color;
        let water_alt = col.water_level as i32;
        let bridge = match authored_design {
            Some(AuthoredBridgeDesign::GrandStoneIron {
                deck_width,
                clearance,
                deck_thickness,
            }) => BridgeKind::GrandStoneIron {
                deck_width,
                clearance,
                deck_thickness,
            },
            Some(AuthoredBridgeDesign::StoneArch {
                deck_width,
                clearance,
                deck_thickness,
            }) => BridgeKind::StoneArch {
                deck_width,
                clearance,
                deck_thickness,
            },
            Some(AuthoredBridgeDesign::TimberFootbridge {
                deck_width,
                clearance,
                deck_thickness,
            }) => BridgeKind::TimberFootbridge {
                deck_width,
                clearance,
                deck_thickness,
            },
            Some(AuthoredBridgeDesign::NaturalStoneEarth {
                deck_width,
                clearance,
                deck_thickness,
            }) => BridgeKind::NaturalStoneEarth {
                deck_width,
                clearance,
                deck_thickness,
            },
            None => BridgeKind::random(rng, start, start_dist, end, end_dist, water_alt),
        };
        Self {
            original_start,
            original_end,
            start,
            end,
            center,
            water_alt,
            dir: Dir2::from_vec2(end.xy() - start.xy()),
            kind: bridge,
            biome: land
                .get_chunk_wpos(center.xy())
                .map_or(BiomeKind::Void, |chunk| chunk.get_biome()),
            surface_color,
        }
    }

    pub fn width(&self) -> i32 { self.kind.width() }
}

impl Structure for Bridge {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_bridge")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_bridge"))
    }

    fn render_ordering(&self) -> u32 { 1 }

    fn render_inner(&self, _site: &Site, _land: &Land, painter: &Painter) {
        match &self.kind {
            BridgeKind::Flat => render_flat(self, painter),
            BridgeKind::Tower(roof) => render_tower(self, painter, roof),
            BridgeKind::Short => render_short(self, painter),
            BridgeKind::HeightenedViaduct(data) => render_heightened_viaduct(self, painter, data),
            BridgeKind::HangBridge => render_hang(self, painter),
            BridgeKind::GrandStoneIron {
                deck_width,
                clearance,
                deck_thickness,
            } => render_grand_stone_iron(self, painter, *deck_width, *clearance, *deck_thickness),
            BridgeKind::StoneArch {
                deck_width,
                clearance,
                deck_thickness,
            } => render_stone_arch(self, painter, *deck_width, *clearance, *deck_thickness),
            BridgeKind::TimberFootbridge {
                deck_width,
                clearance,
                deck_thickness,
            } => render_timber_footbridge(self, painter, *deck_width, *clearance, *deck_thickness),
            BridgeKind::NaturalStoneEarth {
                deck_width,
                clearance,
                deck_thickness,
            } => {
                render_natural_stone_earth(self, painter, *deck_width, *clearance, *deck_thickness)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterized_diagonal_spine_is_continuous_without_filling_its_bounding_box() {
        let spine = rasterized_bridge_spine(Vec2::new(0, 0), Vec2::new(15, 12));

        assert_eq!(spine.first(), Some(&Vec2::new(0, 0)));
        assert_eq!(spine.last(), Some(&Vec2::new(15, 12)));
        assert_eq!(spine.len(), 16);
        assert!(spine.windows(2).all(|pair| {
            let delta = pair[1] - pair[0];
            delta.x.abs() <= 1 && delta.y.abs() <= 1 && delta != Vec2::zero()
        }));

        // The old diagonal AABB would fill 16 * 13 positions. The rendered
        // bridge may add deck width around this spine, but its route itself
        // must remain a one-cell-wide ordered path.
        assert!(spine.len() < 16 * 13);
    }
}
