//! XINDELER: keeping persisted NPCs out of authored water.
//!
//! A world's authored water rasters (`world::authored_raster`) can change
//! under an existing rtsim save: a manifest applied for the first time, or a
//! new revision of one. NPCs saved before that may then stand inside an
//! authored channel, lake or bank wall. At startup, when the manifest digest
//! stored in the save differs from the world's, every land NPC standing in an
//! authored wet column (or on a bank lip within 2 m of authored water) is moved
//! to the nearest dry authored-region column, at its ground height. The digest
//! is then recorded, so this runs once per manifest change.
//!
//! Players are not handled here: on login the server repositions a character
//! with `RepositionToFreeSpace`, which searches the generated blocks and so
//! already sees the authored water.

use crate::data::{Actor, actor::ActorKind};
use common::comp::{self, Body};
use tracing::{info, warn};
use vek::*;
use world::{World, authored_raster::AuthoredCell};

/// How far (m) to look for dry ground around an NPC standing in water.
const SEARCH_RADIUS_M: i32 = 64;

/// Bodies that belong in water (never moved out of it).
fn water_bound(body: &Body) -> bool {
    matches!(
        body,
        Body::Ship(comp::ship::Body::SailBoat | comp::ship::Body::Galleon)
            | Body::FishSmall(_)
            | Body::FishMedium(_)
            | Body::Crustacean(_)
    )
}

/// Bodies that do not stand on the ground.
fn airborne(body: &Body) -> bool {
    matches!(
        body,
        Body::Ship(_) | Body::BirdLarge(_) | Body::BirdMedium(_) | Body::Dragon(_)
    )
}

/// Where a land NPC at `wpos` must go, if it must move: `None` when the
/// column is fine. `cell(wpos)` is the authored cell (`None` outside every
/// region), `dist(wpos)` the distance to authored water and `ground(wpos)` the
/// ground height to stand on.
pub fn safe_ground(
    wpos: Vec3<f32>,
    cell: &dyn Fn(Vec2<i32>) -> Option<(AuthoredCell, Option<f32>)>,
    ground: &dyn Fn(Vec2<i32>) -> f32,
) -> Option<Vec3<f32>> {
    let here = wpos.xy().as_::<i32>();
    let unsafe_at = |p: Vec2<i32>| match cell(p) {
        Some((AuthoredCell::Wet { .. }, _)) => true,
        Some((_, dist)) => dist.is_some_and(|d| d < 2.0),
        None => false,
    };
    if !unsafe_at(here) {
        return None;
    }
    // Rings of increasing Chebyshev radius; the first safe column found on
    // the closest ring (scanned in a fixed order: deterministic).
    for r in 1..=SEARCH_RADIUS_M {
        let mut best: Option<(i32, Vec2<i32>)> = None;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let p = here + Vec2::new(dx, dy);
                if unsafe_at(p) {
                    continue;
                }
                let d2 = dx * dx + dy * dy;
                if best.is_none_or(|(b, _)| d2 < b) {
                    best = Some((d2, p));
                }
            }
        }
        if let Some((_, p)) = best {
            return Some(Vec3::new(
                p.x as f32 + 0.5,
                p.y as f32 + 0.5,
                ground(p) + 1.0,
            ));
        }
    }
    None
}

/// Run the once-per-manifest pass over `actors` (see the module doc).
/// Returns how many NPCs moved.
pub fn resolve_npcs<'a>(
    actors: impl Iterator<Item = &'a mut Actor>,
    stored_digest: &mut Option<String>,
    world: &World,
) -> usize {
    let current = world
        .sim()
        .authored_rasters()
        .map(|r| r.digest().to_string());
    if *stored_digest == current {
        return 0;
    }
    let sim = world.sim();
    let cell = |p: Vec2<i32>| sim.authored_column_at(p).map(|c| (c.cell, c.water_dist));
    let ground = |p: Vec2<i32>| sim.surface_alt_at(p) - 1.0;
    let mut moved = 0;
    if current.is_some() {
        for actor in actors {
            if !matches!(actor.kind, ActorKind::Npc(_))
                || water_bound(&actor.body)
                || airborne(&actor.body)
            {
                continue;
            }
            if let Some(to) = safe_ground(actor.wpos, &cell, &ground) {
                actor.wpos = to;
                moved += 1;
            }
        }
    }
    warn!(
        previous = ?stored_digest,
        current = ?current,
        moved_npcs = moved,
        "The world's authored water rasters changed since this rtsim save: land NPCs standing \
         in authored water were moved to dry ground. Sites and persisted terrain edits inside \
         the changed regions are not migrated (see the release checklist)."
    );
    info!(moved_npcs = moved, "Authored water NPC pass done");
    *stored_digest = current;
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npcs_in_water_move_to_the_nearest_dry_column_and_others_stay() {
        // Water at x in 10..20 (all y); distance to it measured along x.
        let cell = |p: Vec2<i32>| -> Option<(AuthoredCell, Option<f32>)> {
            if (0..100).contains(&p.x) {
                Some(if (10..20).contains(&p.x) {
                    (
                        AuthoredCell::Wet {
                            surface_block: 200,
                            bed_block: 195,
                        },
                        Some(0.0),
                    )
                } else {
                    let d = if p.x < 10 { 10 - p.x } else { p.x - 19 };
                    (AuthoredCell::Bank { bed_block: 201 }, Some(d as f32))
                })
            } else {
                None
            }
        };
        let ground = |_: Vec2<i32>| 201.0;
        // In the water, nearer the west bank: lands on x = 8 (2 m from it).
        assert_eq!(
            safe_ground(Vec3::new(11.5, 5.5, 197.0), &cell, &ground),
            Some(Vec3::new(8.5, 5.5, 202.0))
        );
        // Nearer the east bank: x = 21.
        assert_eq!(
            safe_ground(Vec3::new(18.5, 5.5, 197.0), &cell, &ground),
            Some(Vec3::new(21.5, 5.5, 202.0))
        );
        // On the lip (1 m from water): moved; 2 m away: stays.
        assert!(safe_ground(Vec3::new(9.5, 5.5, 202.0), &cell, &ground).is_some());
        assert_eq!(
            safe_ground(Vec3::new(8.5, 5.5, 202.0), &cell, &ground),
            None
        );
        // Outside every region: never moved.
        assert_eq!(
            safe_ground(Vec3::new(150.5, 5.5, 50.0), &cell, &ground),
            None
        );
    }
}
