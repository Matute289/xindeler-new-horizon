use common::{
    Damage, DamageSource,
    assets::{AssetExt, Ron},
    combat::{self, CombatTuning, DamageContributor},
    comp::{
        ActiveSense, Alignment, AttunedItems, Energy, Ethos, Group, Health, HealthChange,
        Inventory, LightEmitter, Mass, ModifierKind, PhysicsState, Player, Pos, Stats,
        agent::{Sound, SoundKind},
        attunement::item_effects_active,
        aura::{Auras, EnteredAuras},
        body::{Body, object},
        buff::{
            Buff, BuffCategory, BuffChange, BuffData, BuffEffect, BuffKey, BuffKind, BuffSource,
            Buffs, DestInfo,
        },
        fluid_dynamics::{Fluid, LiquidKind},
        item::MaterialStatManifest,
        item_condition_buff_data,
    },
    event::{
        BuffEvent, ChangeBodyEvent, ComboChangeEvent, CreateSpriteEvent, EmitExt,
        EnergyChangeEvent, HealthChangeEvent, RemoveLightEmitterEvent, SoundEvent,
    },
    event_emitters,
    outcome::Outcome,
    resources::{DeltaTime, Secs, Time},
    terrain::SpriteKind,
    uid::{IdMaps, Uid},
};
use common_base::prof_span;
use common_ecs::{Job, Origin, ParMode, Phase, System};
use rand::RngExt;
use rayon::iter::ParallelIterator;
use specs::{
    Entities, Entity, LendJoin, ParJoin, Read, ReadExpect, ReadStorage, SystemData, WriteStorage,
    shred,
};
use vek::Vec3;

/// BL-05 RD-7: a `BleedingMark` that runs its full duration detonates for this
/// multiple of its bleed strength as a single burst. Placeholder magnitude —
/// retune in the BL-52/BL-05 balance pass.
const BLEED_DETONATE_MULT: f32 = 3.0;

/// The net `BuffKind` add/remove diff needed to bring `bearer_buffs` in line
/// with what every equipped item's `ItemCondition` currently says should be
/// active on `body`. Pure and ECS-independent so it is cheaply
/// unit-testable; `Sys::run` is a thin wrapper that turns the result into
/// `BuffEvent`s.
///
/// An item whose `RequiresAttunement` flag is set but whose slot is not
/// currently attuned contributes to neither list at all — it is fully inert,
/// matching `item_effects_active`'s existing "an unattuned item's effects
/// don't apply" rule.
///
/// A `BuffKind` already present on `bearer_buffs` is never re-added (so a
/// held condition doesn't spam identical events every tick), and a kind that
/// is requested `to_add` by one item always wins over another item
/// requesting the same kind `to_remove` in the same pass.
pub fn item_condition_buff_diff(
    inventory: &Inventory,
    body: &Body,
    ethos: Option<&Ethos>,
    attuned: Option<&AttunedItems>,
    bearer_buffs: Option<&Buffs>,
) -> (Vec<BuffKind>, Vec<BuffKind>) {
    let mut to_add: Vec<BuffKind> = Vec::new();
    let mut to_remove: Vec<BuffKind> = Vec::new();

    for (slot, item) in inventory.equipped_items_with_slot() {
        let Some(condition) = item.condition() else {
            continue;
        };
        if !item_effects_active(slot, item.requires_attunement(), attuned) {
            continue;
        }
        for kind in condition.active_buffs(ethos, body) {
            if !bearer_buffs.is_some_and(|b| b.contains(*kind)) && !to_add.contains(kind) {
                to_add.push(*kind);
            }
        }
        for kind in condition.inactive_buffs(ethos, body) {
            if bearer_buffs.is_some_and(|b| b.contains(*kind)) && !to_remove.contains(kind) {
                to_remove.push(*kind);
            }
        }
    }

    to_remove.retain(|kind| !to_add.contains(kind));
    (to_add, to_remove)
}

event_emitters! {
    struct Events[EventEmitters] {
        buff: BuffEvent,
        change_body: ChangeBodyEvent,
        remove_light: RemoveLightEmitterEvent,
        health_change: HealthChangeEvent,
        energy_change: EnergyChangeEvent,
        combo_change: ComboChangeEvent,
        sound: SoundEvent,
        create_sprite: CreateSpriteEvent,
        outcome: Outcome,
    }
}

#[derive(SystemData)]
pub struct ReadData<'a> {
    entities: Entities<'a>,
    dt: Read<'a, DeltaTime>,
    events: Events<'a>,
    inventories: ReadStorage<'a, Inventory>,
    attuned_items: ReadStorage<'a, AttunedItems>,
    // Only fetched for an entity once it's known to have at least one
    // equipped item declaring an `ItemCondition` (see `Sys::run`) — an
    // entity with no such item never touches this storage.
    ethos: ReadStorage<'a, Ethos>,
    healths: ReadStorage<'a, Health>,
    energies: ReadStorage<'a, Energy>,
    physics_states: ReadStorage<'a, PhysicsState>,
    groups: ReadStorage<'a, Group>,
    id_maps: Read<'a, IdMaps>,
    time: Read<'a, Time>,
    msm: ReadExpect<'a, MaterialStatManifest>,
    buffs: ReadStorage<'a, Buffs>,
    auras: ReadStorage<'a, Auras>,
    entered_auras: ReadStorage<'a, EnteredAuras>,
    positions: ReadStorage<'a, Pos>,
    bodies: ReadStorage<'a, Body>,
    light_emitters: ReadStorage<'a, LightEmitter>,
    alignments: ReadStorage<'a, Alignment>,
    players: ReadStorage<'a, Player>,
    masses: ReadStorage<'a, Mass>,
    // BL-01: per-class + per-level permanent attribute scaling (applied with racial passives).
    character_classes: ReadStorage<'a, common::comp::CharacterClass>,
    skill_sets: ReadStorage<'a, common::comp::SkillSet>,
}

