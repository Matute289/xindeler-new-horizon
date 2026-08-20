//! Free-standing Warlock-pact mutation, kept separate from the `/pact` chat
//! command (`cmd.rs`) so a future quest/event trigger can bind or sever a
//! pact directly, without depending on the admin-command dispatcher for
//! domain logic. Mirrors `oracle::narrative::send_on_enter_message`'s shape:
//! a plain function taking `&mut Server`, not a `StateExt` method, since it
//! needs to notify the client (`Server::notify_client`), not just mutate the
//! ECS `State`.

use common::{
    comp::{
        ActiveAbilities, ChatType, Content, Inventory, Player, Pos, SkillSet, TriggerSlots,
        ability::AbilityPool,
        buff::{Buff, BuffChange, BuffData, BuffKind, BuffSource, Buffs, DestInfo, MiscBuffData},
        item::ItemKind,
        pact::{Pact, PactBoon, PactStanding},
        skillset::skills::{Skill, WarlockSkill},
    },
    event::{BuffEvent, EventBus},
    resources::Time,
    uid::{IdMaps, Uid},
};
use common_net::msg::ServerGeneral;
use specs::{Entity as EcsEntity, WorldExt, WriteStorage};

use crate::Server;

/// Writes `pact` onto `target`. If this write is the moment the standing
/// becomes `Severed` (was not already), also sends the target a
/// `ChatType::Meta` break-moment notice -- the same channel narrative-event
/// greetings use (`oracle::narrative::send_on_enter_message`), but
/// localized rather than freeform, since this is a fixed system message
/// rather than authored event content.
/// It is also the one place a talisman bond can be invalidated by a pact
/// change: the incoming `pact` is normalized so a bond can only survive on a
/// still-`Bound` `Talisman` pact, and whoever the previous bearer was loses
/// the ward and the recall key the moment the bond stops naming them. Every
/// `/pact` action therefore gets sever/boon-change cleanup for free rather
/// than each remembering to do it.
///
/// The Blade boon's `blade_summoned` is normalized against
/// [`Pact::blade_is_manifest`] on the same principle, and the summoned
/// blade's three attack keys are added to (or stripped from) the Warlock's
/// own [`AbilityPool`] here. Being the single choke point every `/pact`
/// action already routes through, this covers `blade summon`, `blade
/// dismiss`, `sever` and `boon` (the latter two force `blade_summoned: false`
/// themselves) without a hook per route -- the same reason `set_pact` owns
/// the talisman cleanup.
pub fn set_pact(server: &mut Server, target: EcsEntity, pact: Pact) {
    let previous = server
        .state
        .ecs()
        .read_storage::<Pact>()
        .get(target)
        .cloned();
    let was_severed = previous
        .as_ref()
        .is_some_and(|p| p.standing == PactStanding::Severed);
    let now_severed = pact.standing == PactStanding::Severed;

    let pact = Pact {
        talisman_bearer: pact.talisman_bearer.filter(|_| {
            pact.standing == PactStanding::Bound && pact.boon == Some(PactBoon::Talisman)
        }),
        // Same normalization, one boon over: a blade cannot be out on a
        // severed pact or under a boon that isn't Blade. Callers already
        // force this on those routes; doing it here too means the pool grant
        // below can be derived from the stored flag alone.
        blade_summoned: pact.blade_is_manifest(),
        ..pact
    };
    let dropped_bearer = previous
        .and_then(|p| p.talisman_bearer)
        .filter(|old| Some(*old) != pact.talisman_bearer);
    let blade_manifest = pact.blade_summoned;

    let _ = server
        .state
        .ecs_mut()
        .write_storage::<Pact>()
        .insert(target, pact);

    // The Warlock's own pool, not a bearer's: the blade is granted to whoever
    // holds the pact.
    {
        let ecs = server.state.ecs();
        set_blade_pool_keys(
            &mut ecs.write_storage::<AbilityPool>(),
            &mut ecs.write_storage::<ActiveAbilities>(),
            &mut ecs.write_storage::<TriggerSlots>(),
            target,
            blade_manifest,
        );
    }

    let dropped_entity = dropped_bearer.and_then(|dropped| {
        server
            .state
            .ecs()
            .read_resource::<IdMaps>()
            .uid_entity(dropped)
    });
    if let Some(entity) = dropped_entity {
        clear_bearer_state(server, entity);
    }

    if now_severed && !was_severed {
        server.notify_client(
            target,
            ServerGeneral::server_msg(
                ChatType::Meta,
                Content::localized("hud-pact-severed-notice"),
            ),
        );
    }
}

