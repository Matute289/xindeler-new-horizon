use chrono::Utc;
use common_net::synced_components::Heads;
use specs::{
    Entities, LazyUpdate, LendJoin, Read, ReadExpect, ReadStorage, SystemData, WriteStorage, shred,
};

use common::{
    assets::{AssetExt, Ron},
    combat::CombatTuning,
    comp::{
        self, AbilityCooldowns, AbilityPool, ActiveAbilities, AttunedItems, Beam, Body, Buffs,
        CharacterActivity, CharacterState, Combo, Controller, Density, Energy, Hardcore, Health,
        Immovable, Inventory, InventoryManip, Mass, Melee, Ori, PhysicsState, PickupItem, Poise,
        Pos, PreviousPhysCache, Scale, SkillSet, SlotState, Stance, StateUpdate, Stats,
        TriggerSlots, Vel,
        ability::{Ability, AuxiliaryAbility, SpecifiedAbility},
        character_state::{CharacterStateEvents, OutputEvents},
        controller::{ControlAction, InputKind},
        inventory::item::{MaterialStatManifest, tool::AbilityMap},
        trigger::{FIRING_DEADLINE_SECS, MAX_TRIGGER_SLOTS, unlocked_trigger_slots},
    },
    event::{self, EventBus, KnockbackEvent, LocalEvent},
    link::Is,
    mounting::{Rider, VolumeRider},
    outcome::Outcome,
    resources::{DeltaTime, GameMode, OracleLive, Time},
    states::{
        behavior::{JoinData, JoinStruct},
        idle,
    },
    terrain::TerrainGrid,
    tether::{Follower, Leader},
    uid::{IdMaps, Uid},
};
use common_ecs::{Job, Origin, Phase, System};

#[derive(SystemData)]
pub struct ReadData<'a> {
    entities: Entities<'a>,
    events: CharacterStateEvents<'a>,
    local_bus: Read<'a, EventBus<LocalEvent>>,
    dt: Read<'a, DeltaTime>,
    time: Read<'a, Time>,
    lazy_update: Read<'a, LazyUpdate>,
    healths: ReadStorage<'a, Health>,
    hardcores: ReadStorage<'a, Hardcore>,
    heads: ReadStorage<'a, Heads>,
    bodies: ReadStorage<'a, Body>,
    masses: ReadStorage<'a, Mass>,
    scales: ReadStorage<'a, Scale>,
    physics_states: ReadStorage<'a, PhysicsState>,
    melee_attacks: ReadStorage<'a, Melee>,
    beams: ReadStorage<'a, Beam>,
    uids: ReadStorage<'a, Uid>,
    is_riders: ReadStorage<'a, Is<Rider>>,
    is_volume_riders: ReadStorage<'a, Is<VolumeRider>>,
    stats: ReadStorage<'a, Stats>,
    skill_sets: ReadStorage<'a, SkillSet>,
    active_abilities: ReadStorage<'a, ActiveAbilities>,
    ability_cooldowns: ReadStorage<'a, AbilityCooldowns>,
    ability_pool: ReadStorage<'a, AbilityPool>,
    // Xindeler: read alongside `ability_pool` so the spell-level gate can be
    // evaluated against each held class's own level on the activation path.
    character_classes: ReadStorage<'a, comp::CharacterClass>,
    msm: ReadExpect<'a, MaterialStatManifest>,
    ability_map: ReadExpect<'a, AbilityMap>,
    oracle_live: ReadExpect<'a, OracleLive>,
    // Xindeler: the trigger-slot cooldown is held in REAL-WORLD time, and the
    // only clock that may write it is the server's. See `start_slot_cooldowns`.
    game_mode: ReadExpect<'a, GameMode>,
    combos: ReadStorage<'a, Combo>,
    alignments: ReadStorage<'a, comp::Alignment>,
    terrain: ReadExpect<'a, TerrainGrid>,
    inventories: ReadStorage<'a, Inventory>,
    attuned_items: ReadStorage<'a, AttunedItems>,
    stances: ReadStorage<'a, Stance>,
    prev_phys_caches: ReadStorage<'a, PreviousPhysCache>,
    buffs: ReadStorage<'a, Buffs>,
    pickup_items: ReadStorage<'a, PickupItem>,
    immovables: ReadStorage<'a, Immovable>,
    is_followers: ReadStorage<'a, Is<Follower>>,
}