#[derive(Default)]
pub struct Sys;
impl<'a> System<'a> for Sys {
    type SystemData = (ReadData<'a>, WriteStorage<'a, Stats>);

    const NAME: &'static str = "buff";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(job: &mut Job<Self>, (read_data, mut stats): Self::SystemData) {
        let mut emitters = read_data.events.get_emitters();
        let dt = read_data.dt.0;
        // Set to false to avoid spamming server
        stats.set_event_emission(false);

        // Put out underwater campfires. Logically belongs here since this system also
        // removes burning, but campfires don't have healths/stats/energies/buffs, so
        // this needs a separate loop.
        job.cpu_stats.measure(ParMode::Rayon);
        let to_put_out_campfires = (
            &read_data.entities,
            &read_data.bodies,
            &read_data.physics_states,
            &read_data.light_emitters, //to improve iteration speed
        )
            .par_join()
            .map_init(
                || {
                    prof_span!(guard, "buff campfire deactivate");
                    guard
                },
                |_guard, (entity, body, physics_state, _)| {
                    if matches!(*body, Body::Object(object::Body::CampfireLit))
                        && matches!(
                            physics_state.in_fluid,
                            Some(Fluid::Liquid {
                                kind: LiquidKind::Water,
                                ..
                            })
                        )
                    {
                        Some(entity)
                    } else {
                        None
                    }
                },
            )
            .fold(Vec::new, |mut to_put_out_campfires, put_out_campfire| {
                put_out_campfire.map(|put| to_put_out_campfires.push(put));
                to_put_out_campfires
            })
            .reduce(
                Vec::new,
                |mut to_put_out_campfires_a, mut to_put_out_campfires_b| {
                    to_put_out_campfires_a.append(&mut to_put_out_campfires_b);
                    to_put_out_campfires_a
                },
            );
        job.cpu_stats.measure(ParMode::Single);
        {
            prof_span!(_guard, "write deferred campfire deletion");
            // Assume that to_put_out_campfires is near to zero always, so this access isn't
            // slower than parallel checking above
            for e in to_put_out_campfires {
                {
                    emitters.emit(ChangeBodyEvent {
                        entity: e,
                        new_body: Body::Object(object::Body::Campfire),
                        permanent_change: None,
                    });
                    emitters.emit(RemoveLightEmitterEvent { entity: e });
                }
            }
        }

