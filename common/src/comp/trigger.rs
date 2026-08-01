//! Reactive **trigger slots**: an ability that casts itself when a condition
//! about the caster becomes true.
//!
//! A slot holds one ability plus one condition. When the condition becomes
//! true, the server's own evaluator (in
//! `common/systems/src/character_behavior.rs`) pushes an
//! [`InputKind::TriggerAbility`](crate::comp::InputKind::TriggerAbility) input
//! and mints a one-shot, ability-bound authorisation token by moving the slot
//! into [`SlotState::Firing`]. That token — and *only* that token — buys the
//! cast exactly one privilege:
//!
//! > A trigger grants **exactly one** thing a key-press cannot: for one cast,
//! > of one specific ability named by a server-minted token, the ability's own
//! > `AbilityCooldowns` entry is neither read nor written. **Every other
//! > validation the server performs on a key-pressed cast is performed
//! > identically.**
//!
//! The price of that privilege is the *slot* cooldown — 10 minutes to 36
//! **real-world** hours — held in server-authoritative persisted wall-clock
//! state the client cannot write, cannot reset by relogging, and cannot
//! request.

use crate::{
    comp::{Energy, Health, ability::AuxiliaryAbility},
    resources::Time,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};

/// The only kind of ability a trigger slot may hold: an entry in the caster's
/// [`AbilityPool`](crate::comp::ability::AbilityPool) — a class spell, a
/// learned spell, or a racial innate. Exactly what
/// [`AuxiliaryAbility::Innate`] names, and nothing else.
///
/// 🔴 **Deliberately not `AuxiliaryAbility`.** That type also admits
/// `MainWeapon(i)` / `OffWeapon(i)` / `Glider(i)`, and a *contextualized*
/// weapon ability resolves to a `SpecifiedAbility` carrying a
/// `context_index: Some(_)`. The evaluator mints its authorisation token from
/// `SpecifiedAbility { ability, context_index: None }`, so for such an ability
/// the token's id and the resolved cast's id would **differ**: the bypass would
/// silently not apply, and — worse — the cast would then be treated as a normal
/// one and emit `SetAbilityCooldownEvent`, writing the player's *manual*
/// cooldown. That is precisely the grief §10.3d of the plan forbids
/// ("a triggered cast neither reads nor writes the triggered ability's
/// `AbilityCooldowns` entry").
///
/// Narrowing the type is simpler and strictly safer than reconciling
/// `context_index` on the token: `Ability::InnateAux`'s `ability_id` ignores
/// `context_index` entirely, so for the innate case the two ids always agree by
/// construction. `/trigger_slot` could never build anything else anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerAbility(usize);

impl TriggerAbility {
    /// Name the pool entry at `index`.
    pub fn from_pool_index(index: usize) -> Self { Self(index) }

    /// Narrow an [`AuxiliaryAbility`] to what a trigger slot may hold. `None`
    /// for every variant that is not an innate/spell pool entry — the setter
    /// side of the invariant above.
    pub fn from_auxiliary(ability: AuxiliaryAbility) -> Option<Self> {
        match ability {
            AuxiliaryAbility::Innate(index) => Some(Self(index)),
            AuxiliaryAbility::MainWeapon(_)
            | AuxiliaryAbility::OffWeapon(_)
            | AuxiliaryAbility::Glider(_)
            | AuxiliaryAbility::Empty => None,
        }
    }

    /// Its index into `AbilityPool::abilities`.
    pub fn pool_index(self) -> usize { self.0 }

    /// Re-point this binding at `index` (used when the pool is rebuilt).
    pub fn set_pool_index(&mut self, index: usize) { self.0 = index; }
}

impl From<TriggerAbility> for AuxiliaryAbility {
    fn from(ability: TriggerAbility) -> Self { AuxiliaryAbility::Innate(ability.0) }
}

/// How many trigger slots a character can ever hold. The unlock schedule is
/// [`TRIGGER_SLOT_LEVELS`].
pub const MAX_TRIGGER_SLOTS: usize = 4;

/// Character levels at which each successive trigger slot unlocks: one slot
/// every 15 levels, from 15 to 60 (`MAX_CHARACTER_LEVEL`).
pub const TRIGGER_SLOT_LEVELS: [u16; MAX_TRIGGER_SLOTS] = [15, 30, 45, 60];

