//! XINDELER: keeping persisted NPCs out of authored water.
//!
//! A world's authored water rasters (`world::authored_raster`) can change
//! under an existing rtsim save: a manifest applied for the first time, or a
//! new revision of one. NPCs saved before that may then stand inside an
//! authored channel, lake or bank wall. At startup (the last step of the
//! `Migrate` rule), when the manifest digest stored in the save differs from
//! the world's, every land NPC standing in an authored wet column, in a pit
//! of authored ground the sea fills (a dry authored cell of a
//! `SeaFill::Auto` region below the sea's top, see
//! `AuthoredColumn::is_flooded`), or on a bank lip within 2 m of authored
//! water, is moved to the nearest dry column, at its ground height. Left
//! alone:
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
use std::collections::BTreeMap;
use tracing::{info, warn};
use vek::*;
use world::{
    IndexRef, World,
    authored_raster::{AuthoredCell, SEA_TOP_BLOCK},
    site::plot::PlotKind,
};

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
        // Dry authored or unauthored columns: unsafe only on a bank lip.
        Some((
            AuthoredCell::Bank { .. } | AuthoredCell::Ground { .. } | AuthoredCell::None,
            dist,
        )) => dist.is_some_and(|d| d < 2.0),
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

/// The authored cell as a standing NPC sees it: a dry authored cell the sea
/// floods (`flooded`, see `AuthoredColumn::is_flooded`) is water up to the
/// sea's top block, so [`safe_ground`] moves land NPCs off it.
pub fn standing_cell(cell: AuthoredCell, flooded: bool) -> AuthoredCell {
    match cell {
        AuthoredCell::Bank { bed_block }
        | AuthoredCell::Ground {
            block: bed_block, ..
        } if flooded => AuthoredCell::Wet {
            surface_block: SEA_TOP_BLOCK,
            bed_block,
        },
        cell => cell,
    }
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

/// Per-region digests (the authored ground layer): compare each region's
/// digest with the one this save last saw, log how many NPCs stand on the
/// ground layer of the regions that changed (simulated NPCs need no move:
/// every tick snaps them to `surface_alt_at`, which reads the patch), and
/// record the current digests (removed regions are pruned). A save that only
/// knew the manifest digest is migrated without a report when that digest is
/// unchanged. Returns the count logged.
///
/// Must run **before** [`resolve_npcs`], which records the current manifest
/// digest: run after it, every save without per-region digests would look
/// migrated and a changed region would go unreported.
pub fn note_ground_region_changes(
    actors: &Actors,
    stored_manifest: Option<&str>,
    stored_regions: &mut BTreeMap<String, String>,
    world: &World,
) -> usize {
    let Some(rasters) = world.sim().authored_rasters() else {
        return ground_region_changes(actors, stored_manifest, stored_regions, None, &|_| false);
    };
    let regions: Vec<(&str, &str, Aabr<i32>)> = rasters
        .region_entries()
        .map(|r| (r.id, r.digest, r.bounds))
        .collect();
    ground_region_changes(
        actors,
        stored_manifest,
        stored_regions,
        Some((rasters.digest(), &regions)),
        &|p| matches!(rasters.cell_at(p), Some(AuthoredCell::Ground { .. })),
    )
}

/// [`note_ground_region_changes`] with the world abstracted, for tests:
/// `current` is the manifest digest and every region's `(id, digest, box)`
/// (`None` for a world without authored rasters: the record is cleared), and
/// `on_ground(p)` whether `p` is a ground-layer cell.
pub fn ground_region_changes(
    actors: &Actors,
    stored_manifest: Option<&str>,
    stored_regions: &mut BTreeMap<String, String>,
    current: Option<(&str, &[(&str, &str, Aabr<i32>)])>,
    on_ground: &dyn Fn(Vec2<i32>) -> bool,
) -> usize {
    let Some((manifest, regions)) = current else {
        stored_regions.clear();
        return 0;
    };
    let migrated = stored_regions.is_empty() && stored_manifest == Some(manifest);
    let changed: Vec<&(&str, &str, Aabr<i32>)> = regions
        .iter()
        .filter(|(id, digest, _)| {
            !migrated && stored_regions.get(*id).map(String::as_str) != Some(*digest)
        })
        .collect();
    let count = actors
        .values()
        .filter(|a| {
            let p = a.wpos.xy().as_();
            matches!(a.kind, ActorKind::Npc(_))
                && changed.iter().any(|r| r.2.contains_point(p))
                && on_ground(p)
        })
        .count();
    if !changed.is_empty() {
        info!(
            regions = ?changed.iter().map(|r| r.0).collect::<Vec<_>>(),
            npcs_on_ground_cells = count,
            "Authored regions changed since this rtsim save; NPCs on their ground layer follow the \
             patch through the per-tick surface snap"
        );
    }
    *stored_regions = regions
        .iter()
        .map(|(id, digest, _)| (id.to_string(), digest.to_string()))
        .collect();
    count
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
            &|p| {
                sim.authored_column_at(p)
                    .map(|c| (standing_cell(c.cell, c.is_flooded()), c.water_dist))
            },
            &|p| sim.surface_alt_at(p) - 1.0,
            &|p| on_water_structure(index, p),
        )
    } else {
        Outcome::default()
    };
    record_digest(stored_digest, current, out)
}

