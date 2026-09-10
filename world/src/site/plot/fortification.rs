use super::*;
use crate::Land;
use common::terrain::{Block, BlockKind};
use vek::*;

/// A single gate opening along a fortification wall, in render-ready
/// (world-position) form. Its physical open/closed state is locked in at
/// authoring time (see `civ::cromatolis_gate_is_open`) -- this plot only
/// renders whatever state it's handed.
struct FortificationGate {
    wpos: Vec2<i32>,
    clear_width: i32,
    height: i32,
    open: bool,
}

/// A single authored Cromatolis defensive wall (with any number of gates).
/// The renderer is generic -- the source map owns dimensions, direction, and
/// per-gate state -- so it stays reusable by later continents' authored
/// fortification data (mirrors `plot::Bridge`).
pub struct Fortification {
    start: Vec2<i32>,
    end: Vec2<i32>,
    dir: Dir2,
    /// Wall thickness, in blocks.
    depth: i32,
    /// Total wall height, in blocks, from the terrain-following base to the
    /// top of the merlon parapet.
    height: i32,
    gates: Vec<FortificationGate>,
}

impl Fortification {
    /// `start`/`end` are world positions (the wall's centerline). `design`
    /// is `None` only if a `Fortification` site was somehow established
    /// without authored metadata -- the only real caller
    /// (`civ::Civs::establish_authored_cromatolis_fortifications`) always
    /// supplies one, but this falls back to a small, plain wall rather than
    /// panicking.
    pub fn generate(
        start: Vec2<i32>,
        end: Vec2<i32>,
        design: Option<AuthoredFortificationDesign>,
    ) -> Self {
        let dir = Dir2::from_vec2((end - start).as_());
        let (depth, height, gate_designs) = match design {
            Some(design) => (design.depth.max(2), design.height.max(4), design.gates),
            None => (4, 10, Vec::new()),
        };
        let delta = end - start;
        let gates = gate_designs
            .into_iter()
            .map(|gate| {
                let t = gate.t.clamp(0.0, 1.0);
                let wpos = start + (delta.as_::<f32>() * t).as_::<i32>();
                FortificationGate {
                    wpos,
                    clear_width: gate.clear_width.max(1),
                    height: gate.height.max(1),
                    open: gate.open,
                }
            })
            .collect();
        Self {
            start,
            end,
            dir,
            depth,
            height,
            gates,
        }
    }

    /// Wall thickness, in blocks -- used by the site generator to size the
    /// tile-grid footprint this plot occupies.
    pub fn depth(&self) -> i32 { self.depth }
}

fn aabb(min: Vec3<i32>, max: Vec3<i32>) -> Aabb<i32> {
    let aabb = Aabb { min, max }.made_valid();
    Aabb {
        min: aabb.min,
        max: aabb.max + 1,
    }
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

    fn render_inner(&self, _site: &Site, land: &Land, painter: &Painter) {
        let stone = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(80)));
        let gate_fill = Fill::Block(Block::new(BlockKind::Rock, Rgb::gray(40)));

        // Fixed, small merlon band on top of the wall body -- no aesthetic
        // variety needed (see the module-level docs on the loader side).
        const MERLON_HEIGHT: i32 = 3;
        let body_height = (self.height - MERLON_HEIGHT).max(1);
        let radius = (self.depth as f32 / 2.0).max(1.0);
        let orth = self.dir.orthogonal().to_vec2();
        let half_depth = (self.depth / 2 + 1).max(1);

        // Split the wall into sub-segments so both the terrain-following
        // base and its merlon band follow slopes along the wall's length,
        // the same technique `GnarlingFortification` uses for its own
        // perimeter wall.
        const SECTIONS: i32 = 8;
        let get_point = |a: i32| self.start + (self.end - self.start) * a / SECTIONS;
        for i in 0..SECTIONS {
            let a = get_point(i);
            let b = get_point(i + 1);
            let a_alt = land.get_alt_approx(a) as i32;
            let b_alt = land.get_alt_approx(b) as i32;

            painter
                .segment_prism(a.with_z(a_alt), b.with_z(b_alt), radius, body_height as f32)
                .fill(stone.clone());

            // Merlon parapet for this sub-segment: reuses the same
            // alternating-gap primitive `render_tower`'s `RoofKind::Crenelated`
            // branch uses for its own battlements, applied to this
            // sub-segment's own local footprint so it still roughly follows
            // terrain across a long wall.
            let seg_top = ((a_alt + b_alt) / 2) + body_height;
            let seg_aabb = aabb(
                (a.map2(b, |x, y| x.min(y)) - orth * half_depth).with_z(seg_top),
                (a.map2(b, |x, y| x.max(y)) + orth * half_depth).with_z(seg_top + MERLON_HEIGHT),
            );
            painter
                .aabbs_around_aabb(seg_aabb, 2, 2)
                .fill(stone.clone());
        }

        // Gate openings: an open gate clears a passable gap straight through
        // the wall's thickness; a closed gate fills the same volume solid.
        // That solid fill is the only physical enforcement modeled here --
        // no guard/permit logic.
        let forward = self.dir.to_vec2();
        for gate in &self.gates {
            let alt = land.get_alt_approx(gate.wpos) as i32;
            let half_len = (gate.clear_width / 2).max(1);
            let gate_aabb = aabb(
                (gate.wpos - forward * half_len - orth * half_depth).with_z(alt),
                (gate.wpos + forward * half_len + orth * half_depth).with_z(alt + gate.height),
            );
            if gate.open {
                painter.aabb(gate_aabb).clear();
            } else {
                painter.aabb(gate_aabb).fill(gate_fill.clone());
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
        let fortification = Fortification::generate(start, end, Some(design));
        let land = Land::empty();
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
        let fortification = Fortification::generate(Vec2::new(0, 0), Vec2::new(50, 0), None);
        assert_eq!(fortification.depth, 4);
        assert_eq!(fortification.height, 10);
        assert!(fortification.gates.is_empty());
    }
}