/// How long a slot stays in [`SlotState::Firing`] waiting for its pushed input
/// to actually be consumed by a `CharacterState`. If the cast never happens
/// (out of energy, antimagic field, caster busy for too long) the slot returns
/// to [`SlotState::Ready`] having spent **nothing**: a failed cast must never
/// burn a cooldown measured in hours.
pub const FIRING_DEADLINE_SECS: f64 = 5.0;

/// How recently health must have dropped for [`TriggerCondition::DamageTaken`]
/// to count it. Without a window the condition would degenerate into "fire the
/// instant the slot cooldown expires", because `Health::last_change` keeps the
/// last damage forever.
pub const DAMAGE_TAKEN_WINDOW_SECS: f64 = 0.5;

/// How many trigger slots this character may use right now.
///
/// A pure function of level: **nothing is persisted, there is no milestone
/// event and no respec interaction**. This deliberately avoids a
/// `level_before < milestone && level_after >= milestone` hook, which can never
/// fire for the first milestone and would force a character-creation special
/// case. De-levelling and an admin `/set_level` are both automatically correct.
pub fn unlocked_trigger_slots(level: u16) -> usize {
    TRIGGER_SLOT_LEVELS.iter().filter(|l| level >= **l).count()
}

/// What makes a slot fire. Exactly these three, and all three are
/// emergency-shaped — which is the only shape worth spending a resource whose
/// cooldown is measured in real-world hours on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TriggerCondition {
    /// Current health as a fraction of maximum is below this (0.0..=1.0).
    HealthBelow(f32),
    /// Health dropped within the last [`DAMAGE_TAKEN_WINDOW_SECS`].
    DamageTaken,
    /// Current energy as a fraction of maximum is below this (0.0..=1.0).
    EnergyBelow(f32),
}

impl TriggerCondition {
    /// Whether the condition holds right now.
    ///
    /// Cheap by construction: every branch is a couple of float comparisons on
    /// components the caller already had in hand. See [`TriggerSlots`] for why
    /// this is only ever reached for entities that own the component.
    pub fn is_met(&self, now: Time, health: Option<&Health>, energy: Option<&Energy>) -> bool {
        match self {
            Self::HealthBelow(frac) => health.is_some_and(|h| h.fraction() < *frac),
            Self::DamageTaken => health.is_some_and(|h| {
                h.last_change.amount < 0.0
                    && now.0 - h.last_change.time.0 <= DAMAGE_TAKEN_WINDOW_SECS
            }),
            Self::EnergyBelow(frac) => energy.is_some_and(|e| e.fraction() < *frac),
        }
    }

    /// The threshold fraction, for the conditions that have one.
    pub fn threshold(&self) -> Option<f32> {
        match self {
            Self::HealthBelow(f) | Self::EnergyBelow(f) => Some(*f),
            Self::DamageTaken => None,
        }
    }
}

