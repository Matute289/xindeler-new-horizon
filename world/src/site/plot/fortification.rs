use super::*;
use crate::Land;
use common::terrain::{Block, BlockKind};
use vek::*;

/// One terrain-following sub-segment of a wall's run, fully baked at
/// `Fortification::generate` time -- `a`/`b` already carry the sampled
/// altitude, and `merlon_aabb` is the exact volume `render_inner` hands to
/// `Painter::aabbs_around_aabb`. Nothing here needs a `Land` again.
struct WallSegment {
    a: Vec3<i32>,
    b: Vec3<i32>,
    merlon_aabb: Aabb<i32>,
}

/// A single gate opening, fully baked at generate time -- `aabb` already
/// carries the sampled altitude, so `render_inner` only has to `.clear()` or
/// `.fill()` it depending on `open`. The physical open/closed state itself
/// is locked in on the authored data (see
/// `AuthoredCromatolisGate::default_open` in `civ/mod.rs`) -- this plot only
/// renders whatever state it's handed.
struct FortificationGate {
    aabb: Aabb<i32>,
    open: bool,
}

/// A single authored Cromatolis defensive wall (with any number of gates).
/// The renderer is generic -- the source map owns dimensions, direction, and
/// per-gate state -- so it stays reusable by later continents' authored
/// fortification data (mirrors `plot::Bridge`).
pub struct Fortification {
    /// Wall thickness, in blocks.
    depth: i32,
    radius: f32,
    body_height: f32,
    segments: Vec<WallSegment>,
    gates: Vec<FortificationGate>,
}

fn aabb(min: Vec3<i32>, max: Vec3<i32>) -> Aabb<i32> {
    let aabb = Aabb { min, max }.made_valid();
    Aabb {
        min: aabb.min,
        max: aabb.max + 1,
    }
}

impl Fortification {
    /// `start`/`end` are world positions (the wall's centerline). `design`
    /// is `None` only if a `Fortification` site was somehow established
    /// without authored metadata -- the only real caller
    /// (`civ::Civs::establish_authored_cromatolis_fortifications`) always
    /// supplies one, but this falls back to a small, plain wall rather than
    /// panicking.
    ///
    /// All terrain sampling (`land.get_alt_approx`) happens once, right
    /// here, per sub-segment and per gate -- not in `render_inner`, which
    /// would otherwise redo it on every chunk that touches this plot (see
    /// `Bridge::generate`, which bakes world-space geometry the same way).
    pub fn generate(
        land: &Land,
        start: Vec2<i32>,
        end: Vec2<i32>,
        design: Option<AuthoredFortificationDesign>,
    ) -> Self {
        let dir = Dir2::from_vec2((end - start).as_());
        let (depth, height, gate_designs) = match design {
            Some(design) => (design.depth.max(2), design.height.max(4), design.gates),
            None => (4, 10, Vec::new()),
        };

        // Fixed, small merlon band on top of the wall body -- no aesthetic
        // variety needed (see the module-level docs on the loader side).
        const MERLON_HEIGHT: i32 = 3;
        let body_height = (height - MERLON_HEIGHT).max(1);
        let radius = (depth as f32 / 2.0).max(1.0);
        let orth = dir.orthogonal().to_vec2();
        let half_depth = (depth / 2 + 1).max(1);

        // Split the wall into sub-segments so both the terrain-following
        // base and its merlon band follow slopes along the wall's length,
        // the same technique `GnarlingFortification` uses for its own
        // perimeter wall.
        const SECTIONS: i32 = 8;
        let get_point = |a: i32| start + (end - start) * a / SECTIONS;
        let segments = (0..SECTIONS)
            .map(|i| {
                let a = get_point(i);
                let b = get_point(i + 1);
                let a_alt = land.get_alt_approx(a) as i32;
                let b_alt = land.get_alt_approx(b) as i32;

                // Merlon parapet for this sub-segment: reuses the same
                // alternating-gap primitive `render_tower`'s
                // `RoofKind::Crenelated` branch uses for its own
                // battlements, applied to this sub-segment's own local
                // footprint so it still roughly follows terrain across a
                // long wall.
                let seg_top = ((a_alt + b_alt) / 2) + body_height;
                let merlon_aabb = aabb(
                    (a.map2(b, |x, y| x.min(y)) - orth * half_depth).with_z(seg_top),
                    (a.map2(b, |x, y| x.max(y)) + orth * half_depth)
                        .with_z(seg_top + MERLON_HEIGHT),
                );
                WallSegment {
                    a: a.with_z(a_alt),
                    b: b.with_z(b_alt),
                    merlon_aabb,
                }
            })
            .collect();

        let delta = end - start;
        let forward = dir.to_vec2();
        let gates = gate_designs
            .into_iter()
            .map(|gate| {
                let t = gate.t.clamp(0.0, 1.0);
                let wpos = start + (delta.as_::<f32>() * t).as_::<i32>();
                let clear_width = gate.clear_width.max(1);
                let gate_height = gate.height.max(1);
                let half_len = (clear_width / 2).max(1);
                let alt = land.get_alt_approx(wpos) as i32;
                // Gate openings: an open gate clears a passable gap straight
                // through the wall's thickness; a closed gate fills the same
                // volume solid. That solid fill is the only physical
                // enforcement modeled here -- no guard/permit logic.
                let gate_aabb = aabb(
                    (wpos - forward * half_len - orth * half_depth).with_z(alt),
                    (wpos + forward * half_len + orth * half_depth).with_z(alt + gate_height),
                );
                FortificationGate {
                    aabb: gate_aabb,
                    open: gate.open,
                }
            })
            .collect();

        Self {
            depth,
            radius,
            body_height: body_height as f32,
            segments,
            gates,
        }
    }

