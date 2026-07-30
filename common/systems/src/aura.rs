use std::collections::HashSet;

use common::{
    combat::{self, DamageSource, TierEffect},
    comp::{
        Alignment, Aura, Auras, BuffKind, Buffs, CharacterClass, CharacterState, Health,
        HealthChange, Mass, Player, Pos, Stats,
        aura::{AuraChange, AuraKey, AuraKind, AuraTarget, EnteredAuras},
        buff::{Buff, BuffCategory, BuffChange, BuffSource, DestInfo},
        group::Group,
    },
    event::{AuraEvent, BuffEvent, EmitExt, HealthChangeEvent},
    event_emitters, match_some,
    resources::Time,
    uid::{IdMaps, Uid},
};
use common_ecs::{Job, Origin, Phase, System};
use specs::{Entities, Entity as EcsEntity, Join, Read, ReadStorage, SystemData, shred};

event_emitters! {
    struct Events[Emitters] {
        aura: AuraEvent,
        buff: BuffEvent,
        health_change: HealthChangeEvent,
    }
}

#[derive(SystemData)]
pub struct ReadData<'a> {
    entities: Entities<'a>,
    players: ReadStorage<'a, Player>,
    time: Read<'a, Time>,
    events: Events<'a>,
    id_maps: Read<'a, IdMaps>,
    cached_spatial_grid: Read<'a, common::CachedSpatialGrid>,
    positions: ReadStorage<'a, Pos>,
    char_states: ReadStorage<'a, CharacterState>,
    alignments: ReadStorage<'a, Alignment>,
    healths: ReadStorage<'a, Health>,
    groups: ReadStorage<'a, Group>,
    uids: ReadStorage<'a, Uid>,
    stats: ReadStorage<'a, Stats>,
    buffs: ReadStorage<'a, Buffs>,
    auras: ReadStorage<'a, Auras>,
    entered_auras: ReadStorage<'a, EnteredAuras>,
    masses: ReadStorage<'a, Mass>,
    character_classes: ReadStorage<'a, CharacterClass>,
}

#[derive(Default)]
pub struct Sys;
impl<'a> System<'a> for Sys {
    type SystemData = ReadData<'a>;

    const NAME: &'static str = "aura";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(_job: &mut Job<Self>, read_data: Self::SystemData) {
        let mut emitters = read_data.events.get_emitters();
        let mut active_auras: HashSet<(Uid, Uid, AuraKey)> = HashSet::new();

