#[cfg(feature = "worldgen")]
use crate::rtsim::RtSim;
use crate::{
    Server, Settings, SpawnPoint,
    client::Client,
    comp::{
        BuffKind, BuffSource, PhysicsState,
        agent::{Agent, AgentEvent, Sound, SoundKind},
        loot_owner::LootOwner,
        skillset::SkillGroupKind,
    },
    error,
    events::entity_creation::handle_create_npc,
    metrics::GameplayMetrics,
    persistence::character_updater::CharacterUpdater,
    pet::tame_pet,
    state_ext::StateExt,
    sys::terrain::{NpcData, SAFE_ZONE_RADIUS, SpawnEntityData},
};
#[cfg(feature = "worldgen")] use common::rtsim;
use common::{
    CachedSpatialGrid, Damage, DamageKind, DamageSource, GroupTarget, RadiusEffect,
    assets::{AssetExt, Ron},
    combat::{
        self, AttackSource, BASE_PARRIED_POISE_PUNISHMENT, CombatEffect, DamageContributor,
        DeathEffects, StatEffect, StatEffectTarget,
    },
    comp::{
        self, Alignment, Auras, BASE_ABILITY_LIMIT, Body, BuffCategory, BuffEffect, CharacterClass,
        CharacterState, Energy, Group, Hardcore, Health, HealthChange, Inventory, Object,
        PhantomIllusion, PickupItem, Player, Poise, PoiseChange, Pos, Presence, PresenceKind,
        ProjectileConstructor, Skill, SkillSet, SpellMastery, Stats,
        ability::{Dodgeable, MagicSource},
        aura::{self, EnteredAuras},
        buff,
        chat::{KillSource, KillType},
        inventory::item::{AbilityMap, MaterialStatManifest},
        item::flatten_counted_items,
        loot_owner::{LootOwnerKind, ONWERSHIP_TIMEOUT_SLOW},
        projectile::{ProjectileAttack, ProjectileConstructorKind, ProjectileExplosionTarget},
        skills::MageSkill,
        spell_mastery::{
            NON_DAMAGE_WEIGHT, POLYGLOT_BONUS_PER_RANK, grant_source_mastery, level_delta_weight,
        },
    },
    consts::TELEPORTER_RADIUS,
    event::{
        AuraEvent, BanishEvent, BonkEvent, BuffEvent, ChangeAbilityEvent, ChangeBodyEvent,
        ChangeStanceEvent, ChatEvent, ComboChangeEvent, CreateItemDropEvent, CreateNpcEvent,
        CreateObjectEvent, DeleteEvent, DestroyEvent, DispelIllusionEvent, DownedEvent, EmitExt,
        Emitter, EnergyChangeEvent, EntityAttackedHookEvent, EventBus, ExplosionEvent,
        HealthChangeEvent, HelpDownedEvent, KillEvent, KnockbackEvent, LandOnGroundEvent,
        MakeAdminEvent, ParryHookEvent, PermanentChange, PoiseChangeEvent, RegrowHeadEvent,
        RemoveLightEmitterEvent, ResolveIdentifyEvent, ResolveRemoteSenseEvent, RespawnEvent,
        SetAbilityCooldownEvent, ShootEvent, SoundEvent, StartInteractionEvent,
        StartTeleportingEvent, TeleportToEvent, TeleportToPositionEvent, TranscribeSpellEvent,
        TransformEvent, UpdateMapMarkerEvent,
    },
    event_emitters,
    explosion::{ColorPreset, TerrainReplacementPreset},
    generation::{EntityConfig, EntityInfo},
    link::Is,
    lottery::distribute_many,
    mounting::{Mounting, Rider, VolumeRider},
    npc::NPC_NAMES,
    outcome::{HealthChangeInfo, Outcome},
    resources::{EntitiesDiedLastTick, ProgramTime, Secs, Time},
    spiral::Spiral2d,
    states::utils::StageSection,
    terrain::{Block, BlockKind, TerrainGrid},
    trade::{TradeResult, Trades},
    uid::{IdMaps, Uid},
    util::Dir,
    vol::ReadVol,
};
use common_net::{msg::ServerGeneral, sync::WorldSyncExt, synced_components::Heads};
use common_state::{AreasContainer, BlockChange, NoDurabilityArea, ScheduledBlockChange};
use hashbrown::HashSet;
use rand::RngExt;
use specs::{
    DispatcherBuilder, Entities, Entity as EcsEntity, Entity, Join, LendJoin, Read, ReadExpect,
    ReadStorage, SystemData, WorldExt, Write, WriteExpect, WriteStorage, shred,
};
#[cfg(feature = "worldgen")] use std::sync::Arc;
use std::{borrow::Cow, collections::HashMap, f32::consts::PI, iter, time::Duration};
use tracing::{debug, warn};
use vek::{Rgb, Vec2, Vec3};
#[cfg(feature = "worldgen")]
use world::{IndexOwned, World};

use super::{ServerEvent, event_dispatch, event_sys_name};

pub(super) fn register_event_systems(builder: &mut DispatcherBuilder) {
    event_dispatch::<PoiseChangeEvent>(builder, &[]);
    event_dispatch::<HealthChangeEvent>(builder, &[]);
    event_dispatch::<KillEvent>(builder, &[]);
    event_dispatch::<HelpDownedEvent>(builder, &[]);
    event_dispatch::<DownedEvent>(builder, &[&event_sys_name::<HealthChangeEvent>()]);
    event_dispatch::<KnockbackEvent>(builder, &[]);
    // 🔴 Ordering is load-bearing, in both directions.
    //
    // *After* every handler that writes `Health`. `BanishEvent` only reads it,
    // so `shred` already refuses to run them concurrently — but which side it
    // ran *first* was left to the scheduler, and the answer decided whether a
    // creature banished and killed in the same tick left an orphaned registry
    // record behind. With the edges, all of this tick's damage has landed
    // before the saving throw is rolled, so `death_forestalls_banishment` sees
    // the true health and a doomed creature is simply never banished. Costs no
    // throughput: these were serialised against each other either way.
    //
    // `KillEvent` is unreachable for a banishable target today — it comes only
    // from `ControlEvent::GiveUp`, which is gated on death protection, which is
    // `Body::Humanoid`-only, and a humanoid is never a banishable
    // `CreatureKind`. The edge is declared anyway rather than left as an
    // argument the next reader has to re-derive.
    //
    // *Before* `DestroyEvent`: this handler raises the banishment's
    // `DestroyEvent`, and that `DestroyEvent` must be consumed in the *same*
    // tick. Otherwise `banishment::maintain` parks the entity (removing `Pos`)
    // before the reward/loot block ever sees where to drop the loot.
    event_dispatch::<BanishEvent>(builder, &[
        &event_sys_name::<HealthChangeEvent>(),
        &event_sys_name::<KillEvent>(),
    ]);
    event_dispatch::<DestroyEvent>(builder, &[
        &event_sys_name::<HealthChangeEvent>(),
        &event_sys_name::<BanishEvent>(),
    ]);
    event_dispatch::<LandOnGroundEvent>(builder, &[]);
    event_dispatch::<RespawnEvent>(builder, &[]);
    event_dispatch::<ExplosionEvent>(builder, &[]);
    event_dispatch::<BonkEvent>(builder, &[]);
    event_dispatch::<AuraEvent>(builder, &[]);
    event_dispatch::<BuffEvent>(builder, &[&event_sys_name::<DownedEvent>()]);
    event_dispatch::<EnergyChangeEvent>(builder, &[]);
    event_dispatch::<ComboChangeEvent>(builder, &[]);
    event_dispatch::<ParryHookEvent>(builder, &[]);
    event_dispatch::<TeleportToEvent>(builder, &[]);
    event_dispatch::<SetAbilityCooldownEvent>(builder, &[]);
    event_dispatch::<EntityAttackedHookEvent>(builder, &[]);
    event_dispatch::<ChangeAbilityEvent>(builder, &[]);
    event_dispatch::<UpdateMapMarkerEvent>(builder, &[]);
    event_dispatch::<MakeAdminEvent>(builder, &[]);
    event_dispatch::<ChangeStanceEvent>(builder, &[]);
    event_dispatch::<ChangeBodyEvent>(builder, &[]);
    event_dispatch::<RemoveLightEmitterEvent>(builder, &[]);
    event_dispatch::<TeleportToPositionEvent>(builder, &[]);
    event_dispatch::<StartTeleportingEvent>(builder, &[]);
    event_dispatch::<RegrowHeadEvent>(builder, &[]);
    event_dispatch::<TranscribeSpellEvent>(builder, &[]);
    // *After* `BuffEvent`: `server/src/events/remote_sense.rs`'s
    // `teardown_existing_anchor` (run from this handler, for a recast that
    // supersedes an existing remote-sensing link) deliberately does not
    // force-remove the superseded `BuffKind::RemoteSensing` buff, reasoning
    // that the same tick's own `BuffEvent::Add` (emitted alongside this
    // event by `common/src/states/self_buff.rs`) has *already* landed by
    // the time this handler runs, so re-adding a removal here would only
    // apply next tick and could kill the brand-new buff instead of the
    // stale one. That reasoning is only true if `EventHandler<BuffEvent>`
    // really does run first -- both handlers write-conflict on `Buffs`
    // (`BuffEventData`/`ResolveRemoteSenseEventData`), so specs serialises
    // them either way, but without this declared edge the *order* was only
    // an insertion-order tie-break, exactly the hazard
    // `common/systems/src/lib.rs`'s `pilot::Sys` → `interpolation::Sys`
    // dependency (same PR) hardens elsewhere. This edge makes it a
    // compiler-checked constraint instead.
    event_dispatch::<ResolveRemoteSenseEvent>(builder, &[&event_sys_name::<BuffEvent>()]);
    // *After* `BuffEvent` for the same reason as `ResolveRemoteSenseEvent`
    // just above: `server/src/events/identify.rs`'s handler only needs the
    // caster/target's already-current components, but ordering it after the
    // buff add keeps the two `Identifying`/`RemoteSensing` cast-resolution
    // events on the same, compiler-checked footing rather than an
    // insertion-order coincidence.
    event_dispatch::<ResolveIdentifyEvent>(builder, &[&event_sys_name::<BuffEvent>()]);
}

event_emitters! {
    struct ReadExplosionEvents[ExplosionEmitters] {
        health_change: HealthChangeEvent,
        energy_change: EnergyChangeEvent,
        poise_change: PoiseChangeEvent,
        sound: SoundEvent,
        parry_hook: ParryHookEvent,
        knockback: KnockbackEvent,
        entity_attack_hook: EntityAttackedHookEvent,
        combo_change: ComboChangeEvent,
        buff: BuffEvent,
        bonk: BonkEvent,
        change_body: ChangeBodyEvent,
        outcome: Outcome,
        stance: ChangeStanceEvent,
        transform: TransformEvent,
        dispel_illusion: DispelIllusionEvent,
    }

    struct ReadEntityAttackedHookEvents[EntityAttackedHookEmitters] {
        buff: BuffEvent,
        combo_change: ComboChangeEvent,
        knockback: KnockbackEvent,
        energy_change: EnergyChangeEvent,
        transform: TransformEvent,
        health_change: HealthChangeEvent,
        poise_change: PoiseChangeEvent,
    }

    struct HealthChangeEvents[HealthChangeEmitters] {
        destroy: DestroyEvent,
        downed: DownedEvent,
        outcome: Outcome,
        buff: BuffEvent,
    }

    struct DestroyEvents[DestroyEmitters] {
        chat: ChatEvent,
        create_item_drop: CreateItemDropEvent,
        delete: DeleteEvent,
        buff: BuffEvent,
        transform: TransformEvent,
        energy_change: EnergyChangeEvent,
        health_change: HealthChangeEvent,
        combo_change: ComboChangeEvent,
        poise_change: PoiseChangeEvent,
        knockback: KnockbackEvent,
    }
}

pub fn handle_delete(server: &mut Server, DeleteEvent(entity): DeleteEvent) {
    // N27-O: release this entity's Cadena (`PactBoon::Chain`) summon-pool
    // charge, if it has one, before it is torn down. `DeleteEvent` is the
    // single funnel every summon exit route already shares -- creature
    // death (`DestroyEvent`'s handler emits it), lifetime expiry
    // (`common_systems::projectile`'s `time_left == ZERO` emits it), and
    // dismiss (`DismissSummonEvent`'s handler emits it, see
    // `server::events::interaction`) -- so the ledger is decremented from
    // exactly one place no matter which route ended this entity's life,
    // never a second independently-driven timer. A non-summon deletion (the
    // overwhelming majority of calls here) finds no matching ledger entry
    // and is a harmless no-op.
    release_chain_summon_charge(server.state.ecs(), entity);

    let _ = server
        .state_mut()
        .delete_entity_recorded(entity)
        .map_err(|e| error!(?e, ?entity, "Failed to delete destroyed entity"));
}

/// See `handle_delete`'s doc comment. Takes `&specs::World` (not `&mut
/// Server`) purely so it is unit-testable without constructing a full
/// `Server` -- mirrors `server::pet::tame_pet`'s own `ecs: &specs::World`
/// shape for the same reason.
pub(super) fn release_chain_summon_charge(ecs: &specs::World, entity: EcsEntity) {
    let Some(summon_uid) = ecs.read_storage::<Uid>().get(entity).copied() else {
        return;
    };
    let owner_uid =
        ecs.read_storage::<Alignment>()
            .get(entity)
            .and_then(|alignment| match alignment {
                Alignment::Owned(owner_uid) => Some(*owner_uid),
                _ => None,
            });
    let Some(owner) = owner_uid.and_then(|owner_uid| ecs.entity_from_uid(owner_uid)) else {
        return;
    };
    if let Some(mut summons) = ecs.write_storage::<comp::Summons>().get_mut(owner) {
        summons.release(summon_uid);
    }
}

/// Pure: which live entities to dismiss for a dying or logged-out Cadena
/// Warlock, given their `Summons` ledger and a `Uid` resolver. Both the
/// owner-death path (this file's `DestroyEvent` handler, below) and the
/// owner-logout path (`server::events::player::dismiss_active_chain_summons`)
/// resolve the SAME way through this one function, then each emits
/// `DeleteEvent` for every entity it returns through whichever emitter their
/// own handler context provides -- kept pure (no `Server`/`SystemData`) so it
/// is fully unit-testable on its own.
pub(crate) fn summons_to_dismiss(
    summons: Option<&comp::Summons>,
    id_maps: &IdMaps,
) -> Vec<EcsEntity> {
    summons
        .into_iter()
        .flat_map(|summons| summons.active.iter())
        .filter_map(|(summon_uid, _cost)| id_maps.uid_entity(*summon_uid))
        .collect()
}

#[derive(Hash, Eq, PartialEq)]
enum DamageContrib {
    Solo(EcsEntity),
    Group(Group),
    NotFound,
}

impl ServerEvent for PoiseChangeEvent {
    type SystemData<'a> = (
        Entities<'a>,
        ReadStorage<'a, CharacterState>,
        WriteStorage<'a, Poise>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (entities, character_states, mut poises): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Some((character_state, mut poise)) = (&character_states, &mut poises)
                .lend_join()
                .get(ev.entity, &entities)
            {
                // Entity is invincible to poise change during stunned character state.
                if !matches!(character_state, CharacterState::Stunned(_)) {
                    poise.change(ev.change);
                }
            }
        }
    }
}

#[derive(SystemData)]
pub struct HealthChangeEventData<'a> {
    entities: Entities<'a>,
    #[cfg(feature = "worldgen")]
    rtsim: WriteExpect<'a, RtSim>,
    events: HealthChangeEvents<'a>,
    time: Read<'a, Time>,
    // Needed unconditionally (not just under `worldgen`) to resolve a heal's
    // caster `Uid` back to an entity for `SpellMastery` crediting.
    id_maps: Read<'a, IdMaps>,
    #[cfg(feature = "worldgen")]
    world: ReadExpect<'a, Arc<World>>,
    #[cfg(feature = "worldgen")]
    index: ReadExpect<'a, IndexOwned>,
    positions: ReadStorage<'a, Pos>,
    uids: ReadStorage<'a, Uid>,
    #[cfg(feature = "worldgen")]
    rtsim_actors: ReadStorage<'a, rtsim::ActorId>,
    /// The cached gear aggregates the invincibility check and the non-damage
    /// mastery credit both read, instead of re-walking the target's loadout
    /// per health change.
    derived_stats: ReadStorage<'a, comp::DerivedStats>,
    agents: WriteStorage<'a, Agent>,
    healths: WriteStorage<'a, Health>,
    heads: WriteStorage<'a, Heads>,
    /// The healer's Polyglot rank for non-damage mastery crediting (see the
    /// `changed && ev.change.amount > 0.0` block below).
    skill_sets: ReadStorage<'a, SkillSet>,
    spell_masteries: WriteStorage<'a, SpellMastery>,
}

impl ServerEvent for HealthChangeEvent {
    type SystemData<'a> = HealthChangeEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let mut emitters = data.events.get_emitters();
        let mut rng = rand::rng();
        for ev in events {
            if let Some((mut health, pos, uid, heads)) = (
                &mut data.healths,
                data.positions.maybe(),
                data.uids.maybe(),
                (&mut data.heads).maybe(),
            )
                .lend_join()
                .get(ev.entity, &data.entities)
            {
                // Skip damage if invincible. `None` protection indicates
                // invincibility; no cache at all means no `Inventory`, hence
                // no armour, hence never invincible.
                if ev.change.amount < 0.0
                    && data
                        .derived_stats
                        .get(ev.entity)
                        .is_some_and(|derived| derived.protection.is_none())
                {
                    continue;
                }

                // If the change amount was not zero
                // BL-05 RD-6: track temp-HP absorb depletion across this change so
                // the granting `Shielded` buff can be removed once the pool empties.
                let had_absorb = health.absorb() > 0.0;
                let changed = health.change_by(ev.change);
                if had_absorb && health.absorb() <= 0.0 {
                    emitters.emit(BuffEvent {
                        entity: ev.entity,
                        buff_change: buff::BuffChange::RemoveByKind(BuffKind::Shielded),
                    });
                }

                // Non-damage mastery credit: a heal (positive amount) that
                // actually changed the target's HP, tagged with a magic
                // source, from a resolvable caster who holds `SpellMastery`.
                // The heal counterpart to the buff crediting in `BuffEvent`'s
                // handler.
                if changed
                    && ev.change.amount > 0.0
                    && let Some(source) = ev.change.magic_source
                    && let Some(contributor) = ev.change.by
                {
                    let healer_uid = damage_contributor_uid(&contributor);
                    if let Some(target_uid) = uid.copied()
                        && let Some(healer_entity) = data.id_maps.uid_entity(healer_uid)
                        && let Some(mut mastery) = data.spell_masteries.get_mut(healer_entity)
                    {
                        let target_in_combat = health
                            .damaged_recently(ev.change.time, MASTERY_RECENT_COMBAT_WINDOW_SECS);
                        // Finding B: the target's gear/skill/body five-tuple
                        // existed only to re-fold this one number per landed
                        // heal. No cache means no `Inventory`, hence
                        // `DerivedStats::default()`'s rating, 0.0.
                        let target_combat_rating = data
                            .derived_stats
                            .get(ev.entity)
                            .map_or(0.0, |derived| derived.combat_rating);
                        let polyglot_rank = data.skill_sets.get(healer_entity).map_or(0, |ss| {
                            ss.skill_level(Skill::Mage(MageSkill::Polyglot))
                                .unwrap_or(0)
                        });
                        grant_non_damage_mastery(
                            &mut mastery,
                            source,
                            healer_uid,
                            target_uid,
                            target_in_combat,
                            // Every actual HP change is its own real event
                            // (there is no "refresh" concept for a heal the
                            // way there is for a buff instance), so this is
                            // always a fresh grant.
                            true,
                            target_combat_rating,
                            polyglot_rank,
                        );
                    }
                }

                if let Some(mut heads) = heads {
                    // We want some hp to be left for a headless body, so we divide by (max amount
                    // of heads + 2)
                    let hp_per_head = health.maximum() / (heads.capacity() as f32 + 2.0);
                    let target_heads = (health.current() / hp_per_head) as usize;
                    if heads.amount() > 0 && ev.change.amount < 0.0 && heads.amount() > target_heads
                    {
                        for _ in target_heads..heads.amount() {
                            if let Some(head) = heads.remove_one(&mut rng, *data.time) {
                                if let Some(uid) = uid {
                                    emitters.emit(Outcome::HeadLost { uid: *uid, head });
                                }
                            } else {
                                break;
                            }
                        }
                    }
                }

                #[cfg(feature = "worldgen")]
                if changed {
                    let entity_as_actor = |entity| data.rtsim_actors.get(entity).copied();
                    if let Some(actor) = entity_as_actor(ev.entity) {
                        let cause = ev
                            .change
                            .damage_by()
                            .map(|by| by.uid())
                            .and_then(|uid| data.id_maps.uid_entity(uid))
                            .and_then(entity_as_actor);
                        data.rtsim.hook_rtsim_actor_hp_change(
                            &data.world,
                            data.index.as_index_ref(),
                            actor,
                            cause,
                            health.fraction(),
                            ev.change.amount,
                        );
                    }
                }

                if let (Some(pos), Some(uid)) = (pos, uid)
                    && changed
                {
                    emitters.emit(Outcome::HealthChange {
                        pos: pos.0,
                        info: HealthChangeInfo {
                            amount: ev.change.amount,
                            by: ev.change.by,
                            target: *uid,
                            cause: ev.change.cause,
                            precise: ev.change.precise,
                            instance: ev.change.instance,
                        },
                    });
                }

                if ev.change.amount < 0.0 && changed {
                    let dmg = (-ev.change.amount) as u32;
                    let attacker = ev.change.by.map(|b| b.uid());
                    common::telemetry!(
                        "ch",
                        dst = ?uid.copied(),
                        dmg,
                        attacker = ?attacker,
                        hp_after = health.current() as u32
                    );
                }

                if !health.is_dead && health.should_die() {
                    if health.death_protection {
                        emitters.emit(DownedEvent { entity: ev.entity });
                    } else {
                        emitters.emit(DestroyEvent {
                            entity: ev.entity,
                            cause: ev.change,
                            removal: combat::RemovalInfo::killed(),
                        });
                    }
                }
            }

            // This if statement filters out anything under 5 damage, for DOT ticks
            // TODO: Find a better way to separate direct damage from DOT here
            let damage = -ev.change.amount;
            if damage > 5.0
                && let Some(agent) = data.agents.get_mut(ev.entity)
            {
                agent.inbox.push_back(AgentEvent::Hurt);
            }

            // Concentration (ENG-C2 / M5): an external hit at or above the break
            // threshold ends the bearer's concentration. Only hits with a
            // `DamageSource` cause count — self-inflicted costs (e.g. the
            // Hemomancy HP price, cause: None) do not break it.
            if ev.change.amount < 0.0
                && ev.change.cause.is_some()
                && buff::concentration_breaks(
                    damage,
                    data.healths.get(ev.entity).map_or(0.0, |h| h.maximum()),
                )
            {
                emitters.emit(BuffEvent {
                    entity: ev.entity,
                    buff_change: buff::BuffChange::RemoveByCategory {
                        all_required: vec![],
                        any_required: vec![BuffCategory::Concentration],
                        none_required: vec![],
                    },
                });
            }

            // BL-05 rider: any external attacking hit wakes a sleeping target
            // (no threshold). RemoveByKind is a no-op if the target isn't asleep.
            if ev.change.amount < 0.0 && ev.change.cause.is_some() {
                emitters.emit(BuffEvent {
                    entity: ev.entity,
                    buff_change: buff::BuffChange::RemoveByKind(BuffKind::Asleep),
                });
                // RestfulSleep (a voluntary nap granted to willing allies)
                // shares the same wake-on-damage rule as Asleep.
                emitters.emit(BuffEvent {
                    entity: ev.entity,
                    buff_change: buff::BuffChange::RemoveByKind(BuffKind::RestfulSleep),
                });
            }
        }
    }
}

impl ServerEvent for KillEvent {
    type SystemData<'a> = WriteStorage<'a, comp::Health>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut healths: Self::SystemData<'_>) {
        for ev in events {
            if let Some(mut health) = healths.get_mut(ev.entity) {
                health.kill();
            }
        }
    }
}

#[derive(SystemData)]
pub struct HelpDownedEventData<'a> {
    id_maps: Read<'a, IdMaps>,
    #[cfg(feature = "worldgen")]
    rtsim: WriteExpect<'a, RtSim>,
    #[cfg(feature = "worldgen")]
    world: ReadExpect<'a, Arc<World>>,
    #[cfg(feature = "worldgen")]
    index: ReadExpect<'a, IndexOwned>,
    #[cfg(feature = "worldgen")]
    rtsim_actors: ReadStorage<'a, rtsim::ActorId>,
    character_states: WriteStorage<'a, comp::CharacterState>,
    healths: WriteStorage<'a, comp::Health>,
}