/// Why a talisman bond could not be established.
#[derive(Debug, PartialEq, Eq)]
pub enum BondError {
    NotABoundTalismanWarlock,
    BearerIsSelf,
    BearerIsNotAPlayer,
    BearerHoldsNoTalisman,
    OutOfRange,
}

impl BondError {
    pub fn message(&self) -> &'static str {
        match self {
            BondError::NotABoundTalismanWarlock => "The caster has no bound Pact of the Talisman.",
            BondError::BearerIsSelf => "A Warlock cannot bear their own talisman.",
            BondError::BearerIsNotAPlayer => "Only players may bear a talisman.",
            BondError::BearerHoldsNoTalisman => "That character is not carrying a talisman.",
            BondError::OutOfRange => "That character is too far away to be bonded.",
        }
    }
}

/// Adds or removes the bearer's recall key, keeping every index-based binding
/// that survives the rebuild pointed at the same ability.
///
/// The key is appended after everything else the pool holds, never inserted
/// mid-list, so no binding ahead of it can be shifted (see
/// [`AbilityPool::with_talisman_bond`]).
pub fn set_talisman_pool_key(
    pools: &mut WriteStorage<AbilityPool>,
    actives: &mut WriteStorage<ActiveAbilities>,
    triggers: &mut WriteStorage<TriggerSlots>,
    entity: EcsEntity,
    bonded: bool,
) {
    // Membership is tested on the borrowed pool first: this runs on every
    // bond, release and bond-break tick, and the common answer is "nothing to
    // do" -- which must not cost two deep clones of a `Vec<String>` holding
    // every class ability key.
    let Some(pool) = pools.get(entity) else {
        return;
    };
    if pool.has_talisman_bond() == bonded {
        return;
    }
    let old_pool = pool.clone();
    let new_pool = old_pool.clone().with_talisman_bond(bonded);
    if let Some(mut active) = actives.get_mut(entity) {
        common::comp::ability::remap_innate_bindings(&mut active, &old_pool, &new_pool);
    }
    // A live trigger slot holds raw pool indices too; a re-pointed one would
    // mint its cooldown-bypass token for the wrong ability.
    if let Some(mut slots) = triggers.get_mut(entity) {
        slots.remap_innate_bindings(&old_pool, &new_pool);
    }
    let _ = pools.insert(entity, new_pool);
}

/// Adds or removes the summoned blade's three attack keys, re-pointing every
/// index-based binding that survives the rebuild at the same ability.
///
/// Structurally identical to [`set_talisman_pool_key`] above -- the same
/// membership pre-check (this is reached on every `/pact` action, and the
/// common answer is "nothing to do", which must not cost two deep clones of a
/// `Vec<String>` holding every class ability key) and the same two remaps.
/// All three keys are appended after everything else the pool holds, never
/// inserted mid-list (see [`AbilityPool::with_blade_bond`]).
///
/// Note what this does NOT do: no `EquipSlot` is touched, no `Item` is
/// created, `ActiveMainhand` keeps whatever real weapon was already there,
/// and nothing here has a lifetime or expiry -- the blade stays out until the
/// Warlock dismisses it, severs the pact, or changes boon. The individual
/// abilities' own cast cooldowns are authored in their RONs and are a
/// separate thing entirely.
pub fn set_blade_pool_keys(
    pools: &mut WriteStorage<AbilityPool>,
    actives: &mut WriteStorage<ActiveAbilities>,
    triggers: &mut WriteStorage<TriggerSlots>,
    entity: EcsEntity,
    summoned: bool,
) {
    let Some(pool) = pools.get(entity) else {
        return;
    };
    if pool.has_blade_bond() == summoned {
        return;
    }
    let old_pool = pool.clone();
    let new_pool = old_pool.clone().with_blade_bond(summoned);
    if let Some(mut active) = actives.get_mut(entity) {
        common::comp::ability::remap_innate_bindings(&mut active, &old_pool, &new_pool);
    }
    // A live trigger slot holds raw pool indices too; a re-pointed one would
    // mint its cooldown-bypass token for the wrong ability.
    if let Some(mut slots) = triggers.get_mut(entity) {
        slots.remap_innate_bindings(&old_pool, &new_pool);
    }
    let _ = pools.insert(entity, new_pool);
}

