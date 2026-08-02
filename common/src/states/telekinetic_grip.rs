//! `TelekineticGrip` — a shared "grab a loose dropped item at range, hold it,
//! then place or throw it" mechanism.
//!
//! Scope is loose dropped [`crate::comp::PickupItem`] entities only — never
//! creatures, never hand-placed scenery objects.
//!
//! Shape mirrors [`crate::states::basic_beam`]'s buildup → sustain-while-held
//! pattern, but the actual "hold at arm's length while the caster moves"
//! physics is fully delegated to the already-shipped [`crate::tether`] link
//! (`Tethered`/`Is<Leader>`/`Is<Follower>`, today used for leashed pets) — this
//! state only validates the grab and manages the link's lifetime. Release
//! reuses [`crate::states::throw`]'s charge-based speed formula
//! (`initial_projectile_speed + charge_frac * scaled_projectile_speed`) and,
//! for a throw, [`crate::comp::projectile::ProjectileConstructor`] — the same
//! primitive `Throw` uses to deal real damage on hit.

use crate::{
    comp::{
        CharacterState, DerivedStats, StateUpdate, Vel, character_state::OutputEvents,
        projectile::ProjectileConstructor,
    },
    link::{Is, LinkHandle},
    states::{
        behavior::{CharacterBehavior, JoinData},
        utils::*,
    },
    tether::{Follower, Leader, Tethered},
    uid::Uid,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Separated out to condense update portions of character state
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticData {
    /// How long the weapon needs to be prepared for
    pub buildup_duration: Duration,
    /// How long the item can be held while charging a throw. Mirrors
    /// `throw::StaticData::charge_duration` — the release speed scales
    /// linearly with how long the ability has been held, capped here.
    pub charge_duration: Duration,
    /// Minimum hold time before a release counts as a "throw" instead of a
    /// gentle "place". Releasing before this elapses just sets the item back
    /// down at rest.
    pub place_threshold: Duration,
    /// How long the state has until exiting
    pub recover_duration: Duration,
    /// Energy drained per second while holding the grip
    pub energy_drain: f32,
    /// Max distance to the target for the grab to succeed
    pub range: f32,
    /// Distance the gripped item is held at in front of the caster, fed
    /// straight into the `Tethered` link created on a successful grab
    pub tether_length: f32,
    /// Base speed applied to a thrown item
    pub initial_projectile_speed: f32,
    /// Additional speed applied to a thrown item at full charge
    pub scaled_projectile_speed: f32,
    /// Attack the thrown item deals if it lands on a creature — reuses
    /// `ProjectileConstructor`/`comp::Projectile` exactly like `Throw`,
    /// including `CombatEffect::Knockback` via `ProjectileAttack::knockback`.
    /// `pierce_entities` should be set on this so the item itself is never
    /// despawned by the hit (a struck creature just takes the hit; the item
    /// keeps existing as ordinary dropped loot afterwards).
    pub projectile: ProjectileConstructor,
    /// Move speed efficiency while holding the grip
    pub move_speed: f32,
    /// What key is used to press ability
    pub ability_info: AbilityInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    /// Struct containing data that does not change over the course of the
    /// character state
    pub static_data: StaticData,
    /// Timer for each stage
    pub timer: Duration,
    /// What section the character stage is in
    pub stage_section: StageSection,
    /// Uid of the item currently gripped, set once the buildup-stage grab
    /// validates successfully. `None` for the whole state if the grab failed
    /// (no target, or it failed one of the checks in `try_grab`).
    pub item: Option<Uid>,
    /// Whether the grip was released as a throw (vs. a gentle place). Only
    /// meaningful once release has happened (`stage_section` is `Recover` or
    /// later) — informational (frontend/telemetry), the release logic itself
    /// already acted on this in the `Charge` stage.
    pub thrown: bool,
}

impl Data {
    /// How complete the hold-charge is, on a scale of 0.0 to 1.0. Mirrors
    /// `throw::Data::charge_frac`.
    pub fn charge_frac(&self) -> f32 {
        if let StageSection::Charge = self.stage_section {
            (self.timer.as_secs_f32() / self.static_data.charge_duration.as_secs_f32()).min(1.0)
        } else {
            0.0
        }
    }

    /// Buildup-stage validation: the targeted entity must (a) carry
    /// `PickupItem` — it's grabbable loot, (b) not carry `Immovable`, (c) be
    /// within `range`, and (d) not already be `Is<Follower>` of someone
    /// else's grip/leash. Returns the item's `Uid` and resolved `Entity` on
    /// success.
    ///
    /// Doesn't re-check the cycle-prevention invariants `Tethered::create`
    /// enforces (target not already `Is<Leader>` of something, no
    /// self-tether) — those only matter for a follower that could itself be
    /// a leader or the same entity as the caster, which a `PickupItem`
    /// never is. Also doesn't guard against two casters completing buildup
    /// on the very same item within the same tick: both would pass this
    /// check (it reads a per-tick snapshot), and whichever `LazyUpdate`
    /// insert lands second wins the `Is<Follower>`. The loser isn't left
    /// stuck, though — `tether::Sys`'s self-heal (see
    /// `common/systems/src/tether.rs`) now compares the *identity* of the
    /// leader's link, not just whether it has one, so it cleans up the
    /// loser's now-orphaned `Is<Leader>` on the next tick. Still, an
    /// authoritative single-claim guard (mirroring how
    /// `InventoryManip::Pickup` — `server/src/events/inventory_manip.rs` —
    /// avoids this exact race for regular pickup) would be a better fix if
    /// contested grabs turn out to matter in practice once `mage_hand`/
    /// `telekinesis`/`catapult` make ranged grabbing a routine action.
    fn try_grab(&self, data: &JoinData) -> Option<(Uid, specs::Entity)> {
        let target_uid = self
            .static_data
            .ability_info
            .input_attr
            .and_then(|input_attr| input_attr.target_entity)?;
        let target_entity = data.id_maps.uid_entity(target_uid)?;

        let is_grabbable = data.pickup_items.contains(target_entity);
        let is_immovable = data.immovables.contains(target_entity);
        let already_gripped = data.is_followers.contains(target_entity);
        let in_range = data
            .prev_phys_caches
            .get(target_entity)
            .and_then(|cache| cache.pos)
            .is_some_and(|pos| {
                pos.0.distance_squared(data.pos.0) <= self.static_data.range.powi(2)
            });

        (is_grabbable && !is_immovable && !already_gripped && in_range)
            .then_some((target_uid, target_entity))
    }
}

impl CharacterBehavior for Data {
    fn behavior(&self, data: &JoinData, output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);

        handle_orientation(data, &mut update, 1.0, None);
        handle_move(data, &mut update, self.static_data.move_speed);
        handle_jump(data, output_events, &mut update, 1.0);

        match self.stage_section {
            StageSection::Buildup => {
                if self.timer < self.static_data.buildup_duration {
                    // Build up
                    if let CharacterState::TelekineticGrip(c) = &mut update.character {
                        c.timer = tick_attack_or_default(data, self.timer, None);
                    }
                } else if let Some((item_uid, item_entity)) = self.try_grab(data) {
                    // Grab succeeded: wire the existing `Tethered` link (caster=Leader,
                    // item=Follower) directly via `LazyUpdate`, bypassing the
                    // `state.link()`/`LinkHandle` bookkeeping convenience wrapper — that
                    // wrapper's own validation (self-tether, rider/volume-rider conflicts,
                    // cycle prevention) doesn't apply here since the follower side is
                    // always a `PickupItem`, never a mount or another leader, and this
                    // state already re-implements the meaningful checks in `try_grab`.
                    let handle = LinkHandle::from_link(Tethered {
                        leader: *data.uid,
                        follower: item_uid,
                        tether_length: self.static_data.tether_length,
                    });
                    data.updater
                        .insert(data.entity, handle.make_role::<Leader>());
                    data.updater
                        .insert(item_entity, handle.make_role::<Follower>());

                    if let CharacterState::TelekineticGrip(c) = &mut update.character {
                        c.timer = Duration::default();
                        c.stage_section = StageSection::Charge;
                        c.item = Some(item_uid);
                    }
                } else {
                    // Nothing grippable in range/valid: the cast fizzles.
                    end_ability(data, &mut update);
                }
            },
            StageSection::Charge => {
                let held = input_is_pressed(data, self.static_data.ability_info.input);

                if held && self.timer < self.static_data.charge_duration {
                    // Charges
                    if let CharacterState::TelekineticGrip(c) = &mut update.character {
                        c.timer = tick_attack_or_default(data, self.timer, None);
                    }
                    update
                        .energy
                        .change_by(-self.static_data.energy_drain * data.dt.0);
                } else if held {
                    // Fully charged but still held: keep the grip alive at reduced upkeep,
                    // mirroring `Throw`'s "holds charge" branch.
                    update
                        .energy
                        .change_by(-self.static_data.energy_drain * data.dt.0 / 5.0);
                } else {
                    // Released. Distinguish place vs throw by how long the item was held
                    // (mirrors `throw::Data`'s own charge/release shape): a quick tap just
                    // sets it down, a deliberate hold-then-release throws it.
                    let throw = self.timer >= self.static_data.place_threshold;

                    // Free the caster's side of the link unconditionally — don't wait for
                    // `Recover`, so it stops pulling the instant the player lets go — even
                    // if the item itself is already gone (e.g. despawned/merged away by
                    // `server/src/sys/item.rs` mid-charge). Only the item-side effects
                    // below need the item to still resolve.
                    data.updater.remove::<Is<Leader>>(data.entity);

                    if let Some(item_entity) =
                        self.item.and_then(|uid| data.id_maps.uid_entity(uid))
                    {
                        data.updater.remove::<Is<Follower>>(item_entity);

                        if throw {
                            let charge_frac = self.charge_frac();
                            let speed = self.static_data.initial_projectile_speed
                                + charge_frac * self.static_data.scaled_projectile_speed;
                            let vel = *data.inputs.look_dir * speed + data.vel.0;
                            data.updater.insert(item_entity, Vel(vel));

                            let precision_mult = data
                                .derived
                                .map_or(DerivedStats::DEFAULT_PRECISION_MULT, |d| d.precision_mult);
                            let (projectile, _marker) =
                                self.static_data.projectile.clone().create_projectile(
                                    Some(*data.uid),
                                    precision_mult,
                                    Some(self.static_data.ability_info),
                                    Some(data.stats),
                                );
                            data.updater.insert(item_entity, projectile);
                        } else {
                            // Place: bring it to rest right where it's held.
                            data.updater.insert(item_entity, Vel::default());
                        }
                    }

                    if let CharacterState::TelekineticGrip(c) = &mut update.character {
                        c.timer = Duration::default();
                        c.stage_section = StageSection::Recover;
                        c.thrown = throw;
                    }
                }
            },
            StageSection::Recover => {
                if self.timer < self.static_data.recover_duration {
                    // Recovers
                    if let CharacterState::TelekineticGrip(c) = &mut update.character {
                        c.timer = tick_attack_or_default(data, self.timer, None);
                    }
                } else {
                    // Done. Note: the caster's own recovery animation is much shorter
                    // than how long a thrown item needs to stay a live hazard, so the
                    // transient `Projectile` component's own `time_left` (see
                    // `StaticData::projectile`) — not this stage — is what eventually
                    // clears it on a miss.
                    end_ability(data, &mut update);
                }
            },
            _ => {
                // If it somehow ends up in an incorrect stage section
                end_ability(data, &mut update);
            },
        }

        // At end of state logic so an interrupt isn't overwritten
        handle_interrupts(data, &mut update, output_events);

        update
    }
}