impl ServerEvent for HelpDownedEvent {
    type SystemData<'a> = HelpDownedEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        for ev in events {
            if let Some(entity) = data.id_maps.uid_entity(ev.target) {
                if let Some(mut health) = data.healths.get_mut(entity) {
                    health.refresh_death_protection();
                }
                if let Some(mut character_state) = data.character_states.get_mut(entity)
                    && matches!(*character_state, comp::CharacterState::Crawl)
                {
                    *character_state = CharacterState::Idle(Default::default());
                }

                #[cfg(feature = "worldgen")]
                let entity_as_actor = |entity| data.rtsim_actors.get(entity).copied();
                #[cfg(feature = "worldgen")]
                if let Some(actor) = entity_as_actor(entity) {
                    let saver = ev
                        .helper
                        .and_then(|uid| data.id_maps.uid_entity(uid))
                        .and_then(entity_as_actor);
                    data.rtsim.hook_rtsim_actor_helped(
                        &data.world,
                        data.index.as_index_ref(),
                        actor,
                        saver,
                    );
                }
            }
        }
    }
}

impl ServerEvent for DownedEvent {
    type SystemData<'a> = (
        Read<'a, EventBus<BuffEvent>>,
        WriteStorage<'a, comp::CharacterState>,
        WriteStorage<'a, comp::Health>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (buff_event, mut character_states, mut healths): Self::SystemData<'_>,
    ) {
        let mut buff_emitter = buff_event.emitter();
        for ev in events {
            if let Some(mut health) = healths.get_mut(ev.entity) {
                health.consume_death_protection()
            }

            if let Some(mut character_state) = character_states.get_mut(ev.entity) {
                *character_state = CharacterState::Crawl;
            }

            // Remove buffs that don't persist when downed.
            buff_emitter.emit(BuffEvent {
                entity: ev.entity,
                buff_change: comp::BuffChange::RemoveByCategory {
                    all_required: vec![],
                    any_required: vec![],
                    none_required: vec![BuffCategory::PersistOnDowned],
                },
            });
        }
    }
}

impl ServerEvent for KnockbackEvent {
    type SystemData<'a> = (
        Entities<'a>,
        ReadStorage<'a, Client>,
        ReadStorage<'a, PhysicsState>,
        ReadStorage<'a, comp::Mass>,
        WriteStorage<'a, comp::Vel>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (entities, clients, physic_states, mass, mut velocities): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Some((physics, mass, vel, client)) = (
                &physic_states,
                mass.maybe(),
                &mut velocities,
                clients.maybe(),
            )
                .lend_join()
                .get(ev.entity, &entities)
            {
                //Check if the entity is on a surface. If it is not, reduce knockback.
                let mut impulse = ev.impulse
                    * if physics.on_surface().is_some() {
                        1.0
                    } else {
                        0.4
                    };

                // we go easy on the little ones (because they fly so far)
                impulse /= mass.map_or(0.0, |m| m.0).max(40.0);

                vel.0 += impulse;
                if let Some(client) = client {
                    client.send_fallible(ServerGeneral::Knockback(impulse));
                }
            }
        }
    }
}

fn handle_exp_gain(
    exp_reward: f32,
    inventory: &Inventory,
    skill_set: &mut SkillSet,
    character_class: Option<&mut CharacterClass>,
    uid: &Uid,
    outcomes_emitter: &mut Emitter<Outcome>,
) {
    use comp::inventory::{item::ItemKind, slot::EquipSlot};

    // Create hash set of xp pools to consider splitting xp amongst. The class
    // slice is handled separately below: it counts as ONE slot here
    // regardless of how many `Class(_)` groups the character holds, so
    // gaining a second class group (multiclass) never dilutes General or
    // weapon XP, which have nothing to do with multiclassing.
    let mut xp_pools = HashSet::<SkillGroupKind>::new();
    // Insert general pool since it is always accessible
    xp_pools.insert(SkillGroupKind::General);
    // Closure to add xp pool corresponding to weapon type equipped in a particular
    // EquipSlot
    let mut add_tool_from_slot = |equip_slot| {
        let skill_group = inventory
            .equipped(equip_slot)
            .and_then(|i| match &*i.kind() {
                ItemKind::Tool(tool) if tool.kind.gains_combat_xp() => {
                    Some(combat::skill_group_for_weapon(tool))
                },
                _ => None,
            });
        if let Some(skill_group) = skill_group {
            // Only adds to xp pools if entity has that skill group available
            if skill_set.skill_group_accessible(skill_group) {
                xp_pools.insert(skill_group);
            }
        }
    };
    // Add weapons to xp pools considered
    add_tool_from_slot(EquipSlot::ActiveMainhand);
    add_tool_from_slot(EquipSlot::ActiveOffhand);
    add_tool_from_slot(EquipSlot::InactiveMainhand);
    add_tool_from_slot(EquipSlot::InactiveOffhand);

    // The character's class group(s) are always-active (like General), so they
    // earn combat XP from every kill — this is the source of class skill
    // points (spec §1). Without it the class trees can never be unlocked. A
    // multiclass character holds up to 2 `Class(_)` groups; the class slice's
    // reward is split evenly across however many are present (Model A: 50/50
    // for two, unchanged for one).
    let class_groups: Vec<SkillGroupKind> = skill_set
        .skill_groups()
        .map(|sg| sg.skill_group_kind)
        .filter(|kind| matches!(kind, SkillGroupKind::Class(_)))
        .collect();
    let has_class_slice = !class_groups.is_empty();

    let num_pools = xp_pools.len() as f32 + if has_class_slice { 1.0 } else { 0.0 };
    let level_before = skill_set.character_level();
    for pool in xp_pools.iter() {
        if let Some(level_outcome) =
            skill_set.add_experience(*pool, (exp_reward / num_pools).ceil() as u32)
        {
            outcomes_emitter.emit(Outcome::SkillPointGain {
                uid: *uid,
                skill_tree: *pool,
                total_points: level_outcome,
            });
        }
    }
    if has_class_slice {
        let per_class_reward = ((exp_reward / num_pools) / class_groups.len() as f32).ceil() as u32;
        for class_group in &class_groups {
            if let Some(level_outcome) = skill_set.add_experience(*class_group, per_class_reward) {
                outcomes_emitter.emit(Outcome::SkillPointGain {
                    uid: *uid,
                    skill_tree: *class_group,
                    total_points: level_outcome,
                });
            }
            xp_pools.insert(*class_group);
        }
    }
    let level_after = skill_set.character_level();
    if level_after > level_before {
        outcomes_emitter.emit(Outcome::CharacterLevelUp {
            uid: *uid,
            new_level: level_after,
        });
        common::telemetry!(
            "lvl",
            event = "character_level_up",
            uid = ?uid,
            new_level = level_after
        );
        // Set-and-forget routing preference: a no-op unless the player has
        // opted a multiclass character's future levels to the secondary.
        if let Some(character_class) = character_class {
            character_class.route_levels_gained(level_after - level_before, level_after);
        }
    }
    // BL-20: 1 feat point per 10 character levels (15/25/35/45 — lore cadence,
    // max 4 total). Grants directly via `grant_skill_point` (exp-independent),
    // not the standard XP-per-group path. Checks every milestone so a single
    // XP gain that crosses several thresholds at once (e.g. level 13 -> 30)
    // grants one point per milestone crossed.
    for milestone in [15, 25, 35, 45] {
        if level_before < milestone && level_after >= milestone {
            skill_set.grant_skill_point(SkillGroupKind::Feats);
        }
    }
    outcomes_emitter.emit(Outcome::ExpChange {
        uid: *uid,
        exp: exp_reward as u32,
        xp_pools,
    });
}

/// The `Uid` of the entity that actually dealt this damage, regardless of
/// whether it was recorded `Solo` or as part of a `Group` at the time --
/// both variants uniquely identify one attacking entity. Used to key
/// mastery crediting on the ATTACKER, never the group, since a group-XP
/// award (below) can reach bystanders who dealt no damage at all.
fn damage_contributor_uid(contributor: &DamageContributor) -> Uid {
    match contributor {
        DamageContributor::Solo(uid) => *uid,
        DamageContributor::Group { entity_uid, .. } => *entity_uid,
    }
}

/// Credits one attacker's `SpellMastery` for their own share of a kill.
/// Called once per entry of `exp_awards` (spec §2a), i.e. only for an
/// attacker who already cleared every existing XP-eligibility check --
/// in-range, not the victim itself, not a PvP kill. This function takes no
/// PvP/self-kill signal at all, because by the time it runs those cases
/// already never call it: mastery has no separate anti-farm rule to write,
/// it simply inherits the one XP already has.
///
/// `own_total_damage` / `own_damage_by_source` are THIS attacker's own
/// entry in `Health::damage_contributors` -- never the group's summed total
/// -- so mastery tracks personal spellcasting even when the group-XP split
/// (spec-unrelated) divides the reward among bystanders who never cast
/// anything. `exp_reward` is this attacker's already-computed share of the
/// kill (post group-division), matching the currency `handle_exp_gain`
/// itself spends.
///
/// A no-op if this attacker dealt no damage at all (nothing to attribute) or
/// holds no `SpellMastery` component yet (an NPC, a pet, or any entity that
/// predates the component).
fn grant_kill_mastery(
    mastery: &mut SpellMastery,
    exp_reward: f32,
    own_total_damage: u64,
    own_damage_by_source: &[u64; MagicSource::COUNT],
    target_level: u16,
    caster_level: u16,
    polyglot_rank: u16,
) {
    if own_total_damage == 0 {
        return;
    }
    let delta = i32::from(target_level) - i32::from(caster_level);
    let weight =
        level_delta_weight(delta) * (1.0 + POLYGLOT_BONUS_PER_RANK * f32::from(polyglot_rank));
    for source in MagicSource::ALL {
        if matches!(source, MagicSource::Arcane) {
            continue;
        }
        let source_damage = own_damage_by_source[source as usize];
        if source_damage == 0 {
            continue;
        }
        let share = source_damage as f32 / own_total_damage as f32;
        grant_source_mastery(mastery, source, exp_reward * share, weight);
    }
}

/// Credits mastery for a spell that landed a non-damage effect (a buff or a
/// heal) on another entity. The single decision surface both the
/// `HealthChangeEvent` (heal) and `BuffEvent` (buff) server handlers reduce
/// their own ECS state down to and call, so the two anti-farm rules below
/// are asserted exactly once rather than re-derived per handler.
///
/// How recently a target must have taken damage, relative to the moment a
/// heal/buff lands on it, for that landing to be considered a real support
/// moment rather than free credit. Deliberately much shorter than
/// `Health`'s own 600s `damage_contributors` prune window -- that longer
/// window exists to still award kill XP to whoever tapped a target minutes
/// ago, which is the wrong question here: "is this target actively, still
/// fighting right now." A single stray hit taken 9 minutes ago must not
/// leave a 10-minute-wide farmable window for unthrottled non-damage
/// mastery credit on that target.
const MASTERY_RECENT_COMBAT_WINDOW_SECS: f64 = 20.0;

/// A no-op when `caster_uid == target_uid` (a self-only buff or heal earns
/// nothing -- there is no other entity to have landed on), when
/// `target_in_combat` is `false` (nobody has damaged the target within
/// [`MASTERY_RECENT_COMBAT_WINDOW_SECS`], so there is no real fight to
/// credit against and a full-health, never-fought target is worth nothing),
/// or when `is_fresh_grant` is `false` (the buff path only: re-casting an
/// already-active matching buff on the same target is a refresh, not a new
/// landed effect, and must not credit again every recast).
///
/// `target_combat_rating` is what the credit scales with once the gates
/// above pass, mirroring `exp_reward`'s own `combat_rating(...) * 20.0` --
/// there is no `reward_fraction` factor here since nothing died.
/// `NON_DAMAGE_WEIGHT` and the `Mage(Polyglot)` bonus both apply the same
/// way they do on the damage path; `level_delta_weight` deliberately does
/// NOT -- it models a kill-specific overleveled/underleveled anti-farm
/// curve with no natural reading for "how many levels above me is the ally
/// I just healed", and the combat-rating scaling above already does the
/// anti-farm job this path needs (a trivial target is worth ~nothing before
/// this weight would even apply).
fn grant_non_damage_mastery(
    mastery: &mut SpellMastery,
    source: MagicSource,
    caster_uid: Uid,
    target_uid: Uid,
    target_in_combat: bool,
    is_fresh_grant: bool,
    target_combat_rating: f32,
    polyglot_rank: u16,
) {
    if caster_uid == target_uid || !target_in_combat || !is_fresh_grant {
        return;
    }
    let base_xp = target_combat_rating * 20.0;
    let weight = NON_DAMAGE_WEIGHT * (1.0 + POLYGLOT_BONUS_PER_RANK * f32::from(polyglot_rank));
    grant_source_mastery(mastery, source, base_xp, weight);
}

#[cfg(test)]
mod handle_exp_gain_tests {
    use super::*;
    use common::{
        comp::{Item, class::ClassKind, inventory::Inventory},
        event::EventBus,
    };
    use core::num::NonZeroU64;

    fn uid() -> Uid { Uid(NonZeroU64::new(1).unwrap()) }

    fn earned_exp(skill_set: &SkillSet, kind: SkillGroupKind) -> u32 {
        skill_set
            .skill_groups()
            .find(|sg| sg.skill_group_kind == kind)
            .unwrap()
            .earned_exp
    }

    fn gain_exp(skill_set: &mut SkillSet, exp_reward: f32) -> HashSet<SkillGroupKind> {
        let inventory = Inventory::with_empty();
        let bus = EventBus::<Outcome>::default();
        let mut emitter = bus.emitter();
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        handle_exp_gain(
            exp_reward,
            &inventory,
            skill_set,
            Some(&mut character_class),
            &uid(),
            &mut emitter,
        );
        emitter
            .events
            .iter()
            .find_map(|o| match o {
                Outcome::ExpChange { xp_pools, .. } => Some(xp_pools.clone()),
                _ => None,
            })
            .unwrap()
    }

    /// The class slice must count as exactly one slot regardless of how many
    /// `Class(_)` groups are held, so a single-class character's per-pool
    /// split is byte-identical to the pre-multiclass behaviour (General +
    /// one class group = 2 pools, no weapons equipped).
    #[test]
    fn single_class_xp_split_is_unaffected_by_the_class_slice_refactor() {
        let mut skill_set = SkillSet::default();
        skill_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warrior));

        let xp_pools = gain_exp(&mut skill_set, 100.0);

        assert_eq!(
            xp_pools,
            HashSet::from([
                SkillGroupKind::General,
                SkillGroupKind::Class(ClassKind::Warrior)
            ])
        );
        // 2 pools (General + the one class slice) -> 50 exp each, exactly the
        // pre-refactor split.
        assert_eq!(
            earned_exp(&skill_set, SkillGroupKind::Class(ClassKind::Warrior)),
            50
        );
        assert_eq!(earned_exp(&skill_set, SkillGroupKind::General), 50);
    }

    /// A multiclass character's class slice is still ONE slot (so General
    /// stays at half the reward, not a third) and is split deterministically
    /// 50/50 across the two held class groups.
    #[test]
    fn multiclass_xp_split_shares_one_class_slice_deterministically() {
        let mut skill_set = SkillSet::default();
        skill_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warrior));
        skill_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warlock));

        let xp_pools = gain_exp(&mut skill_set, 100.0);

        assert_eq!(
            xp_pools,
            HashSet::from([
                SkillGroupKind::General,
                SkillGroupKind::Class(ClassKind::Warrior),
                SkillGroupKind::Class(ClassKind::Warlock)
            ])
        );
        // Still 2 pools for the purpose of the top-level split (General +
        // ONE class slice) -> General gets 50, same as the single-class
        // case above, not 33.
        assert_eq!(earned_exp(&skill_set, SkillGroupKind::General), 50);
        // The 50-exp class slice splits 50/50 across the two class groups.
        assert_eq!(
            earned_exp(&skill_set, SkillGroupKind::Class(ClassKind::Warrior)),
            25
        );
        assert_eq!(
            earned_exp(&skill_set, SkillGroupKind::Class(ClassKind::Warlock)),
            25
        );
    }

    /// The banishment reward is the *same* generic `combat_rating`-derived XP
    /// every kill uses, scaled by `RemovalInfo::reward_fraction` — not a
    /// separate reward system. This pins the multiplier at the one point it is
    /// applied, so a later refactor of the XP block cannot silently drop it.
    #[test]
    fn a_banishment_awards_a_quarter_of_the_kill_experience() {
        const RAW_EXP: f32 = 400.0;
        let killed = RAW_EXP * combat::RemovalInfo::killed().reward_fraction;
        let banished = RAW_EXP * combat::RemovalInfo::banished(0.25).reward_fraction;
        assert!((killed - 400.0).abs() < f32::EPSILON);
        assert!((banished - 100.0).abs() < f32::EPSILON);

        let mut killed_set = SkillSet::default();
        killed_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warrior));
        gain_exp(&mut killed_set, killed);

        let mut banished_set = SkillSet::default();
        banished_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warrior));
        gain_exp(&mut banished_set, banished);

        assert_eq!(
            earned_exp(&killed_set, SkillGroupKind::General),
            (killed / 2.0).ceil() as u32
        );
        assert_eq!(
            earned_exp(&banished_set, SkillGroupKind::General),
            (banished / 2.0).ceil() as u32
        );
        assert!(
            earned_exp(&banished_set, SkillGroupKind::General)
                < earned_exp(&killed_set, SkillGroupKind::General)
        );
    }

    /// A martial-role Staff must route combat XP to its own `WeaponRoled`
    /// skill group, not the caster `Weapon(Staff)` tree it shares a
    /// `ToolKind` with -- otherwise a character who invested points in the
    /// martial tree earns zero weapon-XP from every kill while wielding it,
    /// and the `WeaponRoled` tree becomes permanently unpurchasable through
    /// live play. Mirrors `combat::skill_group_for_weapon`'s own contract.
    #[test]
    fn martial_staff_routes_combat_xp_to_its_own_skill_group() {
        use common::{comp::inventory::slot::EquipSlot, resources::Time};

        let mut skill_set = SkillSet::default();
        skill_set.unlock_skill_group(SkillGroupKind::Class(ClassKind::Warrior));
        skill_set.unlock_skill_group(SkillGroupKind::WeaponRoled(
            common::comp::inventory::item::tool::ToolKind::Staff,
            common::comp::inventory::item::tool::WeaponRole::Martial,
        ));

        let mut inventory = Inventory::with_empty();
        inventory.replace_loadout_item(
            EquipSlot::ActiveMainhand,
            Some(Item::new_from_asset_expect(
                "common.items.weapons.staff.frostbound_quarterstaff",
            )),
            Time(0.0),
        );

        let bus = EventBus::<Outcome>::default();
        let mut emitter = bus.emitter();
        let mut character_class = CharacterClass::single(ClassKind::Warrior);
        handle_exp_gain(
            100.0,
            &inventory,
            &mut skill_set,
            Some(&mut character_class),
            &uid(),
            &mut emitter,
        );
        let xp_pools = emitter
            .events
            .iter()
            .find_map(|o| match o {
                Outcome::ExpChange { xp_pools, .. } => Some(xp_pools.clone()),
                _ => None,
            })
            .unwrap();

        assert!(
            xp_pools.contains(&SkillGroupKind::WeaponRoled(
                common::comp::inventory::item::tool::ToolKind::Staff,
                common::comp::inventory::item::tool::WeaponRole::Martial,
            )),
            "expected the martial-staff kill XP to land in WeaponRoled(Staff, Martial), got \
             {xp_pools:?}"
        );
        assert!(
            !xp_pools.contains(&SkillGroupKind::Weapon(
                common::comp::inventory::item::tool::ToolKind::Staff
            )),
            "martial-staff kill XP must not be misrouted to the caster Weapon(Staff) tree, got \
             {xp_pools:?}"
        );
    }
}

#[cfg(test)]
mod grant_kill_mastery_tests {
    use super::*;

    /// A kill done 70% Divine / 30% weapon credits Divine
    /// `0.70 * exp_reward` (at Delta=0, W=1.0, no Polyglot) and nothing else.
    #[test]
    fn credits_only_the_source_actually_used_by_its_own_damage_share() {
        let mut mastery = SpellMastery::default();
        let mut by_source = [0u64; MagicSource::COUNT];
        by_source[MagicSource::Divine as usize] = 70;
        // own_total_damage = 100: 70 Divine-tagged, 30 untagged (weapon).

        grant_kill_mastery(&mut mastery, 1000.0, 100, &by_source, 20, 20, 0);

        assert_eq!(mastery.source_xp(MagicSource::Divine), 700);
        assert_eq!(mastery.source_xp(MagicSource::Primordial), 0);
        assert_eq!(mastery.source_xp(MagicSource::Psionic), 0);
        assert_eq!(mastery.source_xp(MagicSource::Ki), 0);
        assert_eq!(mastery.source_xp(MagicSource::Arcane), 0);
    }

    /// A kill done entirely with a weapon (no magic source tagged on any of
    /// the damage) credits zero mastery, even though `own_total_damage` is
    /// nonzero -- there is nothing in `by_source` to attribute.
    #[test]
    fn a_kill_done_entirely_with_a_weapon_credits_zero_mastery() {
        let mut mastery = SpellMastery::default();
        let by_source = [0u64; MagicSource::COUNT];

        grant_kill_mastery(&mut mastery, 1000.0, 100, &by_source, 20, 20, 0);

        for source in MagicSource::ALL {
            assert_eq!(mastery.source_xp(source), 0, "{source:?}");
        }
    }

    /// `level_delta_weight` is actually applied: the same kill against a
    /// target 10 levels below the caster yields 0.15x the at-level result.
    #[test]
    fn the_level_delta_weight_scales_the_credited_xp() {
        let mut at_level = SpellMastery::default();
        let mut by_source = [0u64; MagicSource::COUNT];
        by_source[MagicSource::Primordial as usize] = 100;
        grant_kill_mastery(&mut at_level, 1000.0, 100, &by_source, 20, 20, 0);

        let mut ten_below = SpellMastery::default();
        grant_kill_mastery(&mut ten_below, 1000.0, 100, &by_source, 10, 20, 0);

        assert_eq!(at_level.source_xp(MagicSource::Primordial), 1000);
        assert_eq!(ten_below.source_xp(MagicSource::Primordial), 150);
    }

    /// A caster dealing no damage at all (present in `exp_awards` only
    /// because a group kill split XP to a bystander) attributes nothing --
    /// there is no own damage to derive a source share from.
    #[test]
    fn an_attacker_with_no_own_damage_credits_nothing() {
        let mut mastery = SpellMastery::default();
        let mut by_source = [0u64; MagicSource::COUNT];
        by_source[MagicSource::Divine as usize] = 0;

        grant_kill_mastery(&mut mastery, 1000.0, 0, &by_source, 20, 20, 0);

        for source in MagicSource::ALL {
            assert_eq!(mastery.source_xp(source), 0, "{source:?}");
        }
    }

    /// `Mage(Polyglot)` multiplies the credited XP: rank 3 -> x1.24.
    #[test]
    fn polyglot_rank_multiplies_the_credited_xp() {
        let mut by_source = [0u64; MagicSource::COUNT];
        by_source[MagicSource::Ki as usize] = 100;

        let mut no_polyglot = SpellMastery::default();
        grant_kill_mastery(&mut no_polyglot, 1000.0, 100, &by_source, 20, 20, 0);

        let mut rank_three = SpellMastery::default();
        grant_kill_mastery(&mut rank_three, 1000.0, 100, &by_source, 20, 20, 3);

        assert_eq!(no_polyglot.source_xp(MagicSource::Ki), 1000);
        assert_eq!(rank_three.source_xp(MagicSource::Ki), 1240);
    }

    /// `polyglot_rank` above is always a value read from
    /// `skill_set.skill_level(...)`, itself bounded by
    /// `skill_max_levels.ron` through `Skill::max_level`. This pins the
    /// manifest's current cap so `POLYGLOT_BONUS_PER_RANK`'s "+24% at rank
    /// 3" doc comment stays true if the manifest is ever retuned -- the
    /// mistake a hardcoded `3` made elsewhere in this codebase before.
    #[test]
    fn mage_polyglot_max_rank_is_read_from_the_manifest() {
        use common::comp::skills::MageSkill;
        assert_eq!(Skill::Mage(MageSkill::Polyglot).max_level(), 3);
    }
}

