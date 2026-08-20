//! Server-only authority system keeping a Warlock's talisman bond and
//! everything it projects in agreement.
//!
//! A bond is two pieces of state on two different entities: the Warlock's
//! `Pact::talisman_bearer`, and the bearer's `PactTalisman` ward plus the
//! recall key in their `AbilityPool`. Anything that ends one side must end the
//! other, and there are more ways to end one than there are places to hook:
//! the Warlock dies, the Warlock logs out (taking their `Pact` out of the
//! world with them), the pact is severed, the boon is changed, the bearer logs
//! out, the bearer is removed.
//!
//! Rather than a hook per route -- which is how one gets missed -- this
//! re-derives validity each tick from
//! [`common::comp::pact::bond_is_intact`], the single pure rule, and repairs
//! whichever side disagrees. Every exit route is therefore covered by
//! construction, including ones nobody enumerated.
//!
//! Registered ONLY in `server/src/sys/mod.rs`, never in
//! `common/systems/src/lib.rs::add_local_systems`: the client must not be able
//! to grant itself a ward or a teleport.

use common::{
    comp::{
        ActiveAbilities, Buffs, Health, Mass, Player, SkillSet, Stats, TriggerSlots,
        ability::AbilityPool,
        buff::{Buff, BuffChange, BuffData, BuffKind, BuffSource, DestInfo, MiscBuffData},
        pact::{Pact, bond_is_intact, talisman_protection, talisman_tuning_manifest},
        skillset::skills::{Skill, WarlockSkill},
    },
    event::{BuffEvent, EmitExt},
    event_emitters,
    resources::Time,
    uid::{IdMaps, Uid},
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Entity, Join, Read, ReadStorage, WriteStorage};

use crate::pact::set_talisman_pool_key;

event_emitters! {
    struct Events[Emitters] {
        buff: BuffEvent,
    }
}