/// ## Character Behavior System
/// Passes `JoinData` to `CharacterState`'s `behavior` handler fn's. Receives a
/// `StateUpdate` in return and performs updates to ECS Components from that.
#[derive(Default)]
pub struct Sys;

impl<'a> System<'a> for Sys {
    type SystemData = (
        ReadData<'a>,
        WriteStorage<'a, CharacterState>,
        WriteStorage<'a, CharacterActivity>,
        WriteStorage<'a, Pos>,
        WriteStorage<'a, Vel>,
        WriteStorage<'a, Ori>,
        WriteStorage<'a, Density>,
        WriteStorage<'a, Energy>,
        WriteStorage<'a, Controller>,
        WriteStorage<'a, Poise>,
        WriteStorage<'a, TriggerSlots>,
        Read<'a, EventBus<Outcome>>,
        Read<'a, IdMaps>,
    );

    const NAME: &'static str = "character_behavior";
    const ORIGIN: Origin = Origin::Common;
    const PHASE: Phase = Phase::Create;

    fn run(
        _job: &mut Job<Self>,
        (
            read_data,
            mut character_states,
            mut character_activities,
            mut positions,
            mut velocities,
            mut orientations,
            mut densities,
            mut energies,
            mut controllers,
            mut poises,
            mut trigger_slots,
            outcomes,
            id_maps,
        ): Self::SystemData,
    ) {
        // Reactive trigger slots: evaluate conditions and fire, BEFORE
        // `controller.actions` is drained below, so a slot that fires this tick
        // is cast this tick.
        //
        // 🔴 Perf: this joins on `TriggerSlots` FIRST. The component is present
        // on approximately zero entities in a live world, so the bitset
        // intersection rejects them before `Health` / `Energy` / `SkillSet` are
        // ever touched, and the whole block costs a single bitset test per
        // entity. Nothing here reads the system clock.
        Self::evaluate_triggers(
            &read_data,
            &mut trigger_slots,
            &mut controllers,
            &character_states,
            &energies,
        );

        // Slots whose authorisation token was actually spent this tick, applied
        // after the main loop releases its borrows.
        let mut triggers_fired: Vec<(specs::Entity, u8)> = Vec::new();

        let mut local_emitter = read_data.local_bus.emitter();
        let mut outcomes_emitter = outcomes.emitter();
        let mut emitters = read_data.events.get_emitters();

        let mut local_events = Vec::new();
        let mut output_events = OutputEvents::new(&mut local_events, &mut emitters);

        let join = (
            &read_data.entities,
            &read_data.uids,
            &mut character_states,
            &mut character_activities,
            &mut positions,
            &mut velocities,
            &mut orientations,
            &read_data.masses,
            &mut densities,
            &mut energies,
            read_data.inventories.maybe(),
            &mut controllers,
            read_data.healths.maybe(),
            read_data.heads.maybe(),
            (
                &read_data.bodies,
                &read_data.physics_states,
                read_data.scales.maybe(),
                &read_data.stats,
                &read_data.skill_sets,
                read_data.active_abilities.maybe(),
                read_data.ability_cooldowns.maybe(),
                read_data.ability_pool.maybe(),
                read_data.is_riders.maybe(),
            ),
            read_data.combos.maybe(),
        )
            .lend_join();
        join.for_each(|comps| {
            let (
                entity,
                uid,
                mut char_state,
                character_activity,
                pos,
                vel,
                ori,
                mass,
                density,
                energy,
                inventory,
                controller,
                health,
                heads,
                (
                    body,
                    physics,
                    scale,
                    stat,
                    skill_set,
                    active_abilities,
                    ability_cooldowns,
                    ability_pool,
                    is_rider,
                ),
                combo,
            ) = comps;
            // Safety net for `TelekineticGrip`: if the caster leaves that state through any
            // path that skips its own release logic (an interrupt, disconnect, death, ...),
            // make sure the `Is<Leader>` half of the tether link doesn't outlive it. The
            // `tether` system self-heals the follower's `Is<Follower>` once it notices the
            // leader lost its `Is<Leader>` (see `common/systems/src/tether.rs`), so
            // dropping just this side here is enough to unwind the whole link.
            //
            // This must run *before*, and independently of, the death early-return below:
            // dying doesn't itself change `char_state` away from `TelekineticGrip` (death
            // is tracked separately via `Health.is_dead`), so a dead caster's state can
            // still read as `TelekineticGrip` here — without the explicit `is_dead` check
            // a corpse would keep holding its item tethered to it indefinitely.
            let is_dead = health.is_some_and(|h| h.is_dead);
            if is_dead || !matches!(&*char_state, CharacterState::TelekineticGrip(_)) {
                read_data.lazy_update.remove::<Is<Leader>>(entity);
            }

            // Being dead overrides all other states
            if is_dead {
                // Do nothing
                return;
            }

            // Remove components that entity should not have if not in relevant char state
            if !char_state.is_melee_attack() {
                read_data.lazy_update.remove::<Melee>(entity);
            }
            if !char_state.is_beam_attack() {
                read_data.lazy_update.remove::<Beam>(entity);
            }

            // Enter stunned state if poise damage is enough
            if let Some(mut poise) = poises.get_mut(entity) {
                let was_wielded = char_state.is_wield();
                let poise_state = poise.poise_state();
                let pos = pos.0;
                if let (Some((stunned_state, stunned_duration)), impulse_strength) =
                    poise_state.poise_effect(was_wielded)
                {
                    // Reset poise if there is some stunned state to apply
                    poise.reset(*read_data.time, stunned_duration);
                    if !comp::is_downed(health, Some(&char_state)) {
                        *char_state = stunned_state;
                    }
                    outcomes_emitter.emit(Outcome::PoiseChange {
                        pos,
                        state: poise_state,
                    });
                    if let Some(impulse_strength) = impulse_strength {
                        output_events.emit_server(KnockbackEvent {
                            entity,
                            impulse: impulse_strength * *poise.knockback(),
                        });
                    }
                }
            }

            // Controller actions
            let actions = std::mem::take(&mut controller.actions);

            let mut join_struct = JoinStruct {
                entity,
                uid,
                char_state,
                character_activity,
                pos,
                vel,
                ori,
                scale,
                mass,
                density,
                energy,
                inventory,
                attuned: read_data.attuned_items.get(entity),
                controller,
                health,
                hardcore: read_data.hardcores.get(entity).is_some(),
                heads,
                body,
                physics,
                melee_attack: read_data.melee_attacks.get(entity),
                beam: read_data.beams.get(entity),
                stat,
                skill_set,
                active_abilities,
                ability_cooldowns,
                ability_pool,
                character_class: read_data.character_classes.get(entity),
                trigger_slots: trigger_slots.get(entity),
                combo,
                alignment: read_data.alignments.get(entity),
                terrain: &read_data.terrain,
                mount_data: read_data.is_riders.get(entity),
                volume_mount_data: read_data.is_volume_riders.get(entity),
                stance: read_data.stances.get(entity),
                id_maps: &id_maps,
                alignments: &read_data.alignments,
                prev_phys_caches: &read_data.prev_phys_caches,
                bodies: &read_data.bodies,
                buffs: read_data.buffs.get(entity),
                pickup_items: &read_data.pickup_items,
                immovables: &read_data.immovables,
                is_followers: &read_data.is_followers,
            };

            for action in actions {
                let j = JoinData::new(
                    &join_struct,
                    &read_data.lazy_update,
                    &read_data.dt,
                    &read_data.time,
                    &read_data.msm,
                    &read_data.ability_map,
                    &read_data.oracle_live,
                );
                let state_update = j.character.handle_event(&j, &mut output_events, action);
                if let Some(slot) = state_update.triggered_slot_cast {
                    triggers_fired.push((entity, slot));
                }
                Self::publish_state_update(&mut join_struct, state_update, &mut output_events);
            }

            // Mounted occurs after control actions have been handled
            // If mounted, character state is controlled by mount
            if is_rider.is_some() && !join_struct.char_state.can_perform_mounted() {
                // TODO: A better way to swap between mount inputs and rider inputs
                *join_struct.char_state = CharacterState::Idle(idle::Data::default());
                return;
            }

            let j = JoinData::new(
                &join_struct,
                &read_data.lazy_update,
                &read_data.dt,
                &read_data.time,
                &read_data.msm,
                &read_data.ability_map,
                &read_data.oracle_live,
            );

            let state_update = j.character.behavior(&j, &mut output_events);
            if let Some(slot) = state_update.triggered_slot_cast {
                triggers_fired.push((entity, slot));
            }
            Self::publish_state_update(&mut join_struct, state_update, &mut output_events);
        });

        Self::start_slot_cooldowns(
            &read_data,
            &mut trigger_slots,
            &mut controllers,
            &triggers_fired,
        );

        local_emitter.append_vec(local_events);
    }
}