#[cfg(test)]
mod grant_non_damage_mastery_tests {
    use super::*;
    use core::num::NonZeroU64;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    /// Spec's self-only exclusion: `caster_uid == target_uid` is exactly
    /// what a self-buff or self-heal looks like once reduced to this
    /// function's inputs -- it credits nothing. The same call against a
    /// distinct target (a party member) credits.
    #[test]
    fn a_self_only_buff_credits_nothing_but_the_same_buff_on_a_party_member_credits() {
        let caster = uid(1);

        let mut self_cast = SpellMastery::default();
        grant_non_damage_mastery(
            &mut self_cast,
            MagicSource::Divine,
            caster,
            caster,
            true,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        assert_eq!(self_cast.source_xp(MagicSource::Divine), 0);

        let mut party_member = SpellMastery::default();
        grant_non_damage_mastery(
            &mut party_member,
            MagicSource::Divine,
            caster,
            uid(2),
            true,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        assert!(party_member.source_xp(MagicSource::Divine) > 0);
    }

    /// A full-health, never-damaged target (empty damage-contributor ledger,
    /// `target_in_combat == false`) credits zero no matter how high its
    /// combat_rating is. A mid-fight ally (`target_in_combat == true`)
    /// credits exactly `combat_rating * 20.0 * NON_DAMAGE_WEIGHT` at rank 0 --
    /// the non-damage path's counterpart to `exp_reward`.
    #[test]
    fn healing_a_never_damaged_target_credits_zero_but_a_mid_fight_ally_credits_the_non_damage_weight()
     {
        let caster = uid(1);
        let target = uid(2);

        let mut never_damaged = SpellMastery::default();
        grant_non_damage_mastery(
            &mut never_damaged,
            MagicSource::Divine,
            caster,
            target,
            false,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        assert_eq!(never_damaged.source_xp(MagicSource::Divine), 0);

        let mut mid_fight = SpellMastery::default();
        grant_non_damage_mastery(
            &mut mid_fight,
            MagicSource::Divine,
            caster,
            target,
            true,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        assert_eq!(
            mid_fight.source_xp(MagicSource::Divine),
            (1000.0 * 20.0 * NON_DAMAGE_WEIGHT).round() as u32
        );
    }

    /// `Mage(Polyglot)` multiplies the non-damage credit the same way it
    /// does the damage path: rank 3 -> x1.24 on top of `NON_DAMAGE_WEIGHT`.
    #[test]
    fn polyglot_rank_multiplies_the_non_damage_credit_too() {
        let caster = uid(1);
        let target = uid(2);

        let mut no_polyglot = SpellMastery::default();
        grant_non_damage_mastery(
            &mut no_polyglot,
            MagicSource::Primordial,
            caster,
            target,
            true,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        let mut rank_three = SpellMastery::default();
        grant_non_damage_mastery(
            &mut rank_three,
            MagicSource::Primordial,
            caster,
            target,
            true,
            true, // is_fresh_grant
            1000.0,
            3,
        );

        let no_polyglot_xp = no_polyglot.source_xp(MagicSource::Primordial);
        let rank_three_xp = rank_three.source_xp(MagicSource::Primordial);
        assert_eq!(no_polyglot_xp, 5_000);
        assert_eq!(rank_three_xp, 6_200);
    }

    /// Arcane is never written on the non-damage path either -- a landed
    /// Arcane buff/heal on a genuine in-combat ally still earns zero
    /// source-XP for it, the same invariant `grant_kill_mastery` upholds on
    /// the damage path.
    #[test]
    fn arcane_is_never_written_by_the_non_damage_path_either() {
        let mut mastery = SpellMastery::default();
        grant_non_damage_mastery(
            &mut mastery,
            MagicSource::Arcane,
            uid(1),
            uid(2),
            true,
            true, // is_fresh_grant
            1000.0,
            0,
        );
        assert_eq!(mastery.source_xp(MagicSource::Arcane), 0);
    }

    /// The named anti-farm failure mode: standing still and casting into
    /// the air (or at nothing at all) never earns mastery. There is no
    /// separate "cast" signal this function -- or its callers -- ever
    /// consumes: the server only reaches a `HealthChangeEvent`/`BuffEvent`
    /// handler when an effect actually landed on some entity, and
    /// `grant_source_mastery` is the only place `SpellMastery.source_xp` is
    /// ever written. A spell with no target in range emits neither event, so
    /// nothing here is ever called and the ledger cannot move.
    #[test]
    fn standing_still_casting_into_the_air_earns_no_mastery() {
        let mastery = SpellMastery::default();
        for source in MagicSource::ALL {
            assert_eq!(mastery.source_xp(source), 0, "{source:?}");
        }
    }

    /// Re-casting an already-active matching buff on the same target (the
    /// buff-spam farming vector this function's `is_fresh_grant` gate
    /// closes) credits nothing, even though every other gate (distinct
    /// target, in-combat) passes.
    #[test]
    fn a_refreshed_buff_credits_nothing_but_a_fresh_one_credits() {
        let caster = uid(1);
        let target = uid(2);

        let mut refreshed = SpellMastery::default();
        grant_non_damage_mastery(
            &mut refreshed,
            MagicSource::Divine,
            caster,
            target,
            true,
            false, // is_fresh_grant: a recast of an already-active buff
            1000.0,
            0,
        );
        assert_eq!(refreshed.source_xp(MagicSource::Divine), 0);

        let mut fresh = SpellMastery::default();
        grant_non_damage_mastery(
            &mut fresh,
            MagicSource::Divine,
            caster,
            target,
            true,
            true,
            1000.0,
            0,
        );
        assert!(fresh.source_xp(MagicSource::Divine) > 0);
    }
}

/// The heal path's `target_combat_rating` now comes off the `DerivedStats`
/// cache instead of being re-folded from the target's `(inventory, energy,
/// poise, skill_set, body)` at every landed heal. This pins that the swap is
/// value-preserving end to end: the rating the cache holds for a really-geared
/// entity, run through the same `grant_non_damage_mastery` the handler calls,
/// must credit the identical XP the direct-compute path credits.
#[cfg(test)]
mod non_damage_mastery_reads_the_cache_tests {
    use super::*;
    use common::{
        comp::{
            DerivedStats,
            inventory::{
                item::{
                    Item, ItemBase, ItemDef, ItemKind,
                    armor::{self, Armor, ArmorKind, Protection},
                },
                loadout_builder::LoadoutBuilder,
            },
        },
        resources::GameMode,
        shared_server_config::ServerConstants,
        skillset_builder::SkillSetBuilder,
        terrain::{MapSizeLg, TerrainChunk},
    };
    use core::num::NonZeroU64;
    use specs::{Builder, WorldExt};
    use std::{sync::Arc, time::Duration};
    use vek::Vec2;

    const WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn setup() -> common_state::State {
        let pools = common_state::State::pools(GameMode::Server);
        let mut state = common_state::State::new(
            GameMode::Server,
            pools,
            WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                common_systems::add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        state
            .ecs_mut()
            .insert(MaterialStatManifest::load().cloned());
        state.ecs_mut().insert(AbilityMap::load().cloned());
        state
    }

    /// A chest piece with real, non-degenerate stats on every channel the
    /// rating folds, so a cache read that silently returned the default would
    /// visibly differ from the direct computation.
    fn geared_loadout() -> Inventory {
        let item = Item::new_from_item_base(
            ItemBase::Simple(Arc::new(ItemDef::create_test_itemdef_from_kind(
                ItemKind::Armor(Armor::new(
                    ArmorKind::Chest,
                    armor::StatsSource::Direct(armor::Stats {
                        protection: Some(Protection::Normal(12.5)),
                        poise_resilience: Some(Protection::Normal(7.25)),
                        energy_max: Some(13.0),
                        energy_reward: Some(0.35),
                        precision_power: Some(0.17),
                        stealth: Some(0.9),
                        ground_contact: Default::default(),
                    }),
                )),
            ))),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        );
        Inventory::with_loadout_humanoid(LoadoutBuilder::empty().chest(Some(item)).build())
    }

    #[test]
    fn a_heal_on_a_geared_ally_credits_the_same_xp_from_the_cache_as_from_a_direct_computation() {
        let mut state = setup();
        let body = Body::Humanoid(common::comp::humanoid::Body::random());
        let inventory = geared_loadout();
        let mut skill_set = SkillSetBuilder::default().build();
        skill_set.grant_skill_point(SkillGroupKind::General);

        let ally = state
            .ecs_mut()
            .create_entity_synced()
            .with(body)
            .with(Health::new(body))
            .with(Energy::new(body))
            .with(Poise::new(body))
            .with(Stats::empty(body))
            .with(skill_set)
            .with(inventory)
            .build();

        state.tick(
            Duration::from_millis(16),
            false,
            None,
            &ServerConstants {
                day_cycle_coefficient: 24.0,
                oracle_live: false,
            },
            |_, _| {},
        );

        let ecs = state.ecs();
        let cached_rating = ecs
            .read_storage::<DerivedStats>()
            .get(ally)
            .expect("a geared entity has a cache after one tick")
            .combat_rating;
        assert!(cached_rating > 0.0, "the fixture must be non-degenerate");

        // The direct-compute path, with exactly the arguments the deleted
        // `combat::combat_rating` free function used to pass: attunement-blind,
        // and the three base maxima taken off the live components.
        let healths = ecs.read_storage::<Health>();
        let energies = ecs.read_storage::<Energy>();
        let poises = ecs.read_storage::<Poise>();
        let inventories = ecs.read_storage::<Inventory>();
        let skill_sets = ecs.read_storage::<SkillSet>();
        let direct_rating = DerivedStats::compute(
            inventories.get(ally),
            None,
            skill_sets.get(ally),
            Some(body),
            healths.get(ally).map(|h| h.base_max()),
            energies.get(ally).map(|e| e.base_max()),
            poises.get(ally).map(|p| p.base_max()),
            &ecs.read_resource::<MaterialStatManifest>(),
        )
        .combat_rating;
        assert_eq!(cached_rating, direct_rating);

        // …and the XP the healer actually banks is identical either way.
        let credit = |rating| {
            let mut mastery = SpellMastery::default();
            grant_non_damage_mastery(
                &mut mastery,
                MagicSource::Divine,
                uid(1),
                uid(2),
                true,
                true,
                rating,
                0,
            );
            mastery.source_xp(MagicSource::Divine)
        };
        assert_eq!(credit(cached_rating), credit(direct_rating));
        assert!(
            credit(cached_rating) > 0,
            "a real heal on a geared, mid-fight ally must credit something"
        );
    }
}

#[cfg(test)]
mod death_supersedes_banishment_tests {
    use super::*;
    use common::combat::RemovalInfo;
    use specs::{Builder, WorldExt};

    fn removal_of(entity: EcsEntity, removal: RemovalInfo) -> DestroyEvent {
        DestroyEvent {
            entity,
            cause: HealthChange {
                amount: 0.0,
                by: None,
                cause: None,
                magic_source: None,
                time: Time(0.0),
                precise: false,
                instance: 0,
            },
            removal,
        }
    }

    /// Drives the *production* filter — the same call `DestroyEvent::handle`
    /// makes — rather than restating the rule in test code.
    fn honoured(batch: Vec<DestroyEvent>) -> Vec<DestroyEvent> {
        let deaths = DeathsInBatch::of(&batch);
        deaths.honoured(batch).collect()
    }

    /// 🔴 The race. A `DestroyEvent{Killed}` can be raised by
    /// `HealthChangeEvent`'s handler or, from outside the dispatcher entirely,
    /// by `common/systems`' stats system during `State::tick`; a
    /// `DestroyEvent{Banished}` is raised by `BanishEvent`'s handler. Nothing
    /// orders all three against each other, so one creature can be banished
    /// and killed inside a single tick and both removals land in this batch.
    /// Which one was emitted first is the scheduler's choice; the outcome must
    /// not be.
    ///
    /// Real death always wins: exactly one removal survives for that entity,
    /// and it is the kill. Anything else is either ~125% XP (the banishment's
    /// quarter *plus* the kill's whole) or a banishment record with no
    /// creature behind it.
    #[test]
    fn a_death_supersedes_a_banishment_in_the_same_batch_whichever_arrives_first() {
        let mut world = specs::World::new();
        let doomed = world.create_entity().build();
        let bystander = world.create_entity().build();

        let kill_first = vec![
            removal_of(doomed, RemovalInfo::killed()),
            removal_of(doomed, RemovalInfo::banished(0.25)),
            removal_of(bystander, RemovalInfo::banished(0.25)),
        ];
        let banish_first = vec![
            removal_of(doomed, RemovalInfo::banished(0.25)),
            removal_of(doomed, RemovalInfo::killed()),
            removal_of(bystander, RemovalInfo::banished(0.25)),
        ];

        for batch in [kill_first, banish_first] {
            let honoured = honoured(batch);
            let for_doomed: Vec<_> = honoured.iter().filter(|ev| ev.entity == doomed).collect();
            assert_eq!(
                for_doomed.len(),
                1,
                "the doomed creature must be removed exactly once, so it is rewarded exactly once"
            );
            assert!(
                for_doomed[0].removal.cause.counts_as_kill(),
                "the surviving removal must be the death, not the banishment"
            );
            assert_eq!(
                honoured.iter().filter(|ev| ev.entity == bystander).count(),
                1,
                "an unrelated banishment in the same batch is untouched"
            );
        }
    }

    /// The rule must not quietly swallow ordinary banishments: with no death
    /// for that entity in the batch, the banishment is honoured exactly as
    /// before.
    #[test]
    fn a_banishment_alone_in_its_batch_is_still_honoured() {
        let mut world = specs::World::new();
        let banished = world.create_entity().build();
        let batch = vec![removal_of(banished, RemovalInfo::banished(0.25))];

        assert_eq!(honoured(batch).len(), 1);
    }

    /// A creature killed while it is *already* in limbo arrives as a lone kill
    /// in a later tick. Nothing supersedes it; revoking its record is the
    /// handler's job, not this rule's.
    #[test]
    fn a_lone_death_is_never_superseded() {
        let mut world = specs::World::new();
        let slain = world.create_entity().build();
        let batch = vec![removal_of(slain, RemovalInfo::killed())];

        assert_eq!(honoured(batch).len(), 1);
    }

    /// Duplicate `Destroy{Killed}` events for one entity are a shipped
    /// (TODO-flagged) reality. They are left to the existing `is_dead` latch —
    /// this rule must not start dropping kills, or a double-kill batch would
    /// remove the entity zero times.
    #[test]
    fn duplicate_deaths_are_left_to_the_is_dead_latch() {
        let mut world = specs::World::new();
        let slain = world.create_entity().build();
        let batch = vec![
            removal_of(slain, RemovalInfo::killed()),
            removal_of(slain, RemovalInfo::killed()),
        ];

        assert_eq!(honoured(batch).len(), 2);
    }
}

#[derive(SystemData)]
pub struct DestroyEventData<'a> {
    entities: Entities<'a>,
    #[cfg(feature = "worldgen")]
    rtsim: WriteExpect<'a, RtSim>,
    id_maps: Read<'a, IdMaps>,
    msm: ReadExpect<'a, MaterialStatManifest>,
    ability_map: ReadExpect<'a, AbilityMap>,
    time: Read<'a, Time>,
    program_time: ReadExpect<'a, ProgramTime>,
    #[cfg(feature = "worldgen")]
    world: ReadExpect<'a, Arc<World>>,
    #[cfg(feature = "worldgen")]
    index: ReadExpect<'a, IndexOwned>,
    areas_container: Read<'a, AreasContainer<NoDurabilityArea>>,
    outcomes: Read<'a, EventBus<Outcome>>,
    entities_died_last_tick: Write<'a, EntitiesDiedLastTick>,
    melees: WriteStorage<'a, comp::Melee>,
    beams: WriteStorage<'a, comp::Beam>,
    skill_sets: WriteStorage<'a, SkillSet>,
    /// Read here (not written -- crediting happens through a short-lived
    /// `get_mut` per attacker inside the exp-award loop) so an attacker with
    /// no `SpellMastery` yet (an NPC, a pet, or any entity that predates this
    /// component) is simply skipped rather than gating the rest of their
    /// combat-XP award on it.
    spell_masteries: WriteStorage<'a, SpellMastery>,
    character_classes: WriteStorage<'a, CharacterClass>,
    inventories: WriteStorage<'a, Inventory>,
    /// The cached gear aggregates the death-effect energy/poise formulas read,
    /// instead of re-walking the affected entity's loadout per effect.
    derived_stats: ReadStorage<'a, comp::DerivedStats>,
    item_drops: WriteStorage<'a, comp::ItemDrops>,
    velocities: WriteStorage<'a, comp::Vel>,
    force_updates: WriteStorage<'a, comp::ForceUpdate>,
    energies: WriteStorage<'a, Energy>,
    character_states: WriteStorage<'a, CharacterState>,
    death_effects: WriteStorage<'a, DeathEffects>,
    players: ReadStorage<'a, Player>,
    clients: ReadStorage<'a, Client>,
    uids: ReadStorage<'a, Uid>,
    positions: ReadStorage<'a, Pos>,
    healths: WriteStorage<'a, Health>,
    bodies: ReadStorage<'a, Body>,
    groups: ReadStorage<'a, Group>,
    alignments: ReadStorage<'a, Alignment>,
    ethos: WriteStorage<'a, comp::Ethos>,
    stats: ReadStorage<'a, Stats>,
    agents: ReadStorage<'a, Agent>,
    #[cfg(feature = "worldgen")]
    rtsim_actors: ReadStorage<'a, rtsim::ActorId>,
    masses: ReadStorage<'a, comp::Mass>,
    event_buses: DestroyEvents<'a>,
    buffs: ReadStorage<'a, comp::Buffs>,
    orientations: ReadStorage<'a, comp::Ori>,
    combos: ReadStorage<'a, comp::Combo>,
    gameplay_metrics: ReadExpect<'a, GameplayMetrics>,
    /// N27-O: read (never written here) so a dying Warlock's active Cadena
    /// summons can be dismissed alongside them -- see this handler's
    /// `data.clients.contains(ev.entity)` branch.
    summons: ReadStorage<'a, comp::Summons>,
    /// Written, not read: a genuine kill *revokes* the entity's banishment.
    /// Only reachable with `worldgen`, since without rtsim nothing ever
    /// inserts the marker in the first place.
    #[cfg(feature = "worldgen")]
    banished: WriteStorage<'a, comp::Banished>,
}

/// The entities that a **real death** claims inside one batch of
/// [`DestroyEvent`]s.
///
/// A removal can reach this handler as a death or as a banishment, and both can
/// be raised for the same creature in the same tick. Honouring both would pay
/// the banishment's fractional reward *and* the kill's whole one — ~125% of a
/// single creature's XP — and would leave a persisted banishment record behind
/// for a creature that is about to be deleted.
///
/// So death wins, atomically: every removal that yields to death is dropped for
/// an entity that also died in this batch, no matter which of the two was
/// emitted first. The surviving kill is what pays out, exactly once, and it is
/// also what revokes the banishment.
///
/// 🔴 This is not redundant with the guards that keep a doomed creature from
/// being banished in the first place, because a `DestroyEvent{Killed}` can be
/// raised from **outside** the event dispatcher entirely: `common/systems`'
/// stats system emits one during `State::tick`, before any of these handlers
/// run. A creature it condemned at zero HP that is then *healed* by a
/// `HealthChangeEvent` in the same tick reads as perfectly alive by the time
/// the banishment commits — and both removals still land in this batch.
struct DeathsInBatch(HashSet<EcsEntity>);

impl DeathsInBatch {
    fn of(events: &[DestroyEvent]) -> Self {
        // Nothing can be superseded unless some removal in this batch yields
        // to death at all. The ordinary batch — a handful of creatures that
        // simply died — takes this branch and hashes nothing.
        if !events.iter().any(|ev| Self::yields_to_death(ev.removal)) {
            return Self(HashSet::new());
        }
        Self(
            events
                .iter()
                .filter(|ev| ev.removal.cause.counts_as_kill())
                .map(|ev| ev.entity)
                .collect(),
        )
    }

    /// Whether a genuine death in the same tick voids this removal.
    ///
    /// Exhaustive on purpose. `RemovalCause`'s own contract invites new
    /// variants ("Extend the enum rather than adding a parallel flag"), and a
    /// new one must state whether death outranks it instead of silently
    /// inheriting an answer from a negated `counts_as_kill()`.
    fn yields_to_death(removal: combat::RemovalInfo) -> bool {
        match removal.cause {
            // A death cannot void itself. Duplicate `Destroy{Killed}` events
            // are a separate, shipped concern handled by the `is_dead` latch
            // below; dropping one here would remove the entity zero times.
            combat::RemovalCause::Killed => false,
            // The creature was to be taken away and brought back. It died
            // instead, so there is nothing to take away and nothing to return.
            combat::RemovalCause::Banished => true,
        }
    }

    /// The removals that must actually be acted on: everything except a
    /// yields-to-death removal for an entity this batch also killed.
    ///
    /// The production loop iterates exactly this, so a test that drives it is
    /// driving the real filter rather than a restatement of it.
    fn honoured(&self, events: Vec<DestroyEvent>) -> impl Iterator<Item = DestroyEvent> + '_ {
        events
            .into_iter()
            .filter(|ev| !(Self::yields_to_death(ev.removal) && self.0.contains(&ev.entity)))
    }
}

/// Handle an entity dying. If it is a player, it will send a message to all
/// other players. If the entity that killed it had stats, then give it exp for
/// the kill. Experience given is equal to the level of the entity that was
/// killed times 10.
impl ServerEvent for DestroyEvent {
    type SystemData<'a> = DestroyEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let mut outcomes_emitter = data.outcomes.emitter();
        let mut emitters = data.event_buses.get_emitters();
        let mut rng = rand::rng();
        data.entities_died_last_tick.0.clear();

        // Collected up front so the whole batch can be consulted before any of
        // it is acted on — the only way "a real death always wins" can hold
        // without depending on the order the scheduler happened to pick. The
        // bus already hands over an owned `vec::IntoIter`, so re-collecting it
        // reuses that allocation rather than making a second one.
        let events: Vec<Self> = events.collect();
        let deaths = DeathsInBatch::of(&events);

        // A removal the batch's own deaths supersede never reaches the loop:
        // the creature really died this tick, so its banishment never happened
        // — no fractional reward, no partial loot roll. The surviving kill pays
        // the whole reward once and revokes the banishment.
        for ev in deaths.honoured(events) {
            // TODO: Investigate duplicate `Destroy` events (but don't remove this).
            // If the entity was already deleted, it can't be destroyed again.
            if !data.entities.is_alive(ev.entity) {
                continue;
            }

            // A genuine death voids any banishment the entity is carrying,
            // whether it was granted moments ago this tick or a whole session
            // ago while the creature sat in limbo — `HealthChangeEvent` joins
            // `positions.maybe()`, so damage queued before the park pass
            // stripped `Pos` still lands on a parked mob a tick later.
            // Forgetting the record is what keeps a killed creature from ever
            // returning *and* releases the worldgen spawn its chunk was holding
            // for it.
            //
            // Deliberately outside the `is_dead` latch below: a duplicate kill
            // for an already-flagged corpse must still leave no record behind.
            #[cfg(feature = "worldgen")]
            if ev.removal.cause.counts_as_kill() && data.banished.contains(ev.entity) {
                let banished = &mut data.banished;
                data.rtsim.with_banishments(|banishments| {
                    crate::banishment::revoke_banishment(banished, banishments, ev.entity)
                });
            }

            let mut outcomes = data.outcomes.emitter();
            // A banishment removes the creature *without killing it*: no death
            // flag, no `entities_died_last_tick`, no ethos drift, no kill
            // metrics, no `Outcome::Death`, no `effects_on_death`, no death
            // chat line, no equipment-durability hit, and no deletion. It still
            // runs the reward and loot paths below, scaled by
            // `removal.reward_fraction` (spec §6).
            let is_kill = ev.removal.cause.counts_as_kill();
            if let Some(mut health) = data.healths.get_mut(ev.entity) {
                // A corpse is skipped whatever the removal cause: re-running
                // the reward block for an entity that already paid out would
                // double-award it.
                if health.is_dead {
                    continue;
                }
                if is_kill {
                    health.is_dead = true;

                    // BL-33 Phase 3: a player's deeds drift their moral
                    // alignment. Killing a peaceful NPC pulls toward Evil;
                    // slaying a hostile nudges slightly Good/Lawful. Only PCs
                    // drift (NPC drift is AURORA-era); beasts/objects and
                    // self-kills carry no moral weight, and PvP (victim has no
                    // AI `Alignment`) is left to AURORA. Inside the `is_dead`
                    // latch so a duplicate `Destroy` event can't double-apply.
                    if let Some(killer) =
                        ev.cause.by.and_then(|by| data.id_maps.uid_entity(by.uid()))
                        && killer != ev.entity
                        && data.players.contains(killer)
                        && let Some(victim_alignment) = data.alignments.get(ev.entity).copied()
                        && let Some((d_ge, d_lc)) = comp::Ethos::kill_drift(victim_alignment)
                        && let Some(mut ethos) = data.ethos.get_mut(killer)
                    {
                        ethos.nudge(d_ge, d_lc);
                    }

                    if let Some(pos) = data.positions.get(ev.entity).copied() {
                        data.entities_died_last_tick.0.push((ev.entity, pos));
                    }

                    if let Some(body) = data.bodies.get(ev.entity) {
                        let npc_names = NPC_NAMES.read();
                        let body_type = npc_names
                            .get_species_meta(body)
                            .map(|meta| meta.keyword.as_str())
                            .unwrap_or("other");

                        let weapon = match ev.cause.cause {
                            Some(DamageSource::Attack(AttackSource::Melee)) => "melee",
                            Some(DamageSource::Attack(AttackSource::Projectile)) => "projectile",
                            Some(DamageSource::Attack(AttackSource::Beam)) => "beam",
                            Some(DamageSource::Attack(AttackSource::GroundShockwave))
                            | Some(DamageSource::Attack(AttackSource::AirShockwave))
                            | Some(DamageSource::Attack(AttackSource::UndodgeableShockwave)) => {
                                "shockwave"
                            },
                            Some(DamageSource::Attack(AttackSource::Explosion)) => "explosion",
                            Some(DamageSource::Attack(AttackSource::Arc)) => "arc",
                            Some(DamageSource::Attack(AttackSource::Pool)) => "pool",
                            Some(DamageSource::Buff(_)) => "buff",
                            Some(DamageSource::Other) => "other",
                            Some(DamageSource::Falling) => "fall",
                            None => "unknown",
                        };

                        data.gameplay_metrics
                            .entity_kills_by_type
                            .with_label_values(&[body_type, weapon])
                            .inc();

                        // quantize the kill locations to a grid size that won't
                        // create a million prometheus series - in other words,
                        // round down to the nearest 1000th block
                        if let Some(pos) = data.positions.get(ev.entity) {
                            const QUANTIZE: i32 = 1000;
                            let wpos = pos.0.xy();

                            let x = ((wpos.x.floor() as i32).div_euclid(QUANTIZE) * QUANTIZE)
                                .to_string();
                            let y = ((wpos.y.floor() as i32).div_euclid(QUANTIZE) * QUANTIZE)
                                .to_string();

                            data.gameplay_metrics
                                .entity_kills_by_location
                                .with_label_values(&[body_type, &x, &y])
                                .inc();
                        }
                    }
                }
            }

            // Remove components that should not persist across death
            data.melees.remove(ev.entity);
            data.beams.remove(ev.entity);

            let get_attacker_name = |cause_of_death: KillType, by: Uid| -> KillSource {
                // Get attacker entity
                if let Some(char_entity) = data.id_maps.uid_entity(by) {
                    // Check if attacker is another player or entity with stats (npc)
                    if data.players.contains(char_entity) {
                        KillSource::Player(by, cause_of_death)
                    } else if let Some(stats) = data.stats.get(char_entity) {
                        KillSource::NonPlayer(stats.name.clone(), cause_of_death)
                    } else {
                        KillSource::NonExistent(cause_of_death)
                    }
                } else {
                    KillSource::NonExistent(cause_of_death)
                }
            };

            // Push an outcome if entity is has a character state (entities that don't have
            // one, we probably don't care about emitting death outcome)
            if is_kill
                && let Some((pos, _)) = (&data.positions, &data.character_states)
                    .lend_join()
                    .get(ev.entity, &data.entities)
            {
                outcomes_emitter.emit(Outcome::Death { pos: pos.0 });
            }

            let mut should_delete = true;

            // Handle any effects on death
            if is_kill && let Some(killed_stats) = data.stats.get(ev.entity) {
                let attacker_entity = ev.cause.by.and_then(|x| data.id_maps.uid_entity(x.uid()));
                let attacker_dir = attacker_entity
                    .and_then(|a| data.positions.get(a))
                    .map(|p| p.0)
                    .zip(data.positions.get(ev.entity).map(|p| p.0))
                    .and_then(|(pos_a, pos_t)| Dir::from_unnormalized(pos_a - pos_t))
                    .unwrap_or_default();
                let damage_dealt = ev.cause.amount.abs();
                let attack_source = ev.cause.cause.and_then(|c| {
                    if let DamageSource::Attack(attack) = c {
                        Some(attack)
                    } else {
                        None
                    }
                });

                let mut death_effects = data
                    .death_effects
                    .remove(ev.entity)
                    .map(|ef| ef.0.into_iter().map(Cow::Owned));

                for effect in killed_stats
                    .effects_on_death
                    .iter()
                    .map(Cow::Borrowed)
                    .chain(death_effects.as_mut().map_or(
                        &mut core::iter::empty() as &mut dyn Iterator<Item = Cow<StatEffect>>,
                        |death_effects| death_effects as &mut dyn Iterator<Item = Cow<StatEffect>>,
                    ))
                {
                    let dir = match effect.target {
                        StatEffectTarget::Target => -attacker_dir,
                        StatEffectTarget::Attacker => attacker_dir,
                    };

                    let dmg_contrib = data.uids.get(ev.entity).map(|uid| {
                        DamageContributor::new(*uid, data.groups.get(ev.entity).copied())
                    });

                    let (effect_target, other_entity) = match effect.target {
                        StatEffectTarget::Target => (ev.entity, attacker_entity),
                        StatEffectTarget::Attacker => {
                            if let Some(attacker) = attacker_entity {
                                (attacker, Some(ev.entity))
                            } else {
                                continue;
                            }
                        },
                    };

                    let requirements_met = effect.requirements().all(|req| {
                        req.requirement_met(
                            (
                                data.healths.get(effect_target),
                                data.buffs.get(effect_target),
                                data.character_states.get(effect_target),
                                data.orientations.get(effect_target),
                                data.uids.get(effect_target).copied(),
                            ),
                            (
                                Some(ev.entity),
                                data.energies.get(ev.entity),
                                data.combos.get(ev.entity),
                            ),
                            ev.cause.by.map(|x| x.uid()),
                            damage_dealt,
                            &mut emitters,
                            dir,
                            attack_source,
                            None,
                            &mut rng,
                            attacker_entity
                                .and_then(|a| data.stats.get(a))
                                .map(|s| s.character_level),
                            attacker_entity.and_then(|a| data.character_classes.get(a)),
                        )
                    });

                    if requirements_met {
                        let mut strength_modifier = 1.0;
                        for modification in effect.modifications() {
                            modification.apply_mod(
                                data.positions.get(effect_target).map(|x| x.0),
                                data.positions.get(ev.entity).map(|x| x.0),
                                &mut strength_modifier,
                            )
                        }
                        let strength_modifier = strength_modifier;

                        match &effect.effect {
                            CombatEffect::Knockback(kb) => {
                                let char_state = data.character_states.get(effect_target);
                                let impulse = kb.calculate_impulse(
                                    dir,
                                    char_state,
                                    attacker_entity.and_then(|ae| data.stats.get(ae)),
                                ) * strength_modifier;
                                if !impulse.is_approx_zero() {
                                    emitters.emit(KnockbackEvent {
                                        entity: effect_target,
                                        impulse,
                                    });
                                }
                            },
                            CombatEffect::EnergyReward(ec) => {
                                emitters.emit(EnergyChangeEvent {
                                    entity: effect_target,
                                    change: ec
                                        * data
                                            .derived_stats
                                            .get(effect_target)
                                            .map_or(1.0, |d| d.energy_reward_mod)
                                        * strength_modifier
                                        * data
                                            .stats
                                            .get(effect_target)
                                            .map_or(1.0, |s| s.energy_reward_modifier),
                                    reset_rate: false,
                                });
                            },
                            CombatEffect::Buff(b) => {
                                if rng.random::<f32>() < b.chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Add(b.to_buff(
                                            *data.time,
                                            (
                                                data.uids.get(ev.entity).copied(),
                                                data.masses.get(ev.entity),
                                            ),
                                            (
                                                data.stats.get(effect_target),
                                                data.masses.get(effect_target),
                                            ),
                                            damage_dealt,
                                            strength_modifier,
                                            None,
                                        )),
                                    });
                                }
                            },
                            CombatEffect::Lifesteal(l) => {
                                let change = HealthChange {
                                    amount: damage_dealt * l * strength_modifier,
                                    by: dmg_contrib,
                                    cause: None,
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                if change.amount.abs() > Health::HEALTH_EPSILON {
                                    emitters.emit(HealthChangeEvent {
                                        entity: effect_target,
                                        change,
                                    });
                                }
                            },
                            CombatEffect::Poise(p) => {
                                let change = -Poise::apply_poise_reduction(
                                    *p,
                                    data.derived_stats.get(effect_target),
                                    data.character_states.get(effect_target),
                                    data.stats.get(effect_target),
                                ) * strength_modifier
                                    * data
                                        .stats
                                        .get(ev.entity)
                                        .map_or(1.0, |s| s.poise_damage_modifier);
                                if change.abs() > Poise::POISE_EPSILON {
                                    let poise_change = PoiseChange {
                                        amount: change,
                                        impulse: *dir,
                                        by: dmg_contrib,
                                        cause: None,
                                        time: *data.time,
                                    };
                                    emitters.emit(PoiseChangeEvent {
                                        entity: effect_target,
                                        change: poise_change,
                                    });
                                }
                            },
                            CombatEffect::Heal(h) => {
                                let change = HealthChange {
                                    amount: *h * strength_modifier,
                                    by: dmg_contrib,
                                    cause: None,
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                if change.amount.abs() > Health::HEALTH_EPSILON {
                                    emitters.emit(HealthChangeEvent {
                                        entity: effect_target,
                                        change,
                                    });
                                }
                            },
                            CombatEffect::RemoveBuff(buff_change) => {
                                emitters.emit(BuffEvent {
                                    entity: effect_target,
                                    buff_change: buff_change.clone(),
                                });
                            },
                            CombatEffect::Combo(c) => {
                                emitters.emit(ComboChangeEvent {
                                    entity: effect_target,
                                    change: (*c as f32 * strength_modifier).ceil() as i32,
                                });
                            },
                            CombatEffect::AdditionalDamage(damage) => {
                                let change = HealthChange {
                                    amount: -damage_dealt * damage * strength_modifier,
                                    by: dmg_contrib,
                                    cause: Some(DamageSource::Other),
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                emitters.emit(HealthChangeEvent {
                                    entity: effect_target,
                                    change,
                                });
                            },
                            CombatEffect::RefreshBuff(chance, b) => {
                                if rng.random::<f32>() < *chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Refresh(*b),
                                    });
                                }
                            },
                            CombatEffect::SelfBuff(b) => {
                                if rng.random::<f32>() < b.chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Add(b.to_self_buff(
                                            *data.time,
                                            (
                                                data.uids.get(effect_target).copied(),
                                                data.stats.get(effect_target),
                                                data.masses.get(effect_target),
                                            ),
                                            damage_dealt,
                                            strength_modifier,
                                            None,
                                        )),
                                    });
                                }
                            },
                            CombatEffect::Energy(e) => {
                                emitters.emit(EnergyChangeEvent {
                                    entity: effect_target,
                                    change: *e * strength_modifier,
                                    reset_rate: true,
                                });
                            },
                            CombatEffect::Transform {
                                entity_spec,
                                allow_players,
                            } => {
                                if (data.players.get(effect_target).is_none() || *allow_players)
                                    && let Some(tgt_uid) = data.uids.get(effect_target)
                                {
                                    if matches!(effect.target, StatEffectTarget::Target) {
                                        should_delete = false;
                                    }
                                    emitters.emit(TransformEvent {
                                        target_entity: *tgt_uid,
                                        entity_info: {
                                            let Ok(entity_config) = Ron::<EntityConfig>::load(
                                                entity_spec,
                                            )
                                            .inspect_err(|error| {
                                                error!(
                                                    ?entity_spec,
                                                    ?error,
                                                    "Could not load entity configuration for \
                                                     death effect"
                                                )
                                            }) else {
                                                continue;
                                            };

                                            EntityInfo::at(
                                                data.positions
                                                    .get(effect_target)
                                                    .map(|p| p.0)
                                                    .unwrap_or_default(),
                                            )
                                            .with_entity_config(
                                                entity_config.read().clone().into_inner(),
                                                Some(entity_spec),
                                                &mut rng,
                                                None,
                                            )
                                        },
                                        allow_players: *allow_players,
                                        delete_on_failure: false,
                                    });
                                }
                            },
                            CombatEffect::DebuffsVulnerable {
                                mult,
                                scaling,
                                filter_attacker,
                                filter_weapon,
                            } => {
                                if let Some(buffs) = data.buffs.get(effect_target) {
                                    let num_debuffs = buffs.iter_active().flatten().filter(|b| {
                                        let debuff_filter = matches!(b.kind.differentiate(), buff::BuffDescriptor::SimpleNegative);
                                        let attacker_filter = !filter_attacker || matches!(b.source, BuffSource::Character { by, .. } if Some(by) == other_entity.and_then(|e| data.uids.get(e)).copied());
                                        let weapon_filter = filter_weapon.is_none_or(|w| matches!(b.source, BuffSource::Character { tool_kind, .. } if Some(w) == tool_kind));
                                        debuff_filter && attacker_filter && weapon_filter
                                    }).count();
                                    if num_debuffs > 0 {
                                        let change = HealthChange {
                                            amount: -damage_dealt
                                                * scaling.factor(num_debuffs as f32, 1.0)
                                                * mult
                                                * strength_modifier,
                                            by: dmg_contrib,
                                            cause: Some(DamageSource::Other),
                                            magic_source: None,
                                            time: *data.time,
                                            precise: false,
                                            instance: rand::random(),
                                        };
                                        emitters.emit(HealthChangeEvent {
                                            entity: effect_target,
                                            change,
                                        });
                                    }
                                }
                            },
                        }
                    }
                }
            }

            // Chat message
            // If it was a player that died
            if is_kill
                && let Some((uid, _player)) = (&data.uids, &data.players)
                    .lend_join()
                    .get(ev.entity, &data.entities)
            {
                let kill_source = match (ev.cause.cause, ev.cause.by.map(|x| x.uid())) {
                    (Some(DamageSource::Attack(AttackSource::Melee)), Some(by)) => {
                        get_attacker_name(KillType::Melee, by)
                    },
                    (Some(DamageSource::Attack(AttackSource::Projectile)), Some(by)) => {
                        get_attacker_name(KillType::Projectile, by)
                    },
                    (Some(DamageSource::Attack(AttackSource::Explosion)), Some(by)) => {
                        get_attacker_name(KillType::Explosion, by)
                    },
                    (
                        Some(DamageSource::Attack(AttackSource::Beam | AttackSource::Arc)),
                        Some(by),
                    ) => get_attacker_name(KillType::Energy, by),
                    (Some(DamageSource::Buff(buff_kind)), by) => {
                        if let Some(by) = by {
                            get_attacker_name(KillType::Buff(buff_kind), by)
                        } else {
                            KillSource::NonExistent(KillType::Buff(buff_kind))
                        }
                    },
                    (Some(DamageSource::Other), Some(by)) => get_attacker_name(KillType::Other, by),
                    (Some(DamageSource::Falling), _) => KillSource::FallDamage,
                    // HealthSource::Suicide => KillSource::Suicide,
                    _ => KillSource::Other,
                };

                emitters.emit(ChatEvent {
                    msg: comp::UnresolvedChatMsg::death(kill_source, *uid),
                    from_client: false,
                });
                common::telemetry!(
                    "pd",
                    uid = ?uid,
                    cause = ?ev.cause.cause,
                    by = ?ev.cause.by.map(|b| b.uid())
                );
            }

            let mut exp_awards = Vec::<(Entity, f32, Option<Group>)>::new();
            // Award EXP to damage contributors
            //
            // NOTE: Debug logging is disabled by default for this module - to enable it add
            // xindeler_server::events::entity_manipulation=debug to RUST_LOG
            'xp: {
                let Some((entity_skill_set, entity_health, entity_pos)) =
                    (&data.skill_sets, &data.healths, &data.positions)
                        .lend_join()
                        .get(ev.entity, &data.entities)
                else {
                    break 'xp;
                };

                // Calculate the total EXP award for the removal. A banishment
                // pays `RemovalInfo::reward_fraction` of a kill's XP (spec §6)
                // — the same generic `combat_rating`-derived number, scaled,
                // never a separate reward table. No cache means no
                // `Inventory`, hence `DerivedStats::default()`'s rating, 0.0.
                let exp_reward = data
                    .derived_stats
                    .get(ev.entity)
                    .map_or(0.0, |derived| derived.combat_rating)
                    * 20.0
                    * ev.removal.reward_fraction;

                // Mastery's Delta = target_level - caster_level (spec §4);
                // captured by value so the immutable borrow of
                // `data.skill_sets` behind `entity_skill_set` ends here,
                // before the exp-award loop below needs it mutably for each
                // attacker in turn.
                let target_level = entity_skill_set.character_level();

                // Per-attacker (never per-group) damage totals and
                // per-source splits, keyed by the attacking entity's own
                // `Uid` -- mastery tracks personal casting, never the
                // group-XP split below, which can spread the reward to
                // bystanders who dealt no damage at all.
                let mastery_totals: HashMap<Uid, u64> = entity_health
                    .damage_contributions()
                    .map(|(contributor, total)| (damage_contributor_uid(contributor), *total))
                    .collect();
                let mastery_by_source: HashMap<Uid, [u64; MagicSource::COUNT]> = entity_health
                    .damage_contributions_by_source()
                    .map(|(contributor, by_source)| {
                        (damage_contributor_uid(contributor), *by_source)
                    })
                    .collect();

                let mut damage_contributors = HashMap::<DamageContrib, (u64, f32)>::new();
                for (damage_contributor, damage) in entity_health.damage_contributions() {
                    match damage_contributor {
                        DamageContributor::Solo(uid) => {
                            if let Some(attacker) = data.id_maps.uid_entity(*uid) {
                                damage_contributors
                                    .insert(DamageContrib::Solo(attacker), (*damage, 0.0));
                            } else {
                                // An entity who was not in a group contributed damage but is now
                                // either dead or offline. Add a
                                // placeholder to ensure that the contributor's
                                // exp is discarded, not distributed between
                                // the other contributors
                                damage_contributors.insert(DamageContrib::NotFound, (*damage, 0.0));
                            }
                        },
                        DamageContributor::Group {
                            entity_uid: _,
                            group,
                        } => {
                            // Damage made by entities who were in a group at the time of attack is
                            // attributed to their group rather than themselves. This allows for all
                            // members of a group to receive EXP, not just the damage dealers.
                            let entry = damage_contributors
                                .entry(DamageContrib::Group(*group))
                                .or_insert((0, 0.0));
                            entry.0 += damage;
                        },
                    }
                }

                // A banishment is not gated on the target's current health, so
                // it can land on a creature nobody ever hit. The
                // damage-proportional split has nothing to divide in that case
                // (`total_damage == 0` would make every percentage `NaN`), so
                // credit the banisher with the whole already-scaled reward.
                if !is_kill && damage_contributors.is_empty() {
                    let contrib = match ev.cause.by {
                        Some(DamageContributor::Solo(uid)) => {
                            data.id_maps.uid_entity(uid).map(DamageContrib::Solo)
                        },
                        Some(DamageContributor::Group { group, .. }) => {
                            Some(DamageContrib::Group(group))
                        },
                        None => None,
                    };
                    if let Some(contrib) = contrib {
                        damage_contributors.insert(contrib, (1, 0.0));
                    }
                }

                // Calculate the percentage of total damage that each DamageContributor
                // contributed
                let total_damage: f64 = damage_contributors
                    .values()
                    .map(|(damage, _)| *damage as f64)
                    .sum();
                damage_contributors
                    .iter_mut()
                    .for_each(|(_, (damage, percentage))| {
                        *percentage = (*damage as f64 / total_damage) as f32
                    });

                let destroyed_group = data.groups.get(ev.entity);

                let within_range = |attacker_pos: &Pos| {
                    // Maximum distance that an attacker must be from an entity at the time of its
                    // death to receive EXP for the kill
                    const MAX_EXP_DIST: f32 = 150.0;
                    entity_pos.0.distance_squared(attacker_pos.0) < MAX_EXP_DIST.powi(2)
                };

                let is_pvp_kill = |attacker: Entity| {
                    data.players.contains(ev.entity) && data.players.contains(attacker)
                };

                // Iterate through all contributors of damage for the killed entity, calculating
                // how much EXP each contributor should be awarded based on their
                // percentage of damage contribution
                exp_awards = damage_contributors.iter().filter_map(|(damage_contributor, (_, damage_percent))| {
                let contributor_exp = exp_reward * damage_percent;
                match damage_contributor {
                    DamageContrib::Solo(attacker) => {
                        // No exp for self kills or PvP
                        if *attacker == ev.entity || is_pvp_kill(*attacker) { return None; }

                        // Only give EXP to the attacker if they are within EXP range of the killed entity
                        data.positions.get(*attacker).and_then(|attacker_pos| {
                            if within_range(attacker_pos) {
                                debug!("Awarding {} exp to individual {:?} who contributed {}% damage to the kill of {:?}", contributor_exp, attacker, *damage_percent * 100.0, ev.entity);
                                Some(iter::once((*attacker, contributor_exp, None)).collect())
                            } else {
                                None
                            }
                        })
                    },
                    DamageContrib::Group(group) => {
                        // Don't give EXP to members in the destroyed entity's group
                        if destroyed_group == Some(group) { return None; }

                        // Only give EXP to members of the group that are within EXP range of the killed entity and aren't a pet
                        let members_in_range = (
                            &data.entities,
                            &data.groups,
                            &data.positions,
                            data.alignments.maybe(),
                            &data.uids,
                        )
                            .join()
                            .filter_map(|(member_entity, member_group, member_pos, alignment, uid)| {
                                if *member_group == *group && within_range(member_pos) && !is_pvp_kill(member_entity) && !matches!(alignment, Some(Alignment::Owned(owner)) if owner != uid) {
                                    Some(member_entity)
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>();

                        if members_in_range.is_empty() { return None; }

                        // Divide EXP reward by square root of number of people in group for group EXP scaling
                        let exp_per_member = contributor_exp / (members_in_range.len() as f32).sqrt();

                        debug!("Awarding {} exp per member of group ID {:?} with {} members which contributed {}% damage to the kill of {:?}", exp_per_member, group, members_in_range.len(), *damage_percent * 100.0, ev.entity);
                        Some(members_in_range.into_iter().map(|entity| (entity, exp_per_member, Some(*group))).collect::<Vec<(Entity, f32, Option<Group>)>>())
                    },
                    DamageContrib::NotFound => {
                        // Discard exp for dead/offline individual damage contributors
                        None
                    }
                }
            }).flatten().collect::<Vec<(Entity, f32, Option<Group>)>>();

                exp_awards.iter().for_each(|(attacker, exp_reward, _)| {
                    // Process the calculated EXP rewards
                    if let Some((
                        mut attacker_skill_set,
                        attacker_uid,
                        attacker_inventory,
                        mut attacker_character_class,
                    )) = (
                        &mut data.skill_sets,
                        &data.uids,
                        &data.inventories,
                        (&mut data.character_classes).maybe(),
                    )
                        .lend_join()
                        .get(*attacker, &data.entities)
                    {
                        handle_exp_gain(
                            *exp_reward,
                            attacker_inventory,
                            &mut attacker_skill_set,
                            attacker_character_class.as_deref_mut(),
                            attacker_uid,
                            &mut outcomes,
                        );

                        // Mastery crediting piggybacks on the same
                        // already-filtered `exp_awards` entry: self-kills and
                        // PvP never reach this closure at all (filtered
                        // above), so mastery inherits that exclusion rather
                        // than re-deriving it.
                        if let (Some(&own_total), Some(own_by_source)) = (
                            mastery_totals.get(attacker_uid),
                            mastery_by_source.get(attacker_uid),
                        ) && let Some(mut mastery) = data.spell_masteries.get_mut(*attacker)
                        {
                            let polyglot_rank = attacker_skill_set
                                .skill_level(Skill::Mage(MageSkill::Polyglot))
                                .unwrap_or(0);
                            grant_kill_mastery(
                                &mut mastery,
                                *exp_reward,
                                own_total,
                                own_by_source,
                                target_level,
                                attacker_skill_set.character_level(),
                                polyglot_rank,
                            );
                        }
                    }
                });
            };

            // A banished creature is never deleted — it is coming back, and the
            // same ECS entity is what `banishment::maintain` parks and
            // un-parks. `&=` (not `&&`) on purpose: the loot/reward branch
            // below must still run.
            should_delete &= is_kill;
            should_delete &= if data.clients.contains(ev.entity) {
                if let Some(vel) = data.velocities.get_mut(ev.entity) {
                    vel.0 = Vec3::zero();
                }
                if let Some(force_update) = data.force_updates.get_mut(ev.entity) {
                    force_update.update();
                }
                if let Some(mut energy) = data.energies.get_mut(ev.entity) {
                    energy.refresh();
                }
                if let Some(mut character_state) = data.character_states.get_mut(ev.entity) {
                    *character_state = CharacterState::default();
                }

                // N27-O: a player's own death never deletes their entity
                // (it resets `CharacterState` and waits for respawn, as
                // above) -- so it never reaches this handler's own
                // `DeleteEvent` funnel, and a dying Warlock's Cadena
                // summons would otherwise be orphaned rather than freed.
                // Dismiss them explicitly, the same way
                // `player::dismiss_active_chain_summons` does for a
                // logout, routing through the SAME `DeleteEvent` funnel so
                // `handle_delete` frees the ledger identically either way.
                if is_kill {
                    for summon_entity in
                        summons_to_dismiss(data.summons.get(ev.entity), &data.id_maps)
                    {
                        emitters.emit(DeleteEvent(summon_entity));
                    }
                }

                false
            } else {
                if let Some((_agent, pos, alignment, vel)) = (
                    &data.agents,
                    &data.positions,
                    data.alignments.maybe(),
                    data.velocities.maybe(),
                )
                    .lend_join()
                    .get(ev.entity, &data.entities)
                {
                    // Only drop loot if entity has agency (not a player),
                    // and if it is not owned by another entity (not a pet)
                    if !matches!(alignment, Some(Alignment::Owned(_)))
                        && let Some(items) = if is_kill {
                            data.item_drops
                                .remove(ev.entity)
                                .map(|comp::ItemDrops(item)| item)
                        } else if let Some(comp::ItemDrops(remaining)) =
                            data.item_drops.get_mut(ev.entity)
                        {
                            // A banishment yields `reward_fraction` of the loot
                            // entries and the creature keeps the rest — it is
                            // coming back, and killing it later must still pay
                            // out. Deliberately NOT `remove()`: dropping the
                            // whole component would leave a returned creature
                            // lootless forever.
                            //
                            // Per **entry**, chosen over per-item-count on
                            // purpose. `ItemDrops(Vec<(u32, Item)>)` pairs a
                            // stack count with an item; rolling each count down
                            // by `reward_fraction` would round every 1–3-count
                            // entry to zero, i.e. a quarter-reward would drop
                            // *nothing* off most tables. Rolling per entry
                            // keeps stacks intact and matches the shipped
                            // contract on `RemovalInfo::reward_fraction`
                            // ("applied to … each loot entry's chance to
                            // drop"). The cost is variance: a single-entry
                            // table pays everything or nothing. Expected value
                            // is exactly `reward_fraction` either way.
                            let fraction = ev.removal.reward_fraction;
                            let (dropped, kept): (Vec<_>, Vec<_>) = remaining
                                .drain(..)
                                .partition(|_| rng.random::<f32>() < fraction);
                            *remaining = kept;
                            Some(dropped)
                        } else {
                            None
                        }
                    {
                        // Remove entries where zero exp was awarded - this happens because some
                        // entities like Object bodies don't give EXP.
                        let mut item_receivers = HashMap::new();
                        for (entity, exp, group) in exp_awards {
                            if exp >= f32::EPSILON {
                                let loot_owner = if let Some(group) = group {
                                    Some(LootOwnerKind::Group(group))
                                } else {
                                    let uid = data.bodies.get(entity).and_then(|body| {
                                        // Only humanoids are awarded loot ownership - if the winner
                                        // was a non-humanoid NPC the loot will be free-for-all
                                        if matches!(body, Body::Humanoid(_)) {
                                            data.uids.get(entity).copied()
                                        } else {
                                            None
                                        }
                                    });

                                    uid.map(LootOwnerKind::Player)
                                };

                                *item_receivers.entry(loot_owner).or_insert(0.0) += exp;
                            }
                        }

                        let mut item_offset_spiral =
                            Spiral2d::new().map(|offset| offset.as_::<f32>() * 0.5);

                        let mut rng = rand::rng();
                        let mut spawn_item = |item, loot_owner| {
                            let offset = item_offset_spiral.next().unwrap_or_default();
                            emitters.emit(CreateItemDropEvent {
                                pos: Pos(pos.0 + Vec3::unit_z() * 0.25 + offset),
                                vel: vel.copied().unwrap_or(comp::Vel(Vec3::zero())),
                                ori: comp::Ori::from(Dir::random_2d(&mut rng)),
                                item: PickupItem::new(item, *data.program_time, false),
                                loot_owner: if let Some(loot_owner) = loot_owner {
                                    debug!(
                                        "Assigned UID {loot_owner:?} as the winner for the loot \
                                         drop"
                                    );
                                    Some(LootOwner::new(loot_owner, false, ONWERSHIP_TIMEOUT_SLOW))
                                } else {
                                    debug!("No loot owner");
                                    None
                                },
                            })
                        };

                        if item_receivers.is_empty() {
                            debug!("No item receivers");
                            for item in flatten_counted_items(&items, &data.ability_map, &data.msm)
                            {
                                spawn_item(item, None)
                            }
                        } else {
                            let mut rng = rand::rng();
                            distribute_many(
                                item_receivers
                                    .iter()
                                    .map(|(loot_owner, weight)| (*weight, *loot_owner)),
                                &mut rng,
                                &items,
                                |(amount, _)| *amount,
                                |(_, item), loot_owner, count| {
                                    for item in
                                        item.stacked_duplicates(&data.ability_map, &data.msm, count)
                                    {
                                        spawn_item(item, loot_owner)
                                    }
                                },
                            );
                        }
                    }
                }
                true
            };
            if !should_delete {
                let resists_durability =
                    data.positions
                        .get(ev.entity)
                        .cloned()
                        .is_some_and(|our_pos| {
                            let our_pos = our_pos.0.map(|i| i as i32);

                            data.areas_container
                                .areas()
                                .iter()
                                .any(|(_, area)| area.contains_point(our_pos))
                        });

                // Modify durability on all equipped items. Not for a
                // banishment: the creature did not die, so its gear takes no
                // death penalty.
                if is_kill
                    && !resists_durability
                    && let Some(mut inventory) = data.inventories.get_mut(ev.entity)
                {
                    inventory.damage_items(&data.ability_map, &data.msm, *data.time);
                }
            }

            #[cfg(feature = "worldgen")]
            {
                let entity_as_actor = |entity| data.rtsim_actors.get(entity).copied();
                if let Some(actor) = entity_as_actor(ev.entity)
                    // Skip the death hook for rtsim entities if they aren't deleted, otherwise
                    // we'll end up with rtsim respawning an entity that wasn't actually
                    // removed, producing 2 entities having the same ActorId.
                    && should_delete
                {
                    data.rtsim.hook_rtsim_actor_death(
                        &data.world,
                        data.index.as_index_ref(),
                        actor,
                        data.positions.get(ev.entity).map(|p| p.0),
                        ev.cause
                            .by
                            .as_ref()
                            .and_then(
                                |(DamageContributor::Solo(entity_uid)
                                | DamageContributor::Group { entity_uid, .. })| {
                                    data.id_maps.uid_entity(*entity_uid)
                                },
                            )
                            .and_then(entity_as_actor),
                    );
                }
            }

            if should_delete {
                emitters.emit(DeleteEvent(ev.entity));
            }
        }
    }
}