/// The slot's position in its state machine:
///
/// ```text
/// Ready ──condition true──▶ Firing ──cast resolves──▶ CoolingDown ──clock──▶ Ready
///                             │
///                             └── deadline expires, cast never happened ──▶ Ready
///                                 (no cooldown burned)
/// ```
///
/// 🔴 There is deliberately **no `armed` flag**: this enum subsumes it, and
/// does it better. The reason a slot does not re-fire during its own cast is
/// not an inferred latch, it is the explicit `Firing` state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SlotState {
    /// Evaluating its condition every tick.
    Ready,
    /// Input pushed; waiting for the cast to be consumed and resolve.
    Firing {
        /// The **authorisation token**: `handle_ability` grants the cooldown
        /// bypass only when the ability it resolved has exactly this id.
        ///
        /// 🔴 `#[serde(skip)]`: a server-minted authorisation token has no
        /// business crossing the wire, not even to its owner. It deserialises
        /// as the empty string, which no `ability_id` can ever equal, so a
        /// client predicting a triggered cast simply predicts it the way it
        /// predicts a key press — without the bypass. A prediction that is
        /// pessimistic about a cooldown is corrected by the next sync; leaking
        /// the token would not be.
        #[serde(skip)]
        ability_id: String,
        /// Abandons the attempt if the cast never happens.
        deadline: Time,
        /// The spell circle the slot cooldown will be priced from, fixed **at
        /// the moment the token is minted** rather than re-read from the slot's
        /// configuration when the cast resolves.
        ///
        /// 🔴 Nothing can re-configure a slot mid-`Firing` today, but the
        /// player-facing configure UI will make that possible — and then a slot
        /// would fire one spell and be charged the price of another. Carrying
        /// the price with the token closes that by construction.
        circle: u8,
    },
    /// The slot cooldown is running.
    CoolingDown {
        /// Real-world wall clock — the **authoritative, persisted** field.
        ///
        /// 🔴 `#[serde(skip)]`: it never crosses the wire, so it is `None` on
        /// the client, which has no business knowing (or being trusted with) a
        /// wall-clock instant. Persistence uses its own RFC-3339 conversion in
        /// `server/src/persistence/character/conversions.rs`, not this
        /// `Serialize` impl.
        #[serde(skip)]
        ready_at: Option<DateTime<Utc>>,
        /// In-game [`Time`] projection of `ready_at`, computed **server-side**
        /// when the slot fires and again when the character loads — the only
        /// two moments a wall-clock read is allowed, so the per-tick path never
        /// touches the system clock. Both the tick path and the client compare
        /// against this, exactly the split already used for `cooldown_ready`
        /// (the server-side check is authoritative; the client uses the synced
        /// component for prediction). A stale projection can only make a slot
        /// look *less* ready than it is, never more, so drift is fail-safe.
        ready_at_time: Time,
    },
}

impl SlotState {
    /// Mint an authorisation token for `ability_id`, priced at `circle`.
    pub fn firing(ability_id: String, deadline: Time, circle: u8) -> Self {
        Self::Firing {
            ability_id,
            deadline,
            circle,
        }
    }

    /// The outstanding authorisation token's ability id, if this slot is
    /// currently `Firing`.
    pub fn firing_token(&self) -> Option<&str> {
        match self {
            Self::Firing { ability_id, .. } => Some(ability_id.as_str()),
            _ => None,
        }
    }

    /// The spell circle a `Firing` slot's cooldown will be priced from.
    pub fn firing_circle(&self) -> Option<u8> {
        match self {
            Self::Firing { circle, .. } => Some(*circle),
            _ => None,
        }
    }

    /// The wall-clock instant this slot becomes ready again. Server-side only
    /// (see the field docs); always `None` on a client.
    pub fn ready_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::CoolingDown { ready_at, .. } => *ready_at,
            _ => None,
        }
    }

    /// The in-game `Time` projection at which this slot becomes ready again.
    pub fn ready_at_time(&self) -> Option<Time> {
        match self {
            Self::CoolingDown { ready_at_time, .. } => Some(*ready_at_time),
            _ => None,
        }
    }
}

/// One configured trigger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerSlot {
    /// A [`TriggerAbility`] rather than an [`AuxiliaryAbility`] — see that
    /// type's doc comment for why the weapon/glider variants are excluded.
    pub ability: TriggerAbility,
    pub condition: TriggerCondition,
    pub state: SlotState,
}

impl TriggerSlot {
    /// A freshly configured slot, ready to fire.
    pub fn new(ability: TriggerAbility, condition: TriggerCondition) -> Self {
        Self {
            ability,
            condition,
            state: SlotState::Ready,
        }
    }

    /// A freshly configured slot naming pool entry `index`.
    pub fn from_pool_index(index: usize, condition: TriggerCondition) -> Self {
        Self::new(TriggerAbility::from_pool_index(index), condition)
    }

    /// A freshly configured slot, **refusing** any [`AuxiliaryAbility`] a
    /// trigger may not hold. The setter half of [`TriggerAbility`]'s invariant.
    pub fn from_auxiliary(ability: AuxiliaryAbility, condition: TriggerCondition) -> Option<Self> {
        Some(Self::new(
            TriggerAbility::from_auxiliary(ability)?,
            condition,
        ))
    }
}

