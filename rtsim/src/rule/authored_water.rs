//! XINDELER: keeping persisted NPCs out of authored water.
//!
//! A world's authored water rasters (`world::authored_raster`) can change
//! under an existing rtsim save: a manifest applied for the first time, or a
//! new revision of one. NPCs saved before that may then stand inside an
//! authored channel, lake or bank wall. At startup (the last step of the
//! `Migrate` rule), when the manifest digest stored in the save differs from
//! the world's, every land NPC standing in an authored wet column (or on a
//! bank lip within 2 m of authored water) is moved to the nearest dry column,
//! at its ground height. Left alone:
//!
//! * water-bound and amphibious bodies ([`stays_in_water`]: fish, crustaceans,
//!   boats, crocodiles, sahagin, kappa, kelpies, frogs, seals...) and airborne
//!   ones;
//! * riders: they stay with their mount, which carries them when it moves;
//! * NPCs standing on a bridge or naval-port plot (piers, decks).
//!
//! When every NPC that needed it moved, the digest is recorded and the pass
//! does not run again until the manifest changes. If any NPC found no dry
//! ground within [`SEARCH_RADIUS_M`], the digest is *not* recorded: the
//! pass warns and runs again on the next start.
//!
//! Players are not handled here: on login the server repositions a character
//! with `RepositionToFreeSpace`, which on worlds with authored water uses a
//! liquid-aware ground search (`TerrainGrid::try_find_dry_ground`).

use crate::data::{
    Actor,
    actor::{ActorKind, Actors},
};
use common::comp::{
    self, Body, biped_large, biped_small, quadruped_low, quadruped_medium, quadruped_small,
};
use tracing::{info, warn};
use vek::*;
use world::{IndexRef, World, authored_raster::AuthoredCell, site::plot::PlotKind};

/// How far (m) to look for dry ground around an NPC standing in water.
pub const SEARCH_RADIUS_M: i32 = 64;

/// Bodies that live in or by water and are never moved out of it: the
/// water-bound ones and a documented list of amphibious species (the engine
/// lets almost every body swim, so swimming ability does not discriminate).
pub fn stays_in_water(body: &Body) -> bool {
    match body {
        Body::FishSmall(_) | Body::FishMedium(_) | Body::Crustacean(_) => true,
        Body::Ship(ship) => ship.has_water_thrust(),
        Body::QuadrupedLow(b) => matches!(
            b.species,
            quadruped_low::Species::Crocodile
                | quadruped_low::Species::Alligator
                | quadruped_low::Species::Salamander
                | quadruped_low::Species::Hakulaq
                | quadruped_low::Species::SeaCrocodile
                | quadruped_low::Species::Dagon
                | quadruped_low::Species::Reefsnapper
                | quadruped_low::Species::Elbst
                | quadruped_low::Species::Hydra
        ),
        Body::BipedSmall(b) => matches!(
            b.species,
            biped_small::Species::Sahagin | biped_small::Species::Kappa
        ),
        Body::QuadrupedMedium(b) => matches!(b.species, quadruped_medium::Species::Kelpie),
        Body::BipedLarge(b) => matches!(
            b.species,
            biped_large::Species::Tidalwarrior | biped_large::Species::SeaBishop
        ),
        Body::QuadrupedSmall(b) => matches!(
            b.species,
            quadruped_small::Species::Frog
                | quadruped_small::Species::Axolotl
                | quadruped_small::Species::Turtle
                | quadruped_small::Species::Beaver
                | quadruped_small::Species::Seal
        ),
        _ => false,
    }
}

/// Bodies that do not stand on the ground.
fn airborne(body: &Body) -> bool {
    matches!(
        body,
        Body::Ship(_) | Body::BirdLarge(_) | Body::BirdMedium(_) | Body::Dragon(_)
    ) || matches!(body, Body::Object(comp::object::Body::Crux))
}