impl ServerEvent for LandOnGroundEvent {
    type SystemData<'a> = (
        Read<'a, Time>,
        Read<'a, EventBus<HealthChangeEvent>>,
        Read<'a, EventBus<PoiseChangeEvent>>,
        ReadStorage<'a, PhysicsState>,
        ReadStorage<'a, CharacterState>,
        ReadStorage<'a, comp::Mass>,
        ReadStorage<'a, comp::DerivedStats>,
        ReadStorage<'a, Stats>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            time,
            health_change_events,
            poise_change_events,
            physic_states,
            character_states,
            masses,
            derived_stats,
            stats,
        ): Self::SystemData<'_>,
    ) {
        let mut health_change_emitter = health_change_events.emitter();
        let mut poise_change_emitter = poise_change_events.emitter();
        for ev in events {
            // HACK: Certain ability movements currently take us above the fall damage
            // threshold in the horizontal axis. This factor dampens velocity in the
            // horizontal axis when applying fall damage.
            let horizontal_damp = 0.5
                + ev.vel
                    .try_normalized()
                    .unwrap_or_default()
                    .dot(Vec3::unit_z())
                    .abs()
                    * 0.5;

            let relative_vel = ev.vel.dot(-ev.surface_normal) * horizontal_damp;
            // The second part of this if statement disables all fall damage when in the
            // water. This was added as a *temporary* fix a bug that causes you to take
            // fall damage while swimming downwards. FIXME: Fix the actual bug and
            // remove the following relevant part of the if statement.
            let falldmg_threshold = 30.0;
            if relative_vel >= falldmg_threshold
                && physic_states
                    .get(ev.entity)
                    .is_none_or(|ps| ps.in_liquid().is_none())
            {
                let reduced_vel =
                    if let Some(CharacterState::DiveMelee(c)) = character_states.get(ev.entity) {
                        (relative_vel + c.static_data.vertical_speed).min(0.0)
                    } else {
                        relative_vel
                    };

                let mass = masses.get(ev.entity).copied().unwrap_or_default();
                let excess_energy = mass.0 * (reduced_vel - falldmg_threshold).powi(2) / 2.0;
                let falldmg = excess_energy / 1000.0;

                // Emit health change
                let damage = Damage {
                    kind: DamageKind::Crushing,
                    value: falldmg,
                };
                let damage_reduction = Damage::compute_damage_reduction(
                    Some(damage),
                    derived_stats.get(ev.entity),
                    stats.get(ev.entity),
                );
                let change = damage.calculate_health_change(
                    damage_reduction,
                    0.0,
                    None,
                    None,
                    0.0,
                    1.0, // crit_damage_mult — unused (no precision on falling damage)
                    1.0,
                    *time,
                    rand::random(),
                    DamageSource::Falling,
                );

                health_change_emitter.emit(HealthChangeEvent {
                    entity: ev.entity,
                    change,
                });

                // Emit poise change
                let poise_damage = -(mass.0 * reduced_vel.powi(2) / 1500.0);
                let poise_change = Poise::apply_poise_reduction(
                    poise_damage,
                    derived_stats.get(ev.entity),
                    character_states.get(ev.entity),
                    stats.get(ev.entity),
                );
                let poise_change = comp::PoiseChange {
                    amount: poise_change,
                    impulse: Vec3::unit_z(),
                    by: None,
                    cause: None,
                    time: *time,
                };
                poise_change_emitter.emit(PoiseChangeEvent {
                    entity: ev.entity,
                    change: poise_change,
                });
            }
        }
    }
}

