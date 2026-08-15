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
        buff::{Buff, BuffChange, BuffData, BuffKind, BuffSource, Buffs, DestInfo},
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
pub fn set_pact(server: &mut Server, target: EcsEntity, pact: Pact) {
    let previous = server
        .state
        .ecs()
        .read_storage::<Pact>()
        .get(target)
        .copied();
    let was_severed = previous.is_some_and(|p| p.standing == PactStanding::Severed);
    let now_severed = pact.standing == PactStanding::Severed;

    let pact = Pact {
        talisman_bearer: pact.talisman_bearer.filter(|_| {
            pact.standing == PactStanding::Bound && pact.boon == Some(PactBoon::Talisman)
        }),
        ..pact
    };
    let dropped_bearer = previous
        .and_then(|p| p.talisman_bearer)
        .filter(|old| Some(*old) != pact.talisman_bearer);

    let _ = server
        .state
        .ecs_mut()
        .write_storage::<Pact>()
        .insert(target, pact);

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
    let Some(old_pool) = pools.get(entity).cloned() else {
        return;
    };
    let new_pool = old_pool.clone().with_talisman_bond(bonded);
    if new_pool.abilities == old_pool.abilities {
        return;
    }
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
        .copied()
        .unwrap_or_default();
    if pact.standing != PactStanding::Bound || pact.boon != Some(PactBoon::Talisman) {
        return Err(BondError::NotABoundTalismanWarlock);
    }

    let (bearer_uid, protection) = {
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
        (bearer_uid, common::comp::pact::talisman_protection(rank))
    };

    // One bearer at a time: whoever held it loses the ward before the new
    // bearer gains it, so a moved bond can never leave two wards standing.
    if let Some(previous) = pact
        .talisman_bearer
        .and_then(|uid| server.state.ecs().read_resource::<IdMaps>().uid_entity(uid))
    {
        clear_bearer_state(server, previous);
    }

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
                    BuffData::new(protection, None),
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
        .copied()
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