/// A character's four trigger slots.
///
/// 🔴 **Class-agnostic on purpose.** Nothing here knows about `ClassKind`, so
/// opening triggers to another class later is a data decision, not a code
/// change. Who *gets* the component is decided by whoever inserts it.
///
/// 🔴 **All four are always stored.** A slot at or above
/// [`unlocked_trigger_slots`] is *hidden*, never *cleared*: if a bug or an
/// admin command briefly lowered a level, clearing would silently destroy the
/// player's setup.
///
/// 🔴 **Perf.** This component is present on approximately zero entities in a
/// live world. The tick-path evaluator joins on it **first** so ~100 % of
/// entities early-out before `Health` / `Energy` / `SkillSet` are ever read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerSlots {
    pub slots: [Option<TriggerSlot>; MAX_TRIGGER_SLOTS],
}

impl TriggerSlots {
    /// The configured slot at `index`, if any. Does **not** consider whether
    /// the slot is unlocked — that is [`Self::is_unlocked`]'s job, and the two
    /// are deliberately separate so a locked slot's configuration survives.
    pub fn get(&self, index: usize) -> Option<&TriggerSlot> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut TriggerSlot> {
        self.slots.get_mut(index).and_then(Option::as_mut)
    }

    /// Whether slot `index` is usable at `level`.
    pub fn is_unlocked(index: usize, level: u16) -> bool { index < unlocked_trigger_slots(level) }

    /// The ability configured in slot `index`, used by
    /// `ActiveAbilities::get_ability` to resolve
    /// [`AbilityInput::Trigger`](crate::comp::AbilityInput::Trigger).
    pub fn configured_ability(&self, index: usize) -> Option<AuxiliaryAbility> {
        self.get(index).map(|s| s.ability.into())
    }

    /// Re-point every binding from its index in `old_pool` to the index of the
    /// SAME key in `new_pool`, **clearing** a slot whose key is gone.
    ///
    /// The trigger-slot twin of
    /// [`remap_innate_bindings`](crate::comp::ability::remap_innate_bindings),
    /// and needed for the identical reason: a slot holds a raw pool index while
    /// persistence holds a pool *key*, so any in-session pool rebuild (granting
    /// a class, learning a spell) that reorders anything silently re-points a
    /// live trigger. The evaluator would then mint its authorisation token from
    /// the shifted index and grant the cooldown bypass for the **wrong
    /// ability**, with no warning and at the price of a cooldown measured in
    /// real-world hours.
    ///
    /// A slot whose key vanished is cleared outright, because
    /// [`TriggerAbility`] cannot represent "nothing" — the deliberate
    /// difference from the hotbar, which empties the binding and keeps the
    /// slot. In practice no player-reachable path removes a pool key (both
    /// rebuild sites only ever grow the pool), so this is the unreachable
    /// branch rather than the routine one.
    pub fn remap_innate_bindings(
        &mut self,
        old_pool: &crate::comp::ability::AbilityPool,
        new_pool: &crate::comp::ability::AbilityPool,
    ) {
        for entry in self.slots.iter_mut() {
            let Some(slot) = entry else { continue };
            match old_pool
                .abilities
                .get(slot.ability.pool_index())
                .and_then(|key| new_pool.abilities.iter().position(|k| k == key))
            {
                Some(new_index) => slot.ability.set_pool_index(new_index),
                None => *entry = None,
            }
        }
    }

    /// The outstanding authorisation token for slot `index`, if it holds one.
    ///
    /// This is the **only** way `handle_ability` learns that a `TriggerAbility`
    /// input is authorised. The token is server-minted state; nothing the
    /// client can send produces it.
    pub fn firing_token(&self, index: usize) -> Option<&str> {
        self.get(index).and_then(|s| s.state.firing_token())
    }

    /// Whether any slot is configured at all. Lets the evaluator skip a
    /// component that is present but entirely empty.
    pub fn has_any_configured(&self) -> bool { self.slots.iter().any(Option::is_some) }