        // Iterate through all entities with an aura
        for (entity, pos, auras_comp, uid) in (
            &read_data.entities,
            &read_data.positions,
            &read_data.auras,
            &read_data.uids,
        )
            .join()
        {
            let mut expired_auras = Vec::<AuraKey>::new();
            // Iterate through the auras attached to this entity
            for (key, aura) in auras_comp.auras.iter() {
                // Tick the aura and subtract dt from it
                if let Some(end_time) = aura.end_time
                    && read_data.time.0 > end_time.0
                {
                    expired_auras.push(key);
                }
                let eligible = |target: specs::Entity, target_pos: &Pos, target_uid: &Uid| {
                    // Ensure entity is within the aura radius
                    target_pos.0.distance_squared(pos.0) < aura.radius.powi(2) && {
                        // Ensure the entity is in the group we want to target
                        let same_group = |uid: Uid| {
                            read_data
                                .id_maps
                                .uid_entity(uid)
                                .and_then(|e| read_data.groups.get(e))
                                .is_some_and(|owner_group| {
                                    Some(owner_group) == read_data.groups.get(target)
                                })
                                || *target_uid == uid
                        };
                        let allow_friendly_fire =
                            combat::allow_friendly_fire(&read_data.entered_auras, entity, target);
                        allow_friendly_fire && entity != target
                            || match aura.target {
                                AuraTarget::GroupOf(uid) => same_group(uid),
                                AuraTarget::NotGroupOf(uid) => !same_group(uid),
                                AuraTarget::All => true,
                            }
                    }
                };

                // Shared by any aura kind that needs "up to N nearest
                // eligible targets" instead of everyone in radius -- resolves
                // who's selected once per activation before the per-target
                // loop, so both pool-split buffs and tiered health effects
                // can build on the same capped-nearest-N selection.
                let nearest_eligible = |max_targets: usize| -> Vec<(specs::Entity, f32)> {
                    let mut nearest = read_data
                        .cached_spatial_grid
                        .0
                        .in_circle_aabr(pos.0.xy(), aura.radius)
                        .filter_map(|target| {
                            let target_pos = read_data.positions.get(target)?;
                            let target_uid = read_data.uids.get(target)?;
                            eligible(target, target_pos, target_uid)
                                .then(|| (target, target_pos.0.distance_squared(pos.0)))
                        })
                        .collect::<Vec<_>>();
                    nearest.sort_by(|(_, a), (_, b)| a.total_cmp(b));
                    nearest.truncate(max_targets.max(1));
                    nearest
                };

                // A pool-split aura shares one total among the nearest
                // eligible targets (capped) instead of applying a flat
                // strength to everyone found.
                let pool_split_strength: std::collections::HashMap<specs::Entity, f32> =
                    if let AuraKind::Buff {
                        pool_split: Some(split),
                        ..
                    } = &aura.aura_kind
                    {
                        let nearest = nearest_eligible(split.max_targets);
                        let total = split.resolved_total(
                            read_data.stats.get(entity).map(|s| s.character_level),
                            read_data.character_classes.get(entity),
                        );
                        let share = total / nearest.len().max(1) as f32;
                        nearest.into_iter().map(|(e, _)| (e, share)).collect()
                    } else {
                        std::collections::HashMap::new()
                    };
                let has_pool_split = matches!(aura.aura_kind, AuraKind::Buff {
                    pool_split: Some(_),
                    ..
                });

                // A tiered-health-effect aura resolves an independent effect
                // per selected target (against that target's own current
                // health), rather than sharing a pool -- only the selection
                // (who's in range/eligible, capped nearest-first) is shared
                // logic with pool-split.
                let tiered_health_targets: HashSet<specs::Entity> =
                    if let AuraKind::TieredHealthEffect { max_targets, .. } = &aura.aura_kind {
                        nearest_eligible(*max_targets)
                            .into_iter()
                            .map(|(e, _)| e)
                            .collect()
                    } else {
                        HashSet::new()
                    };
                let has_tiered_health_effect =
                    matches!(aura.aura_kind, AuraKind::TieredHealthEffect { .. });

                let target_iter = read_data
                    .cached_spatial_grid
                    .0
                    .in_circle_aabr(pos.0.xy(), aura.radius)
                    .filter_map(|target| {
                        read_data.positions.get(target).and_then(|target_pos| {
                            Some((
                                target,
                                target_pos,
                                read_data.healths.get(target)?,
                                read_data.uids.get(target)?,
                                read_data.entered_auras.get(target)?,
                            ))
                        })
                    });
                target_iter.for_each(|(target, target_pos, health, target_uid, entered_auras)| {
                    let target_buffs = match read_data.buffs.get(target) {
                        Some(buff) => buff,
                        None => return,
                    };

                    // A pool-split aura only ever activates for the targets
                    // resolved above, at their computed share -- never the
                    // aura's own (unused) flat strength.
                    if has_pool_split && !pool_split_strength.contains_key(&target) {
                        return;
                    }
                    // Likewise, a tiered-health-effect aura only ever
                    // activates for its capped nearest-N selection.
                    if has_tiered_health_effect && !tiered_health_targets.contains(&target) {
                        return;
                    }

                    // Ensure entity is within the aura radius
                    if eligible(target, target_pos, target_uid) {
                        let allow_friendly_fire =
                            combat::allow_friendly_fire(&read_data.entered_auras, entity, target);

                        let mut aura_for_target = std::borrow::Cow::Borrowed(aura);
                        if let Some(share) = pool_split_strength.get(&target) {
                            let mut owned = aura.clone();
                            if let AuraKind::Buff { ref mut data, .. } = owned.aura_kind {
                                data.strength = *share;
                            }
                            aura_for_target = std::borrow::Cow::Owned(owned);
                        }

                        let did_activate = activate_aura(
                            key,
                            &aura_for_target,
                            entity,
                            *uid,
                            target,
                            health,
                            target_buffs,
                            allow_friendly_fire,
                            &read_data,
                            &mut emitters,
                        );

                        if did_activate {
                            if entered_auras
                                .auras
                                .get(aura.aura_kind.as_ref())
                                .is_none_or(|auras| !auras.contains(&(*uid, key)))
                            {
                                emitters.emit(AuraEvent {
                                    entity: target,
                                    aura_change: AuraChange::EnterAura(
                                        *uid,
                                        key,
                                        *aura.aura_kind.as_ref(),
                                    ),
                                });
                            }
                            active_auras.insert((*uid, *target_uid, key));
                        }
                    }
                });
            }
            if !expired_auras.is_empty() {
                emitters.emit(AuraEvent {
                    entity,
                    aura_change: AuraChange::RemoveByKey(expired_auras),
                });
            }
        }