/// Strips a bearer's ward and recall key. Safe to call on an entity that was
/// never bonded.
pub fn clear_bearer_state(server: &mut Server, bearer: EcsEntity) {
    let ecs = server.state.ecs();
    ecs.read_resource::<EventBus<BuffEvent>>()
        .emit_now(BuffEvent {
            entity: bearer,
            buff_change: BuffChange::RemoveByKind(BuffKind::PactTalisman),
        });
    set_talisman_pool_key(
        &mut ecs.write_storage::<AbilityPool>(),
        &mut ecs.write_storage::<ActiveAbilities>(),
        &mut ecs.write_storage::<TriggerSlots>(),
        bearer,
        false,
    );
}

/// Whether `entity` is carrying at least one talisman.
///
/// Possession is the whole check: the item is stateless and interchangeable,
/// so *which* talisman it is can never matter.
fn carries_talisman(inventories: &specs::ReadStorage<Inventory>, entity: EcsEntity) -> bool {
    inventories.get(entity).is_some_and(|inv| {
        inv.slots().flatten().any(|item| {
            matches!(&*item.kind(), ItemKind::Utility {
                kind: common::comp::item::Utility::Talisman,
                ..
            })
        })
    })
}

/// The nearest player within bonding reach of `warlock` who is carrying a
/// talisman, if any.
///
/// Modelled on the Collar's taming scan: no target is named, because carrying
/// the token (which the carrier had to accept via trade or pickup) is the
/// consent signal and standing in reach is the range check.
pub fn find_talisman_bearer(server: &Server, warlock: EcsEntity) -> Option<EcsEntity> {
    use specs::{Join, WorldExt};

    let ecs = server.state.ecs();
    let positions = ecs.read_storage::<Pos>();
    let inventories = ecs.read_storage::<Inventory>();
    let players = ecs.read_storage::<Player>();
    let entities = ecs.entities();

    let origin = positions.get(warlock)?.0;
    let range = common::comp::pact::talisman_tuning_manifest().0.bond_range;
    let range_sq = range * range;

    (&entities, &positions, &players)
        .join()
        .filter(|(entity, _, _)| *entity != warlock)
        .filter(|(_, pos, _)| pos.0.distance_squared(origin) <= range_sq)
        .filter(|(entity, _, _)| carries_talisman(&inventories, *entity))
        .min_by_key(|(_, pos, _)| (pos.0.distance_squared(origin) * 100.0) as i32)
        .map(|(entity, _, _)| entity)
}