    /// Recompute every cooling slot's in-game `Time` projection from its
    /// authoritative wall-clock instant, and release the ones whose wait has
    /// already elapsed.
    ///
    /// Called **server-side at character load** — one of only two moments the
    /// system clock may be read (the other is a slot firing). A slot restored
    /// from storage carries an infinite projection until this runs, so
    /// forgetting to call it leaves the slot cooling forever rather than
    /// instantly ready: the failure mode points away from the exploit.
    pub fn reproject_cooldowns(&mut self, now_utc: DateTime<Utc>, now_game: Time) {
        for slot in self.slots.iter_mut().flatten() {
            if let SlotState::CoolingDown {
                ready_at,
                ready_at_time,
            } = &mut slot.state
            {
                match ready_at {
                    Some(at) if *at > now_utc => {
                        let remaining = (*at - now_utc).num_milliseconds() as f64 / 1000.0;
                        *ready_at_time = now_game.add_seconds(remaining);
                    },
                    // Elapsed while logged out (or unreadable): the wait is
                    // over, which is the whole point of holding it in real time.
                    _ => slot.state = SlotState::Ready,
                }
            }
        }
    }
}

impl Component for TriggerSlots {
    type Storage = DerefFlaggedStorage<Self, specs::DenseVecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unlock schedule, at every boundary.
    #[test]
    fn slot_count_is_derived_from_level_at_every_boundary() {
        assert_eq!(unlocked_trigger_slots(1), 0);
        assert_eq!(unlocked_trigger_slots(14), 0);
        assert_eq!(unlocked_trigger_slots(15), 1);
        assert_eq!(unlocked_trigger_slots(29), 1);
        assert_eq!(unlocked_trigger_slots(30), 2);
        assert_eq!(unlocked_trigger_slots(44), 2);
        assert_eq!(unlocked_trigger_slots(45), 3);
        assert_eq!(unlocked_trigger_slots(59), 3);
        assert_eq!(unlocked_trigger_slots(60), 4);
        // De-levelling and `/set_level` are automatically safe: the count is a
        // pure function, never an event.
        assert_eq!(unlocked_trigger_slots(u16::MAX), 4);
    }

    fn human() -> crate::comp::Body {
        crate::comp::Body::Humanoid(crate::comp::humanoid::Body::random_with(
            &mut rand::rng(),
            &crate::comp::humanoid::Species::Human,
        ))
    }

    #[test]
    fn a_threshold_condition_compares_against_the_current_fraction() {
        let body = human();
        let health = Health::new(body);
        let energy = Energy::new(body);
        let now = Time(10.0);

        // Full bars: only a threshold above 1.0 can be under-cut.
        assert!(!TriggerCondition::HealthBelow(0.99).is_met(now, Some(&health), Some(&energy)));
        assert!(TriggerCondition::HealthBelow(1.01).is_met(now, Some(&health), Some(&energy)));
        assert!(!TriggerCondition::EnergyBelow(0.99).is_met(now, Some(&health), Some(&energy)));
        assert!(TriggerCondition::EnergyBelow(1.01).is_met(now, Some(&health), Some(&energy)));

        // A missing component is never "below" anything.
        assert!(!TriggerCondition::HealthBelow(1.5).is_met(now, None, Some(&energy)));
        assert!(!TriggerCondition::EnergyBelow(1.5).is_met(now, Some(&health), None));
    }

    /// `DamageTaken` looks at recent damage only. Without the window it would
    /// degenerate into "fire the instant the slot cooldown expires", because
    /// the last change is remembered forever.
    #[test]
    fn damage_taken_only_counts_a_recent_loss() {
        let body = human();
        let mut health = Health::new(body);
        health.last_change.amount = -5.0;
        health.last_change.time = Time(100.0);

        assert!(TriggerCondition::DamageTaken.is_met(Time(100.0), Some(&health), None));
        assert!(TriggerCondition::DamageTaken.is_met(
            Time(100.0 + DAMAGE_TAKEN_WINDOW_SECS),
            Some(&health),
            None
        ));
        // Long past: not an emergency any more.
        assert!(!TriggerCondition::DamageTaken.is_met(Time(400.0), Some(&health), None));

        // Healing is not damage.
        health.last_change.amount = 5.0;
        assert!(!TriggerCondition::DamageTaken.is_met(Time(100.0), Some(&health), None));
    }

    #[test]
    fn only_the_threshold_conditions_report_a_threshold() {
        assert_eq!(TriggerCondition::HealthBelow(0.25).threshold(), Some(0.25));
        assert_eq!(TriggerCondition::EnergyBelow(0.4).threshold(), Some(0.4));
        assert_eq!(TriggerCondition::DamageTaken.threshold(), None);
    }