#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        Entities<'a>,
        Events<'a>,
        Read<'a, IdMaps>,
        Read<'a, Time>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Player>,
        ReadStorage<'a, Buffs>,
        ReadStorage<'a, Health>,
        ReadStorage<'a, SkillSet>,
        ReadStorage<'a, Stats>,
        ReadStorage<'a, Mass>,
        WriteStorage<'a, Pact>,
        WriteStorage<'a, AbilityPool>,
        WriteStorage<'a, ActiveAbilities>,
        WriteStorage<'a, TriggerSlots>,
    );

    const NAME: &'static str = "pact_talisman";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            entities,
            events,
            id_maps,
            time,
            uids,
            players,
            buffs,
            healths,
            skill_sets,
            stats,
            masses,
            mut pacts,
            mut pools,
            mut actives,
            mut triggers,
        ): Self::SystemData,
    ) {
        let mut emitters = events.get_emitters();

        // Pass 1 -- a ward with nothing sustaining it.
        //
        // Restricted to players: a bearer is always a player in this version,
        // so this walks a handful of entities and does one O(1) `EnumMap`
        // lookup on each, rather than scanning everything that can hold a
        // buff.
        let mut orphaned: Vec<Entity> = Vec::new();
        for (entity, _, entity_buffs, uid) in (&entities, &players, &buffs, &uids).join() {
            let Some(source) =
                entity_buffs
                    .iter_kind(BuffKind::PactTalisman)
                    .find_map(|(_, buff)| match buff.source {
                        BuffSource::Character { by, .. } => Some(by),
                        _ => None,
                    })
            else {
                continue;
            };
            // A Warlock who logged out has no entity and therefore no `Pact`,
            // which `bond_is_intact` reads as "not intact" -- the logout route
            // needs no separate handling.
            let warlock = id_maps.uid_entity(source);
            let warlock_alive = warlock.is_some_and(|w| healths.get(w).is_none_or(|h| !h.is_dead));
            if !bond_is_intact(warlock.and_then(|w| pacts.get(w)), warlock_alive, *uid) {
                orphaned.push(entity);
            }
        }
        for entity in orphaned {
            emitters.emit(BuffEvent {
                entity,
                buff_change: BuffChange::RemoveByKind(BuffKind::PactTalisman),
            });
            set_talisman_pool_key(&mut pools, &mut actives, &mut triggers, entity, false);
        }

        // Pass 2 -- a record naming a bearer who is gone (logged out, removed,
        // or already stripped by pass 1) or whose ward was stripped by
        // something OTHER than pass 1 (e.g. the shipped downed-handler, which
        // removes every non-`PersistOnDowned` buff -- `PactTalisman` is one --
        // before this system ever sees it, so pass 1's buff-driven scan never
        // finds that bearer at all). Left alone, the Warlock could never bond
        // anyone again without first releasing a bond to a bearer who no
        // longer exists, AND the bearer would keep the recall key in their
        // `AbilityPool` forever, since nothing else clears it for this route.
        let mut dangling: Vec<(Entity, Uid)> = Vec::new();
        for (entity, pact) in (&entities, &pacts).join() {
            let Some(bearer_uid) = pact.talisman_bearer else {
                continue;
            };
            let bearer_still_warded = id_maps
                .uid_entity(bearer_uid)
                .and_then(|bearer| buffs.get(bearer))
                .is_some_and(|b| b.contains(BuffKind::PactTalisman));
            if !bearer_still_warded {
                dangling.push((entity, bearer_uid));
            }
        }
        for (entity, bearer_uid) in dangling {
            if let Some(mut pact) = pacts.get_mut(entity) {
                pact.talisman_bearer = None;
            }
            if let Some(bearer) = id_maps.uid_entity(bearer_uid) {
                set_talisman_pool_key(&mut pools, &mut actives, &mut triggers, bearer, false);
            }
        }

        // Pass 3 -- an intact bond whose ward strength has drifted from what
        // the Warlock's CURRENT `TalismanMastery` rank would grant.
        // `bond_talisman` bakes the rank into the ward at the moment the
        // bond is established (`Buff::new` freezes `effects` at
        // construction), so a rank bought mid-session otherwise has no
        // effect until the bond is broken and re-established. Re-derives
        // every tick and, on a mismatch, replaces the whole buff (never a
        // live in-place field mutation, since `strength` is not the only
        // thing `Buff::new` derives from it).
        let mut stale_strength: Vec<(Entity, Entity, Uid, f32)> = Vec::new();
        for (warlock, pact) in (&entities, &pacts).join() {
            let Some(bearer_uid) = pact.talisman_bearer else {
                continue;
            };
            let Some(bearer) = id_maps.uid_entity(bearer_uid) else {
                continue;
            };
            // No entry yet, or the ward was just stripped this same tick by
            // pass 1/2 above -- either way, not this pass's job.
            let Some(current_strength) = buffs
                .get(bearer)
                .and_then(|b| b.iter_kind(BuffKind::PactTalisman).next())
                .map(|(_, buff)| buff.data.strength)
            else {
                continue;
            };
            let rank = skill_sets
                .get(warlock)
                .and_then(|skills| {
                    skills
                        .skill_level(Skill::Warlock(WarlockSkill::TalismanMastery))
                        .ok()
                })
                .unwrap_or(0);
            if let Some(expected_strength) = stale_ward_strength(current_strength, rank) {
                stale_strength.push((warlock, bearer, bearer_uid, expected_strength));
            }
        }
        for (warlock, bearer, bearer_uid, strength) in stale_strength {
            let Some(warlock_uid) = uids.get(warlock).copied() else {
                continue;
            };
            let tuning = talisman_tuning_manifest();
            let ward_data = BuffData::new(strength, None).with_misc_data(MiscBuffData::Reflect {
                fraction: tuning.0.rebuke_fraction,
                cap: tuning.0.rebuke_cap,
                kind: tuning.0.rebuke_kind,
            });
            let dest_info = DestInfo {
                stats: stats.get(bearer),
                mass: masses.get(bearer),
            };
            // Remove-then-add, not a live mutation: `Buffs::insert` only
            // refreshes an existing non-stacking entry when its `data`
            // matches exactly, so inserting a strength-updated buff without
            // first removing the old one would leave both standing rather
            // than replacing it.
            emitters.emit(BuffEvent {
                entity: bearer,
                buff_change: BuffChange::RemoveByKind(BuffKind::PactTalisman),
            });
            emitters.emit(BuffEvent {
                entity: bearer,
                buff_change: BuffChange::Add(Buff::new(
                    BuffKind::PactTalisman,
                    ward_data,
                    Vec::new(),
                    BuffSource::Character {
                        by: warlock_uid,
                        tool_kind: None,
                    },
                    *time,
                    dest_info,
                    masses.get(warlock),
                    Some(bearer_uid),
                    None,
                )),
            });
        }
    }
}