/// Where a land NPC at `wpos` must go, if it must move: `Ok(None)` when the
/// column is fine, `Err(())` when it must move but no dry column lies within
/// [`SEARCH_RADIUS_M`]. `cell(wpos)` is the authored cell and the distance to
/// authored water (`None` outside every region) and `ground(wpos)` the
/// ground height to stand on.
#[expect(clippy::result_unit_err)]
pub fn safe_ground(
    wpos: Vec3<f32>,
    cell: &dyn Fn(Vec2<i32>) -> Option<(AuthoredCell, Option<f32>)>,
    ground: &dyn Fn(Vec2<i32>) -> f32,
) -> Result<Option<Vec3<f32>>, ()> {
    let here = wpos.xy().as_::<i32>();
    let unsafe_at = |p: Vec2<i32>| match cell(p) {
        Some((AuthoredCell::Wet { .. }, _)) => true,
        Some((_, dist)) => dist.is_some_and(|d| d < 2.0),
        None => false,
    };
    if !unsafe_at(here) {
        return Ok(None);
    }
    // Rings of increasing Chebyshev radius; the closest safe column of the
    // first ring that has one (fixed scan order: deterministic).
    for r in 1..=SEARCH_RADIUS_M {
        let mut best: Option<(i32, Vec2<i32>)> = None;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let p = here + Vec2::new(dx, dy);
                let d2 = dx * dx + dy * dy;
                if best.is_some_and(|(b, _)| b <= d2) || unsafe_at(p) {
                    continue;
                }
                best = Some((d2, p));
            }
        }
        if let Some((_, p)) = best {
            return Ok(Some(Vec3::new(
                p.x as f32 + 0.5,
                p.y as f32 + 0.5,
                ground(p) + 1.0,
            )));
        }
    }
    Err(())
}

/// Whether `wpos` lies on a bridge or naval-port plot (piers, decks,
/// causeways), where standing above water is intended.
fn on_water_structure(index: IndexRef, wpos: Vec2<i32>) -> bool {
    index.sites.values().any(|site| {
        let b = site.bounds();
        wpos.x >= b.min.x
            && wpos.y >= b.min.y
            && wpos.x < b.max.x
            && wpos.y < b.max.y
            && site.wpos_tile(wpos).plot.is_some_and(|plot| {
                matches!(
                    site.plot(plot).kind(),
                    PlotKind::Bridge(_) | PlotKind::NavalPort(_)
                )
            })
    })
}

/// What the pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub moved: usize,
    pub unresolved: usize,
}

/// The pass over `actors` with the queries abstracted, for tests.
pub fn resolve_with(
    actors: &mut Actors,
    cell: &dyn Fn(Vec2<i32>) -> Option<(AuthoredCell, Option<f32>)>,
    ground: &dyn Fn(Vec2<i32>) -> f32,
    on_structure: &dyn Fn(Vec2<i32>) -> bool,
) -> Outcome {
    let mut out = Outcome::default();
    let ids: Vec<_> = actors.keys().collect();
    for id in ids {
        // Riders stay with their mount (moved below with it).
        if actors.mounts.get_mount_link(id).is_some() {
            continue;
        }
        let Some(actor) = actors.get(id) else {
            continue;
        };
        let movable = |a: &Actor| {
            matches!(a.kind, ActorKind::Npc(_)) && !stays_in_water(&a.body) && !airborne(&a.body)
        };
        if !movable(actor) || on_structure(actor.wpos.xy().as_()) {
            continue;
        }
        match safe_ground(actor.wpos, cell, ground) {
            Ok(None) => {},
            Ok(Some(to)) => {
                let delta = to - actor.wpos;
                let riders: Vec<_> = actors
                    .mounts
                    .iter()
                    .filter(|l| l.mount == id)
                    .map(|l| l.rider)
                    .collect();
                if let Some(a) = actors.get_mut(id) {
                    a.wpos = to;
                }
                for r in riders {
                    if let Some(a) = actors.get_mut(r) {
                        a.wpos += delta;
                    }
                }
                out.moved += 1;
            },
            Err(()) => out.unresolved += 1,
        }
    }
    out
}

