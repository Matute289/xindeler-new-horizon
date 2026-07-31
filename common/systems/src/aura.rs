use std::collections::HashSet;

use common::{
    combat::{self, DamageSource, TierEffect},
    comp::{
        Alignment, Aura, Auras, Banished, BuffKind, Buffs, CharacterClass, CharacterState, Health,
        HealthChange, Mass, Player, Pos, Stats,
        aura::{AuraChange, AuraKey, AuraKind, AuraTarget, EnteredAuras},
        buff::{Buff, BuffCategory, BuffChange, BuffData, BuffSource, DestInfo},
        group::Group,
    },
    event::{AuraEvent, BanishEvent, BuffEvent, EmitExt, HealthChangeEvent},
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
        banish: BanishEvent,
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
    banished: ReadStorage<'a, Banished>,
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
                // Both pool-split and tiered-health-effect auras already had
                // their final, capped target set resolved by
                // `nearest_eligible` above (`pool_split_strength`'s keys /
                // `tiered_health_targets`). For those, `target_iter` below
                // iterates that resolved set directly instead of rescanning
                // `cached_spatial_grid` and re-running `eligible()` against
                // every entity in radius a second time, only to discard
                // everything outside the set. A plain `Buff` (no
                // `pool_split`), `FriendlyFire` or `ForcePvP` aura has no such
                // pre-resolved set, so it still does the one full-grid scan.
                let has_resolved_target_set = has_pool_split || has_tiered_health_effect;

                // Builds the (target_pos, health, target_uid, entered_auras,
                // target_buffs) tuple for one candidate and, if it still
                // qualifies, activates the aura against it.
                //
                // `needs_eligibility_check` is false when `target` came from
                // the already-resolved pool-split/tiered-health-effect set:
                // `nearest_eligible` ran `eligible()` for every one of those
                // candidates once already while building that set.
                let mut process_target = |target: specs::Entity, needs_eligibility_check: bool| {
                    let Some(target_pos) = read_data.positions.get(target) else {
                        return;
                    };
                    let Some(health) = read_data.healths.get(target) else {
                        return;
                    };
                    let Some(target_uid) = read_data.uids.get(target) else {
                        return;
                    };
                    let Some(entered_auras) = read_data.entered_auras.get(target) else {
                        return;
                    };
                    let Some(target_buffs) = read_data.buffs.get(target) else {
                        return;
                    };

                    if needs_eligibility_check && !eligible(target, target_pos, target_uid) {
                        return;
                    }

                    let allow_friendly_fire =
                        combat::allow_friendly_fire(&read_data.entered_auras, entity, target);

                    // A pool-split aura only ever activates at its computed
                    // per-target share -- never the aura's own (unused) flat
                    // strength. `None` for every other aura kind/target.
                    let strength_override = pool_split_strength.get(&target).copied();

                    let did_activate = activate_aura(
                        key,
                        aura,
                        entity,
                        *uid,
                        target,
                        health,
                        target_buffs,
                        allow_friendly_fire,
                        strength_override,
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
                };

                if has_resolved_target_set {
                    let resolved_targets: Vec<specs::Entity> = if has_pool_split {
                        pool_split_strength.keys().copied().collect()
                    } else {
                        tiered_health_targets.iter().copied().collect()
                    };
                    for target in resolved_targets {
                        process_target(target, false);
                    }
                } else {
                    for target in read_data
                        .cached_spatial_grid
                        .0
                        .in_circle_aabr(pos.0.xy(), aura.radius)
                    {
                        process_target(target, true);
                    }
                }
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
    // A pool-split aura's computed per-target share, overriding the aura's
    // own (unused) flat `BuffData::strength` -- `None` for every other aura
    // kind/target. Passed as a plain value instead of cloning the whole
    // `Aura` (which would deep-clone its `Box<PoolSplit>`) just to overwrite
    // one field.
    strength_override: Option<f32>,
    read_data: &ReadData,
    emitters: &mut (impl EmitExt<BuffEvent> + EmitExt<HealthChangeEvent> + EmitExt<BanishEvent>),
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
        } => {
            // Overwrite just the one field a pool-split activation needs
            // changed; `BuffData` is `Copy`, so this is a cheap struct-update,
            // not a clone of the surrounding `Aura`/`PoolSplit`.
            let data = match strength_override {
                Some(strength) => BuffData { strength, ..data },
                None => data,
            };
            apply_buff_aura(
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
            );
        },
        AuraKind::TieredHealthEffect {
            ref tiers,
            ref banishment,
            ..
        } => apply_tiered_health_effect(
            tiers,
            banishment.as_deref(),
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

/// Resolves the effect for `target`. A `CreatureKind` the aura's
/// `BanishmentEffect` applies to is resolved *exclusively* via banishment
/// and never enters the HP-tier ladder at all, at any HP: outcome is binary
/// — save fails → banished (below); save succeeds → `Outcome::Resisted`
/// (rolled server-side), no effect at all. Every other creature kind is
/// unaffected and still resolves the single worst tier `target`'s own
/// current health qualifies for, exactly as before. Split out of
/// `activate_aura` to keep that function under clippy's line-count lint.
fn apply_tiered_health_effect(
    tiers: &[combat::HealthTier],
    banishment: Option<&common::comp::aura::BanishmentEffect>,
    health: &Health,
    applier: EcsEntity,
    applier_uid: Uid,
    target: EcsEntity,
    read_data: &ReadData,
    emitters: &mut (impl EmitExt<BuffEvent> + EmitExt<HealthChangeEvent> + EmitExt<BanishEvent>),
) {
    if let Some(banishment) = banishment
        && banishment.applies_to(
            read_data
                .stats
                .get(target)
                .and_then(|stats| stats.creature_kind),
        )
    {
        // Already parked — a 0.5 s aura ticks more than once, and the
        // server-side handler is idempotent too, but not emitting is cheaper.
        // rtsim actors are NOT excluded: the rtsim-vs-mob fork is the server
        // handler's job, not the aura's.
        if !read_data.banished.contains(target) {
            emitters.emit(BanishEvent {
                entity: target,
                banished_by: applier_uid,
                min_return_hours: banishment.min_return_hours,
                max_return_hours: banishment.max_return_hours,
                reward_fraction: banishment.reward_fraction,
            });
        }
        // A banishable creature kind never falls through to the tier
        // ladder below — not even to be Resisted-and-then-tiered. The
        // banishment check is its only resolution path.
        return;
    }

    let Some(tier) = tiers
        .iter()
        .find(|t| health.current() <= t.max_current_health)
    else {
        return;
    };

    // A tier's optional `requirement` (currently only ever a
    // `CasterLevelRoll`, e.g. `power_word_divine_word`'s instant-death tier)
    // is a pass/fail gate on the *entire* tier, resolved with the same
    // roll `power_word_kill`/`pain`/`stun` already use. A failed roll is not
    // "fall through to the next tier" -- it is "no tier effect at all this
    // cast", matching those spells' own all-or-nothing result.
    if !tier.requirement_met(
        read_data.stats.get(applier).map(|s| s.character_level),
        read_data.character_classes.get(applier),
    ) {
        return;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{
            Body, Content, Stats,
            aura::{BanishmentEffect, PoolSplit},
            creature_type::CreatureKind,
            humanoid,
        },
        event::EventBus,
        uid::IdMaps,
    };
    use specs::{Builder, WorldExt};
    use std::num::NonZeroU64;
    use vek::{Vec2, Vec3};

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    /// A minimal aura target: positioned, with just enough components for
    /// `process_target` to reach its eligibility/activation checks (`Health`,
    /// `Buffs`, `EnteredAuras`), placed at `x` on the x-axis so distance from
    /// a caster at the origin is simply `x`.
    fn make_target(world: &mut specs::World, body: Body, target_uid: Uid, x: f32) -> EcsEntity {
        world
            .create_entity()
            .with(target_uid)
            .with(Pos(Vec3::new(x, 0.0, 0.0)))
            .with(Health::new(body))
            .with(Buffs::default())
            .with(EnteredAuras::default())
            .build()
    }

    /// Registers every storage/resource `ReadData` needs to `fetch`
    /// successfully, mirroring `server/src/sys/remote_sense.rs`'s
    /// `setup_world` idiom.
    fn setup_world() -> specs::World {
        let mut world = specs::World::new();
        world.register::<Player>();
        world.register::<Pos>();
        world.register::<CharacterState>();
        world.register::<Alignment>();
        world.register::<Health>();
        world.register::<Group>();
        world.register::<Uid>();
        world.register::<Stats>();
        world.register::<Buffs>();
        world.register::<Auras>();
        world.register::<EnteredAuras>();
        world.register::<Mass>();
        world.register::<CharacterClass>();
        world.register::<Banished>();
        world.insert(Time(0.0));
        world.insert(IdMaps::default());
        world.insert(common::CachedSpatialGrid::default());
        world.insert(EventBus::<AuraEvent>::default());
        world.insert(EventBus::<BuffEvent>::default());
        world.insert(EventBus::<HealthChangeEvent>::default());
        world.insert(EventBus::<BanishEvent>::default());
        world
    }

    /// A synthetic tier-4 fixture shaped like the lethal
    /// `AdditionalDamage` tier Divine Word's RON is expected to eventually
    /// carry -- not (yet) the shipped asset, which currently authors only
    /// its first 3 tiers and no `banishment` block. The lethal tier goes
    /// first per `HealthTier`'s first-match doc comment.
    fn divine_word_tiers() -> Vec<combat::HealthTier> {
        vec![combat::HealthTier {
            max_current_health: 35.0,
            effect: TierEffect::AdditionalDamage(2000.0),
            requirement: None,
        }]
    }

    fn divine_word_banishment() -> BanishmentEffect {
        BanishmentEffect {
            creature_kinds: vec![
                CreatureKind::Celestial,
                CreatureKind::Elemental,
                CreatureKind::Fey,
                CreatureKind::Fiend,
            ],
            min_return_hours: 24.0,
            max_return_hours: 168.0,
            reward_fraction: 0.25,
        }
    }

    /// A banishable creature kind landing in the tier-4 threshold must be
    /// resolved *exclusively* via banishment -- never also (or instead) via
    /// the tier ladder's instant-death `AdditionalDamage`.
    #[test]
    fn a_banishable_creature_kind_at_lethal_health_is_banished_not_killed() {
        let mut world = setup_world();

        let applier_uid = uid(1);
        let target_uid = uid(2);

        let applier = world.create_entity().with(applier_uid).build();

        let body = Body::Humanoid(humanoid::Body::random());
        let mut stats = Stats::new(Content::dummy(), body);
        // A reskin: the body's own `creature_kind()` is irrelevant here, only
        // the (overridable) `Stats::creature_kind` the aura reads.
        stats.creature_kind = Some(CreatureKind::Fiend);
        let target = world.create_entity().with(target_uid).with(stats).build();

        let mut health = Health::new(body);
        // Exactly the tier-4 threshold: if the tier ladder were consulted at
        // all, this is the HP value that would trigger instant death.
        health.set_amount(35.0);

        let tiers = divine_word_tiers();
        let banishment = divine_word_banishment();

        let read_data = ReadData::fetch(&world);
        let mut emitters = read_data.events.get_emitters();

        apply_tiered_health_effect(
            &tiers,
            Some(&banishment),
            &health,
            applier,
            applier_uid,
            target,
            &read_data,
            &mut emitters,
        );
        drop(emitters);

        let banish_events: Vec<_> = world
            .read_resource::<EventBus<BanishEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            banish_events.len(),
            1,
            "a banishable creature kind must be banished"
        );
        assert_eq!(banish_events[0].entity, target);

        let health_change_events = world
            .read_resource::<EventBus<HealthChangeEvent>>()
            .recv_all()
            .count();
        assert_eq!(
            health_change_events, 0,
            "a banishable creature kind must never resolve the tier ladder's lethal \
             AdditionalDamage -- it is exclusively banished, not also killed"
        );

        let buff_events = world
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .count();
        assert_eq!(
            buff_events, 0,
            "a banishable creature kind must never resolve any tier effect at all"
        );
    }

    /// Non-banishable creature kinds are unaffected by the fix: they still
    /// evaluate against the tier ladder exactly as before.
    #[test]
    fn a_non_banishable_creature_kind_at_lethal_health_still_resolves_the_tier() {
        let mut world = setup_world();

        let applier_uid = uid(1);
        let target_uid = uid(2);

        let applier = world.create_entity().with(applier_uid).build();

        let body = Body::Humanoid(humanoid::Body::random());
        let mut stats = Stats::new(Content::dummy(), body);
        stats.creature_kind = Some(CreatureKind::Beast);
        let target = world.create_entity().with(target_uid).with(stats).build();

        let mut health = Health::new(body);
        health.set_amount(35.0);

        let tiers = divine_word_tiers();
        let banishment = divine_word_banishment();

        let read_data = ReadData::fetch(&world);
        let mut emitters = read_data.events.get_emitters();

        apply_tiered_health_effect(
            &tiers,
            Some(&banishment),
            &health,
            applier,
            applier_uid,
            target,
            &read_data,
            &mut emitters,
        );
        drop(emitters);

        let banish_events = world
            .read_resource::<EventBus<BanishEvent>>()
            .recv_all()
            .count();
        assert_eq!(
            banish_events, 0,
            "a non-banishable creature kind must never be banished"
        );

        let health_change_events: Vec<_> = world
            .read_resource::<EventBus<HealthChangeEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            health_change_events.len(),
            1,
            "a non-banishable creature kind must still resolve the tier ladder"
        );
        assert_eq!(health_change_events[0].change.amount, -2000.0);
    }

    /// Tier 4's instant death is gated by a `CasterLevelRoll`, the same
    /// all-or-nothing margin `power_word_kill`/`pain`/`stun` already give
    /// their own permanent results -- unlike the target-side saving throw
    /// the banishment path resolves server-side, this is a *caster*-side
    /// check, resolved right here in the aura tick. A failed roll must not
    /// merely weaken the effect or fall through to a lower tier -- it must
    /// produce no tier effect at all this cast.
    #[test]
    fn divine_word_tier4_death_is_gated_by_caster_level_roll() {
        use common::comp::class::ClassKind;

        let mut world = setup_world();

        let applier_uid = uid(1);
        let target_uid = uid(2);

        let body = Body::Humanoid(humanoid::Body::random());
        let mut applier_stats = Stats::new(Content::dummy(), body);
        applier_stats.character_level = 60;
        let applier = world
            .create_entity()
            .with(applier_uid)
            .with(applier_stats)
            .with(CharacterClass::single(ClassKind::Cleric))
            .build();

        let mut target_stats = Stats::new(Content::dummy(), body);
        target_stats.creature_kind = Some(CreatureKind::Beast);
        let target = world
            .create_entity()
            .with(target_uid)
            .with(target_stats)
            .build();

        let mut health = Health::new(body);
        health.set_amount(35.0);

        let never_passes = vec![combat::HealthTier {
            max_current_health: 35.0,
            effect: TierEffect::AdditionalDamage(2000.0),
            requirement: Some(combat::CombatRequirement::CasterLevelRoll(Box::new(
                combat::CasterLevelFailChance {
                    unlock_level: 42,
                    fail_chance_at_unlock: 1.0,
                    fail_chance_at_max_level: 1.0,
                    source_classes: vec![ClassKind::Cleric],
                },
            ))),
        }];

        {
            let read_data = ReadData::fetch(&world);
            let mut emitters = read_data.events.get_emitters();
            apply_tiered_health_effect(
                &never_passes,
                None,
                &health,
                applier,
                applier_uid,
                target,
                &read_data,
                &mut emitters,
            );
        }
        assert_eq!(
            world
                .read_resource::<EventBus<HealthChangeEvent>>()
                .recv_all()
                .count(),
            0,
            "a failed CasterLevelRoll must not apply tier 4's AdditionalDamage"
        );

        let always_passes = vec![combat::HealthTier {
            max_current_health: 35.0,
            effect: TierEffect::AdditionalDamage(2000.0),
            requirement: Some(combat::CombatRequirement::CasterLevelRoll(Box::new(
                combat::CasterLevelFailChance {
                    unlock_level: 42,
                    fail_chance_at_unlock: 0.0,
                    fail_chance_at_max_level: 0.0,
                    source_classes: vec![ClassKind::Cleric],
                },
            ))),
        }];

        {
            let read_data = ReadData::fetch(&world);
            let mut emitters = read_data.events.get_emitters();
            apply_tiered_health_effect(
                &always_passes,
                None,
                &health,
                applier,
                applier_uid,
                target,
                &read_data,
                &mut emitters,
            );
        }
        let health_change_events: Vec<_> = world
            .read_resource::<EventBus<HealthChangeEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            health_change_events.len(),
            1,
            "a passed CasterLevelRoll must apply tier 4's AdditionalDamage"
        );
        assert_eq!(health_change_events[0].change.amount, -2000.0);
    }

    // ── Cleanup pass: `nearest_eligible`'s resolved set drives target_iter
    // directly for pool-split/tiered-health-effect auras, instead of
    // `target_iter` rescanning `cached_spatial_grid` a second time. These
    // two tests run the real `Sys::run` (not just the inner helper
    // functions above) to pin the *observable* result of that selection --
    // who gets activated, and at what strength -- so the scan-avoidance
    // refactor can't silently change it. ──────────────────────────────────

    /// A pool-split aura must activate only the capped nearest-N targets
    /// `nearest_eligible` resolved, each at its even share of the pool --
    /// never every entity found in radius, and never the aura's own (unused)
    /// flat strength.
    #[test]
    fn pool_split_aura_activates_only_the_capped_nearest_targets_at_their_even_share() {
        let mut world = setup_world();

        let caster_uid = uid(1);
        let body = Body::Humanoid(humanoid::Body::random());

        // Three candidates within the aura's radius, at increasing distance.
        // `max_targets` below caps the pool to the nearest two, so the
        // farthest must be excluded entirely -- not merely under-shared.
        let near = make_target(&mut world, body, uid(2), 10.0);
        let mid = make_target(&mut world, body, uid(3), 20.0);
        let far = make_target(&mut world, body, uid(4), 30.0);

        {
            let mut grid = world.write_resource::<common::CachedSpatialGrid>();
            for (entity, x) in [(near, 10.0_f32), (mid, 20.0), (far, 30.0)] {
                grid.0.insert(Vec2::new(x as i32, 0), 0, entity);
            }
        }

        let pool_split = PoolSplit {
            unlock_level: 0,
            value_at_unlock: 60.0,
            value_at_max_level: 60.0,
            source_classes: vec![],
            max_targets: 2,
        };
        let aura = Aura::new(
            AuraKind::Buff {
                kind: BuffKind::Shielded,
                data: BuffData {
                    strength: 0.0,
                    duration: None,
                    delay: None,
                    secondary_duration: None,
                    misc_data: None,
                },
                category: None,
                source: BuffSource::Character {
                    by: caster_uid,
                    tool_kind: None,
                },
                pool_split: Some(Box::new(pool_split)),
            },
            50.0,
            None,
            AuraTarget::All,
            Time(0.0),
            None,
        );
        let mut auras = Auras::default();
        auras.insert(aura);

        world
            .create_entity()
            .with(caster_uid)
            .with(Pos(Vec3::new(0.0, 0.0, 0.0)))
            .with(auras)
            .build();

        let mut job = Job::<Sys>::default();
        let read_data = ReadData::fetch(&world);
        Sys::run(&mut job, read_data);

        let buff_events: Vec<_> = world
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            buff_events.len(),
            2,
            "only the capped nearest-2 targets must be activated, not all 3 in radius"
        );
        let targeted: std::collections::HashSet<_> =
            buff_events.iter().map(|ev| ev.entity).collect();
        assert!(targeted.contains(&near) && targeted.contains(&mid));
        assert!(
            !targeted.contains(&far),
            "the third-nearest target is outside max_targets and must be excluded entirely, not \
             merely given a smaller share"
        );
        for ev in &buff_events {
            let BuffChange::Add(buff) = &ev.buff_change else {
                panic!("expected BuffChange::Add");
            };
            assert_eq!(
                buff.data.strength, 30.0,
                "the pool must be split evenly (60.0 / 2 resolved targets) across the resolved \
                 targets, never the aura's own flat (and unused) strength"
            );
        }
    }

    /// The scan-avoidance shortcut is reserved for pool-split /
    /// tiered-health-effect auras: a plain `Buff` aura with no `pool_split`
    /// has no pre-resolved target set, so it must still take the full
    /// `cached_spatial_grid` scan and activate every eligible target found in
    /// radius -- unaffected by that refactor.
    #[test]
    fn a_plain_buff_aura_without_pool_split_still_activates_every_eligible_target_in_radius() {
        let mut world = setup_world();

        let caster_uid = uid(1);
        let body = Body::Humanoid(humanoid::Body::random());

        let target_a = make_target(&mut world, body, uid(2), 10.0);
        let target_b = make_target(&mut world, body, uid(3), 20.0);
        let target_c = make_target(&mut world, body, uid(4), 30.0);

        {
            let mut grid = world.write_resource::<common::CachedSpatialGrid>();
            for (entity, x) in [(target_a, 10.0_f32), (target_b, 20.0), (target_c, 30.0)] {
                grid.0.insert(Vec2::new(x as i32, 0), 0, entity);
            }
        }

        let aura = Aura::new(
            AuraKind::Buff {
                kind: BuffKind::Shielded,
                data: BuffData {
                    strength: 5.0,
                    duration: None,
                    delay: None,
                    secondary_duration: None,
                    misc_data: None,
                },
                category: None,
                source: BuffSource::Character {
                    by: caster_uid,
                    tool_kind: None,
                },
                pool_split: None,
            },
            50.0,
            None,
            AuraTarget::All,
            Time(0.0),
            None,
        );
        let mut auras = Auras::default();
        auras.insert(aura);

        world
            .create_entity()
            .with(caster_uid)
            .with(Pos(Vec3::new(0.0, 0.0, 0.0)))
            .with(auras)
            .build();

        let mut job = Job::<Sys>::default();
        let read_data = ReadData::fetch(&world);
        Sys::run(&mut job, read_data);

        let buff_events: Vec<_> = world
            .read_resource::<EventBus<BuffEvent>>()
            .recv_all()
            .collect();
        assert_eq!(
            buff_events.len(),
            3,
            "the full-grid-scan path (no pool_split) must still activate every eligible target in \
             radius, unaffected by the pool-split/tiered-health-effect target-selection shortcut"
        );
        let targeted: std::collections::HashSet<_> =
            buff_events.iter().map(|ev| ev.entity).collect();
        assert!(
            targeted.contains(&target_a)
                && targeted.contains(&target_b)
                && targeted.contains(&target_c)
        );
        for ev in &buff_events {
            let BuffChange::Add(buff) = &ev.buff_change else {
                panic!("expected BuffChange::Add");
            };
            assert_eq!(buff.data.strength, 5.0);
        }
    }
}