impl ServerEvent for RespawnEvent {
    type SystemData<'a> = (
        Read<'a, SpawnPoint>,
        WriteStorage<'a, Health>,
        WriteStorage<'a, comp::Combo>,
        WriteStorage<'a, Pos>,
        WriteStorage<'a, comp::PhysicsState>,
        WriteStorage<'a, comp::ForceUpdate>,
        WriteStorage<'a, Heads>,
        ReadStorage<'a, Client>,
        ReadStorage<'a, Hardcore>,
        ReadStorage<'a, comp::Waypoint>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            spawn_point,
            mut healths,
            mut combos,
            mut positions,
            mut physic_states,
            mut force_updates,
            mut heads,
            clients,
            hardcore,
            waypoints,
        ): Self::SystemData<'_>,
    ) {
        for RespawnEvent(entity) in events {
            // Hardcore characters cannot respawn
            if !hardcore.contains(entity) && clients.contains(entity) {
                let respawn_point = waypoints
                    .get(entity)
                    .map(|wp| wp.get_pos())
                    .unwrap_or(spawn_point.0);

                healths.get_mut(entity).map(|mut health| health.revive());
                combos.get_mut(entity).map(|mut combo| combo.reset());
                positions.get_mut(entity).map(|pos| pos.0 = respawn_point);
                heads.get_mut(entity).map(|mut heads| heads.reset());
                physic_states
                    .get_mut(entity)
                    .map(|phys_state| phys_state.reset());
                force_updates
                    .get_mut(entity)
                    .map(|force_update| force_update.update());
            }
        }
    }
}

#[derive(SystemData)]
pub struct ExplosionData<'a> {
    entities: Entities<'a>,
    block_change: Write<'a, BlockChange>,
    scheduled_block_change: WriteExpect<'a, ScheduledBlockChange>,
    settings: Read<'a, Settings>,
    time: Read<'a, Time>,
    id_maps: Read<'a, IdMaps>,
    spatial_grid: Read<'a, CachedSpatialGrid>,
    terrain: ReadExpect<'a, TerrainGrid>,
    event_busses: ReadExplosionEvents<'a>,
    outcomes: Read<'a, EventBus<Outcome>>,
    groups: ReadStorage<'a, Group>,
    auras: ReadStorage<'a, Auras>,
    positions: ReadStorage<'a, Pos>,
    players: ReadStorage<'a, Player>,
    energies: ReadStorage<'a, Energy>,
    combos: ReadStorage<'a, comp::Combo>,
    inventories: ReadStorage<'a, Inventory>,
    /// The cached gear aggregates every damage/poise/evasion formula reads,
    /// instead of re-walking the loadout once per damage instance.
    derived_stats: ReadStorage<'a, comp::DerivedStats>,
    alignments: ReadStorage<'a, Alignment>,
    entered_auras: ReadStorage<'a, EnteredAuras>,
    buffs: ReadStorage<'a, comp::Buffs>,
    stats: ReadStorage<'a, comp::Stats>,
    healths: ReadStorage<'a, Health>,
    bodies: ReadStorage<'a, Body>,
    orientations: ReadStorage<'a, comp::Ori>,
    character_states: ReadStorage<'a, CharacterState>,
    physics_states: ReadStorage<'a, PhysicsState>,
    uids: ReadStorage<'a, Uid>,
    masses: ReadStorage<'a, comp::Mass>,
    character_classes: ReadStorage<'a, CharacterClass>,
    phantom_illusions: ReadStorage<'a, PhantomIllusion>,
}

impl ServerEvent for ExplosionEvent {
    type SystemData<'a> = ExplosionData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let mut emitters = data.event_busses.get_emitters();
        let mut outcome_emitter = data.outcomes.emitter();

        // TODO: Faster RNG?
        let mut rng = rand::rng();