        for (entity, entered_auras, uid) in (
            &read_data.entities,
            &read_data.entered_auras,
            &read_data.uids,
        )
            .join()
            .filter(|(_, active_auras, _)| !active_auras.auras.is_empty())
        {
            emitters.emit_many(
                entered_auras
                    .auras
                    .iter()
                    .flat_map(|(variant, entered_auras)| {
                        entered_auras.iter().zip(core::iter::repeat(*variant))
                    })
                    .filter_map(|((caster_uid, key), variant)| {
                        (!active_auras.contains(&(*caster_uid, *uid, *key))).then_some(AuraEvent {
                            entity,
                            aura_change: AuraChange::ExitAura(*caster_uid, *key, variant),
                        })
                    }),
            );
        }
    }
}

#[warn(clippy::pedantic)]
//#[warn(clippy::nursery)]
fn activate_aura(
    key: AuraKey,
    aura: &Aura,
    applier: EcsEntity,
    applier_uid: Uid,
    target: EcsEntity,
    health: &Health,
    target_buffs: &Buffs,
    allow_friendly_fire: bool,
    read_data: &ReadData,
    emitters: &mut (impl EmitExt<BuffEvent> + EmitExt<HealthChangeEvent>),
) -> bool {
    let should_activate = match aura.aura_kind {
        AuraKind::Buff { kind, source, .. } => {
            let conditions_held = match kind {
                BuffKind::RestingHeal => {
                    // true if sitting or if owned and owner is sitting + not full health
                    health.current() < health.maximum()
                        && (read_data
                            .char_states
                            .get(target)
                            .is_some_and(CharacterState::is_sitting)
                            || read_data
                                .alignments
                                .get(target)
                                .and_then(|alignment| match_some!(alignment, Alignment::Owned(uid) => uid))
                                .and_then(|uid| read_data.id_maps.uid_entity(*uid))
                                .and_then(|owner| read_data.char_states.get(owner))
                                .is_some_and(CharacterState::is_sitting))
                },
                // Add other specific buff conditions here
                _ => true,
            };

            // TODO: this check will disable friendly fire with PvE switch.
            //
            // Which means that you can't apply debuffs on you and your group
            // even if it's intended mechanic.
            //
            // We don't have this for now, but think about this
            // when we will add this.
            let permit_pvp = || {
                let owner = match source {
                    BuffSource::Character { by, .. } => read_data.id_maps.uid_entity(by),
                    _ => None,
                };
                combat::permit_pvp(
                    &read_data.alignments,
                    &read_data.players,
                    &read_data.entered_auras,
                    &read_data.id_maps,
                    owner,
                    target,
                )
            };

            // A debuff aura that explicitly targets `All` is intentionally
            // indiscriminate (e.g. BL-36 antimagic field) — it applies to the
            // caster, allies and enemies alike, bypassing the usual debuff PvP
            // gate. GroupOf/NotGroupOf debuffs still require permit_pvp.
            let indiscriminate = matches!(aura.target, AuraTarget::All);
            conditions_held
                && (kind.is_buff() || allow_friendly_fire || indiscriminate || permit_pvp())
        },
        // Selection (who's eligible, capped nearest-N) was already resolved
        // in `Sys::run` before this target was allowed to reach here.
        AuraKind::FriendlyFire | AuraKind::TieredHealthEffect { .. } => true,
        AuraKind::ForcePvP => {
            // Only apply this aura to players
            read_data.players.contains(target)
        },
    };

    if !should_activate {
        return false;
    }

    // TODO: When more aura kinds (besides Buff) are
    // implemented, match on them here
    match aura.aura_kind {
        AuraKind::Buff {
            kind,
            data,
            ref category,
            source,
            pool_split: _,
        } => apply_buff_aura(
            kind,
            data,
            category,
            source,
            key,
            applier,
            applier_uid,
            target,
            target_buffs,
            read_data,
            emitters,
        ),
        AuraKind::TieredHealthEffect { ref tiers, .. } => apply_tiered_health_effect(
            tiers,
            health,
            applier,
            applier_uid,
            target,
            read_data,
            emitters,
        ),
        // No implementation needed for these auras
        AuraKind::FriendlyFire | AuraKind::ForcePvP => {},
    }

    true
}