/// Run the once-per-manifest pass (see the module doc).
pub fn resolve_npcs(
    actors: &mut Actors,
    stored_digest: &mut Option<String>,
    world: &World,
    index: IndexRef,
) -> Outcome {
    let current = world
        .sim()
        .authored_rasters()
        .map(|r| r.digest().to_string());
    if *stored_digest == current {
        return Outcome::default();
    }
    let sim = world.sim();
    let out = if current.is_some() {
        resolve_with(
            actors,
            &|p| sim.authored_column_at(p).map(|c| (c.cell, c.water_dist)),
            &|p| sim.surface_alt_at(p) - 1.0,
            &|p| on_water_structure(index, p),
        )
    } else {
        Outcome::default()
    };
    if out.unresolved > 0 {
        warn!(
            moved_npcs = out.moved,
            unresolved_npcs = out.unresolved,
            radius_m = SEARCH_RADIUS_M,
            "Authored water: some land NPCs stand in authored water with no dry ground within \
             reach; the manifest digest is not recorded, so this pass runs again on the next start"
        );
    } else {
        warn!(
            previous = ?stored_digest,
            current = ?current,
            moved_npcs = out.moved,
            "The world's authored water rasters changed since this rtsim save: land NPCs standing \
             in authored water were moved to dry ground. Sites and persisted terrain edits inside \
             the changed regions are not migrated (see the release checklist)."
        );
        *stored_digest = current;
    }
    info!(?out, "Authored water NPC pass done");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{humanoid, quadruped_low},
        rtsim::Role,
    };

    // Water at x in 10..20 (all y); distance to it measured along x.
    fn cell(p: Vec2<i32>) -> Option<(AuthoredCell, Option<f32>)> {
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
    }

    fn ground(_: Vec2<i32>) -> f32 { 201.0 }

    #[test]
    fn npcs_in_water_move_to_the_nearest_dry_column_and_others_stay() {
        assert_eq!(
            safe_ground(Vec3::new(11.5, 5.5, 197.0), &cell, &ground),
            Ok(Some(Vec3::new(8.5, 5.5, 202.0)))
        );
        assert_eq!(
            safe_ground(Vec3::new(18.5, 5.5, 197.0), &cell, &ground),
            Ok(Some(Vec3::new(21.5, 5.5, 202.0)))
        );
        assert!(matches!(
            safe_ground(Vec3::new(9.5, 5.5, 202.0), &cell, &ground),
            Ok(Some(_))
        ));
        assert_eq!(
            safe_ground(Vec3::new(8.5, 5.5, 202.0), &cell, &ground),
            Ok(None)
        );
        assert_eq!(
            safe_ground(Vec3::new(150.5, 5.5, 50.0), &cell, &ground),
            Ok(None)
        );
        // All water as far as the search reaches: unresolved.
        let flood = |_: Vec2<i32>| {
            Some((
                AuthoredCell::Wet {
                    surface_block: 200,
                    bed_block: 195,
                },
                Some(0.0),
            ))
        };
        assert_eq!(
            safe_ground(Vec3::new(0.5, 0.5, 197.0), &flood, &ground),
            Err(())
        );
    }

    fn npc(body: Body, wpos: Vec3<f32>) -> Actor { Actor::new_npc(7, wpos, body, Role::Wild) }

    #[test]
    fn aquatic_mounted_and_pier_npcs_are_left_alone_and_mounts_carry_riders() {
        let human = Body::Humanoid(humanoid::Body::random());
        let croc = Body::QuadrupedLow(quadruped_low::Body::random_with(
            &mut rand::rng(),
            &quadruped_low::Species::Crocodile,
        ));
        let horse = Body::QuadrupedMedium(comp::quadruped_medium::Body::random_with(
            &mut rand::rng(),
            &comp::quadruped_medium::Species::Horse,
        ));
        let mut actors = Actors::default();
        let swimmer = actors.create_actor(npc(human, Vec3::new(12.5, 5.5, 197.0)));
        let crocodile = actors.create_actor(npc(croc, Vec3::new(12.5, 6.5, 197.0)));
        let on_pier = actors.create_actor(npc(human, Vec3::new(15.5, 50.5, 202.0)));
        let mount = actors.create_actor(npc(horse, Vec3::new(18.5, 7.5, 197.0)));
        let rider = actors.create_actor(npc(human, Vec3::new(18.5, 7.5, 199.0)));
        actors.mounts.ride(mount, rider).unwrap();
        let pier = |p: Vec2<i32>| p.y >= 40;
        let out = resolve_with(&mut actors, &cell, &ground, &pier);
        assert_eq!(out, Outcome {
            moved: 2,
            unresolved: 0
        });
        assert_eq!(actors[swimmer].wpos, Vec3::new(8.5, 5.5, 202.0));
        assert_eq!(actors[crocodile].wpos, Vec3::new(12.5, 6.5, 197.0));
        assert_eq!(actors[on_pier].wpos, Vec3::new(15.5, 50.5, 202.0));
        assert_eq!(actors[mount].wpos, Vec3::new(21.5, 7.5, 202.0));
        // The rider moved by the mount's offset, not on its own.
        assert_eq!(actors[rider].wpos, Vec3::new(21.5, 7.5, 204.0));
    }
}