/// Moves `warlock`'s talisman bond onto `bearer`.
///
/// The bond is recorded on the WARLOCK's [`Pact`], never on the talisman item
/// -- the item is only the physical token whose possession is checked here.
/// Exactly one bearer at a time: an existing bond is released first, so this
/// moves the bond rather than adding a second one.
pub fn bond_talisman(
    server: &mut Server,
    warlock: EcsEntity,
    bearer: EcsEntity,
) -> Result<(), BondError> {
    if warlock == bearer {
        return Err(BondError::BearerIsSelf);
    }

    let pact = server
        .state
        .ecs()
        .read_storage::<Pact>()
        .get(warlock)
        .cloned()
        .unwrap_or_default();
    if pact.standing != PactStanding::Bound || pact.boon != Some(PactBoon::Talisman) {
        return Err(BondError::NotABoundTalismanWarlock);
    }

    let (bearer_uid, ward_data) = {
        let ecs = server.state.ecs();
        // v1 is players only. An NPC bearer costs nothing to allow later, but
        // nothing here (the recall grant, the ward, the cleanup) has been
        // designed against an agent-driven carrier.
        if ecs.read_storage::<Player>().get(bearer).is_none() {
            return Err(BondError::BearerIsNotAPlayer);
        }
        if !carries_talisman(&ecs.read_storage::<Inventory>(), bearer) {
            return Err(BondError::BearerHoldsNoTalisman);
        }

        let positions = ecs.read_storage::<Pos>();
        let tuning = common::comp::pact::talisman_tuning_manifest();
        let in_range = match (positions.get(warlock), positions.get(bearer)) {
            (Some(a), Some(b)) => {
                a.0.distance_squared(b.0) <= tuning.0.bond_range * tuning.0.bond_range
            },
            _ => false,
        };
        if !in_range {
            return Err(BondError::OutOfRange);
        }

        let Some(bearer_uid) = ecs.read_storage::<Uid>().get(bearer).copied() else {
            return Err(BondError::BearerIsNotAPlayer);
        };
        let rank = ecs
            .read_storage::<SkillSet>()
            .get(warlock)
            .and_then(|skills| {
                skills
                    .skill_level(Skill::Warlock(WarlockSkill::TalismanMastery))
                    .ok()
            })
            .unwrap_or(0);
        (
            bearer_uid,
            BuffData::new(common::comp::pact::talisman_protection(rank), None).with_misc_data(
                // The rebuke's three dials are resolved here, at the one
                // place a bond is granted, rather than inside `BuffKind::
                // effects()` -- which must stay free of content-specific
                // asset reads.
                MiscBuffData::Reflect {
                    fraction: tuning.0.rebuke_fraction,
                    cap: tuning.0.rebuke_cap,
                    kind: tuning.0.rebuke_kind,
                },
            ),
        )
    };

    // One bearer at a time. `set_pact` is what strips whoever held it before,
    // so a moved bond can never leave two wards standing -- and re-bonding the
    // SAME bearer refreshes rather than removes, since the previous and new
    // bearer match.
    set_pact(server, warlock, Pact {
        talisman_bearer: Some(bearer_uid),
        ..pact
    });

    {
        let ecs = server.state.ecs();
        let warlock_uid = ecs.read_storage::<Uid>().get(warlock).copied();
        let time = *ecs.read_resource::<Time>();
        let stats = ecs.read_storage::<common::comp::Stats>();
        let masses = ecs.read_storage::<common::comp::Mass>();
        let mut all_buffs = ecs.write_storage::<Buffs>();
        if let (Some(warlock_uid), Some(mut buffs)) = (warlock_uid, all_buffs.get_mut(bearer)) {
            let dest_info = DestInfo {
                stats: stats.get(bearer),
                mass: masses.get(bearer),
            };
            buffs.insert(
                Buff::new(
                    BuffKind::PactTalisman,
                    // No duration: the bond's own lifetime is the ward's
                    // lifetime, and that is governed by the cleanup pass, not
                    // by a timer that could expire out of step with it.
                    ward_data,
                    Vec::new(),
                    BuffSource::Character {
                        by: warlock_uid,
                        tool_kind: None,
                    },
                    time,
                    dest_info,
                    masses.get(warlock),
                    Some(bearer_uid),
                    None,
                ),
                time,
            );
        }
        set_talisman_pool_key(
            &mut ecs.write_storage::<AbilityPool>(),
            &mut ecs.write_storage::<ActiveAbilities>(),
            &mut ecs.write_storage::<TriggerSlots>(),
            bearer,
            true,
        );
    }

    Ok(())
}