/// Log the pass and record `current` as handled, unless some NPC was left
/// in authored water (then the pass runs again on the next start).
fn record_digest(
    stored_digest: &mut Option<String>,
    current: Option<String>,
    out: Outcome,
) -> Outcome {
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

    /// A pit of authored ground the sea fills is water for a land NPC: it is
    /// moved to the nearest dry column; the same cell above the sea's top is
    /// dry ground and it stays.
    #[test]
    fn npcs_in_a_sea_filled_pit_move_to_dry_ground() {
        // Ground cells at x in 0..100; a pit (block 129) at x in 10..20.
        let pit = |flooded: bool| {
            move |p: Vec2<i32>| -> Option<(AuthoredCell, Option<f32>)> {
                (0..100).contains(&p.x).then(|| {
                    let cell = if (10..20).contains(&p.x) {
                        AuthoredCell::Ground {
                            block: 129,
                            weight: 255,
                        }
                    } else {
                        AuthoredCell::Ground {
                            block: 145,
                            weight: 255,
                        }
                    };
                    let flooded = flooded && (10..20).contains(&p.x);
                    (standing_cell(cell, flooded), None)
                })
            }
        };
        let ground = |_: Vec2<i32>| 145.0;
        assert_eq!(
            safe_ground(Vec3::new(12.5, 5.5, 130.0), &pit(true), &ground),
            Ok(Some(Vec3::new(9.5, 5.5, 146.0)))
        );
        assert_eq!(
            safe_ground(Vec3::new(12.5, 5.5, 130.0), &pit(false), &ground),
            Ok(None)
        );
        // Unflooded cells pass through unchanged.
        let bank = AuthoredCell::Bank { bed_block: 150 };
        assert_eq!(standing_cell(bank, false), bank);
        assert_eq!(
            standing_cell(AuthoredCell::Bank { bed_block: 120 }, true),
            AuthoredCell::Wet {
                surface_block: SEA_TOP_BLOCK,
                bed_block: 120
            }
        );
    }

    fn region_box(x0: i32, x1: i32) -> Aabr<i32> {
        Aabr {
            min: Vec2::new(x0, 0),
            max: Vec2::new(x1, 100),
        }
    }

    /// The per-region digest record: a migrated save (no region digests, the
    /// manifest digest unchanged) reports nothing; a changed region counts
    /// the NPCs on its ground cells (not those in an unchanged region, nor
    /// off the ground layer); removed regions are pruned; a world without
    /// rasters clears the record.
    #[test]
    fn ground_region_changes_migrate_count_and_prune() {
        let human = || Body::Humanoid(humanoid::Body::random());
        let mut actors = Actors::default();
        actors.create_actor(npc(human(), Vec3::new(10.5, 5.5, 150.0)));
        actors.create_actor(npc(human(), Vec3::new(20.5, 5.5, 150.0)));
        actors.create_actor(npc(human(), Vec3::new(60.5, 5.5, 150.0)));
        // Ground cells everywhere but x in 15..25.
        let on_ground = |p: Vec2<i32>| !(15..25).contains(&p.x);
        let v1 = [
            ("a", "a1", region_box(0, 50)),
            ("b", "b1", region_box(50, 100)),
        ];
        let mut stored = BTreeMap::new();
        // Migration: the old single digest equals the manifest's.
        let n = ground_region_changes(
            &actors,
            Some("m1"),
            &mut stored,
            Some(("m1", &v1)),
            &on_ground,
        );
        assert_eq!(n, 0);
        assert_eq!(stored.get("a").map(String::as_str), Some("a1"));
        // The same with a changed manifest digest: every region is new.
        let mut fresh = BTreeMap::new();
        let n = ground_region_changes(
            &actors,
            Some("m0"),
            &mut fresh,
            Some(("m1", &v1)),
            &on_ground,
        );
        assert_eq!(n, 2, "the NPCs at x 10 and 60 stand on ground cells");
        // Region a changes, b does not: one NPC on a's ground (x 20 is not).
        let v2 = [
            ("a", "a2", region_box(0, 50)),
            ("b", "b1", region_box(50, 100)),
        ];
        let n = ground_region_changes(
            &actors,
            Some("m1"),
            &mut stored,
            Some(("m2", &v2)),
            &on_ground,
        );
        assert_eq!(n, 1);
        assert_eq!(stored.get("a").map(String::as_str), Some("a2"));
        // Region b removed: pruned; nothing changed, nothing counted.
        let v3 = [("a", "a2", region_box(0, 50))];
        let n = ground_region_changes(
            &actors,
            Some("m2"),
            &mut stored,
            Some(("m3", &v3)),
            &on_ground,
        );
        assert_eq!(n, 0);
        assert_eq!(stored.len(), 1);
        // No rasters: the record is cleared.
        let n = ground_region_changes(&actors, Some("m3"), &mut stored, None, &on_ground);
        assert_eq!(n, 0);
        assert!(stored.is_empty());
    }

    /// `note_ground_region_changes` reads the manifest digest the save last
    /// saw, which `resolve_npcs` overwrites: the startup migration must call
    /// it first.
    #[test]
    fn the_region_note_runs_before_the_water_pass() {
        let src = include_str!("migrate.rs");
        let note = src
            .find("authored_water::note_ground_region_changes(")
            .expect("the migration notes ground region changes");
        let resolve = src
            .find("authored_water::resolve_npcs(")
            .expect("the migration runs the water pass");
        assert!(
            note < resolve,
            "note_ground_region_changes must run before resolve_npcs"
        );
    }

    #[test]
    fn unresolved_npcs_keep_the_digest_unrecorded() {
        let flood = |_: Vec2<i32>| {
            Some((
                AuthoredCell::Wet {
                    surface_block: 200,
                    bed_block: 195,
                },
                Some(0.0),
            ))
        };
        let mut actors = Actors::default();
        let stuck = actors.create_actor(npc(
            Body::Humanoid(humanoid::Body::random()),
            Vec3::new(0.5, 0.5, 197.0),
        ));
        let out = resolve_with(&mut actors, &flood, &ground, &|_| false);
        assert_eq!(out, Outcome {
            moved: 0,
            unresolved: 1
        });
        assert_eq!(actors[stuck].wpos, Vec3::new(0.5, 0.5, 197.0));
        let mut stored = Some("old".to_owned());
        record_digest(&mut stored, Some("new".to_owned()), out);
        assert_eq!(stored.as_deref(), Some("old"));
        record_digest(&mut stored, Some("new".to_owned()), Outcome {
            moved: 1,
            unresolved: 0,
        });
        assert_eq!(stored.as_deref(), Some("new"));
    }
}