        for ev in events {
            let owner_entity = ev.owner.and_then(|uid| data.id_maps.uid_entity(uid));

            let explosion_volume = 6.25 * ev.explosion.radius;

            emitters.emit(SoundEvent {
                sound: Sound::new(SoundKind::Explosion, ev.pos, explosion_volume, data.time.0),
            });

            let outcome_power = (ev.explosion.radius * 0.25).powi(2);
            outcome_emitter.emit(Outcome::Explosion {
                pos: ev.pos,
                power: outcome_power,
                radius: ev.explosion.radius,
                is_attack: ev
                    .explosion
                    .effects
                    .iter()
                    .any(|e| matches!(e, RadiusEffect::Attack { .. })),
                reagent: ev.explosion.reagent,
            });

            /// Used to get strength of explosion effects as they falloff over
            /// distance
            fn cylinder_sphere_strength(
                sphere_pos: Vec3<f32>,
                radius: f32,
                min_falloff: f32,
                cyl_pos: Vec3<f32>,
                cyl_body: Body,
            ) -> f32 {
                // 2d check
                let horiz_dist = Vec2::<f32>::from(sphere_pos - cyl_pos).distance(Vec2::default())
                    - cyl_body.max_radius();
                // z check
                let half_body_height = cyl_body.height() / 2.0;
                let vert_distance =
                    (sphere_pos.z - (cyl_pos.z + half_body_height)).abs() - half_body_height;

                // Use whichever gives maximum distance as that closer to real value. Sets
                // minimum to 0 as negative values would indicate inside entity.
                let distance = horiz_dist.max(vert_distance).max(0.0);

                if distance > radius {
                    // If further than exploion radius, no strength
                    0.0
                } else {
                    // Falloff inversely proportional to radius
                    let fall_off = ((distance / radius).min(1.0) - 1.0).abs();
                    let min_falloff = min_falloff.clamp(0.0, 1.0);
                    min_falloff + fall_off * (1.0 - min_falloff)
                }
            }

            // TODO: Process terrain destruction first so that entities don't get protected
            // by terrain that gets destroyed?
            'effects: for effect in ev.explosion.effects {
                match effect {
                    RadiusEffect::TerrainDestruction(power, new_color) => {
                        const RAYS: usize = 500;

                        // Prevent block colour changes within the radius of a safe zone aura
                        if data
                            .spatial_grid
                            .0
                            .in_circle_aabr(ev.pos.xy(), SAFE_ZONE_RADIUS)
                            .filter_map(|entity| {
                                data.auras
                                    .get(entity)
                                    .zip(data.positions.get(entity))
                                    .and_then(|(entity_auras, pos)| {
                                        entity_auras
                                            .auras
                                            .iter()
                                            .find(|(_, aura)| {
                                                matches!(aura.aura_kind, aura::AuraKind::Buff {
                                                    kind: BuffKind::Invulnerability,
                                                    source: BuffSource::World,
                                                    ..
                                                })
                                            })
                                            .map(|(_, aura)| (*pos, aura.radius))
                                    })
                            })
                            .any(|(aura_pos, aura_radius)| {
                                ev.pos.distance_squared(aura_pos.0) < aura_radius.powi(2)
                            })
                        {
                            continue 'effects;
                        }

                        // Color terrain
                        let mut touched_blocks = Vec::new();
                        let color_range = power * 2.7;
                        for _ in 0..RAYS {
                            let dir = Vec3::new(
                                rng.random::<f32>() - 0.5,
                                rng.random::<f32>() - 0.5,
                                rng.random::<f32>() - 0.5,
                            )
                            .normalized();

                            let _ = data
                                .terrain
                                .ray(ev.pos, ev.pos + dir * color_range)
                                .until(|_| rng.random::<f32>() < 0.05)
                                .for_each(|_: &Block, pos| touched_blocks.push(pos))
                                .cast();
                        }

                        for block_pos in touched_blocks {
                            if let Ok(block) = data.terrain.get(block_pos) {
                                if !matches!(block.kind(), BlockKind::Lava | BlockKind::GlowingRock)
                                    && (
                                        // Check that owner is not player or explosion_burn_marks by
                                        // players
                                        // is enabled
                                        owner_entity.is_none_or(|e| data.players.get(e).is_none())
                                            || data.settings.gameplay.explosion_burn_marks
                                    )
                                {
                                    let diff2 =
                                        block_pos.map(|b| b as f32).distance_squared(ev.pos);
                                    let fade = (1.0 - diff2 / color_range.powi(2)).max(0.0);
                                    if let Some(mut color) = block.get_color() {
                                        let r = color[0] as f32
                                            + (fade
                                                * (color[0] as f32 * 0.5 - color[0] as f32
                                                    + new_color[0]));
                                        let g = color[1] as f32
                                            + (fade
                                                * (color[1] as f32 * 0.3 - color[1] as f32
                                                    + new_color[1]));
                                        let b = color[2] as f32
                                            + (fade
                                                * (color[2] as f32 * 0.3 - color[2] as f32
                                                    + new_color[2]));
                                        // Darken blocks, but not too much
                                        color[0] = (r as u8).max(30);
                                        color[1] = (g as u8).max(30);
                                        color[2] = (b as u8).max(30);
                                        data.block_change
                                            .set(block_pos, Block::new(block.kind(), color));
                                    }
                                }

                                if block.is_bonkable() {
                                    emitters.emit(BonkEvent {
                                        pos: block_pos.map(|e| e as f32 + 0.5),
                                        owner: ev.owner,
                                        target: None,
                                    });
                                }
                            }
                        }

                        // Destroy terrain
                        for _ in 0..RAYS {
                            let dir = Vec3::new(
                                rng.random::<f32>() - 0.5,
                                rng.random::<f32>() - 0.5,
                                rng.random::<f32>() - 0.15,
                            )
                            .normalized();

                            let mut ray_energy = power;

                            let from = ev.pos;
                            let to = ev.pos + dir * power;
                            let _ = data
                                .terrain
                                .ray(from, to)
                                .while_(|block: &Block| {
                                    ray_energy -= block.explode_power().unwrap_or(0.0)
                                        + rng.random::<f32>() * 0.1;

                                    // Stop if:
                                    // 1) Block is liquid
                                    // 2) Consumed all energy
                                    // 3) Can't explode block (for example we hit stone wall)
                                    block.is_liquid()
                                        || block.explode_power().is_none()
                                        || ray_energy <= 0.0
                                })
                                .for_each(|block: &Block, pos| {
                                    if block.explode_power().is_some() {
                                        data.block_change.set(pos, block.into_vacant());
                                    }
                                })
                                .cast();
                        }
                    },
                    RadiusEffect::ReplaceTerrain(radius, terrain_replacement_preset) => {
                        const RAY_DENSITY: f32 = 20.0;
                        const RAY_LENGTH: f32 = 50.0;

                        // Prevent block colour changes within the radius of a safe zone aura
                        if data
                            .spatial_grid
                            .0
                            .in_circle_aabr(ev.pos.xy(), SAFE_ZONE_RADIUS)
                            .filter_map(|entity| {
                                data.auras
                                    .get(entity)
                                    .zip(data.positions.get(entity))
                                    .and_then(|(entity_auras, pos)| {
                                        entity_auras
                                            .auras
                                            .iter()
                                            .find(|(_, aura)| {
                                                matches!(aura.aura_kind, aura::AuraKind::Buff {
                                                    kind: BuffKind::Invulnerability,
                                                    source: BuffSource::World,
                                                    ..
                                                })
                                            })
                                            .map(|(_, aura)| (*pos, aura.radius))
                                    })
                            })
                            .any(|(aura_pos, aura_radius)| {
                                ev.pos.distance_squared(aura_pos.0) < aura_radius.powi(2)
                            })
                        {
                            continue 'effects;
                        }

                        // Replace terrain
                        let mut touched_blocks = Vec::new();
                        let height = data
                            .terrain
                            .ray(ev.pos, ev.pos - RAY_LENGTH * Vec3::unit_z())
                            .until(Block::is_solid)
                            .cast()
                            .0;
                        let max_phi = (height / radius).atan();
                        for _ in 0..(RAY_DENSITY * radius.powi(2)) as usize {
                            let phi = rng.random_range(-PI / 2.0..-max_phi);
                            let theta = rng.random_range(0.0..2.0 * PI);
                            let ray = Vec3::new(
                                RAY_LENGTH * phi.cos() * theta.cos(),
                                RAY_LENGTH * phi.cos() * theta.sin(),
                                RAY_LENGTH * phi.sin(),
                            );

                            let _ = data
                                .terrain
                                .ray(ev.pos, ev.pos + ray)
                                .until(Block::is_solid)
                                .for_each(|_: &Block, pos| touched_blocks.push(pos))
                                .cast();
                        }

                        for block_pos in touched_blocks {
                            if let Ok(block) = data.terrain.get(block_pos) {
                                match terrain_replacement_preset {
                                    TerrainReplacementPreset::Lava {
                                        timeout,
                                        timeout_offset,
                                        timeout_chance,
                                    } => {
                                        if !matches!(
                                            block.kind(),
                                            BlockKind::Air
                                                | BlockKind::Water
                                                | BlockKind::Lava
                                                | BlockKind::GlowingRock
                                        ) {
                                            data.block_change.set(
                                                block_pos,
                                                Block::new(BlockKind::Lava, Rgb::new(255, 65, 0)),
                                            );

                                            if rng.random_bool(timeout_chance as f64) {
                                                let current_time: f64 = data.time.0;
                                                let replace_time = current_time
                                                    + (timeout
                                                        + rng.random_range(0.0..timeout_offset))
                                                        as f64;
                                                data.scheduled_block_change.set(
                                                    block_pos,
                                                    Block::new(
                                                        BlockKind::Rock,
                                                        Rgb::new(12, 10, 25),
                                                    ),
                                                    replace_time,
                                                );
                                            }
                                        }
                                    },
                                }
                            }
                        }
                    },
                    RadiusEffect::Attack { attack, dodgeable } => {
                        for (
                            entity_b,
                            pos_b,
                            health_b,
                            (
                                body_b_maybe,
                                ori_b_maybe,
                                char_state_b_maybe,
                                physics_state_b_maybe,
                                uid_b,
                            ),
                        ) in (
                            &data.entities,
                            &data.positions,
                            &data.healths,
                            (
                                data.bodies.maybe(),
                                data.orientations.maybe(),
                                data.character_states.maybe(),
                                data.physics_states.maybe(),
                                &data.uids,
                            ),
                        )
                            .join()
                            .filter(|(_, _, h, _)| !h.is_dead)
                        {
                            let pos_b = Pos(pos_b.0
                                + Vec3::unit_z() * body_b_maybe.map_or(0.5, |b| b.height() / 2.0));
                            let dist_sqrd = ev.pos.distance_squared(pos_b.0);

                            // Check if it is a hit
                            let strength = if let Some(body) = body_b_maybe {
                                cylinder_sphere_strength(
                                    ev.pos,
                                    ev.explosion.radius,
                                    ev.explosion.min_falloff,
                                    pos_b.0,
                                    *body,
                                )
                            } else {
                                1.0 - dist_sqrd / ev.explosion.radius.powi(2)
                            };

                            // Cast a ray from the explosion to the entity to check visibility
                            if strength > 0.0
                                && (data
                                    .terrain
                                    .ray(ev.pos, pos_b.0)
                                    .until(Block::is_opaque)
                                    .cast()
                                    .0
                                    + 0.1)
                                    .powi(2)
                                    >= dist_sqrd
                            {
                                // See if entities are in the same group
                                let same_group = owner_entity
                                    .and_then(|e| data.groups.get(e))
                                    .map(|group_a| Some(group_a) == data.groups.get(entity_b))
                                    .unwrap_or(Some(entity_b) == owner_entity);

                                let target_group = if same_group {
                                    GroupTarget::InGroup
                                } else {
                                    GroupTarget::OutOfGroup
                                };

                                let dir = Dir::new(
                                    (pos_b.0 - ev.pos)
                                        .try_normalized()
                                        .unwrap_or_else(Vec3::unit_z),
                                );

                                let attacker_info =
                                    owner_entity.zip(ev.owner).map(|(entity, uid)| {
                                        combat::AttackerInfo {
                                            entity,
                                            uid,
                                            group: data.groups.get(entity),
                                            energy: data.energies.get(entity),
                                            combo: data.combos.get(entity),
                                            derived: data.derived_stats.get(entity),
                                            stats: data.stats.get(entity),
                                            mass: data.masses.get(entity),
                                            pos: data.positions.get(entity).map(|p| p.0),
                                            buffs: data.buffs.get(entity),
                                            character_class: data.character_classes.get(entity),
                                        }
                                    });

                                let target_info = combat::TargetInfo {
                                    entity: entity_b,
                                    uid: *uid_b,
                                    inventory: data.inventories.get(entity_b),
                                    derived: data.derived_stats.get(entity_b),
                                    stats: data.stats.get(entity_b),
                                    health: Some(health_b),
                                    pos: pos_b.0,
                                    ori: ori_b_maybe,
                                    char_state: char_state_b_maybe,
                                    energy: data.energies.get(entity_b),
                                    buffs: data.buffs.get(entity_b),
                                    mass: data.masses.get(entity_b),
                                    player: data.players.get(entity_b),
                                    phantom_illusion: data
                                        .phantom_illusions
                                        .get(entity_b)
                                        .is_some(),
                                };

                                // Check if entity is dodging
                                let target_dodging = match dodgeable {
                                    Dodgeable::Roll => char_state_b_maybe
                                        .and_then(|cs| cs.roll_attack_immunities())
                                        .is_some_and(|i| i.melee),
                                    Dodgeable::Jump => physics_state_b_maybe
                                        .is_some_and(|ps| ps.on_ground.is_none()),
                                    Dodgeable::No => false,
                                };
                                let allow_friendly_fire =
                                    owner_entity.is_some_and(|owner_entity| {
                                        combat::allow_friendly_fire(
                                            &data.entered_auras,
                                            owner_entity,
                                            entity_b,
                                        )
                                    });
                                // PvP check
                                let permit_pvp = combat::permit_pvp(
                                    &data.alignments,
                                    &data.players,
                                    &data.entered_auras,
                                    &data.id_maps,
                                    owner_entity,
                                    entity_b,
                                );
                                let attack_options = combat::AttackOptions {
                                    target_dodging,
                                    permit_pvp,
                                    allow_friendly_fire,
                                    target_group,
                                    precision_mult: None,
                                };

                                attack.apply_attack(
                                    attacker_info,
                                    &target_info,
                                    dir,
                                    attack_options,
                                    strength,
                                    combat::AttackSource::Explosion,
                                    *data.time,
                                    &mut emitters,
                                    |o| outcome_emitter.emit(o),
                                    &mut rng,
                                    0,
                                );
                            }
                        }
                    },
                    RadiusEffect::PooledDebuff(combat::PooledDebuff {
                        pool,
                        buff,
                        ability_info,
                    }) => {
                        // A pool-selected target is always affected -- see
                        // `PooledDebuff::buff`'s own doc comment. `chance`
                        // is never consulted below, so a future spell
                        // reusing this primitive with `chance < 1.0`
                        // (expecting an extra resist roll on top of the
                        // pool) would silently always land instead.
                        debug_assert!(
                            buff.chance >= 1.0,
                            "RadiusEffect::PooledDebuff ignores CombatBuff::chance; author it as \
                             1.0 or add a roll before calling to_buff below"
                        );
                        // Gather every living, eligible target inside the
                        // sphere first -- whether one target is affected
                        // depends on which other targets already consumed
                        // from the shared pool, so this can't be resolved
                        // as each entity is visited independently the way
                        // `Attack`/`Entity` above are. Containment check
                        // mirrors `RadiusEffect::Entity` above (no LOS
                        // raycast -- this isn't a directed attack).
                        let mut candidates: Vec<(Entity, f32)> = Vec::new();
                        for (entity_b, pos_b, health_b, body_b_maybe) in (
                            &data.entities,
                            &data.positions,
                            &data.healths,
                            data.bodies.maybe(),
                        )
                            .join()
                            .filter(|(_, _, health, _)| !health.is_dead)
                        {
                            let strength = if let Some(body) = body_b_maybe {
                                cylinder_sphere_strength(
                                    ev.pos,
                                    ev.explosion.radius,
                                    ev.explosion.min_falloff,
                                    pos_b.0,
                                    *body,
                                )
                            } else {
                                let dist_sqrd = ev.pos.distance_squared(pos_b.0);
                                1.0 - dist_sqrd / ev.explosion.radius.powi(2)
                            };
                            // Unlike `RadiusEffect::Attack`/`Entity`, `strength` is used only as
                            // a binary containment gate here, never threaded into a graduated
                            // magnitude -- a "50%-applied" sleep debuff has no obvious meaning.
                            // `min_falloff` still reshapes the containment boundary (a smaller
                            // effective radius near the falloff edge), it just doesn't scale
                            // anything past that.
                            if strength <= 0.0 {
                                continue;
                            }

                            // Same group/PvP gating a normal Attack would
                            // apply -- a debuff this strong shouldn't
                            // bypass friendly-fire rules just because it
                            // isn't routed through `Attack`.
                            let same_group = owner_entity
                                .and_then(|e| data.groups.get(e))
                                .map(|group_a| Some(group_a) == data.groups.get(entity_b))
                                .unwrap_or(Some(entity_b) == owner_entity);
                            let allow_friendly_fire = owner_entity.is_some_and(|owner_entity| {
                                combat::allow_friendly_fire(
                                    &data.entered_auras,
                                    owner_entity,
                                    entity_b,
                                )
                            });
                            if same_group && !allow_friendly_fire {
                                continue;
                            }
                            let permit_pvp = combat::permit_pvp(
                                &data.alignments,
                                &data.players,
                                &data.entered_auras,
                                &data.id_maps,
                                owner_entity,
                                entity_b,
                            );
                            if !permit_pvp {
                                continue;
                            }

                            candidates.push((entity_b, health_b.current()));
                        }

                        for entity_b in combat::resolve_pooled_debuff_targets(candidates, pool) {
                            emitters.emit(BuffEvent {
                                entity: entity_b,
                                buff_change: buff::BuffChange::Add(buff.to_buff(
                                    *data.time,
                                    (ev.owner, owner_entity.and_then(|e| data.masses.get(e))),
                                    (data.stats.get(entity_b), data.masses.get(entity_b)),
                                    // `damage` is a no-op for `CombatBuffStrength::Value` (what
                                    // every `PooledDebuff` spell should author -- there's no
                                    // damage roll here to scale off of). A future spell
                                    // configured with `DamageFraction` would silently compute a
                                    // strength of 0.0; that combination isn't supported by this
                                    // resolution path.
                                    0.0,
                                    1.0,
                                    ability_info,
                                )),
                            });
                        }
                    },
                    RadiusEffect::Entity(mut effect) => {
                        for (entity_b, pos_b, body_b_maybe) in
                            (&data.entities, &data.positions, data.bodies.maybe()).join()
                        {
                            let strength = if let Some(body) = body_b_maybe {
                                cylinder_sphere_strength(
                                    ev.pos,
                                    ev.explosion.radius,
                                    ev.explosion.min_falloff,
                                    pos_b.0,
                                    *body,
                                )
                            } else {
                                let distance_squared = ev.pos.distance_squared(pos_b.0);
                                1.0 - distance_squared / ev.explosion.radius.powi(2)
                            };

                            // Player check only accounts for PvP/PvE flag (unless in a friendly
                            // fire aura), but bombs are intented to do
                            // friendly fire.
                            //
                            // What exactly is friendly fire is subject to discussion.
                            // As we probably want to minimize possibility of being dick
                            // even to your group members, the only exception is when
                            // you want to harm yourself.
                            //
                            // This can be changed later.
                            let permit_pvp = || {
                                combat::permit_pvp(
                                    &data.alignments,
                                    &data.players,
                                    &data.entered_auras,
                                    &data.id_maps,
                                    owner_entity,
                                    entity_b,
                                ) || owner_entity.is_none_or(|entity_a| entity_a == entity_b)
                            };
                            if strength > 0.0 {
                                let is_alive =
                                    data.healths.get(entity_b).is_none_or(|h| !h.is_dead);

                                if is_alive {
                                    effect.modify_strength(strength);
                                    if !effect.is_harm() || permit_pvp() {
                                        emit_effect_events(
                                            &mut emitters,
                                            *data.time,
                                            entity_b,
                                            effect.clone(),
                                            ev.owner.map(|owner| {
                                                (
                                                    owner,
                                                    data.id_maps
                                                        .uid_entity(owner)
                                                        .and_then(|e| data.groups.get(e))
                                                        .copied(),
                                                )
                                            }),
                                            data.derived_stats.get(entity_b),
                                            data.character_states.get(entity_b),
                                            data.stats.get(entity_b),
                                            data.masses.get(entity_b),
                                            owner_entity.and_then(|e| data.masses.get(e)),
                                            data.bodies.get(entity_b),
                                            data.positions.get(entity_b),
                                        );
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

pub fn emit_effect_events(
    emitters: &mut (
             impl EmitExt<HealthChangeEvent>
             + EmitExt<PoiseChangeEvent>
             + EmitExt<BuffEvent>
             + EmitExt<ChangeBodyEvent>
             + EmitExt<Outcome>
             + EmitExt<ChangeStanceEvent>
         ),
    time: Time,
    entity: EcsEntity,
    effect: common::effect::Effect,
    source: Option<(Uid, Option<Group>)>,
    // The affected entity's cached gear aggregates, supplying the armour
    // protection and poise resilience this effect is mitigated by. `None`
    // means the entity has no `Inventory`, i.e. no mitigation at all.
    derived: Option<&comp::DerivedStats>,
    char_state: Option<&CharacterState>,
    stats: Option<&Stats>,
    tgt_mass: Option<&comp::Mass>,
    source_mass: Option<&comp::Mass>,
    tgt_body: Option<&Body>,
    tgt_pos: Option<&Pos>,
) {
    let damage_contributor = source.map(|(uid, group)| DamageContributor::new(uid, group));
    match effect {
        common::effect::Effect::Health(change) => {
            emitters.emit(HealthChangeEvent { entity, change })
        },
        common::effect::Effect::Poise(amount) => {
            let amount = Poise::apply_poise_reduction(amount, derived, char_state, stats);
            emitters.emit(PoiseChangeEvent {
                entity,
                change: comp::PoiseChange {
                    amount,
                    impulse: Vec3::zero(),
                    by: damage_contributor,
                    cause: None,
                    time,
                },
            })
        },
        common::effect::Effect::Damage(damage) => {
            let change = damage.calculate_health_change(
                combat::Damage::compute_damage_reduction(Some(damage), derived, stats),
                0.0,
                damage_contributor,
                None,
                0.0,
                1.0, // crit_damage_mult — unused (no precision on this effect path)
                1.0,
                time,
                rand::random(),
                DamageSource::Other,
            );
            emitters.emit(HealthChangeEvent { entity, change })
        },
        common::effect::Effect::Buff(buff) => {
            let dest_info = buff::DestInfo {
                stats,
                mass: tgt_mass,
            };
            emitters.emit(BuffEvent {
                entity,
                buff_change: comp::BuffChange::Add(comp::Buff::new(
                    buff.kind,
                    buff.data,
                    buff.cat_ids,
                    comp::BuffSource::Item,
                    time,
                    dest_info,
                    source_mass,
                    None,
                    None,
                )),
            });
        },
        common::effect::Effect::Permanent(permanent_effect) => match permanent_effect {
            common::effect::PermanentEffect::CycleBodyType => {
                if let Some(body) = tgt_body
                    && let Some(new_body) = match body {
                        Body::Humanoid(body) => Some(Body::Humanoid(comp::humanoid::Body {
                            body_type: match body.body_type {
                                comp::humanoid::BodyType::Female => comp::humanoid::BodyType::Male,
                                comp::humanoid::BodyType::Male => comp::humanoid::BodyType::Female,
                            },
                            ..*body
                        })),
                        // Only allow humanoids for now.
                        _ => None,
                    }
                {
                    // TODO: Change only the body from the character list?
                    emitters.emit(ChangeBodyEvent {
                        entity,
                        new_body,
                        permanent_change: Some(PermanentChange {
                            expected_old_body: *body,
                        }),
                    });
                    if let Some(pos) = tgt_pos {
                        emitters.emit(Outcome::Transformation { pos: pos.0 });
                    }
                }
            },
        },
        common::effect::Effect::Stance(stance) => {
            emitters.emit(ChangeStanceEvent { entity, stance });
        },
    }
}

impl ServerEvent for BonkEvent {
    type SystemData<'a> = (
        Write<'a, BlockChange>,
        ReadExpect<'a, TerrainGrid>,
        ReadExpect<'a, ProgramTime>,
        Read<'a, EventBus<CreateObjectEvent>>,
        Read<'a, EventBus<ShootEvent>>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut block_change, terrain, program_time, create_object_events, shoot_events): Self::SystemData<'_>,
    ) {
        let mut create_object_emitter = create_object_events.emitter();
        let mut shoot_emitter = shoot_events.emitter();
        for ev in events {
            if let Some(_target) = ev.target {
                // TODO: bonk entities but do no damage?
            } else {
                use common::terrain::SpriteKind;
                let pos = ev.pos.map(|e| e.floor() as i32);
                if let Some(block) = terrain.get(pos).ok().copied().filter(|b| b.is_bonkable())
                    && block_change
                        .try_set(pos, block.with_sprite(SpriteKind::Empty))
                        .is_some()
                {
                    let sprite_cfg = terrain.sprite_cfg_at(pos);
                    if let Some(items) = comp::Item::try_reclaim_from_block(block, sprite_cfg) {
                        let msm = &MaterialStatManifest::load().read();
                        let ability_map = &AbilityMap::load().read();
                        for item in flatten_counted_items(&items, ability_map, msm) {
                            let pos = Pos(pos.map(|e| e as f32) + Vec3::new(0.5, 0.5, 0.0));
                            let vel = comp::Vel::default();
                            // TODO: Use the `ItemDrop` body for this.
                            let body = match block.get_sprite() {
                                // Create different containers depending on the original
                                // sprite
                                Some(SpriteKind::Apple) => comp::object::Body::Apple,
                                Some(SpriteKind::Beehive) => comp::object::Body::Hive,
                                Some(SpriteKind::Coconut) => comp::object::Body::Coconut,
                                Some(SpriteKind::Bomb) => comp::object::Body::Bomb,
                                _ => comp::object::Body::Pebble,
                            };

                            if matches!(block.get_sprite(), Some(SpriteKind::Bomb)) {
                                let (projectile, marker) = ProjectileConstructor {
                                    kind: ProjectileConstructorKind::Explosive {
                                        radius: 12.0,
                                        min_falloff: 0.75,
                                        reagent: None,
                                        terrain: Some((4.0, ColorPreset::Black)),
                                        target: ProjectileExplosionTarget::Both,
                                    },
                                    attack: Some(ProjectileAttack {
                                        damage: 40.0,
                                        poise: Some(100.0),
                                        knockback: None,
                                        energy: None,
                                        buff: None,
                                        friendly_fire: true,
                                        blockable: true,
                                        attack_effect: None,
                                        damage_effect: None,
                                        without_combo: false,
                                        damage_kind: DamageKind::Energy,
                                    }),
                                    scaled: None,
                                    homing_rate: None,
                                    split: None,
                                    lifetime_override: None,
                                    limit_per_ability: false,
                                    override_collider: None,
                                    pierce_entities: false,
                                    is_point: true,
                                    is_sticky: true,
                                    hazard: false,
                                }
                                .create_projectile(None, 1.0, None, None);
                                shoot_emitter.emit(ShootEvent {
                                    entity: None,
                                    source_vel: None,
                                    pos,
                                    dir: Dir::from_unnormalized(vel.0).unwrap_or_default(),
                                    body: Body::Object(body),
                                    light: None,
                                    projectile,
                                    speed: vel.0.magnitude(),
                                    object: None,
                                    marker,
                                });
                            } else {
                                create_object_emitter.emit(CreateObjectEvent {
                                    pos,
                                    vel,
                                    body,
                                    object: None,
                                    item: Some(comp::PickupItem::new(item, *program_time, false)),
                                    light_emitter: None,
                                    stats: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

impl ServerEvent for AuraEvent {
    type SystemData<'a> = (WriteStorage<'a, Auras>, WriteStorage<'a, EnteredAuras>);

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut auras, mut entered_auras): Self::SystemData<'_>,
    ) {
        for ev in events {
            use aura::AuraChange;
            match ev.aura_change {
                AuraChange::Add(new_aura) => {
                    if let Some(mut auras) = auras.get_mut(ev.entity) {
                        auras.insert(new_aura);
                    }
                },
                AuraChange::RemoveByKey(keys) => {
                    if let Some(mut auras) = auras.get_mut(ev.entity) {
                        for key in keys {
                            auras.remove(key);
                        }
                    }
                },
                AuraChange::EnterAura(uid, key, variant) => {
                    if let Some(mut entered_auras) = entered_auras.get_mut(ev.entity) {
                        entered_auras
                            .auras
                            .entry(variant)
                            .and_modify(|entered_auras| {
                                entered_auras.insert((uid, key));
                            })
                            .or_insert_with(|| <_ as Into<_>>::into([(uid, key)]));
                    }
                },
                AuraChange::ExitAura(uid, key, variant) => {
                    if let Some(mut entered_auras) = entered_auras.get_mut(ev.entity)
                        && let Some(entered_auras_variant) = entered_auras.auras.get_mut(&variant)
                    {
                        entered_auras_variant.remove(&(uid, key));

                        if entered_auras_variant.is_empty() {
                            entered_auras.auras.remove(&variant);
                        }
                    }
                },
            }
        }
    }
}

#[derive(SystemData)]
pub struct BuffEventData<'a> {
    time: Read<'a, Time>,
    buffs: WriteStorage<'a, comp::Buffs>,
    bodies: ReadStorage<'a, Body>,
    // Written here to grant/clear the temp-HP absorb pool when a `Shielded`
    // buff is added/removed.
    healths: WriteStorage<'a, Health>,
    stats: ReadStorage<'a, Stats>,
    masses: ReadStorage<'a, comp::Mass>,
    id_maps: Read<'a, IdMaps>,
    uids: ReadStorage<'a, Uid>,
    positions: ReadStorage<'a, Pos>,
    /// The cached `combat_rating` both the mind-altering saving throw and the
    /// non-damage mastery credit read, instead of re-folding the target's
    /// loadout, skillset and body per buff application.
    derived_stats: ReadStorage<'a, comp::DerivedStats>,
    /// Presence-only, for the mind-altering saving throw's eligibility guard:
    /// the pre-cache code required `Energy`/`Poise`/`Inventory` (the last
    /// implied here by `derived_stats` -- see the rebuild system, which only
    /// ever builds a cache for an `Inventory`-having entity) before a target
    /// was even eligible to roll a save. Without this, a target missing any
    /// of them would go from "no cache, so `combat_rating` is `0.0`" to
    /// "guaranteed unresisted" silently swapping to "actually rolls a save
    /// with near-zero evasion" -- a real behavior change, not just a
    /// computation-location swap.
    energies: ReadStorage<'a, Energy>,
    poises: ReadStorage<'a, Poise>,
    skill_sets: ReadStorage<'a, SkillSet>,
    groups: ReadStorage<'a, Group>,
    agents: ReadStorage<'a, Agent>,
    outcomes: Read<'a, EventBus<Outcome>>,
    // Read here (not written directly -- crediting happens through a
    // short-lived `get_mut` per landed buff) so a caster with no
    // `SpellMastery` yet (an NPC, a pet, or any entity that predates the
    // component) is simply skipped.
    spell_masteries: WriteStorage<'a, SpellMastery>,
}

impl ServerEvent for BuffEvent {
    type SystemData<'a> = BuffEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, data: Self::SystemData<'_>) {
        let BuffEventData {
            time,
            mut buffs,
            bodies,
            mut healths,
            stats,
            masses,
            id_maps,
            uids,
            positions,
            derived_stats,
            energies,
            poises,
            skill_sets,
            groups,
            agents,
            outcomes,
            mut spell_masteries,
        } = data;
        let mut outcomes_emitter = outcomes.emitter();
        let mut rng = rand::rng();

        for ev in events {
            if let Some(mut buffs) = buffs.get_mut(ev.entity) {
                use buff::BuffChange;
                // BL-05 RD-6: invariant "absorb > 0 ⟺ a Shielded buff is active".
                // The absorb pool is granted on Shielded-add below; here we clear
                // it whenever *any* removal path (dispel, expiry, deplete, and
                // crucially the death `RemoveByCategory`) drops the last Shielded
                // buff — a single post-match check covers every arm uniformly.
                let had_shield = buffs.buffs.values().any(|b| b.kind == BuffKind::Shielded);
                match ev.buff_change {
                    BuffChange::Add(mut new_buff) => {
                        let immunity_by_buff = buffs
                            .buffs
                            .values_mut()
                            .flat_map(|b| b.kind.effects(&b.data, None, None))
                            .find(|b| match b {
                                BuffEffect::BuffImmunity(kind) => new_buff.kind == *kind,
                                _ => false,
                            });

                        let not_immune = !bodies
                            .get(ev.entity)
                            .is_some_and(|body| body.immune_to(new_buff.kind))
                            && immunity_by_buff.is_none()
                            && healths.get(ev.entity).is_none_or(|h| !h.is_dead);

                        // A resisted mind-altering effect (Charmed/Dominated/Maddened/
                        // Paralyzed) rolls against the target's magic resistance once,
                        // here, rather than being applied unconditionally. Any other
                        // buff kind, or a source with no caster entity to roll against,
                        // always hits.
                        let resisted = not_immune
                            && matches!(
                                new_buff.kind,
                                BuffKind::Charmed
                                    | BuffKind::Dominated
                                    | BuffKind::Maddened
                                    | BuffKind::Paralyzed
                            )
                            && 'resist: {
                                let buff::BuffSource::Character { by: caster_uid, .. } =
                                    new_buff.source
                                else {
                                    break 'resist false;
                                };
                                let Some(caster) = id_maps.uid_entity(caster_uid) else {
                                    break 'resist false;
                                };
                                let Some(caster_stats) = stats.get(caster) else {
                                    break 'resist false;
                                };
                                let (
                                    Some(target_uid),
                                    Some(target_body),
                                    Some(target_health),
                                    Some(derived),
                                    Some(_target_energy),
                                    Some(_target_poise),
                                    Some(_target_skill_set),
                                ) = (
                                    uids.get(ev.entity).copied(),
                                    bodies.get(ev.entity).copied(),
                                    healths.get(ev.entity),
                                    derived_stats.get(ev.entity),
                                    energies.get(ev.entity),
                                    poises.get(ev.entity),
                                    skill_sets.get(ev.entity),
                                )
                                else {
                                    break 'resist false;
                                };

                                let tuning = Ron::<combat::CombatTuning>::load_expect(
                                    "common.combat_tuning",
                                )
                                .read();
                                let combat_rating = derived.combat_rating;
                                let target_stats = stats.get(ev.entity);

                                let caster_info = combat::SaveCasterInfo {
                                    magic_accuracy: caster_stats.magic_accuracy,
                                };
                                let target_info = combat::SaveTargetInfo {
                                    stats_magic_evasion: target_stats
                                        .map_or(0.0, |s| s.magic_evasion),
                                    crowd_control_resistance: target_stats
                                        .map_or(0.0, |s| s.crowd_control_resistance),
                                    stats_magic_resistance: target_stats
                                        .map_or(0.0, |s| s.magic_resistance),
                                    magic_resist_tier: target_body.magic_resist_tier(),
                                    combat_rating,
                                };
                                let ctx = combat::SaveCombatContext {
                                    caster_uid,
                                    caster_group: groups.get(caster).copied(),
                                    target_uid,
                                    target_group: groups.get(ev.entity).copied(),
                                    target_hostile_focus: agents
                                        .get(ev.entity)
                                        .and_then(|agent| agent.target)
                                        .filter(|target| target.hostile)
                                        .and_then(|target| {
                                            uids.get(target.target).map(|uid| {
                                                (*uid, groups.get(target.target).copied())
                                            })
                                        }),
                                    target_last_change: Some(&target_health.last_change),
                                    caster_last_change: healths.get(caster).map(|h| &h.last_change),
                                    now: time.0,
                                };
                                let fighting_caster = combat::is_fighting_caster(&ctx);
                                let chance = combat::saving_throw_chance(
                                    &caster_info,
                                    &target_info,
                                    fighting_caster,
                                    &tuning.0,
                                );
                                rng.random::<f32>() >= chance
                            };

                        if resisted {
                            if let Some(target_uid) = uids.get(ev.entity).copied() {
                                outcomes_emitter.emit(Outcome::Resisted {
                                    pos: positions.get(ev.entity).map_or(Vec3::zero(), |pos| pos.0),
                                    target: target_uid,
                                });
                            }
                        } else if not_immune {
                            if let Some(strength) =
                                new_buff.kind.resilience_ccr_strength(new_buff.data)
                            {
                                let resilience_buff = buff::Buff::new(
                                    BuffKind::Resilience,
                                    buff::BuffData::new(
                                        strength,
                                        Some(
                                            new_buff
                                                .data
                                                .duration
                                                .map_or(Secs(30.0), |dur| dur * 5.0),
                                        ),
                                    ),
                                    Vec::new(),
                                    BuffSource::Buff,
                                    *time,
                                    buff::DestInfo {
                                        stats: stats.get(ev.entity),
                                        mass: masses.get(ev.entity),
                                    },
                                    // There is no source entity
                                    None,
                                    // There is no target entity
                                    None,
                                    None,
                                );
                                buffs.insert(resilience_buff, *time);
                            }

                            if bodies
                                .get(ev.entity)
                                .is_some_and(|body| body.negates_buff(new_buff.kind))
                            {
                                new_buff.effects.clear();
                            }

                            // Only one concentration buff at a time: adding a new
                            // concentration buff removes any prior concentration.
                            if new_buff.cat_ids.contains(&BuffCategory::Concentration) {
                                let prior: Vec<_> = buffs
                                    .buffs
                                    .iter()
                                    .filter(|(_, b)| {
                                        b.cat_ids.contains(&BuffCategory::Concentration)
                                    })
                                    .map(|(key, _)| key)
                                    .collect();
                                for key in prior {
                                    buffs.remove(key);
                                }
                            }

                            // Last dominator wins: adding a new Dominated buff
                            // removes any prior one, so exactly one entity is
                            // ever the acting dominator.
                            if new_buff.kind == BuffKind::Dominated {
                                let prior: Vec<_> = buffs
                                    .buffs
                                    .iter()
                                    .filter(|(_, b)| b.kind == BuffKind::Dominated)
                                    .map(|(key, _)| key)
                                    .collect();
                                for key in prior {
                                    buffs.remove(key);
                                }
                            }

                            // Granting a Shielded buff fills the temp-HP absorb pool
                            // (take-higher: a re-cast refreshes rather than stacks; a
                            // future shield *kind* would add).
                            if new_buff.kind == BuffKind::Shielded
                                && let Some(mut health) = healths.get_mut(ev.entity)
                            {
                                health.raise_absorb_to(new_buff.data.strength);
                            }

                            if new_buff.cat_ids.contains(&BuffCategory::WeaponCoating) {
                                buffs.remove_by_category(
                                    vec![BuffCategory::WeaponCoating],
                                    Vec::new(),
                                    Vec::new(),
                                );
                            }

                            // Non-damage mastery credit: `new_buff` is about to
                            // land on `ev.entity` for real (past the immunity
                            // and resist checks above), so if it carries a
                            // magic source and its caster is resolvable and
                            // holds `SpellMastery`, credit it -- the buff
                            // counterpart to `grant_kill_mastery`'s handling of
                            // damage.
                            if let buff::BuffSource::Character { by: caster_uid, .. } =
                                new_buff.source
                                && let Some(source) = new_buff.magic_source
                                && let Some(target_uid) = uids.get(ev.entity).copied()
                                && let Some(caster_entity) = id_maps.uid_entity(caster_uid)
                                && let Some(mut mastery) = spell_masteries.get_mut(caster_entity)
                                && let Some(target_health) = healths.get(ev.entity)
                            {
                                let target_in_combat = target_health
                                    .damaged_recently(*time, MASTERY_RECENT_COMBAT_WINDOW_SECS);
                                // A re-cast of an already-active matching buff
                                // from the same caster is a refresh, not a
                                // fresh landed effect -- spamming the same
                                // short buff on an already-buffed target must
                                // not credit mastery every recast. Computed
                                // before `buffs.insert` below, against the
                                // target's buff state as it stood before this
                                // application.
                                let is_fresh_grant = !buffs.kinds[new_buff.kind]
                                    .as_ref()
                                    .is_some_and(|(keys, _)| {
                                        keys.iter().any(|key| {
                                            buffs.buffs.get(*key).is_some_and(|existing| {
                                                matches!(
                                                    existing.source,
                                                    buff::BuffSource::Character { by, .. }
                                                        if by == caster_uid
                                                )
                                            })
                                        })
                                    });
                                // Finding B: the target's gear/skill/body
                                // five-tuple existed only to re-fold this one
                                // number per landed buff. No cache means no
                                // `Inventory`, hence
                                // `DerivedStats::default()`'s rating, 0.0.
                                let target_combat_rating = derived_stats
                                    .get(ev.entity)
                                    .map_or(0.0, |derived| derived.combat_rating);
                                let polyglot_rank = skill_sets.get(caster_entity).map_or(0, |ss| {
                                    ss.skill_level(Skill::Mage(MageSkill::Polyglot))
                                        .unwrap_or(0)
                                });
                                grant_non_damage_mastery(
                                    &mut mastery,
                                    source,
                                    caster_uid,
                                    target_uid,
                                    target_in_combat,
                                    is_fresh_grant,
                                    target_combat_rating,
                                    polyglot_rank,
                                );
                            }

                            buffs.insert(new_buff, *time);
                        }
                    },
                    BuffChange::RemoveByKey(keys) => {
                        for key in keys {
                            buffs.remove(key);
                        }
                    },
                    BuffChange::RemoveByKind(kind) => {
                        buffs.remove_kind(kind);
                    },
                    BuffChange::RemoveFromController(kind) => {
                        if kind.is_buff() {
                            buffs.remove_kind(kind);
                        }
                    },
                    BuffChange::RemoveByCategory {
                        all_required,
                        any_required,
                        none_required,
                    } => {
                        buffs.remove_by_category(all_required, any_required, none_required);
                    },
                    BuffChange::Refresh(kind) => {
                        buffs
                            .buffs
                            .values_mut()
                            .filter(|b| b.kind == kind)
                            .for_each(|buff| {
                                // Resets buff so that its remaining duration is equal to its
                                // original duration
                                buff.start_time = *time;
                                buff.end_time = buff.data.duration.map(|dur| Time(time.0 + dur.0));
                            })
                    },
                }
                // BL-05 RD-6: if this change dropped the last Shielded buff, empty
                // the absorb pool so it can never linger without its grant (covers
                // dispel/expiry/deplete and the on-death buff strip).
                if had_shield
                    && !buffs.buffs.values().any(|b| b.kind == BuffKind::Shielded)
                    && let Some(mut health) = healths.get_mut(ev.entity)
                {
                    health.clear_absorb();
                }
            }
        }
    }
}

impl ServerEvent for EnergyChangeEvent {
    type SystemData<'a> = WriteStorage<'a, Energy>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut energies: Self::SystemData<'_>) {
        for ev in events {
            if let Some(mut energy) = energies.get_mut(ev.entity) {
                energy.change_by(ev.change);
                if ev.reset_rate {
                    energy.reset_regen_rate();
                }
            }
        }
    }
}

impl ServerEvent for ComboChangeEvent {
    type SystemData<'a> = (
        Read<'a, Time>,
        Read<'a, EventBus<Outcome>>,
        WriteStorage<'a, comp::Combo>,
        ReadStorage<'a, Uid>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (time, outcomes, mut combos, uids): Self::SystemData<'_>,
    ) {
        let mut outcome_emitter = outcomes.emitter();
        for ev in events {
            if let Some(mut combo) = combos.get_mut(ev.entity) {
                combo.change_by(ev.change, time.0);
                if let Some(uid) = uids.get(ev.entity) {
                    outcome_emitter.emit(Outcome::ComboChange {
                        uid: *uid,
                        combo: combo.counter(),
                    });
                }
            }
        }
    }
}

impl ServerEvent for ParryHookEvent {
    type SystemData<'a> = (
        Read<'a, Time>,
        Read<'a, EventBus<EnergyChangeEvent>>,
        Read<'a, EventBus<PoiseChangeEvent>>,
        Read<'a, EventBus<BuffEvent>>,
        WriteStorage<'a, CharacterState>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Stats>,
        ReadStorage<'a, comp::Mass>,
        ReadStorage<'a, comp::DerivedStats>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            time,
            energy_change_events,
            poise_change_events,
            buff_events,
            mut character_states,
            uids,
            stats,
            masses,
            derived_stats,
        ): Self::SystemData<'_>,
    ) {
        let mut energy_change_emitter = energy_change_events.emitter();
        let mut poise_change_emitter = poise_change_events.emitter();
        let mut buff_emitter = buff_events.emitter();
        for ev in events {
            let mut defender_tool = None;

            if let Some(mut char_state) = character_states.get_mut(ev.defender) {
                defender_tool = char_state.ability_info().and_then(|ai| ai.tool);
                let return_to_wield = match &mut *char_state {
                    CharacterState::RiposteMelee(c) => {
                        c.stage_section = StageSection::Action;
                        c.timer = Duration::default();
                        c.whiffed = false;
                        false
                    },
                    CharacterState::BasicBlock(c) => {
                        // Refund half the energy of entering the block for a successful parry
                        energy_change_emitter.emit(EnergyChangeEvent {
                            entity: ev.defender,
                            change: c.static_data.energy_regen,
                            reset_rate: false,
                        });
                        c.is_parry = true;
                        false
                    },
                    _ => false,
                };
                if return_to_wield {
                    *char_state = CharacterState::Wielding(common::states::wielding::Data {
                        is_sneaking: false,
                    });
                }
            };

            if let Some(attacker) = ev.attacker
                && matches!(ev.source, AttackSource::Melee)
            {
                // When attacker is parried, the debuff lasts 2 seconds, the attacker takes
                // poise damage, get precision vulnerability and get slower recovery speed
                let data = buff::BuffData::new(1.0, Some(Secs(2.0)));
                let source = if let Some(uid) = uids.get(ev.defender) {
                    BuffSource::Character {
                        by: *uid,
                        tool_kind: defender_tool,
                    }
                } else {
                    BuffSource::World
                };
                let dest_info = buff::DestInfo {
                    stats: stats.get(attacker),
                    mass: masses.get(attacker),
                };
                let buff = buff::Buff::new(
                    BuffKind::Parried,
                    data,
                    vec![],
                    source,
                    *time,
                    dest_info,
                    masses.get(ev.defender),
                    ev.attacker.and_then(|a| uids.get(a).copied()),
                    None,
                );
                buff_emitter.emit(BuffEvent {
                    entity: attacker,
                    buff_change: buff::BuffChange::Add(buff),
                });

                let attacker_poise_change = Poise::apply_poise_reduction(
                    ev.poise_multiplier.clamp(1.0, 2.0) * BASE_PARRIED_POISE_PUNISHMENT,
                    derived_stats.get(attacker),
                    character_states.get(attacker),
                    stats.get(attacker),
                );

                poise_change_emitter.emit(PoiseChangeEvent {
                    entity: attacker,
                    change: PoiseChange {
                        amount: -attacker_poise_change,
                        impulse: Vec3::zero(),
                        by: uids
                            .get(ev.defender)
                            .map(|d| DamageContributor::new(*d, None)),
                        cause: Some(DamageSource::Attack(ev.source)),
                        time: *time,
                    },
                });
            }
        }
    }
}

impl ServerEvent for TeleportToEvent {
    type SystemData<'a> = (
        Read<'a, IdMaps>,
        WriteStorage<'a, Pos>,
        WriteStorage<'a, comp::ForceUpdate>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (id_maps, mut positions, mut force_updates): Self::SystemData<'_>,
    ) {
        for ev in events {
            let target_pos = id_maps
                .uid_entity(ev.target)
                .and_then(|e| positions.get(e))
                .copied();

            if let (Some(pos), Some(target_pos)) = (positions.get_mut(ev.entity), target_pos)
                && ev
                    .max_range
                    .is_none_or(|r| pos.0.distance_squared(target_pos.0) < r.powi(2))
            {
                *pos = target_pos;
                force_updates
                    .get_mut(ev.entity)
                    .map(|force_update| force_update.update());
            }
        }
    }
}

impl ServerEvent for SetAbilityCooldownEvent {
    type SystemData<'a> = (Read<'a, Time>, WriteStorage<'a, comp::AbilityCooldowns>);

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (time, mut ability_cooldowns): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Some(mut cooldowns) = ability_cooldowns.get_mut(ev.entity) {
                cooldowns.set(&ev.ability_id, *time, ev.cooldown_secs);
            }
        }
    }
}

#[derive(SystemData)]
pub struct EntityAttackedHookData<'a> {
    entities: Entities<'a>,
    trades: Write<'a, Trades>,
    id_maps: Read<'a, IdMaps>,
    time: Read<'a, Time>,
    event_busses: ReadEntityAttackedHookEvents<'a>,
    outcomes: Read<'a, EventBus<Outcome>>,
    character_states: WriteStorage<'a, CharacterState>,
    poises: WriteStorage<'a, Poise>,
    agents: WriteStorage<'a, Agent>,
    positions: ReadStorage<'a, Pos>,
    uids: ReadStorage<'a, Uid>,
    clients: ReadStorage<'a, Client>,
    stats: ReadStorage<'a, Stats>,
    healths: ReadStorage<'a, Health>,
    /// The cached gear aggregates the attacked-hook energy/poise formulas
    /// read, instead of re-walking the affected entity's loadout per effect.
    derived_stats: ReadStorage<'a, comp::DerivedStats>,
    buffs: ReadStorage<'a, comp::Buffs>,
    players: ReadStorage<'a, Player>,
    masses: ReadStorage<'a, comp::Mass>,
    groups: ReadStorage<'a, Group>,
    orientations: ReadStorage<'a, comp::Ori>,
    combos: ReadStorage<'a, comp::Combo>,
    energies: ReadStorage<'a, comp::Energy>,
    character_classes: ReadStorage<'a, CharacterClass>,
}

impl ServerEvent for EntityAttackedHookEvent {
    type SystemData<'a> = EntityAttackedHookData<'a>;

    /// Intended to handle things that should happen for any successful attack,
    /// regardless of the damages and effects specific to that attack
    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let mut emitters = data.event_busses.get_emitters();
        let mut outcomes = data.outcomes.emitter();
        let mut rng = rand::rng();

        for ev in events {
            if let Some(attacker) = ev.attacker {
                emitters.emit(BuffEvent {
                    entity: attacker,
                    buff_change: buff::BuffChange::RemoveByCategory {
                        all_required: vec![buff::BuffCategory::RemoveOnAttack],
                        any_required: vec![],
                        none_required: vec![],
                    },
                });
            }

            if let Some((mut char_state, mut poise, pos)) = (
                &mut data.character_states,
                &mut data.poises,
                &data.positions,
            )
                .lend_join()
                .get(ev.entity, &data.entities)
            {
                // Interrupt sprite interaction and item use if any attack is applied to entity
                if matches!(
                    *char_state,
                    CharacterState::Interact(_) | CharacterState::UseItem(_)
                ) {
                    let poise_state = comp::poise::PoiseState::Interrupted;
                    let was_wielded = char_state.is_wield();
                    if let (Some((stunned_state, stunned_duration)), impulse_strength) =
                        poise_state.poise_effect(was_wielded)
                    {
                        // Reset poise if there is some stunned state to apply
                        poise.reset(*data.time, stunned_duration);
                        if !comp::is_downed(data.healths.get(ev.entity), Some(&char_state)) {
                            *char_state = stunned_state;
                        }
                        outcomes.emit(Outcome::PoiseChange {
                            pos: pos.0,
                            state: poise_state,
                        });
                        if let Some(impulse_strength) = impulse_strength {
                            emitters.emit(KnockbackEvent {
                                entity: ev.entity,
                                impulse: impulse_strength * *poise.knockback(),
                            });
                        }
                    }
                }
            }

            // Remove potion/saturation buff if attacked
            emitters.emit(BuffEvent {
                entity: ev.entity,
                buff_change: buff::BuffChange::RemoveByKind(BuffKind::Potion),
            });
            emitters.emit(BuffEvent {
                entity: ev.entity,
                buff_change: buff::BuffChange::RemoveByKind(BuffKind::Saturation),
            });

            // If entity was in an active trade, cancel it
            if let Some(uid) = data.uids.get(ev.entity)
                && let Some(trade) = data.trades.entity_trades.get(uid).copied()
            {
                data.trades
                    .decline_trade(trade, *uid)
                    .and_then(|uid| data.id_maps.uid_entity(uid))
                    .map(|entity_b| {
                        // Notify both parties that the trade ended
                        let mut notify_trade_party = |entity| {
                            // TODO: Can probably improve UX here for the user that sent the
                            // trade invite, since right now it
                            // may seems like their request was
                            // purposefully declined, rather than e.g. being interrupted.
                            if let Some(client) = data.clients.get(entity) {
                                client.send_fallible(ServerGeneral::FinishedTrade(
                                    TradeResult::Declined,
                                ));
                            }
                            if let Some(agent) = data.agents.get_mut(entity) {
                                agent
                                    .inbox
                                    .push_back(AgentEvent::FinishedTrade(TradeResult::Declined));
                            }
                        };
                        notify_trade_party(ev.entity);
                        notify_trade_party(entity_b);
                    });
            }

            if let Some(stats) = data.stats.get(ev.entity) {
                for effect in &stats.effects_on_damaged {
                    let (effect_target, other_entity) = match effect.target {
                        StatEffectTarget::Target => (ev.entity, ev.attacker),
                        StatEffectTarget::Attacker => {
                            if let Some(attacker) = ev.attacker {
                                (attacker, Some(ev.entity))
                            } else {
                                continue;
                            }
                        },
                    };

                    let dir = match effect.target {
                        StatEffectTarget::Target => ev.attack_dir,
                        StatEffectTarget::Attacker => -ev.attack_dir,
                    };

                    let dmg_contrib = data.uids.get(ev.entity).map(|uid| {
                        DamageContributor::new(*uid, data.groups.get(ev.entity).copied())
                    });

                    let requirements_met = effect.requirements().all(|req| {
                        req.requirement_met(
                            (
                                data.healths.get(effect_target),
                                data.buffs.get(effect_target),
                                data.character_states.get(effect_target),
                                data.orientations.get(effect_target),
                                data.uids.get(effect_target).copied(),
                            ),
                            (
                                Some(ev.entity),
                                data.energies.get(ev.entity),
                                data.combos.get(ev.entity),
                            ),
                            ev.attacker.and_then(|e| data.uids.get(e)).copied(),
                            ev.damage_dealt,
                            &mut emitters,
                            dir,
                            Some(ev.attack_source),
                            None,
                            &mut rng,
                            ev.attacker
                                .and_then(|e| data.stats.get(e))
                                .map(|s| s.character_level),
                            ev.attacker.and_then(|e| data.character_classes.get(e)),
                        )
                    });

                    if requirements_met {
                        let mut strength_modifier = 1.0;
                        for modification in effect.modifications() {
                            modification.apply_mod(
                                data.positions.get(effect_target).map(|x| x.0),
                                data.positions.get(ev.entity).map(|x| x.0),
                                &mut strength_modifier,
                            );
                        }
                        let strength_modifier = strength_modifier;

                        match &effect.effect {
                            CombatEffect::Knockback(kb) => {
                                let char_state = data.character_states.get(effect_target);
                                let impulse = kb.calculate_impulse(
                                    dir,
                                    char_state,
                                    ev.attacker.and_then(|ae| data.stats.get(ae)),
                                ) * strength_modifier;
                                if !impulse.is_approx_zero() {
                                    emitters.emit(KnockbackEvent {
                                        entity: effect_target,
                                        impulse,
                                    });
                                }
                            },
                            CombatEffect::EnergyReward(ec) => {
                                emitters.emit(EnergyChangeEvent {
                                    entity: effect_target,
                                    change: ec
                                        * data
                                            .derived_stats
                                            .get(effect_target)
                                            .map_or(1.0, |d| d.energy_reward_mod)
                                        * strength_modifier
                                        * data
                                            .stats
                                            .get(effect_target)
                                            .map_or(1.0, |s| s.energy_reward_modifier),
                                    reset_rate: false,
                                });
                            },
                            CombatEffect::Buff(b) => {
                                if rng.random::<f32>() < b.chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Add(b.to_buff(
                                            *data.time,
                                            (
                                                data.uids.get(ev.entity).copied(),
                                                data.masses.get(ev.entity),
                                            ),
                                            (
                                                data.stats.get(effect_target),
                                                data.masses.get(effect_target),
                                            ),
                                            ev.damage_dealt,
                                            strength_modifier,
                                            None,
                                        )),
                                    });
                                }
                            },
                            CombatEffect::Lifesteal(l) => {
                                let change = HealthChange {
                                    amount: ev.damage_dealt * l * strength_modifier,
                                    by: dmg_contrib,
                                    cause: None,
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                if change.amount.abs() > Health::HEALTH_EPSILON {
                                    emitters.emit(HealthChangeEvent {
                                        entity: effect_target,
                                        change,
                                    });
                                }
                            },
                            CombatEffect::Poise(p) => {
                                let change = -Poise::apply_poise_reduction(
                                    *p,
                                    data.derived_stats.get(effect_target),
                                    data.character_states.get(effect_target),
                                    data.stats.get(effect_target),
                                ) * strength_modifier
                                    * data
                                        .stats
                                        .get(ev.entity)
                                        .map_or(1.0, |s| s.poise_damage_modifier);
                                if change.abs() > Poise::POISE_EPSILON {
                                    let poise_change = PoiseChange {
                                        amount: change,
                                        impulse: *dir,
                                        by: dmg_contrib,
                                        cause: None,
                                        time: *data.time,
                                    };
                                    emitters.emit(PoiseChangeEvent {
                                        entity: effect_target,
                                        change: poise_change,
                                    });
                                }
                            },
                            CombatEffect::Heal(h) => {
                                let change = HealthChange {
                                    amount: *h * strength_modifier,
                                    by: dmg_contrib,
                                    cause: None,
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                if change.amount.abs() > Health::HEALTH_EPSILON {
                                    emitters.emit(HealthChangeEvent {
                                        entity: effect_target,
                                        change,
                                    });
                                }
                            },
                            CombatEffect::RemoveBuff(buff_change) => {
                                emitters.emit(BuffEvent {
                                    entity: effect_target,
                                    buff_change: buff_change.clone(),
                                });
                            },
                            CombatEffect::Combo(c) => {
                                emitters.emit(ComboChangeEvent {
                                    entity: effect_target,
                                    change: (*c as f32 * strength_modifier).ceil() as i32,
                                });
                            },
                            CombatEffect::AdditionalDamage(damage) => {
                                let change = HealthChange {
                                    amount: -ev.damage_dealt * damage * strength_modifier,
                                    by: dmg_contrib,
                                    cause: Some(DamageSource::Other),
                                    magic_source: None,
                                    time: *data.time,
                                    precise: false,
                                    instance: rand::random(),
                                };
                                emitters.emit(HealthChangeEvent {
                                    entity: effect_target,
                                    change,
                                });
                            },
                            CombatEffect::RefreshBuff(chance, b) => {
                                if rng.random::<f32>() < *chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Refresh(*b),
                                    });
                                }
                            },
                            CombatEffect::SelfBuff(b) => {
                                if rng.random::<f32>() < b.chance {
                                    emitters.emit(BuffEvent {
                                        entity: effect_target,
                                        buff_change: buff::BuffChange::Add(b.to_self_buff(
                                            *data.time,
                                            (
                                                data.uids.get(effect_target).copied(),
                                                data.stats.get(effect_target),
                                                data.masses.get(effect_target),
                                            ),
                                            ev.damage_dealt,
                                            strength_modifier,
                                            None,
                                        )),
                                    });
                                }
                            },
                            CombatEffect::Energy(e) => {
                                emitters.emit(EnergyChangeEvent {
                                    entity: effect_target,
                                    change: *e * strength_modifier,
                                    reset_rate: true,
                                });
                            },
                            CombatEffect::Transform {
                                entity_spec,
                                allow_players,
                            } => {
                                if (data.players.get(effect_target).is_none() || *allow_players)
                                    && let Some(tgt_uid) = data.uids.get(effect_target)
                                {
                                    emitters.emit(TransformEvent {
                                        target_entity: *tgt_uid,
                                        entity_info: {
                                            let Ok(entity_config) = Ron::<EntityConfig>::load(
                                                entity_spec,
                                            )
                                            .inspect_err(|error| {
                                                error!(
                                                    ?entity_spec,
                                                    ?error,
                                                    "Could not load entity configuration for \
                                                     death effect"
                                                )
                                            }) else {
                                                continue;
                                            };

                                            EntityInfo::at(
                                                data.positions
                                                    .get(effect_target)
                                                    .map(|p| p.0)
                                                    .unwrap_or_default(),
                                            )
                                            .with_entity_config(
                                                entity_config.read().clone().into_inner(),
                                                Some(entity_spec),
                                                &mut rng,
                                                None,
                                            )
                                        },
                                        allow_players: *allow_players,
                                        delete_on_failure: false,
                                    });
                                }
                            },
                            CombatEffect::DebuffsVulnerable {
                                mult,
                                scaling,
                                filter_attacker,
                                filter_weapon,
                            } => {
                                if let Some(buffs) = data.buffs.get(effect_target) {
                                    let num_debuffs = buffs.iter_active().flatten().filter(|b| {
                                        let debuff_filter = matches!(b.kind.differentiate(), buff::BuffDescriptor::SimpleNegative);
                                        let attacker_filter = !filter_attacker || matches!(b.source, BuffSource::Character { by, .. } if Some(by) == other_entity.and_then(|e| data.uids.get(e)).copied());
                                        let weapon_filter = filter_weapon.is_none_or(|w| matches!(b.source, BuffSource::Character { tool_kind, .. } if Some(w) == tool_kind));
                                        debuff_filter && attacker_filter && weapon_filter
                                    }).count();
                                    if num_debuffs > 0 {
                                        let change = HealthChange {
                                            amount: -ev.damage_dealt
                                                * scaling.factor(num_debuffs as f32, 1.0)
                                                * mult
                                                * strength_modifier,
                                            by: dmg_contrib,
                                            cause: Some(DamageSource::Other),
                                            magic_source: None,
                                            time: *data.time,
                                            precise: false,
                                            instance: rand::random(),
                                        };
                                        emitters.emit(HealthChangeEvent {
                                            entity: effect_target,
                                            change,
                                        });
                                    }
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

impl ServerEvent for ChangeAbilityEvent {
    type SystemData<'a> = (
        WriteStorage<'a, comp::ActiveAbilities>,
        ReadStorage<'a, Inventory>,
        ReadStorage<'a, SkillSet>,
        // Xindeler: needed to refuse binding a spell whose class-level band
        // the character has not reached.
        ReadStorage<'a, comp::AbilityPool>,
        ReadStorage<'a, comp::CharacterClass>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut active_abilities, inventories, skill_sets, ability_pools, character_classes): Self::SystemData<'_>,
    ) {
        for ev in events {
            // Xindeler: the client picks what goes on the action bar, so a
            // modified one could otherwise bind a spell it cannot yet cast.
            // Drop such a write silently — a well-behaved client never sends
            // it (the Diary makes locked spells undraggable).
            if !comp::ability::may_bind_ability(
                ability_pools.get(ev.entity),
                character_classes.get(ev.entity),
                skill_sets
                    .get(ev.entity)
                    .map_or(1, |skill_set| skill_set.character_level()),
                ev.new_ability,
            ) {
                continue;
            }
            if let Some(mut active_abilities) = active_abilities.get_mut(ev.entity) {
                active_abilities.change_ability(
                    ev.slot,
                    ev.auxiliary_key,
                    ev.new_ability,
                    inventories.get(ev.entity),
                    skill_sets.get(ev.entity),
                );
            }
        }
    }
}

impl ServerEvent for UpdateMapMarkerEvent {
    type SystemData<'a> = (
        Entities<'a>,
        WriteStorage<'a, comp::MapMarker>,
        ReadStorage<'a, Group>,
        ReadStorage<'a, Uid>,
        ReadStorage<'a, Client>,
        ReadStorage<'a, Alignment>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (entities, mut map_markers, groups, uids, clients, alignments): Self::SystemData<'_>,
    ) {
        for ev in events {
            match ev.update {
                comp::MapMarkerChange::Update(waypoint) => {
                    let _ = map_markers.insert(ev.entity, comp::MapMarker(waypoint));
                },
                comp::MapMarkerChange::Remove => {
                    map_markers.remove(ev.entity);
                },
            }
            // Send updated waypoint to group members
            if let Some((group_id, uid)) = (&groups, &uids).lend_join().get(ev.entity, &entities) {
                for client in
                    comp::group::members(*group_id, &groups, &entities, &alignments, &uids)
                        .filter_map(|(e, _)| if e != ev.entity { clients.get(e) } else { None })
                {
                    client.send_fallible(ServerGeneral::MapMarker(
                        comp::MapMarkerUpdate::GroupMember(*uid, ev.update),
                    ));
                }
            }
        }
    }
}

impl ServerEvent for MakeAdminEvent {
    type SystemData<'a> = (WriteStorage<'a, comp::Admin>, ReadStorage<'a, Player>);

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (mut admins, players): Self::SystemData<'_>,
    ) {
        for ev in events {
            if players
                .get(ev.entity)
                .is_some_and(|player| player.uuid() == ev.uuid)
            {
                let _ = admins.insert(ev.entity, ev.admin);
            }
        }
    }
}

impl ServerEvent for ChangeStanceEvent {
    type SystemData<'a> = WriteStorage<'a, comp::Stance>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut stances: Self::SystemData<'_>) {
        for ev in events {
            if let Some(mut stance) = stances.get_mut(ev.entity) {
                *stance = ev.stance;
            }
        }
    }
}

impl ServerEvent for ChangeBodyEvent {
    type SystemData<'a> = (
        WriteExpect<'a, CharacterUpdater>,
        WriteStorage<'a, comp::Body>,
        WriteStorage<'a, comp::Mass>,
        WriteStorage<'a, comp::Density>,
        WriteStorage<'a, comp::Collider>,
        WriteStorage<'a, comp::Stats>,
        ReadStorage<'a, comp::Player>,
        ReadStorage<'a, comp::Presence>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            mut character_updater,
            mut bodies,
            mut masses,
            mut densities,
            mut colliders,
            mut stats,
            players,
            presences,
        ): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Some(mut body) = bodies.get_mut(ev.entity) {
                if let Some(permanent_change) = ev.permanent_change {
                    // If we aren't changing the right body, skip this change.
                    if permanent_change.expected_old_body != *body {
                        continue;
                    }

                    if let Some(mut stats) = stats.get_mut(ev.entity)
                        && stats.original_body == permanent_change.expected_old_body
                    {
                        stats.original_body = ev.new_body;
                    }

                    if let Some(player) = players.get(ev.entity)
                        && let Some(comp::Presence {
                            kind: comp::PresenceKind::Character(character_id),
                            ..
                        }) = presences.get(ev.entity)
                    {
                        character_updater.edit_character(
                            ev.entity,
                            player.uuid().to_string(),
                            *character_id,
                            None,
                            (ev.new_body,),
                            Some(permanent_change),
                        );
                    }
                }

                *body = ev.new_body;
                masses
                    .insert(ev.entity, ev.new_body.mass())
                    .expect("We just got this entities body");
                densities
                    .insert(ev.entity, ev.new_body.density())
                    .expect("We just got this entities body");
                colliders
                    .insert(ev.entity, ev.new_body.collider())
                    .expect("We just got this entities body");
            }
        }
    }
}

impl ServerEvent for RemoveLightEmitterEvent {
    type SystemData<'a> = WriteStorage<'a, comp::LightEmitter>;

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        mut light_emitters: Self::SystemData<'_>,
    ) {
        for ev in events {
            light_emitters.remove(ev.entity);
        }
    }
}