impl Sys {
    /// Advance every trigger slot's state machine and fire the ones whose
    /// condition just became true.
    ///
    /// ```text
    /// Ready ──condition true──▶ Firing ──cast resolves──▶ CoolingDown ──clock──▶ Ready
    ///                             │
    ///                             └── deadline expires, cast never happened ──▶ Ready
    /// ```
    ///
    /// Firing means two things, together: push a `StartInput` naming the slot,
    /// and mint the one-shot authorisation token by moving the slot to
    /// `Firing`. It deliberately does **not** write `CharacterState` directly
    /// the way the poise → stunned override just above does: that route skips
    /// `requirements_paid`, the energy charge, the antimagic check and the
    /// castable-source gate, which would hand a client an unvalidated cast
    /// path. A trigger takes exactly the same road a key press does.
    ///
    /// Only the `Ready` → `Firing` edge is gated on the slot being unlocked at
    /// the character's level; a slot that is cooling down keeps cooling down
    /// even if a level change hid it, and its configuration is never cleared.
    ///
    /// 🔴 **A corpse neither arms nor fires.** This runs before the main loop's
    /// own `if is_dead { return; }` guard, so it has to repeat it: at 0 HP
    /// `HealthBelow(x)` and `DamageTaken` are both true, and without the check
    /// a death would arm the slot, never cast (the corpse has no character
    /// state to consume the input), re-arm on the next tick for as long as the
    /// corpse exists — and, because respawning restores `Health` without
    /// clearing `Controller::queued_inputs`, finally cast the queued input at
    /// full health on the first tick after the respawn. That burns a cooldown
    /// of up to thirty-six real-world hours on a cast the player never wanted.
    /// So death both refuses the `Ready` → `Firing` edge and forces an
    /// outstanding `Firing` back to `Ready`, taking its queued input with it.
    fn evaluate_triggers(
        read_data: &ReadData,
        trigger_slots: &mut WriteStorage<TriggerSlots>,
        controllers: &mut WriteStorage<Controller>,
        character_states: &WriteStorage<CharacterState>,
        energies: &WriteStorage<Energy>,
    ) {
        let now = *read_data.time;

        (
            // FIRST on purpose: nearly every entity in the world lacks this
            // component and is rejected by the bitset intersection here, before
            // anything else about it is read.
            trigger_slots,
            controllers,
            &read_data.entities,
            &read_data.skill_sets,
        )
            .lend_join()
            .for_each(|(mut slots, controller, entity, skill_set)| {
                if !slots.has_any_configured() {
                    return;
                }

                let health = read_data.healths.get(entity);
                let energy = energies.get(entity);
                let inventory = read_data.inventories.get(entity);
                let ability_pool = read_data.ability_pool.get(entity);
                let char_state = character_states.get(entity);
                let unlocked = unlocked_trigger_slots(skill_set.character_level());
                // Mirrors the main loop's own guard, which this runs before.
                let is_dead = health.is_some_and(|h| h.is_dead);

                // Decided by reading only, then applied in one pass: touching
                // the component mutably marks it dirty and costs a network
                // resync, so an idle tick must not touch it at all.
                let mut transitions: [Option<SlotState>; MAX_TRIGGER_SLOTS] = Default::default();

                for (index, transition) in transitions.iter_mut().enumerate() {
                    let Some(slot) = slots.get(index) else {
                        continue;
                    };
                    let input = InputKind::TriggerAbility(index as u8);

                    match &slot.state {
                        SlotState::Ready => {
                            if is_dead
                                || index >= unlocked
                                || !slot.condition.is_met(now, health, energy)
                            {
                                continue;
                            }
                            // `AuxiliaryAbility::Innate` by construction (see
                            // `TriggerAbility`), so `ability_id` is independent
                            // of `context_index` and the token the evaluator
                            // mints is exactly the id the cast will resolve to.
                            let spec = SpecifiedAbility {
                                ability: Ability::from(AuxiliaryAbility::from(slot.ability)),
                                context_index: None,
                            };
                            let Some(ability_id) = spec
                                .ability_id(char_state, inventory, ability_pool)
                                .map(str::to_string)
                            else {
                                continue;
                            };
                            // Fix the price now, with the token: nothing may
                            // re-configure the slot mid-cast and have it charged
                            // as a different spell.
                            let circle = ability_pool
                                .and_then(|pool| pool.spell_gate(slot.ability.pool_index()))
                                .map_or(0, |gate| gate.spell_level);
                            controller.push_action(ControlAction::StartInput {
                                input,
                                target_entity: None,
                                select_pos: None,
                            });
                            *transition = Some(SlotState::firing(
                                ability_id,
                                now.add_seconds(FIRING_DEADLINE_SECS),
                                circle,
                            ));
                        },
                        SlotState::Firing { deadline, .. } => {
                            // The cast never happened — the caster died, ran out
                            // of energy, was blocked by an antimagic field, or
                            // stayed busy too long. Give the slot straight back,
                            // having spent nothing: a failed cast must never
                            // burn a cooldown measured in hours.
                            if is_dead || now.0 > deadline.0 {
                                controller.queued_inputs.remove(&input);
                                *transition = Some(SlotState::Ready);
                            }
                        },
                        SlotState::CoolingDown {
                            ready_at,
                            ready_at_time,
                        } => {
                            // Compared against the in-game `Time` projection the
                            // server computed when the slot fired, never against
                            // a system clock — this runs on the client too, and
                            // a client's clock is attacker-controlled.
                            if now.0 < ready_at_time.0 {
                                continue;
                            }
                            // 🔴 …but in-game `Time` is `/time_scale`-able, and
                            // `/time_scale 1000` would collapse a thirty-six
                            // hour wait into about two minutes. The wall clock
                            // is the authority (which is exactly why the slot
                            // stores it), so before actually releasing, the
                            // server confirms against it — and, if the
                            // projection merely ran ahead, rebuilds the
                            // projection from the real remaining wait instead of
                            // releasing. `ready_at` is `None` on a client (it is
                            // `#[serde(skip)]`), so a client only ever predicts
                            // from the projection and never reads a clock; and
                            // because a failed confirmation re-projects, the
                            // read happens once per drift, not once per tick.
                            match ready_at {
                                Some(at) => {
                                    let now_utc = Utc::now();
                                    if now_utc < *at {
                                        let remaining =
                                            (*at - now_utc).num_milliseconds() as f64 / 1000.0;
                                        *transition = Some(SlotState::CoolingDown {
                                            ready_at: Some(*at),
                                            ready_at_time: now.add_seconds(remaining),
                                        });
                                    } else {
                                        *transition = Some(SlotState::Ready);
                                    }
                                },
                                None => *transition = Some(SlotState::Ready),
                            }
                        },
                    }
                }

                // 🔴 Guarded, and the order of these two `let`s matters: this
                // component is a `DerefFlaggedStorage`, so reaching for
                // `get_mut` at all — even to write the same value back — marks
                // it modified and costs a full component resync for that player
                // this tick. `an_idle_tick_never_touches_the_component` fails
                // loudly if a later refactor swaps them round.
                if transitions.iter().any(Option::is_some) {
                    for (index, transition) in transitions.into_iter().enumerate() {
                        if let Some(state) = transition
                            && let Some(slot) = slots.get_mut(index)
                        {
                            slot.state = state;
                        }
                    }
                }
            });
    }