/// Drops `warlock`'s bond and everything it projects onto its bearer.
///
/// Idempotent; the single funnel every exit route (death, logout, sever, boon
/// change, re-bonding) reaches.
pub fn release_talisman(server: &mut Server, warlock: EcsEntity) {
    let Some(pact) = server
        .state
        .ecs()
        .read_storage::<Pact>()
        .get(warlock)
        .cloned()
    else {
        return;
    };
    let Some(bearer_uid) = pact.talisman_bearer else {
        return;
    };
    let bearer = server
        .state
        .ecs()
        .read_resource::<IdMaps>()
        .uid_entity(bearer_uid);

    let _ = server
        .state
        .ecs_mut()
        .write_storage::<Pact>()
        .insert(warlock, Pact {
            talisman_bearer: None,
            ..pact
        });

    if let Some(bearer) = bearer {
        clear_bearer_state(server, bearer);
    }
}

/// N27-AB — the ECS half of granting the summoned blade: that
/// [`set_blade_pool_keys`] adds and removes all three keys together, and that
/// it re-points (or clears) the bindings that hold raw pool indices.
///
/// Driven against a real `World`'s storages rather than through [`set_pact`],
/// which needs a whole `Server`; the normalization `set_pact` layers on top is
/// covered by `Pact::blade_is_manifest`'s own tests in `common`.
#[cfg(test)]
mod blade_pool_tests {
    use common::comp::ability::{AbilityPool, AuxiliaryAbility};
    use specs::{Builder, World, WorldExt};

    use super::*;