/// Pure: whether an intact bond's ward, currently at `current_strength`,
/// should be replaced because the Warlock's `rank` now derives a different
/// value -- and if so, what to replace it with. Kept free of `specs` so the
/// actual mismatch arithmetic (not just the ECS wiring around it) is
/// unit-testable on its own.
fn stale_ward_strength(current_strength: f32, rank: u16) -> Option<f32> {
    let expected = talisman_protection(rank);
    ((current_strength - expected).abs() > f32::EPSILON).then_some(expected)
}

#[cfg(test)]
mod stale_ward_strength_tests {
    use super::stale_ward_strength;
    use common::comp::pact::talisman_protection;

    #[test]
    fn a_ward_below_the_ranks_true_strength_is_flagged_for_replacement() {
        let stale = talisman_protection(0);
        let correct = talisman_protection(3);
        assert_eq!(stale_ward_strength(stale, 3), Some(correct));
    }

    #[test]
    fn a_ward_already_at_the_ranks_true_strength_is_left_alone() {
        let strength = talisman_protection(2);
        assert_eq!(stale_ward_strength(strength, 2), None);
    }

    #[test]
    fn a_rank_of_zero_still_detects_a_mismatch() {
        // The common real-world case: the bond was established before any
        // `TalismanMastery` investment, then the Warlock spent points --
        // rank 0 -> rank N is exactly the drift this pass exists to catch.
        let never_invested = talisman_protection(0);
        assert_eq!(
            stale_ward_strength(never_invested, 5),
            Some(talisman_protection(5))
        );
    }
}

/// End-to-end coverage of `Sys::run` itself (as opposed to the pure
/// arithmetic above): drives the real system against a real `World` for the
/// death and dangling-bearer exit routes, the two N27-W flagged as having no
/// integration test of their own. Logout is deliberately not reproduced here
/// -- it is exactly "the Warlock's entity, and with it its `Pact`, leaves the
/// `World`", which the module doc already calls out as needing no separate
/// handling, and the pact-severed route is the same `bond_is_intact` branch
/// as death, just a different one of its `&&` terms.
#[cfg(test)]
mod exit_route_tests {
    use common::{
        comp::{self, PactBoon, PactStanding},
        event::EventBus,
        resources::BattleMode,
        uuid::Uuid,
    };
    use specs::{Builder, World, WorldExt};

    use super::*;

    fn humanoid_body() -> comp::Body {
        use rand::{SeedableRng, rngs::SmallRng};
        comp::Body::Humanoid(comp::humanoid::Body::random_with(
            &mut SmallRng::seed_from_u64(0),
            &comp::humanoid::Species::Human,
        ))
    }

    fn mock_world() -> World {
        let mut world = World::new();
        world.insert(IdMaps::new());
        world.insert(Time(0.0));
        world.insert(EventBus::<BuffEvent>::default());
        world.insert(common_ecs::SysMetrics::default());
        world.register::<Uid>();
        world.register::<Player>();
        world.register::<Health>();
        world.register::<Buffs>();
        world.register::<Pact>();
        world.register::<SkillSet>();
        world.register::<Stats>();
        world.register::<Mass>();
        world.register::<AbilityPool>();
        world.register::<ActiveAbilities>();
        world.register::<TriggerSlots>();
        world
    }

    fn spawn(world: &mut World) -> (Entity, Uid) {
        let entity = world.create_entity().build();
        let uid = {
            let mut uids = world.write_component::<Uid>();
            let mut id_maps = world.write_resource::<IdMaps>();
            let uid = id_maps.allocate(entity);
            uids.insert(entity, uid)
                .expect("fresh entity, insert must succeed");
            uid
        };
        (entity, uid)
    }

    /// A ward exactly matching rank-0 `talisman_protection`, so pass 3 never
    /// treats it as stale and adds noise to a test aimed at passes 1/2.
    fn talisman_ward_buff(warlock_uid: Uid, bearer_uid: Uid) -> Buff {
        let tuning = talisman_tuning_manifest();
        let data =
            BuffData::new(talisman_protection(0), None).with_misc_data(MiscBuffData::Reflect {
                fraction: tuning.0.rebuke_fraction,
                cap: tuning.0.rebuke_cap,
                kind: tuning.0.rebuke_kind,
            });
        Buff::new(
            BuffKind::PactTalisman,
            data,
            Vec::new(),
            BuffSource::Character {
                by: warlock_uid,
                tool_kind: None,
            },
            Time(0.0),
            DestInfo {
                stats: None,
                mass: None,
            },
            None,
            Some(bearer_uid),
            None,
        )
    }