impl ServerEvent for TeleportToPositionEvent {
    type SystemData<'a> = (
        Read<'a, IdMaps>,
        WriteStorage<'a, Is<VolumeRider>>,
        WriteStorage<'a, Pos>,
        WriteStorage<'a, comp::ForceUpdate>,
        ReadStorage<'a, Is<Rider>>,
        ReadStorage<'a, Presence>,
        ReadStorage<'a, Client>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            id_maps,
            mut is_volume_riders,
            mut positions,
            mut force_updates,
            is_riders,
            presences,
            clients,
        ): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Err(error) = crate::state_ext::position_mut(
                ev.entity,
                true,
                |pos| pos.0 = ev.position,
                &id_maps,
                &mut is_volume_riders,
                &mut positions,
                &mut force_updates,
                &is_riders,
                &presences,
                &clients,
            ) {
                warn!(?error, "Failed to teleport entity");
            }
        }
    }
}

impl ServerEvent for StartTeleportingEvent {
    type SystemData<'a> = (
        Read<'a, Time>,
        WriteStorage<'a, comp::Teleporting>,
        ReadStorage<'a, Pos>,
        ReadStorage<'a, comp::Object>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (time, mut teleportings, positions, objects): Self::SystemData<'_>,
    ) {
        for ev in events {
            if let Some(end_time) = (!teleportings.contains(ev.entity))
                .then(|| positions.get(ev.entity))
                .flatten()
                .zip(positions.get(ev.portal))
                .filter(|(entity_pos, portal_pos)| {
                    entity_pos.0.distance_squared(portal_pos.0) <= TELEPORTER_RADIUS.powi(2)
                })
                .and_then(|(_, _)| {
                    Some(
                        time.0
                            + objects.get(ev.portal).and_then(|object| {
                                if let Object::Portal { buildup_time, .. } = object {
                                    Some(buildup_time.0)
                                } else {
                                    None
                                }
                            })?,
                    )
                })
            {
                let _ = teleportings.insert(ev.entity, comp::Teleporting {
                    portal: ev.portal,
                    end_time: Time(end_time),
                });
            }
        }
    }
}