    fn human_body() -> common::comp::Body {
        use rand::{SeedableRng, rngs::SmallRng};
        common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut SmallRng::seed_from_u64(0),
            &common::comp::humanoid::Species::Human,
        ))
    }

    fn warlock_pool() -> AbilityPool {
        AbilityPool::for_character(
            &human_body(),
            &common::comp::CharacterClass::single(common::comp::ClassKind::Warlock),
            AbilityPool::no_learned_spells(),
        )
    }

    fn mock_world() -> World {
        let mut world = World::new();
        world.register::<AbilityPool>();
        world.register::<ActiveAbilities>();
        world.register::<TriggerSlots>();
        world
    }

    fn toggle(world: &World, entity: EcsEntity, summoned: bool) {
        set_blade_pool_keys(
            &mut world.write_storage::<AbilityPool>(),
            &mut world.write_storage::<ActiveAbilities>(),
            &mut world.write_storage::<TriggerSlots>(),
            entity,
            summoned,
        );
    }

    /// Summoning appends the three keys; dismissing takes exactly those three
    /// back out and leaves the rest of the pool byte-identical.
    #[test]
    fn summon_and_dismiss_add_and_remove_all_three_keys_together() {
        let mut world = mock_world();
        let base = warlock_pool();
        let entity = world.create_entity().with(base.clone()).build();

        toggle(&world, entity, true);
        let summoned = world
            .read_storage::<AbilityPool>()
            .get(entity)
            .cloned()
            .expect("pool still present");
        assert!(summoned.has_blade_bond());
        assert_eq!(summoned.abilities.len(), base.abilities.len() + 3);
        assert_eq!(
            &summoned.abilities[..base.abilities.len()],
            &base.abilities[..],
            "no pre-existing key may shift"
        );

        toggle(&world, entity, false);
        let dismissed = world
            .read_storage::<AbilityPool>()
            .get(entity)
            .cloned()
            .expect("pool still present");
        assert!(!dismissed.has_blade_bond());
        assert_eq!(dismissed.abilities, base.abilities);
    }

    /// A hotbar slot bound to an unrelated pool key must be untouched by the
    /// whole summon -> dismiss cycle, while a slot bound to a blade key is
    /// emptied on dismiss (the key genuinely is not in the pool any more,
    /// exactly as a released talisman bearer's recall slot is).
    #[test]
    fn the_cycle_repoints_unrelated_bindings_and_empties_the_blades_own() {
        let mut world = mock_world();
        let base = warlock_pool();
        let unrelated_key = base.abilities[0].clone();

        let mut active = ActiveAbilities::default_limited(5);
        active.change_ability(
            0,
            ActiveAbilities::active_auxiliary_key(None),
            AuxiliaryAbility::Innate(0),
            None,
            None,
        );
        let entity = world.create_entity().with(base).with(active).build();

        toggle(&world, entity, true);

        // Bind slot 1 to the blade's base strike now that it exists.
        let blade_index = world
            .read_storage::<AbilityPool>()
            .get(entity)
            .expect("pool")
            .abilities
            .iter()
            .position(|key| key == AbilityPool::PACT_BLADE_KEYS[0])
            .expect("the base strike is in the pool");
        world
            .write_storage::<ActiveAbilities>()
            .get_mut(entity)
            .expect("active abilities")
            .change_ability(
                1,
                ActiveAbilities::active_auxiliary_key(None),
                AuxiliaryAbility::Innate(blade_index),
                None,
                None,
            );

        toggle(&world, entity, false);

        let pools = world.read_storage::<AbilityPool>();
        let actives = world.read_storage::<ActiveAbilities>();
        let pool = pools.get(entity).expect("pool");
        let active = actives.get(entity).expect("active");
        let set = active.auxiliary_set(None, None);

        let AuxiliaryAbility::Innate(index) = set[0] else {
            panic!(
                "the unrelated binding must still be innate, got {:?}",
                set[0]
            );
        };
        assert_eq!(
            pool.abilities[index], unrelated_key,
            "an unrelated hotbar binding must survive the whole cycle"
        );
        assert!(
            matches!(set[1], AuxiliaryAbility::Empty),
            "a slot bound to a dismissed blade key must be emptied, not left pointing at whatever \
             moved into its index, got {:?}",
            set[1]
        );
    }

    /// Cheap no-op paths: an entity with no pool at all, and a redundant
    /// toggle. `set_pact` reaches this on EVERY `/pact` action, so "nothing
    /// to do" must stay free.
    #[test]
    fn a_redundant_toggle_and_a_poolless_entity_are_both_no_ops() {
        let mut world = mock_world();
        let poolless = world.create_entity().build();
        let entity = world.create_entity().with(warlock_pool()).build();

        // Must not panic, and must insert nothing.
        toggle(&world, poolless, true);
        // Already dismissed; dismissing again changes nothing.
        toggle(&world, entity, false);

        assert!(world.read_storage::<AbilityPool>().get(poolless).is_none());
        assert_eq!(
            world
                .read_storage::<AbilityPool>()
                .get(entity)
                .expect("pool")
                .abilities,
            warlock_pool().abilities
        );
    }

    /// The blade is exactly three pool keys: no `EquipSlot` is touched, no
    /// `Item` is created, and nothing that could be dropped, traded, sold or
    /// persisted as gear comes into existence. An `Inventory` sitting on the
    /// same entity comes out of a summon untouched.
    #[test]
    fn summoning_the_blade_never_touches_the_inventory_or_a_loadout_slot() {
        use common::comp::inventory::slot::EquipSlot;

        let mut world = mock_world();
        world.register::<Inventory>();
        let entity = world
            .create_entity()
            .with(warlock_pool())
            .with(Inventory::with_empty())
            .build();

        let before = world
            .read_storage::<Inventory>()
            .get(entity)
            .map(|inv| inv.slots().flatten().count())
            .expect("inventory");

        toggle(&world, entity, true);

        let inventories = world.read_storage::<Inventory>();
        let after = inventories.get(entity).expect("inventory");
        assert_eq!(
            before,
            after.slots().flatten().count(),
            "summoning the blade must not add an item"
        );
        for slot in [
            EquipSlot::ActiveMainhand,
            EquipSlot::ActiveOffhand,
            EquipSlot::InactiveMainhand,
            EquipSlot::InactiveOffhand,
        ] {
            assert!(
                after.equipped(slot).is_none(),
                "the blade must never occupy {slot:?}"
            );
        }
    }
}