    /// Wall thickness, in blocks -- used by the site generator to size the
    /// tile-grid footprint this plot occupies.
    pub fn depth(&self) -> i32 { self.depth }
}

impl Structure for Fortification {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_fortification")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_fortification"))
    }

    fn spawn_rules_inner(
        &self,
        spawn_rules: &mut SpawnRules,
        _land: &Land,
        _wpos: Vec2<i32>,
        _weight: f32,
    ) {
        spawn_rules.waypoints = false;
    }

    /// Pure geometry replay -- every position and altitude was already
    /// sampled once in `generate`, so this never touches `land` (matches
    /// `Bridge::render_inner`'s unused `_land` shape).
    fn render_inner(&self, _site: &Site, _land: &Land, painter: &Painter) {
        let stone = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(80)));
        let gate_fill = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(40)));

        for segment in &self.segments {
            painter
                .segment_prism(segment.a, segment.b, self.radius, self.body_height)
                .fill(stone.clone());
            painter
                .aabbs_around_aabb(segment.merlon_aabb, 2, 2)
                .fill(stone.clone());
        }

        for gate in &self.gates {
            if gate.open {
                painter.aabb(gate.aabb).clear();
            } else {
                painter.aabb(gate.aabb).fill(gate_fill.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises `render_inner` directly against a synthetic straight wall
    /// with one open and one closed gate, and samples the actual resulting
    /// blocks -- the render-level half of the gate physical-state contract.
    /// The other half (which of the three real authored gates is open or
    /// closed) is locked in and tested in `civ::tests`.
    #[test]
    fn render_inner_clears_open_gates_and_solidifies_closed_gates() {
        let start = Vec2::new(0, 0);
        let end = Vec2::new(200, 0);
        let design = AuthoredFortificationDesign {
            depth: 8,
            height: 30,
            gates: vec![
                AuthoredFortificationGateDesign {
                    t: 0.25,
                    clear_width: 8,
                    height: 10,
                    open: true,
                },
                AuthoredFortificationGateDesign {
                    t: 0.75,
                    clear_width: 8,
                    height: 10,
                    open: false,
                },
            ],
        };
        let land = Land::empty();
        let fortification = Fortification::generate(&land, start, end, Some(design));
        let site = Site::default();
        let painter = Painter::new_for_test(Aabr {
            min: Vec2::new(-20, -20),
            max: Vec2::new(220, 20),
        });

        fortification.render_inner(&site, &land, &painter);

        let open_gate_pos = Vec3::new(50, 0, 5);
        let closed_gate_pos = Vec3::new(150, 0, 5);
        assert!(
            !painter.is_solid_at_for_test(open_gate_pos),
            "an open gate must clear a passable gap through the wall"
        );
        assert!(
            painter.is_solid_at_for_test(closed_gate_pos),
            "a closed gate must fill solid, same as the rest of the wall"
        );

        // Away from either gate, the wall body itself must still be solid.
        assert!(painter.is_solid_at_for_test(Vec3::new(100, 0, 5)));
    }

    #[test]
    fn generate_falls_back_to_a_plain_wall_without_authored_metadata() {
        let land = Land::empty();
        let fortification = Fortification::generate(&land, Vec2::new(0, 0), Vec2::new(50, 0), None);
        assert_eq!(fortification.depth, 4);
        // height defaults to 10, minus the fixed 3-block merlon band.
        assert_eq!(fortification.body_height, 7.0);
        assert!(fortification.gates.is_empty());
    }
}