    /// A slot configured at level 60 and then force-set to level 20 is
    /// **hidden, not cleared**.
    #[test]
    fn a_locked_slot_is_hidden_never_cleared() {
        let mut slots = TriggerSlots::default();
        slots.slots[3] = Some(TriggerSlot::from_pool_index(
            0,
            TriggerCondition::HealthBelow(0.3),
        ));

        // At level 60 slot 3 is unlocked and configured.
        assert!(TriggerSlots::is_unlocked(3, 60));
        assert!(slots.get(3).is_some());

        // Dropped to level 20: hidden ...
        assert!(!TriggerSlots::is_unlocked(3, 20));
        // ... but the configuration is still there, untouched.
        assert_eq!(
            slots.configured_ability(3),
            Some(AuxiliaryAbility::Innate(0)),
        );
        assert_eq!(
            slots.get(3).map(|s| s.condition),
            Some(TriggerCondition::HealthBelow(0.3)),
        );

        // And it comes straight back when the level returns.
        assert!(TriggerSlots::is_unlocked(3, 60));
    }

    #[test]
    fn a_fresh_slot_is_ready_and_holds_no_token() {
        let slots = TriggerSlots {
            slots: [
                Some(TriggerSlot::from_pool_index(
                    2,
                    TriggerCondition::DamageTaken,
                )),
                None,
                None,
                None,
            ],
        };
        assert_eq!(slots.get(0).map(|s| &s.state), Some(&SlotState::Ready));
        assert_eq!(slots.firing_token(0), None);
        assert_eq!(slots.firing_token(1), None);
        assert_eq!(
            slots.configured_ability(0),
            Some(AuxiliaryAbility::Innate(2))
        );
        assert_eq!(slots.configured_ability(1), None);
    }

    #[test]
    fn a_firing_slot_exposes_exactly_one_token() {
        let mut slots = TriggerSlots::default();
        slots.slots[1] = Some(TriggerSlot {
            ability: TriggerAbility::from_pool_index(4),
            condition: TriggerCondition::EnergyBelow(0.25),
            state: SlotState::firing("common.abilities.innate.danari".to_string(), Time(100.0), 0),
        });
        assert_eq!(
            slots.firing_token(1),
            Some("common.abilities.innate.danari")
        );
        // No other slot is authorised by it.
        assert_eq!(slots.firing_token(0), None);
        assert_eq!(slots.firing_token(2), None);
    }

    /// A restored cooldown keeps running across a relog: the projection is
    /// rebuilt from the wall clock, and only a wait that genuinely elapsed
    /// releases the slot.
    #[test]
    fn reprojecting_a_restored_cooldown_preserves_the_remaining_wait() {
        let now_utc = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let now_game = Time(1_000.0);

        let cooling_for = |secs: i64| {
            let mut slots = TriggerSlots::default();
            slots.slots[0] = Some(TriggerSlot {
                ability: TriggerAbility::from_pool_index(0),
                condition: TriggerCondition::HealthBelow(0.3),
                state: SlotState::CoolingDown {
                    ready_at: Some(now_utc + chrono::TimeDelta::seconds(secs)),
                    // As restored from storage: nothing is ready until the
                    // projection is rebuilt.
                    ready_at_time: Time(f64::INFINITY),
                },
            });
            slots
        };

        // Twelve hours still to go.
        let mut still_cooling = cooling_for(12 * 3600);
        still_cooling.reproject_cooldowns(now_utc, now_game);
        assert_eq!(
            still_cooling.get(0).unwrap().state.ready_at_time(),
            Some(Time(1_000.0 + 12.0 * 3600.0)),
        );

        // Elapsed while logged out — which is exactly what a real-world
        // cooldown is for.
        let mut elapsed = cooling_for(-1);
        elapsed.reproject_cooldowns(now_utc, now_game);
        assert_eq!(elapsed.get(0).unwrap().state, SlotState::Ready);
    }