    fn bound_talisman_pact(bearer_uid: Uid) -> Pact {
        Pact {
            standing: PactStanding::Bound,
            boon: Some(PactBoon::Talisman),
            talisman_bearer: Some(bearer_uid),
            ..Pact::default()
        }
    }

    #[test]
    fn pass_1_strips_a_bearers_ward_when_the_warlock_who_granted_it_has_died() {
        let mut world = mock_world();
        let (warlock, warlock_uid) = spawn(&mut world);
        let (bearer, bearer_uid) = spawn(&mut world);

        let mut health = Health::new(humanoid_body());
        health.is_dead = true;
        world
            .write_component::<Health>()
            .insert(warlock, health)
            .expect("fresh entity, insert must succeed");
        world
            .write_component::<Pact>()
            .insert(warlock, bound_talisman_pact(bearer_uid))
            .expect("fresh entity, insert must succeed");

        world
            .write_component::<Player>()
            .insert(
                bearer,
                Player::new("bearer".to_string(), BattleMode::PvE, Uuid::nil(), None),
            )
            .expect("fresh entity, insert must succeed");
        let mut buffs = Buffs::default();
        buffs.insert(talisman_ward_buff(warlock_uid, bearer_uid), Time(0.0));
        world
            .write_component::<Buffs>()
            .insert(bearer, buffs)
            .expect("fresh entity, insert must succeed");

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<BuffEvent> = world
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .collect();
        assert_eq!(events.len(), 1, "expected exactly one strip event");
        assert_eq!(events[0].entity, bearer);
        assert_eq!(
            events[0].buff_change,
            BuffChange::RemoveByKind(BuffKind::PactTalisman)
        );
    }

    #[test]
    fn pass_1_leaves_an_intact_bonds_ward_alone() {
        let mut world = mock_world();
        let (warlock, warlock_uid) = spawn(&mut world);
        let (bearer, bearer_uid) = spawn(&mut world);

        world
            .write_component::<Health>()
            .insert(warlock, Health::new(humanoid_body()))
            .expect("fresh entity, insert must succeed");
        world
            .write_component::<Pact>()
            .insert(warlock, bound_talisman_pact(bearer_uid))
            .expect("fresh entity, insert must succeed");

        world
            .write_component::<Player>()
            .insert(
                bearer,
                Player::new("bearer".to_string(), BattleMode::PvE, Uuid::nil(), None),
            )
            .expect("fresh entity, insert must succeed");
        let mut buffs = Buffs::default();
        buffs.insert(talisman_ward_buff(warlock_uid, bearer_uid), Time(0.0));
        world
            .write_component::<Buffs>()
            .insert(bearer, buffs)
            .expect("fresh entity, insert must succeed");

        common_ecs::run_now::<Sys>(&world);

        let events: Vec<BuffEvent> = world
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .collect();
        assert!(events.is_empty(), "an intact bond must not be touched");
    }

    #[test]
    fn pass_2_releases_a_dangling_bond_when_the_bearer_entity_is_gone() {
        let mut world = mock_world();
        let (warlock, _warlock_uid) = spawn(&mut world);
        // Allocate a bearer UID via a throwaway entity, then delete the
        // entity -- `id_maps.uid_entity` now resolves to `None` for it, the
        // same shape a real logout or removal leaves behind, without needing
        // a second live entity for pass 2 to find a ward on.
        let (ghost_bearer, bearer_uid) = spawn(&mut world);
        world
            .delete_entity(ghost_bearer)
            .expect("freshly spawned entity exists");
        world.maintain();

        world
            .write_component::<Pact>()
            .insert(warlock, bound_talisman_pact(bearer_uid))
            .expect("fresh entity, insert must succeed");

        common_ecs::run_now::<Sys>(&world);

        let bearer_after = world
            .read_component::<Pact>()
            .get(warlock)
            .expect("warlock still has a Pact")
            .talisman_bearer;
        assert_eq!(
            bearer_after, None,
            "a bond naming a gone bearer must be released"
        );
    }
}