impl ServerEvent for RegrowHeadEvent {
    type SystemData<'a> = (
        Read<'a, EventBus<HealthChangeEvent>>,
        Read<'a, Time>,
        WriteStorage<'a, Heads>,
        ReadStorage<'a, Health>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (health_change_events, time, mut heads, healths): Self::SystemData<'_>,
    ) {
        let mut health_change_emitter = health_change_events.emitter();
        for ev in events {
            if let Some(mut heads) = heads.get_mut(ev.entity)
                && heads.regrow_oldest()
                && let Some(health) = healths.get(ev.entity)
            {
                let amount = 1.0 / (heads.capacity() as f32) * health.maximum();
                health_change_emitter.emit(HealthChangeEvent {
                    entity: ev.entity,
                    change: comp::HealthChange {
                        amount,
                        by: None,
                        cause: Some(DamageSource::Other),
                        magic_source: None,
                        time: *time,
                        precise: false,
                        instance: rand::random(),
                    },
                })
            }
        }
    }
}

pub fn handle_transform(
    server: &mut Server,
    TransformEvent {
        target_entity,
        entity_info,
        allow_players,
        delete_on_failure,
    }: TransformEvent,
) {
    let Some(entity) = server.state().ecs().entity_from_uid(target_entity) else {
        return;
    };

    if let Err(error) = transform_entity(server, entity, entity_info, allow_players) {
        if delete_on_failure
            && !server
                .state()
                .ecs()
                .read_storage::<Client>()
                .contains(entity)
        {
            _ = server.state.delete_entity_recorded(entity);
        }

        error!(?error, ?target_entity, "Failed transform entity");
    }
}

#[derive(Debug)]
pub enum TransformEntityError {
    EntityDead,
    UnexpectedSpecialEntity,
    LoadingCharacter,
    EntityIsPlayer,
}

pub fn transform_entity(
    server: &mut Server,
    entity: Entity,
    entity_info: EntityInfo,
    allow_players: bool,
) -> Result<(), TransformEntityError> {
    let is_player = server
        .state()
        .read_storage::<comp::Player>()
        .contains(entity);

    match SpawnEntityData::from_entity_info(entity_info) {
        SpawnEntityData::Npc(NpcData {
            inventory,
            stats,
            skill_set,
            poise,
            health,
            body,
            scale,
            agent,
            loot,
            alignment: _,
            ethos: _,
            pos: _,
            pets,
            rider,
            death_effects,
            rider_effects,
        }) => {
            fn set_or_remove_component<C: specs::Component>(
                server: &mut Server,
                entity: EcsEntity,
                component: Option<C>,
                with: Option<fn(&mut C, Option<C>)>,
            ) -> Result<(), TransformEntityError> {
                let mut storage = server.state.ecs_mut().write_storage::<C>();

                if let Some(mut component) = component {
                    if let Some(with) = with {
                        let prev = storage.remove(entity);
                        with(&mut component, prev);
                    }

                    storage
                        .insert(entity, component)
                        .and(Ok(()))
                        .map_err(|_| TransformEntityError::EntityDead)
                } else {
                    storage.remove(entity);
                    Ok(())
                }
            }

            // Disable persistence
            'persist: {
                match server
                    .state
                    .ecs()
                    .read_storage::<Presence>()
                    .get(entity)
                    .map(|presence| presence.kind)
                {
                    // Transforming while the character is being loaded or is spectating is invalid!
                    Some(PresenceKind::Spectator | PresenceKind::LoadingCharacter(_)) => {
                        return Err(TransformEntityError::LoadingCharacter);
                    },
                    Some(PresenceKind::Character(_)) if !allow_players => {
                        return Err(TransformEntityError::EntityIsPlayer);
                    },
                    Some(PresenceKind::Possessor | PresenceKind::Character(_)) => {},
                    None => break 'persist,
                }

                // Run persistence once before disabling it
                //
                // We must NOT early return between persist_entity() being called and
                // persistence being set to Possessor
                super::player::persist_entity(server.state_mut(), entity);

                // We re-fetch presence here as mutable, because checking for a valid
                // [`PresenceKind`] must be done BEFORE persist_entity but persist_entity needs
                // exclusive mutable access to the server's state
                let mut presences = server.state.ecs().write_storage::<Presence>();
                let Some(presence) = presences.get_mut(entity) else {
                    // Checked above
                    unreachable!("We already know this entity has a Presence");
                };

                if let PresenceKind::Character(id) = presence.kind {
                    server.state.ecs().write_resource::<IdMaps>().remove_entity(
                        Some(entity),
                        None,
                        Some(id),
                        None,
                    );

                    presence.kind = PresenceKind::Possessor;
                }
            }

            // Should do basically what StateExt::create_npc does
            set_or_remove_component(server, entity, Some(inventory), None)?;
            set_or_remove_component(server, entity, Some(stats), None)?;
            set_or_remove_component(server, entity, Some(skill_set), None)?;
            set_or_remove_component(server, entity, Some(poise), None)?;
            set_or_remove_component(server, entity, health, None)?;
            set_or_remove_component(server, entity, Some(comp::Energy::new(body)), None)?;
            set_or_remove_component(server, entity, Some(body), None)?;
            set_or_remove_component(server, entity, Some(body.mass()), None)?;
            set_or_remove_component(server, entity, Some(body.density()), None)?;
            set_or_remove_component(server, entity, Some(body.collider()), None)?;
            set_or_remove_component(server, entity, Some(scale), None)?;
            set_or_remove_component(server, entity, death_effects, None)?;
            set_or_remove_component(server, entity, rider_effects, None)?;
            // Reset active abilities
            set_or_remove_component(
                server,
                entity,
                Some(if body.is_humanoid() {
                    comp::ActiveAbilities::default_limited(BASE_ABILITY_LIMIT)
                } else {
                    comp::ActiveAbilities::default()
                }),
                None,
            )?;
            set_or_remove_component(server, entity, body.heads().map(Heads::new), None)?;

            // Don't add Agent or ItemDrops to players
            if !is_player {
                set_or_remove_component(
                    server,
                    entity,
                    agent,
                    Some(|new_agent, old_agent| {
                        if let Some(old_agent) = old_agent {
                            new_agent.target = old_agent.target;
                            new_agent.awareness = old_agent.awareness;
                        }
                    }),
                )?;
                set_or_remove_component(
                    server,
                    entity,
                    loot.to_items().map(comp::ItemDrops),
                    None,
                )?;
            }

            // Spawn pets
            let position = server.state.read_component_copied::<comp::Pos>(entity);
            if let Some(pos) = position {
                for (pet, offset) in pets
                    .into_iter()
                    .map(|(pet, offset)| (pet.to_npc_builder().0, offset))
                {
                    let pet_entity = handle_create_npc(server, CreateNpcEvent {
                        pos: comp::Pos(pos.0 + offset),
                        ori: comp::Ori::from_unnormalized_vec(offset).unwrap_or_default(),
                        npc: pet,
                    });

                    tame_pet(server.state.ecs(), pet_entity, entity);
                }

                // Spawn rider
                if let Some(rider) = rider {
                    let rider_entity = handle_create_npc(server, CreateNpcEvent {
                        pos,
                        ori: comp::Ori::default(),
                        npc: rider.to_npc_builder().0,
                    });
                    let uids = server.state().ecs().read_storage::<Uid>();
                    let link = Mounting {
                        mount: *uids
                            .get(entity)
                            .expect("We just got the position of this entity"),
                        rider: *uids.get(rider_entity).expect("We just created this entity"),
                    };
                    drop(uids);
                    server
                        .state
                        .link(link)
                        .expect("We know these entities exist");
                }
            }
        },
        SpawnEntityData::Special(_, _) => {
            return Err(TransformEntityError::UnexpectedSpecialEntity);
        },
    }

    Ok(())
}

pub fn handle_start_interaction(
    server: &mut Server,
    StartInteractionEvent(interaction): StartInteractionEvent,
) {
    let i = interaction.interactor;
    let t = interaction.target;
    if let Err(e) = server.state.link(interaction) {
        debug!("Error trying to start interaction between {i:?} and {t:?}: {e:?}");
    }
}

/// N27-O: `release_chain_summon_charge` (the `handle_delete` funnel every
/// summon exit route shares -- death, lifetime expiry, dismiss) and
/// `summons_to_dismiss` (the pure logic behind the owner-death and
/// owner-logout exit routes). Together these cover all five exit routes the
/// acceptance bar names, without needing to construct a full `Server` or a
/// full `DestroyEventData`/`SystemData`.
#[cfg(test)]
mod chain_summon_release_tests {
    use super::*;
    use common::comp::pact::Summons;
    use specs::{Builder, World, WorldExt};

    fn mock_world() -> World {
        let mut world = World::new();
        world.insert(IdMaps::new());
        world.register::<Uid>();
        world.register::<Alignment>();
        world.register::<Summons>();
        world
    }

    fn spawn(world: &mut World) -> (EcsEntity, Uid) {
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

    /// Covers the "creature dies" and "lifetime expiry" exit routes at
    /// once: both funnel into `handle_delete` -> `release_chain_summon_charge`
    /// identically, regardless of which one actually deleted the entity.
    #[test]
    fn releasing_a_charged_summon_frees_exactly_its_own_cost() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        let (summon_a, summon_a_uid) = spawn(&mut world);
        let (summon_b, summon_b_uid) = spawn(&mut world);
        world
            .write_component::<Alignment>()
            .insert(summon_a, Alignment::Owned(owner_uid))
            .unwrap();
        world
            .write_component::<Alignment>()
            .insert(summon_b, Alignment::Owned(owner_uid))
            .unwrap();
        {
            let mut summons = Summons::default();
            summons.charge(summon_a_uid, 3);
            summons.charge(summon_b_uid, 7);
            world
                .write_component::<Summons>()
                .insert(owner, summons)
                .unwrap();
        }

        release_chain_summon_charge(&world, summon_a);

        let ledger = world.read_component::<Summons>();
        let ledger = ledger.get(owner).expect("owner still has a ledger");
        assert_eq!(
            ledger.spent(),
            7,
            "only summon_a's 3 points should be freed"
        );
        assert_eq!(ledger.active, vec![(summon_b_uid, 7)]);
    }

    #[test]
    fn releasing_a_non_summon_entity_is_a_harmless_no_op() {
        let mut world = mock_world();
        let (owner, owner_uid) = spawn(&mut world);
        {
            let mut summons = Summons::default();
            summons.charge(owner_uid, 5);
            world
                .write_component::<Summons>()
                .insert(owner, summons)
                .unwrap();
        }
        // No `Alignment` at all -- the overwhelming-majority case for
        // `handle_delete`'s callers.
        let (bystander, _) = spawn(&mut world);

        release_chain_summon_charge(&world, bystander);

        let ledger = world.read_component::<Summons>();
        assert_eq!(ledger.get(owner).unwrap().spent(), 5);
    }

    #[test]
    fn releasing_an_owned_entity_whose_owner_has_no_ledger_is_a_no_op() {
        let mut world = mock_world();
        let (_owner, owner_uid) = spawn(&mut world);
        let (unrelated_pet, _) = spawn(&mut world);
        world
            .write_component::<Alignment>()
            .insert(unrelated_pet, Alignment::Owned(owner_uid))
            .unwrap();
        // `owner` has no `Summons` at all -- e.g. a tamed-pet owner who
        // never took the Chain boon. Must not panic.

        release_chain_summon_charge(&world, unrelated_pet);
    }

    /// Covers the "owner logs out" and "owner dies" exit routes' shared
    /// logic: both resolve which live entities to dismiss through
    /// `summons_to_dismiss`.
    #[test]
    fn summons_to_dismiss_resolves_every_still_live_active_uid() {
        let mut world = mock_world();
        let (summon_a, summon_a_uid) = spawn(&mut world);
        let (summon_b, summon_b_uid) = spawn(&mut world);
        // A uid never mapped to any entity -- e.g. a summon that already
        // died and was removed from `IdMaps`, but whose ledger entry hadn't
        // been cleaned up for some other reason. Must be skipped, not
        // panic.
        let stale_uid = Uid::from(core::num::NonZeroU64::new(u64::MAX).unwrap());
        let mut summons = Summons::default();
        summons.charge(summon_a_uid, 1);
        summons.charge(stale_uid, 1);
        summons.charge(summon_b_uid, 1);

        let id_maps = world.read_resource::<IdMaps>();
        let to_dismiss = summons_to_dismiss(Some(&summons), &id_maps);

        assert_eq!(to_dismiss, vec![summon_a, summon_b]);
    }

    #[test]
    fn summons_to_dismiss_of_none_is_empty() {
        let world = mock_world();
        let id_maps = world.read_resource::<IdMaps>();
        assert!(summons_to_dismiss(None, &id_maps).is_empty());
    }
}