    /// Move the slots whose authorisation token was spent this tick from
    /// `Firing` to `CoolingDown`, and clear the input they queued.
    ///
    /// The wait is looked up by the spell circle **carried in the token**
    /// (`SlotState::Firing { circle, .. }`), fixed when the slot armed — not
    /// re-read from the slot's configuration here, which would let a slot fire
    /// one spell and be priced as another once the configure UI exists.
    /// Anything without a circle (a racial innate) uses the circle-0 floor.
    ///
    /// 🔴 The wall-clock instant is minted **server-side only**. This is a
    /// common system, so a client running its own predicted fire path would
    /// otherwise stamp `ready_at` from its own — attacker-controlled — system
    /// clock, falsifying the invariant that `ready_at` is always `None` on a
    /// client. The client keeps the in-game `Time` projection it needs for
    /// prediction and nothing else. Together with character load, this is one
    /// of only two places a wall-clock read is allowed, and it is reached at
    /// most once per slot per cooldown, i.e. once per slot per several hours.
    fn start_slot_cooldowns(
        read_data: &ReadData,
        trigger_slots: &mut WriteStorage<TriggerSlots>,
        controllers: &mut WriteStorage<Controller>,
        fired: &[(specs::Entity, u8)],
    ) {
        if fired.is_empty() {
            return;
        }
        let now = *read_data.time;
        let server_side = !matches!(*read_data.game_mode, GameMode::Client);
        let tuning = Ron::<CombatTuning>::load_expect("common.combat_tuning");
        let tuning = &tuning.read().0;

        for &(entity, index) in fired {
            if let Some(mut slots) = trigger_slots.get_mut(entity)
                && let Some(slot) = slots.get_mut(usize::from(index))
                && let Some(circle) = slot.state.firing_circle()
            {
                let cooldown = f64::from(tuning.trigger_slot_cooldown(circle));
                slot.state = SlotState::CoolingDown {
                    ready_at: server_side.then(|| {
                        Utc::now() + chrono::TimeDelta::milliseconds((cooldown * 1000.0) as i64)
                    }),
                    ready_at_time: now.add_seconds(cooldown),
                };
            }
            if let Some(controller) = controllers.get_mut(entity) {
                controller
                    .queued_inputs
                    .remove(&InputKind::TriggerAbility(index));
            }
        }
    }