        let mut rng = rand::rng();
        // Racial passives manifest: one cache access per system run (hot-reloads
        // between runs); per-entity asset-cache hits are banned in tick paths.
        let racial_traits = common::comp::class::racial_traits_manifest();
        // BL-01 per-class scaling manifest: one cache access per system run.
        let class_attributes = common::comp::class::class_attributes_manifest();
        // Weapon-proficiency manifest: one cache access per system run,
        // mirroring the manifest above.
        let class_proficiencies = common::comp::class::class_proficiencies_manifest();
        // Non-proficient damage multiplier: one cached read per system run
        // (mirrors how `Attack::apply_attack` caches `combat_tuning` per
        // attack rather than per damage instance).
        let combat_tuning = &Ron::<CombatTuning>::load_expect("common.combat_tuning")
            .read()
            .0;
        let buff_join = (
            &read_data.entities,
            &read_data.buffs,
            &mut stats,
            &read_data.bodies,
            &read_data.healths,
            &read_data.energies,
            read_data.physics_states.maybe(),
            read_data.masses.maybe(),
        )
            .lend_join();
        buff_join.for_each(|comps| {
            let (entity, buff_comp, mut stat, body, health, energy, physics_state, mass) = comps;
            let dest_info = DestInfo {
                stats: Some(&stat),
                mass,
            };
            // Apply buffs to entity based off of their current physics_state
            if let Some(physics_state) = physics_state {
                let emit_terrain_buff = |emitters: &mut EventEmitters, kind, data| {
                    emitters.emit(BuffEvent {
                        entity,
                        buff_change: BuffChange::Add(Buff::new(
                            kind,
                            data,
                            vec![],
                            BuffSource::World,
                            *read_data.time,
                            dest_info,
                            None,
                            // Terrain effects have no associated ability for there to be a target
                            None,
                            None,
                        )),
                    });
                };
                // Set nearby entities on fire if burning
                if let Some((_, burning)) = buff_comp.iter_kind(BuffKind::Burning).next() {
                    for t_entity in physics_state.touch_entities.keys().filter_map(|te_uid| {
                        read_data.id_maps.uid_entity(*te_uid).filter(|te| {
                            combat::permit_pvp(
                                &read_data.alignments,
                                &read_data.players,
                                &read_data.entered_auras,
                                &read_data.id_maps,
                                Some(entity),
                                *te,
                            )
                        })
                    }) {
                        let duration = burning.data.duration.map(|d| d * 0.9);
                        if duration.is_none_or(|d| d.0 >= 1.0)
                            && rng.random_bool(
                                (dt * burning.data.strength / 5.0).clamp(0.0, 1.0).into(),
                            )
                        {
                            // NOTE: setting source as the burned character is
                            // problematic for whole array of reasons.
                            // 1) It would show non-sensical death message.
                            // 2) It makes NPCs hate the victim of fire, which is rather annoying.
                            // 3) It makes NPCs hate other NPCs and attempting to attack them, even
                            //    when they are in the same group.
                            //
                            // We could reference original source, but it might
                            // have some cheesing & griefing potential.
                            //
                            // Yes, with this implementation you could put
                            // yourself on fire, and then harras NPCs but good
                            // luck with that.
                            emitters.emit(BuffEvent {
                                entity: t_entity,
                                buff_change: BuffChange::Add(Buff::new(
                                    BuffKind::Burning,
                                    BuffData::new(burning.data.strength, duration),
                                    vec![],
                                    BuffSource::World,
                                    *read_data.time,
                                    DestInfo {
                                        // Can't mutably access stats, and for burning debuff stats
                                        // has no effect (for now)
                                        stats: None,
                                        mass: read_data.masses.get(t_entity),
                                    },
                                    mass,
                                    // There is no ability being cast that would cause there to be
                                    // a target
                                    None,
                                    None,
                                )),
                            });
                        }
                    }
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::EnsnaringVines)
                ) {
                    // If on ensnaring vines, apply partial ensnared debuff
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Ensnared,
                        BuffData::new(0.5, Some(Secs(0.1))),
                    );
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::EnsnaringWeb)
                ) {
                    // If on ensnaring web, apply ensnared debuff
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Ensnared,
                        BuffData::new(1.0, Some(Secs(1.0))),
                    );
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::SeaUrchin)
                ) {
                    // If touching Sea Urchin apply Bleeding buff
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Bleeding,
                        BuffData::new(1.0, Some(Secs(6.0))),
                    );
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::HaniwaTrap)
                ) && !body.immune_to(BuffKind::Bleeding)
                {
                    // TODO: Determine a better place to emit sprite change events
                    if let Some(pos) = read_data.positions.get(entity) {
                        // If touching Trap - change sprite and apply Bleeding buff
                        emitters.emit(CreateSpriteEvent {
                            pos: Vec3::new(pos.0.x as i32, pos.0.y as i32, pos.0.z as i32 - 1),
                            sprite: SpriteKind::HaniwaTrapTriggered,
                            del_timeout: Some((4.0, 1.0)),
                        });
                        emitters.emit(SoundEvent {
                            sound: Sound::new(SoundKind::Trap, pos.0, 12.0, read_data.time.0),
                        });
                        emitters.emit(Outcome::Slash { pos: pos.0 });

                        emit_terrain_buff(
                            &mut emitters,
                            BuffKind::Bleeding,
                            BuffData::new(5.0, Some(Secs(3.0))),
                        );
                    }
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::IronSpike | SpriteKind::HaniwaTrapTriggered)
                ) {
                    // If touching Iron Spike apply Bleeding buff
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Bleeding,
                        BuffData::new(1.0, Some(Secs(4.0))),
                    );
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::HotSurface)
                ) {
                    // If touching a hot surface apply Burning buff
                    emit_terrain_buff(&mut emitters, BuffKind::Burning, BuffData::new(10.0, None));
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::IceSpike)
                ) {
                    // When standing on IceSpike, apply bleeding
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Bleeding,
                        BuffData::new(15.0, Some(Secs(0.1))),
                    );
                    // When standing on IceSpike also apply Frozen
                    emit_terrain_buff(
                        &mut emitters,
                        BuffKind::Frozen,
                        BuffData::new(0.2, Some(Secs(3.0))),
                    );
                }
                if matches!(
                    physics_state.on_ground.and_then(|b| b.get_sprite()),
                    Some(SpriteKind::FireBlock)
                ) {
                    // If on FireBlock vines, apply burning buff
                    emit_terrain_buff(&mut emitters, BuffKind::Burning, BuffData::new(20.0, None));
                }
                if matches!(
                    physics_state.in_fluid,
                    Some(Fluid::Liquid {
                        kind: LiquidKind::Lava,
                        ..
                    })
                ) && !body.negates_buff(BuffKind::Burning)
                {
                    // If in lava fluid, apply burning debuff
                    emit_terrain_buff(&mut emitters, BuffKind::Burning, BuffData::new(20.0, None));
                } else if matches!(
                    physics_state.in_fluid,
                    Some(Fluid::Liquid {
                        kind: LiquidKind::Water,
                        ..
                    })
                ) && buff_comp.kinds[BuffKind::Burning].is_some()
                {
                    // If in water fluid and currently burning, remove burning debuffs
                    emitters.emit(BuffEvent {
                        entity,
                        buff_change: BuffChange::RemoveByKind(BuffKind::Burning),
                    });
                }
            }

            let mut expired_buffs = Vec::<BuffKey>::new();

            // Replace buffs from an active aura with a normal buff when out of range of the
            // aura or link no longer active
            for (buff_key, buff) in &buff_comp.buffs {
                let keep = buff.cat_ids.iter().all(|cat| match cat {
                    BuffCategory::FromActiveAura(source, key) => {
                        let Some(source_entity) = read_data.id_maps.uid_entity(*source) else {
                            return false;
                        };

                        let Some(aura) = read_data
                            .auras
                            .get(source_entity)
                            .and_then(|aura| aura.auras.get(*key))
                        else {
                            return false;
                        };

                        let (Some(pos), Some(aura_pos)) = (
                            read_data.positions.get(entity),
                            read_data.positions.get(source_entity),
                        ) else {
                            return false;
                        };

                        pos.0.distance_squared(aura_pos.0) <= aura.radius.powi(2)
                    },
                    BuffCategory::FromLink(l) => l.exists(),
                    _ => true,
                });

                if !keep {
                    expired_buffs.push(buff_key);
                    emitters.emit(BuffEvent {
                        entity,
                        buff_change: BuffChange::Add(Buff::new(
                            buff.kind,
                            buff.data,
                            buff.cat_ids
                                .iter()
                                .filter(|cat_id| {
                                    !matches!(
                                        cat_id,
                                        BuffCategory::FromActiveAura(..)
                                            | BuffCategory::FromLink(..)
                                    )
                                })
                                .cloned()
                                .collect::<Vec<_>>(),
                            buff.source,
                            *read_data.time,
                            dest_info,
                            None,
                            // If we ever need to transfer the "target" entity from an expired
                            // aura, we'll have to store the target entity somewhere (maybe buff,
                            // maybe aura)? Revisit if this causes issues.
                            None,
                            buff.magic_source,
                        )),
                    });
                }
            }

            // A mind-altering buff whose caster entity no longer resolves
            // (disconnected, died and despawned, etc.) is dropped rather than
            // left running with nobody able to command or be excluded by it.
            for (buff_key, buff) in &buff_comp.buffs {
                if matches!(
                    buff.kind,
                    BuffKind::Charmed | BuffKind::Dominated | BuffKind::Maddened
                ) && let BuffSource::Character { by, .. } = buff.source
                    && read_data.id_maps.uid_entity(by).is_none()
                {
                    expired_buffs.push(buff_key);
                }
            }

            buff_comp.buffs.iter().for_each(|(buff_key, buff)| {
                if buff.end_time.is_some_and(|end| end.0 < read_data.time.0) {
                    expired_buffs.push(buff_key);
                    // BL-05 RD-7: a BleedingMark detonates for a burst when it runs
                    // its full course (natural timer expiry). Dispel/cleanse before
                    // then removes it via RemoveByKind and skips this path — the
                    // counterplay. Attribution mirrors the bleed DoT.
                    if buff.kind == BuffKind::BleedingMark {
                        let by = match buff.source {
                            BuffSource::Character { by, .. } => Some(by),
                            _ => None,
                        };
                        let damage_contributor = by.and_then(|uid| {
                            read_data.id_maps.uid_entity(uid).map(|e| {
                                DamageContributor::new(uid, read_data.groups.get(e).cloned())
                            })
                        });
                        emitters.emit(HealthChangeEvent {
                            entity,
                            change: HealthChange {
                                amount: -(buff.data.strength * BLEED_DETONATE_MULT),
                                by: damage_contributor,
                                cause: Some(DamageSource::Buff(BuffKind::BleedingMark)),
                                magic_source: None,
                                time: *read_data.time,
                                precise: false,
                                instance: rand::random(),
                            },
                        });
                    }
                }
            });

            let infinite_damage_reduction = (Damage::compute_damage_reduction(
                None,
                read_data.inventories.get(entity),
                read_data.attuned_items.get(entity),
                Some(&stat),
                &read_data.msm,
            ) - 1.0)
                .abs()
                < f32::EPSILON;
            if infinite_damage_reduction {
                for (key, buff) in buff_comp.buffs.iter() {
                    if !buff.kind.is_buff() {
                        expired_buffs.push(key);
                    }
                }
            }

            // Equipped-item `ItemCondition`s: short-circuits before touching
            // `read_data.ethos` (or evaluating any predicate at all) unless
            // at least one equipped item actually declares a condition —
            // true for ~every item in the game today.
            if let Some(inventory) = read_data.inventories.get(entity)
                && inventory
                    .equipped_items()
                    .any(|item| item.condition().is_some())
            {
                let (to_add, to_remove) = item_condition_buff_diff(
                    inventory,
                    body,
                    read_data.ethos.get(entity),
                    read_data.attuned_items.get(entity),
                    Some(buff_comp),
                );
                for kind in to_remove {
                    emitters.emit(BuffEvent {
                        entity,
                        buff_change: BuffChange::RemoveByKind(kind),
                    });
                }
                for kind in to_add {
                    emitters.emit(BuffEvent {
                        entity,
                        buff_change: BuffChange::Add(Buff::new(
                            kind,
                            item_condition_buff_data(),
                            vec![],
                            BuffSource::Item,
                            *read_data.time,
                            DestInfo {
                                stats: Some(&stat),
                                mass,
                            },
                            mass,
                            None,
                            None,
                        )),
                    });
                }
            }

            // Call to reset stats to base values
            stat.reset_temp_modifiers();

            // Racial passives re-apply each tick right after the reset so
            // they stack with buffs and need no persistence (spec §6).
            if let Body::Humanoid(humanoid_body) = *body {
                racial_traits
                    .0
                    .get(&humanoid_body.species)
                    .copied()
                    .unwrap_or_default()
                    .apply(&mut stat);

                // BL-01: permanent per-class + per-level scaling, same tick slot
                // as racial passives (derived level → no persistence).
                if let (Some(character_class), Some(skill_set)) = (
                    read_data.character_classes.get(entity),
                    read_data.skill_sets.get(entity),
                ) {
                    let character_level = skill_set.character_level();
                    stat.character_level = character_level;

                    // energy_reward_mult composes as the max across every
                    // held class, not the product of applying each one --
                    // computed once here rather than inside `apply` itself,
                    // so it stays a single multiplication regardless of how
                    // many classes are held (see `ClassAttributes::apply`'s
                    // own doc comment for why).
                    let energy_reward_mult = character_class
                        .classes()
                        .map(|class| {
                            class_attributes
                                .0
                                .get(&class)
                                .copied()
                                .unwrap_or_default()
                                .energy_reward_mult
                        })
                        .fold(f32::MIN, f32::max);
                    stat.energy_reward_modifier *= energy_reward_mult;

                    for (class, level, is_primary) in character_class.class_levels(character_level)
                    {
                        class_attributes
                            .0
                            .get(&class)
                            .copied()
                            .unwrap_or_default()
                            .apply(&mut stat, level, is_primary);
                    }

                    // BL-06: passive class-skill bonuses, same per-tick slot as
                    // the per-class attribute scaling (derived from purchased
                    // skill levels → no persistence beyond the SkillSet).
                    skill_set.apply_class_passives(&mut stat);
                    // BL-20: passive feat bonuses, same per-tick slot as the
                    // class-skill passives above.
                    skill_set.apply_feat_passives(&mut stat);

                    // Weapon proficiency: union (bitwise OR) across every held
                    // class, using the manifest hoisted once per system run
                    // above. A class absent from the manifest resolves
                    // permissive (`All`), not empty — see the manifest type's
                    // own doc comment. Running this BEFORE the buff-effect
                    // loop below is load-bearing: it lets a future buff/feat
                    // widen the mask or restore the damage multiplier on top
                    // of the class narrowing, rather than being clobbered by
                    // it.
                    stat.proficient_tools =
                        character_class.proficient_tools_mask(&class_proficiencies.0);
                    stat.non_proficient_damage_mult = combat_tuning.non_proficient_damage_mult;
                }
            }

            // Gear → Stats: caster-role implements (Staff/Sceptre/Tome/
            // HolySymbol/Focus in the Caster role) contribute their
            // effect_power/buff_strength/energy_efficiency tool stats onto
            // this tick's spell_power/heal_power/energy_regen_modifier.
            // Deliberately not gated on holding a CharacterClass: gear should
            // matter for any inventory-carrying entity (e.g. a spellcasting
            // NPC), the same way compute_protection/compute_max_energy_mod
            // don't require one either. Ordering relative to the class-passive
            // block above is immaterial here — every contribution is a plain
            // `*=`, and multiplication is commutative.
            combat::apply_gear_caster_stats(
                &mut stat,
                read_data.inventories.get(entity),
                read_data.attuned_items.get(entity),
            );

            // BL-65: every body contributes a combat-stat baseline by threat
            // tier; humanoid bodies contribute 0 here (PCs/class-gated NPCs get
            // accuracy/evasion/crit from ClassAttributes above, not the body).
            // One `threat_tier()` eval per entity (skips the work for humanoids).
            let (npc_accuracy, npc_evasion, npc_crit) = body.base_combat_stats();
            stat.accuracy += npc_accuracy;
            stat.evasion += npc_evasion;
            stat.crit_chance += npc_crit;

            let mut body_override = None;

            // Iterator over the lists of buffs by kind
            let mut buff_kinds = buff_comp
                .kinds
                .iter()
                .filter_map(|(kind, keys)| keys.as_ref().map(|keys| (kind, keys.clone())))
                .collect::<Vec<(BuffKind, (Vec<BuffKey>, Time))>>();
            buff_kinds.sort_by_key(|(kind, _)| !kind.affects_subsequent_buffs());
            for (buff_kind, (buff_keys, kind_start_time)) in buff_kinds.into_iter() {
                let mut active_buff_keys = Vec::new();
                if infinite_damage_reduction && !buff_kind.is_buff() {
                    continue;
                }

                if buff_kind.stacks() {
                    // Process all the buffs of this kind
                    active_buff_keys = buff_keys;
                } else {
                    // Only process the strongest of this buff kind
                    active_buff_keys.push(buff_keys[0]);
                }
                for buff_key in active_buff_keys.into_iter() {
                    if let Some(buff) = buff_comp.buffs.get(buff_key) {
                        // Skip the effect of buffs whose start delay hasn't expired.
                        if buff.start_time.0 > read_data.time.0 {
                            continue;
                        }
                        // Get buff owner?
                        let buff_owner =
                            if let BuffSource::Character { by: owner, .. } = buff.source {
                                Some(owner)
                            } else {
                                None
                            };

                        // Now, execute the buff, based on it's delta
                        for effect in &buff.effects {
                            execute_effect(
                                effect,
                                buff.kind,
                                buff.start_time,
                                kind_start_time,
                                &read_data,
                                &mut stat,
                                body,
                                &mut body_override,
                                health,
                                energy,
                                entity,
                                buff_owner,
                                &mut emitters,
                                dt,
                                *read_data.time,
                                expired_buffs.contains(&buff_key),
                                buff_comp,
                            );
                        }
                    }
                }
            }

            // Update body if needed.
            let new_body = body_override.unwrap_or(stat.original_body);
            if new_body != *body {
                emitters.emit(ChangeBodyEvent {
                    entity,
                    new_body,
                    permanent_change: None,
                });
            }

            // Remove buffs that expire
            if !expired_buffs.is_empty() {
                emitters.emit(BuffEvent {
                    entity,
                    buff_change: BuffChange::RemoveByKey(expired_buffs),
                });
            }

            // Remove buffs that don't persist on death
            if health.is_dead {
                emitters.emit(BuffEvent {
                    entity,
                    buff_change: BuffChange::RemoveByCategory {
                        all_required: vec![],
                        any_required: vec![],
                        none_required: vec![BuffCategory::PersistOnDeath],
                    },
                });
            }
        });
        // Turned back to true
        stats.set_event_emission(true);
    }
}