/// Adds the aura's buff to `target` unless an equal-or-stronger instance from
/// this same aura is already active. Split out of `activate_aura` to keep
/// that function under clippy's line-count lint.
fn apply_buff_aura(
    kind: BuffKind,
    data: common::comp::buff::BuffData,
    category: &Option<BuffCategory>,
    source: BuffSource,
    key: AuraKey,
    applier: EcsEntity,
    applier_uid: Uid,
    target: EcsEntity,
    target_buffs: &Buffs,
    read_data: &ReadData,
    emitters: &mut impl EmitExt<BuffEvent>,
) {
    // Checks that target is not already receiving a buff from an aura, where
    // the buff is of the same kind, and is of at least the same strength and
    // of at least the same duration. If no such buff is present, adds the
    // buff.
    // Bake the aura applier's heal_power into healing aura strength (same
    // approach combat.rs uses for instant heals) BEFORE the dedup check, so
    // the check compares like-for-like scaled strengths and doesn't re-emit
    // every tick when heal_power < 1.0. Non-healing buffs are untouched.
    // `data`/`BuffData` is Copy, so this reads the aura's base each tick — no
    // compounding.
    let mut data = data;
    if kind.is_heal() {
        let hp = read_data.stats.get(applier).map_or(1.0, |s| s.heal_power);
        data.strength *= hp;
    }
    let emit_buff = !target_buffs.buffs.iter().any(|(_, buff)| {
        buff.cat_ids
            .iter()
            .any(|cat_id| matches!(cat_id, BuffCategory::FromActiveAura(uid, aura_key) if *aura_key == key && *uid == applier_uid))
            && buff.kind == kind
            && buff.data.strength >= data.strength
    });
    if !emit_buff {
        return;
    }
    let dest_info = DestInfo {
        stats: read_data.stats.get(target),
        mass: read_data.masses.get(target),
    };
    let buff_cats = {
        let mut vec = vec![BuffCategory::FromActiveAura(applier_uid, key)];
        if let Some(cat) = category {
            vec.push(cat.clone());
        }
        vec
    };
    emitters.emit(BuffEvent {
        entity: target,
        buff_change: BuffChange::Add(Buff::new(
            kind,
            data,
            buff_cats,
            source,
            *read_data.time,
            dest_info,
            read_data.masses.get(applier),
            // Auras, after the initial creation, do not have a specific target that an
            // ability is designating
            None,
        )),
    });
}

/// Resolves the single worst tier `target`'s own current health qualifies
/// for and applies its effect. Split out of `activate_aura` to keep that
/// function under clippy's line-count lint.
fn apply_tiered_health_effect(
    tiers: &[combat::HealthTier],
    health: &Health,
    applier: EcsEntity,
    applier_uid: Uid,
    target: EcsEntity,
    read_data: &ReadData,
    emitters: &mut (impl EmitExt<BuffEvent> + EmitExt<HealthChangeEvent>),
) {
    let Some(tier) = tiers
        .iter()
        .find(|t| health.current() <= t.max_current_health)
    else {
        return;
    };
    match &tier.effect {
        TierEffect::Buff(b) => {
            if rand::random::<f32>() < b.chance {
                emitters.emit(BuffEvent {
                    entity: target,
                    buff_change: BuffChange::Add(b.to_buff(
                        *read_data.time,
                        (Some(applier_uid), read_data.masses.get(applier)),
                        (read_data.stats.get(target), read_data.masses.get(target)),
                        0.0,
                        1.0,
                        None,
                    )),
                });
            }
        },
        // Semantic difference from the attack pipeline: there is no
        // underlying attack damage to multiply here, so this is a flat
        // health change, not a multiplier.
        TierEffect::AdditionalDamage(amount) => {
            emitters.emit(HealthChangeEvent {
                entity: target,
                change: HealthChange {
                    amount: -amount,
                    by: Some(combat::DamageContributor::Solo(applier_uid)),
                    cause: Some(DamageSource::Other),
                    time: *read_data.time,
                    precise: false,
                    instance: rand::random(),
                },
            });
        },
    }
}