    /// The wall clock must never leave the server: `ready_at` is
    /// `#[serde(skip)]`, so a round-trip through the wire format drops it while
    /// keeping the in-game projection the client actually uses.
    #[test]
    fn the_synced_view_carries_the_projection_never_the_wall_clock() {
        let cooling = SlotState::CoolingDown {
            ready_at: Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
            ready_at_time: Time(4242.0),
        };
        let wire = serde_json::to_string(&cooling).expect("serialize");
        assert!(
            !wire.contains("ready_at\":\"") && !wire.contains("2023"),
            "wall clock leaked into the synced view: {wire}"
        );
        let seen_by_client: SlotState = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(seen_by_client.ready_at(), None);
        assert_eq!(seen_by_client.ready_at_time(), Some(Time(4242.0)));
    }

    /// The authorisation token is server-minted state and must not be
    /// net-synced even to its owner: a round-trip through the wire format
    /// drops it.
    #[test]
    fn the_synced_view_never_carries_the_authorisation_token() {
        let firing = SlotState::firing("common.abilities.innate.danari".into(), Time(5.0), 3);
        let wire = serde_json::to_string(&firing).expect("serialize");
        assert!(
            !wire.contains("danari"),
            "the authorisation token leaked into the synced view: {wire}"
        );
        let seen_by_client: SlotState = serde_json::from_str(&wire).expect("deserialize");
        // Present but empty — and no `ability_id` is ever the empty string, so
        // it can authorise nothing.
        assert_eq!(seen_by_client.firing_token(), Some(""));
        assert_eq!(seen_by_client.firing_circle(), Some(3));
    }

    /// A trigger slot may only ever hold an innate/spell pool entry. See
    /// [`TriggerAbility`]'s doc comment: a contextualized weapon ability would
    /// make the token's id and the cast's id disagree, silently dropping the
    /// bypass *and* writing the player's own manual cooldown.
    #[test]
    fn a_trigger_slot_refuses_every_ability_that_is_not_a_pool_entry() {
        assert_eq!(
            TriggerAbility::from_auxiliary(AuxiliaryAbility::Innate(7))
                .map(TriggerAbility::pool_index),
            Some(7),
        );
        for refused in [
            AuxiliaryAbility::MainWeapon(0),
            AuxiliaryAbility::OffWeapon(1),
            AuxiliaryAbility::Glider(2),
            AuxiliaryAbility::Empty,
        ] {
            assert_eq!(TriggerAbility::from_auxiliary(refused), None, "{refused:?}");
            assert!(
                TriggerSlot::from_auxiliary(refused, TriggerCondition::DamageTaken).is_none(),
                "{refused:?} was accepted by the setter"
            );
        }
    }

    /// An in-session pool rebuild that reorders entries must leave a live
    /// trigger on the SAME ability — the trigger-slot twin of the hotbar
    /// property `remap_innate_bindings` already guarantees.
    #[test]
    fn a_pool_rebuild_keeps_a_trigger_on_its_own_ability() {
        use crate::comp::ability::AbilityPool;

        let before = AbilityPool {
            abilities: vec!["mid".to_string(), "zeta".to_string()],
            spell_gates: vec![None, None],
        };
        // A newly learned key that sorts BEFORE the bound one: every later
        // index shifts by one.
        let after = AbilityPool {
            abilities: vec!["alpha".to_string(), "mid".to_string(), "zeta".to_string()],
            spell_gates: vec![None, None, None],
        };

        let mut slots = TriggerSlots::default();
        slots.slots[0] = Some(TriggerSlot::from_pool_index(
            1,
            TriggerCondition::HealthBelow(0.3),
        ));
        slots.remap_innate_bindings(&before, &after);
        assert_eq!(
            slots.configured_ability(0),
            Some(AuxiliaryAbility::Innate(2)),
            "the trigger re-pointed at a different ability after a pool rebuild"
        );
        // The condition and state are untouched.
        assert_eq!(
            slots.get(0).map(|s| s.condition),
            Some(TriggerCondition::HealthBelow(0.3))
        );

        // A key that is gone leaves nothing to point at.
        let mut orphaned = TriggerSlots::default();
        orphaned.slots[0] = Some(TriggerSlot::from_pool_index(
            1,
            TriggerCondition::DamageTaken,
        ));
        orphaned.remap_innate_bindings(&before, &AbilityPool {
            abilities: vec!["mid".to_string()],
            spell_gates: vec![None],
        });
        assert!(orphaned.get(0).is_none());
    }
}