// TODO: Globally disable this clippy lint
#[expect(clippy::too_many_arguments)]
fn execute_effect(
    effect: &BuffEffect,
    buff_kind: BuffKind,
    buff_start_time: Time,
    buff_kind_start_time: Time,
    read_data: &ReadData,
    stat: &mut Stats,
    current_body: &Body,
    body_override: &mut Option<Body>,
    health: &Health,
    energy: &Energy,
    entity: Entity,
    buff_owner: Option<Uid>,
    server_emitter: &mut (
             impl EmitExt<HealthChangeEvent>
             + EmitExt<EnergyChangeEvent>
             + EmitExt<ComboChangeEvent>
             + EmitExt<BuffEvent>
         ),
    dt: f32,
    time: Time,
    buff_will_expire: bool,
    buffs_comp: &Buffs,
) {
    let num_ticks = |tick_dur: Secs| {
        let time_passed = time.0 - buff_start_time.0;
        let dt = dt as f64;
        // Number of ticks has 3 parts
        //
        // First part checks if delta time was larger than the tick duration, if it was
        // determines number of ticks in that time
        //
        // Second part checks if delta time has just passed the threshold for a tick
        // ending/starting (and accounts for if that delta time was longer than the tick
        // duration)
        // 0.000001 is to account for floating imprecision so this is not applied on the
        // first tick
        //
        // Third part returns the fraction of the current time passed since the last
        // time a tick duration would have happened, this is ignored (by flooring) when
        // the buff is not ending, but is used if the buff is ending this tick
        let curr_tick = (time_passed / tick_dur.0).floor();
        let prev_tick = ((time_passed - dt).max(0.0) / tick_dur.0).floor();
        let whole_ticks = curr_tick - prev_tick;

        if buff_will_expire {
            // If the buff is ending, include the fraction of progress towards the next
            // tick.
            let fractional_tick = (time_passed % tick_dur.0) / tick_dur.0;
            Some((whole_ticks + fractional_tick) as f32)
        } else if whole_ticks >= 1.0 {
            Some(whole_ticks as f32)
        } else {
            None
        }
    };
    match effect {
        BuffEffect::HealthChangeOverTime {
            rate,
            kind,
            instance,
            tick_dur,
        } => {
            if let Some(num_ticks) = num_ticks(*tick_dur) {
                let amount = *rate * num_ticks * tick_dur.0 as f32;

                let (cause, by) = if amount != 0.0 {
                    (Some(DamageSource::Buff(buff_kind)), buff_owner)
                } else {
                    (None, None)
                };
                let amount = match *kind {
                    ModifierKind::Additive => amount,
                    ModifierKind::Multiplicative => health.maximum() * amount,
                };
                let damage_contributor = by.and_then(|uid| {
                    read_data.id_maps.uid_entity(uid).map(|entity| {
                        DamageContributor::new(uid, read_data.groups.get(entity).cloned())
                    })
                });
                server_emitter.emit(HealthChangeEvent {
                    entity,
                    change: HealthChange {
                        amount,
                        by: damage_contributor,
                        cause,
                        magic_source: None,
                        time: *read_data.time,
                        precise: false,
                        instance: *instance,
                    },
                });
            };
        },
        BuffEffect::EnergyChangeOverTime {
            rate,
            kind,
            tick_dur,
            reset_rate_on_tick,
        } => {
            if let Some(num_ticks) = num_ticks(*tick_dur) {
                let amount = *rate * num_ticks * tick_dur.0 as f32;

                let amount = match *kind {
                    ModifierKind::Additive => amount,
                    ModifierKind::Multiplicative => energy.maximum() * amount,
                };
                server_emitter.emit(EnergyChangeEvent {
                    entity,
                    change: amount,
                    reset_rate: *reset_rate_on_tick,
                });
            };
        },
        BuffEffect::ComboChangeOverTime { rate, tick_dur } => {
            if let Some(num_ticks) = num_ticks(*tick_dur) {
                let amount = (*rate * num_ticks * tick_dur.0 as f32) as i32;

                server_emitter.emit(ComboChangeEvent {
                    entity,
                    change: amount,
                });
            };
        },
        BuffEffect::MaxHealthModifier { value, kind } => match kind {
            ModifierKind::Additive => {
                stat.max_health_modifiers.add_mod += *value;
            },
            ModifierKind::Multiplicative => {
                stat.max_health_modifiers.mult_mod *= *value;
            },
        },
        BuffEffect::MaxEnergyModifier { value, kind } => match kind {
            ModifierKind::Additive => {
                stat.max_energy_modifiers.add_mod += *value;
            },
            ModifierKind::Multiplicative => {
                stat.max_energy_modifiers.mult_mod *= *value;
            },
        },
        BuffEffect::DamageReduction(dr) => {
            if *dr > 0.0 {
                stat.damage_reduction.pos_mod = stat.damage_reduction.pos_mod.max(*dr);
            } else {
                stat.damage_reduction.neg_mod += dr;
            }
        },
        BuffEffect::MaxHealthChangeOverTime {
            rate,
            kind,
            target_fraction,
        } => {
            let potential_amount = (time.0 - buff_kind_start_time.0) as f32 * rate;

            // Percentage change that should be applied to max_health
            let potential_fraction = 1.0
                + match kind {
                    ModifierKind::Additive => {
                        // `rate * dt` is amount of health, dividing by base max
                        // creates fraction
                        potential_amount / health.base_max()
                    },
                    ModifierKind::Multiplicative => {
                        // `rate * dt` is the fraction
                        potential_amount
                    },
                };

            // Potential progress towards target fraction, if
            // target_fraction ~ 1.0 then set progress to 1.0 to avoid
            // divide by zero
            let progress = if (1.0 - *target_fraction).abs() > f32::EPSILON {
                (1.0 - potential_fraction) / (1.0 - *target_fraction)
            } else {
                1.0
            };

            // Change achieved_fraction depending on what other buffs have
            // occurred
            let achieved_fraction = if progress > 1.0 {
                // If potential fraction already beyond target fraction,
                // simply multiply max_health_modifier by the target
                // fraction, and set achieved fraction to target_fraction
                *target_fraction
            } else {
                // Else have not achieved target yet, use potential_fraction
                potential_fraction
            };

            // Apply achieved_fraction to max_health_modifier
            stat.max_health_modifiers.mult_mod *= achieved_fraction;
        },
        BuffEffect::MovementSpeed(speed) => {
            stat.move_speed_modifier *= *speed;
        },
        BuffEffect::ChargeMoveSpeed(speed) => {
            stat.charge_move_speed_modifier *= *speed;
        },
        BuffEffect::BuildupMoveSpeed(speed) => {
            stat.buildup_move_speed_modifier *= *speed;
        },
        BuffEffect::AttackSpeed(speed) => {
            stat.attack_speed_modifier *= *speed;
        },
        BuffEffect::RecoverySpeed(speed) => {
            stat.recovery_speed_modifier *= *speed;
        },
        BuffEffect::ChargingSpeed(speed) => {
            stat.charge_speed_modifier *= *speed;
        },
        BuffEffect::BuildupSpeed(speed) => {
            stat.buildup_speed_modifier *= *speed;
        },
        BuffEffect::GroundFriction(gf) => {
            stat.friction_modifier *= *gf;
        },
        BuffEffect::PoiseReduction(pr) => {
            if *pr > 0.0 {
                stat.poise_reduction.pos_mod = stat.poise_reduction.pos_mod.max(*pr);
            } else {
                stat.poise_reduction.neg_mod += pr;
            }
        },
        BuffEffect::PoiseDamageFromLostHealth(strength) => {
            stat.poise_damage_modifier *= 1.0 + (1.0 - health.fraction()) * *strength;
        },
        BuffEffect::AttackDamage(dam) => {
            stat.attack_damage_modifier *= *dam;
        },
        BuffEffect::PrecisionModifier(req, val, ovrd) => {
            stat.conditional_precision_modifiers
                .push((req.clone(), *val, *ovrd));
        },
        BuffEffect::PrecisionVulnerabilityOverride(val) => {
            // Use higher of precision multiplier overrides
            stat.precision_vulnerability_multiplier_override = stat
                .precision_vulnerability_multiplier_override
                .map(|mult| mult.max(*val))
                .or(Some(*val));
        },
        BuffEffect::BodyChange(b) => {
            // For when an entity is under the effects of multiple de/buffs that change the
            // body, to avoid flickering between many bodies only change the body if the
            // override body is not equal to the current body. (If the buff that caused the
            // current body is still active, body override will eventually pick up on it,
            // otherwise this will end up with a new body, though random depending on
            // iteration order)
            if Some(current_body) != body_override.as_ref() {
                *body_override = Some(*b)
            }
        },
        BuffEffect::BuffImmunity(buff_kind) => {
            if buffs_comp.contains(*buff_kind) {
                server_emitter.emit(BuffEvent {
                    entity,
                    buff_change: BuffChange::RemoveByKind(*buff_kind),
                });
            }
        },
        BuffEffect::SwimSpeed(speed) => {
            stat.swim_speed_modifier *= speed;
        },
        BuffEffect::AttackEffect(effect) => stat.effects_on_attack.push(effect.clone()),
        BuffEffect::AttackPoise(p) => {
            stat.poise_damage_modifier *= p;
        },
        BuffEffect::MitigationsPenetration(mp) => {
            stat.mitigations_penetration =
                1.0 - ((1.0 - stat.mitigations_penetration) * (1.0 - *mp));
        },
        BuffEffect::EnergyReward(er) => {
            stat.energy_reward_modifier *= er;
        },
        BuffEffect::EnergyEfficiency(ef) => {
            stat.energy_efficiency_modifier *= *ef;
        },
        BuffEffect::DamagedEffect(effect) => stat.effects_on_damaged.push(effect.clone()),
        BuffEffect::DeathEffect(effect) => stat.effects_on_death.push(effect.clone()),
        BuffEffect::DisableAuxiliaryAbilities => stat.disable_auxiliary_abilities = true,
        BuffEffect::DisableMagic => stat.disable_magic = true,
        BuffEffect::DisableTeleport => stat.disable_teleport = true,
        BuffEffect::Accuracy(acc) => stat.accuracy += *acc,
        BuffEffect::Evasion(eva) => stat.evasion += *eva,
        BuffEffect::CritChance(cc) => stat.crit_chance += *cc,
        BuffEffect::Resistance(kind, amount) => stat.add_resistance(*kind, *amount),
        BuffEffect::CrowdControlResistance(ccr) => {
            stat.crowd_control_resistance += ccr;
        },
        BuffEffect::ItemEffectReduction(ier) => {
            stat.item_effect_reduction *= 1.0 - ier;
        },
        BuffEffect::AttackedModification(am) => {
            stat.attacked_modifications.push(am.clone());
        },
        BuffEffect::PrecisionPowerMult(ppm) => {
            stat.precision_power_mult *= ppm;
        },
        BuffEffect::KnockbackMult(km) => {
            stat.knockback_mult *= km;
        },
        BuffEffect::ProjectileSpeedMult(ps) => {
            stat.projectile_speed_mult *= *ps;
        },
        BuffEffect::ProjectileConstructorEffect(pce) => {
            stat.projectile_constructor_effects.push(pce.clone())
        },
        BuffEffect::MarkEntity(e) => {
            stat.marked_entities.push(*e);
        },
        BuffEffect::Sense { kind, radius, mode } => {
            stat.senses.push(ActiveSense {
                kind: *kind,
                radius: *radius,
                mode: *mode,
            });
        },
        // Deliberately a no-op here: this system is shared by the client and
        // the server (dispatched in `add_local_systems`), and its only output
        // channel is `Stats`, which is synced `SyncFrom::AnyEntity`. Writing
        // the resolved anchor into anything reachable from here would leak
        // it to every nearby client. The owner-private `RemoteSense`
        // component is written directly by the server-only cast-resolution
        // code that applies this buff, never derived from this effect.
        BuffEffect::RemoteSense { .. } => {},
    };
}