    fn publish_state_update(
        join: &mut JoinStruct,
        state_update: StateUpdate,
        output_events: &mut OutputEvents,
    ) {
        // Here we check for equality with the previous value of these components before
        // updating them so that the modification detection will not be
        // triggered unnecessarily. This is important for minimizing updates
        // sent to the clients (and thus keeping bandwidth usage down).
        //
        // TODO: if checking equality is expensive for char_state use optional field in
        // StateUpdate
        if *join.char_state != state_update.character {
            *join.char_state = state_update.character
        }
        if *join.character_activity != state_update.character_activity {
            *join.character_activity = state_update.character_activity
        }
        if *join.density != state_update.density {
            *join.density = state_update.density
        }
        if *join.energy != state_update.energy {
            *join.energy = state_update.energy;
        };

        // These components use a different type of change detection.
        *join.pos = state_update.pos;
        *join.vel = state_update.vel;
        *join.ori = state_update.ori;

        for (input, attr) in state_update.queued_inputs {
            join.controller.queued_inputs.insert(input, attr);
        }
        for input in state_update.removed_inputs {
            join.controller.queued_inputs.remove(&input);
        }
        if state_update.swap_equipped_weapons {
            output_events.emit_server(event::InventoryManipEvent(
                join.entity,
                InventoryManip::SwapEquippedWeapons,
            ));
        }
    }
}