#[cfg(test)]
mod item_condition_diff_tests {
    use super::*;
    use common::{
        LoadoutBuilder,
        comp::{
            ConditionPredicate, ItemCondition,
            body::humanoid,
            inventory::item::{
                AbilityMap, Hands, Item, ItemBase, ItemDef, ItemKind, ItemTag,
                MaterialStatManifest, Tool, ToolKind, tool,
            },
            slot::EquipSlot,
        },
    };
    use std::sync::Arc;

    fn human_body() -> Body {
        Body::Humanoid(humanoid::Body::random_with(
            &mut rand::rng(),
            &humanoid::Species::Human,
        ))
    }

    /// A `Tome` item, optionally carrying `condition` and optionally tagged
    /// `RequiresAttunement`.
    fn tome_item(condition: Option<ItemCondition>, requires_attunement: bool) -> Item {
        let mut item_def = ItemDef::create_test_itemdef_from_kind(ItemKind::Tool(Tool::new(
            ToolKind::Tome,
            Hands::One,
            None,
            tool::Stats::one(),
        )));
        if requires_attunement {
            item_def.tags = vec![ItemTag::RequiresAttunement];
        }
        item_def.condition = condition;
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(item_def)),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        )
    }

    fn inventory_with_mainhand(item: Item, body: Body) -> Inventory {
        let loadout = LoadoutBuilder::empty().active_mainhand(Some(item)).build();
        Inventory::with_loadout(loadout, body)
    }

    fn shielded_buff() -> Buff {
        Buff::new(
            BuffKind::Shielded,
            item_condition_buff_data(),
            vec![],
            BuffSource::Item,
            Time(0.0),
            DestInfo {
                stats: None,
                mass: None,
            },
            None,
            None,
            None,
        )
    }

    #[test]
    fn no_conditioned_item_yields_no_diff() {
        let body = human_body();
        let inventory = inventory_with_mainhand(tome_item(None, false), body);
        let (to_add, to_remove) = item_condition_buff_diff(&inventory, &body, None, None, None);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());
    }

    #[test]
    fn met_condition_grants_the_when_met_buff() {
        let body = human_body();
        let condition = ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Human]),
            when_met: vec![BuffKind::Shielded],
            when_unmet: vec![],
        };
        let inventory = inventory_with_mainhand(tome_item(Some(condition), false), body);
        let (to_add, to_remove) = item_condition_buff_diff(&inventory, &body, None, None, None);
        assert_eq!(to_add, vec![BuffKind::Shielded]);
        assert!(to_remove.is_empty());
    }

    #[test]
    fn a_buff_already_present_is_not_re_added() {
        let body = human_body();
        let condition = ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Human]),
            when_met: vec![BuffKind::Shielded],
            when_unmet: vec![],
        };
        let inventory = inventory_with_mainhand(tome_item(Some(condition), false), body);
        let mut buffs = Buffs::default();
        buffs.insert(shielded_buff(), Time(0.0));

        let (to_add, _) = item_condition_buff_diff(&inventory, &body, None, None, Some(&buffs));
        assert!(
            to_add.is_empty(),
            "a held condition must not spam re-add events every tick"
        );
    }

    #[test]
    fn a_condition_going_unmet_removes_its_previously_granted_buff() {
        let body = human_body();
        // Predicate never matches this bearer's species, so `when_unmet`
        // (empty here) is what's active -> the previously-granted
        // `when_met` buff must be removed.
        let condition = ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Draugr]),
            when_met: vec![BuffKind::Shielded],
            when_unmet: vec![],
        };
        let inventory = inventory_with_mainhand(tome_item(Some(condition), false), body);
        let mut buffs = Buffs::default();
        buffs.insert(shielded_buff(), Time(0.0));

        let (to_add, to_remove) =
            item_condition_buff_diff(&inventory, &body, None, None, Some(&buffs));
        assert!(to_add.is_empty());
        assert_eq!(to_remove, vec![BuffKind::Shielded]);
    }

    #[test]
    fn unattuned_requires_attunement_item_grants_no_buff_attuned_it_does() {
        let body = human_body();
        let condition = ItemCondition {
            predicate: ConditionPredicate::Species(vec![humanoid::Species::Human]),
            when_met: vec![BuffKind::Shielded],
            when_unmet: vec![],
        };
        let inventory = inventory_with_mainhand(tome_item(Some(condition), true), body);

        // Unattuned: the item's condition is fully inert.
        let (to_add, to_remove) = item_condition_buff_diff(&inventory, &body, None, None, None);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());

        // Attuned: the when_met buff is granted.
        let attuned = AttunedItems(vec![EquipSlot::ActiveMainhand]);
        let (to_add, _) = item_condition_buff_diff(&inventory, &body, None, Some(&attuned), None);
        assert_eq!(to_add, vec![BuffKind::Shielded]);
    }
}
