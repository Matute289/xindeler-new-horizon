//! Attunement (ENG-D2 / I1): some magic items grant their effects only while
//! the wearer is *attuned* to them. A character may attune a limited number of
//! items at once (the cap grows with level); attuning takes time scaled by the
//! item's rarity, and un-attuning is instant.
//!
//! This module is the **data model + the (tunable) rules**. The
//! attune/un-attune actions and the effect-gating (an attuned-required item is
//! inert until attuned) are wired in follow-ups (ENG-D2b / D2c).

use super::inventory::{item::Quality, slot::EquipSlot};
use crate::resources::Time;
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage};

/// The equipment slots a character currently has attuned. Session-only for now
/// (persistence is a follow-up). Its length is bounded by `max_attuned_items`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AttunedItems(pub Vec<EquipSlot>);

impl AttunedItems {
    /// Whether the item in `slot` is currently attuned.
    pub fn is_attuned(&self, slot: EquipSlot) -> bool { self.0.contains(&slot) }

    /// How many items are currently attuned.
    pub fn count(&self) -> usize { self.0.len() }

    /// Attune the item in `slot` if it is not already attuned and the wearer is
    /// under the `max` cap. Returns whether the slot is attuned afterwards.
    pub fn attune(&mut self, slot: EquipSlot, max: u32) -> bool {
        if self.is_attuned(slot) {
            return true;
        }
        if self.0.len() as u32 >= max {
            return false;
        }
        self.0.push(slot);
        true
    }

    /// Un-attune the item in `slot` (instant). Returns whether it had been
    /// attuned.
    pub fn unattune(&mut self, slot: EquipSlot) -> bool {
        let before = self.0.len();
        self.0.retain(|s| *s != slot);
        self.0.len() != before
    }
}

impl Component for AttunedItems {
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

/// How many items a character of `level` may keep attuned at once.
///
/// Matias (2026-06-20): **no attunement before L10**; the first item unlocks at
/// **L10**, then **+1 item every 2 levels** (L12 → 2, L14 → 3, …). The level
/// cap is 60 (→ 26 by the formula), but a character has only ~18 non-bag equip
/// slots (see `EquipSlot`), so the cap stops binding around L44 — past that,
/// late-game item *min-level* requirements (`ItemRequirements`), not attunement
/// scarcity, are what pull players toward 60. Tunable.
pub fn max_attuned_items(level: u16) -> u32 {
    if level < 10 {
        0
    } else {
        1 + u32::from((level - 10) / 2)
    }
}

/// Seconds to attune an item of the given rarity (`Quality`). Un-attuning is
/// instant, so it has no equivalent.
///
/// Matias §I1 (tunable — "los tiempos los podemos ir charlando"): común ~3 ·
/// raro ~6 · muy raro ~12 · legendario ~21 · mítico ~35. The engine `Quality`
/// ladder maps onto those tiers; the in-between `Moderate` ("uncommon") is
/// interpolated, and `Low`/`Debug` cost nothing (never attunement items).
pub fn attune_time(quality: Quality) -> f32 {
    match quality {
        Quality::Low | Quality::Debug => 0.0,
        Quality::Common => 3.0,     // común
        Quality::Moderate => 4.5,   // "uncommon" — interpolated, not specified
        Quality::High => 6.0,       // raro
        Quality::Epic => 12.0,      // muy raro
        Quality::Legendary => 21.0, // legendario
        Quality::Artifact => 35.0,  // mítico
    }
}

/// In-progress attunements: each is an equip slot and the `Time` its attune
/// channel finishes. Populated when a `RequiresAttunement` item is equipped and
/// drained by `reconcile_attunement` as channels complete. (ENG-D2b.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attuning(pub Vec<(EquipSlot, Time)>);

impl Component for Attuning {
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

/// Whether an equipped item's effects currently apply (ENG-D2c effect-gating):
/// an item that `RequiresAttunement` contributes only while its `slot` is
/// attuned; every other item always contributes. `requires` is the item's
/// `RequiresAttunement` flag — pass `item.requires_attunement()`.
///
/// **v1 scope (Matias: "defensa + habilidades").** This gate is applied to:
/// HP-damage protection (`DerivedStats::protection`), max-energy from gear
/// (`DerivedStats::max_energy_mod`), weapon abilities
/// (`ActiveAbilities::activate_ability`), and caster stat buffs granted by an
/// item's `ItemCondition` (the per-tick evaluation in
/// `common/systems/src/buff.rs`: an unattuned `RequiresAttunement` item's
/// condition grants neither its `when_met` nor its `when_unmet` buffs). It is
/// intentionally **not** applied yet to: offensive stats
/// (`DerivedStats::precision_mult` / `::stealth` / `::energy_reward_mod`),
/// poise resilience (`DerivedStats::poise_resilience`, to avoid the
/// `apply_poise_reduction` ripple), and fall-damage. Those are documented
/// follow-ups; until then an unattuned item still grants *those* effects.
///
/// The gated aggregates are folded once per gear change into `DerivedStats`,
/// which caches an attunement-aware and an attunement-blind variant of each
/// one; the read sites pick between them rather than re-applying this gate.
pub fn item_effects_active(
    slot: EquipSlot,
    requires: bool,
    attuned: Option<&AttunedItems>,
) -> bool {
    !requires || attuned.is_some_and(|a| a.is_attuned(slot))
}

/// The auto-attune-on-equip brain (Matias, 2026-06-20). Reconciles `attuning`
/// (in-progress) and `attuned` (done) against the currently-equipped items:
///
/// - `equipped` lists every equipped item as `(slot, requires_attunement,
///   quality)`.
/// - An equipped attunement item with no attune in flight or done starts a
///   channel of `attune_time(quality)` seconds.
/// - A channel whose finish time has passed completes: the slot is attuned if
///   the wearer is under the level cap (`max_attuned_items`). If the cap is
///   full the (finished) channel stays **pending** and retries each reconcile,
///   so the item is inert but auto-attunes the moment a slot frees — no
///   re-equip, and no wasteful re-channeling.
/// - A slot that no longer holds an attunement item is cleared from both — this
///   is the **instant** un-attune on unequip.
pub fn reconcile_attunement(
    equipped: &[(EquipSlot, bool, Quality)],
    level: u16,
    now: Time,
    attuning: &mut Attuning,
    attuned: &mut AttunedItems,
) {
    // A slot is "live" only while it still holds an attunement-requiring item.
    let is_live = |slot: EquipSlot| {
        equipped
            .iter()
            .any(|(s, requires, _)| *s == slot && *requires)
    };

    // 1. Instant un-attune: drop any attuned/attuning slot whose item is gone or no
    //    longer requires attunement (covers unequip, swap-out, and drop).
    attuned.0.retain(|slot| is_live(*slot));
    attuning.0.retain(|(slot, _)| is_live(*slot));

    // 2. Start a channel for each equipped attunement item that has neither a
    //    channel in flight nor a completed attune.
    for (slot, requires, quality) in equipped {
        if *requires && !attuned.is_attuned(*slot) && !attuning.0.iter().any(|(s, _)| s == slot) {
            attuning
                .0
                .push((*slot, Time(now.0 + f64::from(attune_time(*quality)))));
        }
    }

    // 3. Complete channels whose finish time has passed: attune if under the level
    //    cap. `attune` returns whether the slot is attuned afterwards, so a channel
    //    is kept only while it is still running OR finished-but-over-cap (retry
    //    next reconcile — it attunes as soon as a slot frees, with no
    //    re-channeling).
    let max = max_attuned_items(level);
    attuning
        .0
        .retain(|(slot, finish)| now.0 < finish.0 || !attuned.attune(*slot, max));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::inventory::slot::ArmorSlot;

    #[test]
    fn cap_grows_with_level() {
        assert_eq!(max_attuned_items(1), 0);
        assert_eq!(max_attuned_items(9), 0); // nothing before L10
        assert_eq!(max_attuned_items(10), 1); // first attunement unlocks at L10
        assert_eq!(max_attuned_items(11), 1);
        assert_eq!(max_attuned_items(12), 2); // +1 every 2 levels
        assert_eq!(max_attuned_items(14), 3);
        assert_eq!(max_attuned_items(52), 22); // 22 items reached at L52
        assert_eq!(max_attuned_items(60), 26); // at the level cap
    }

    #[test]
    fn attune_time_scales_with_rarity() {
        assert!(attune_time(Quality::Common) < attune_time(Quality::High));
        assert!(attune_time(Quality::High) < attune_time(Quality::Epic));
        assert!(attune_time(Quality::Epic) < attune_time(Quality::Legendary));
        assert!(attune_time(Quality::Legendary) < attune_time(Quality::Artifact));
        assert_eq!(attune_time(Quality::Common), 3.0);
        assert_eq!(attune_time(Quality::Artifact), 35.0);
        assert_eq!(attune_time(Quality::Low), 0.0);
    }

    #[test]
    fn attune_respects_cap() {
        let mut a = AttunedItems::default();
        let r1 = EquipSlot::Armor(ArmorSlot::Ring1);
        let r2 = EquipSlot::Armor(ArmorSlot::Ring2);
        let neck = EquipSlot::Armor(ArmorSlot::Neck);
        assert!(a.attune(r1, 2));
        assert!(a.attune(r2, 2));
        assert_eq!(a.count(), 2);
        assert!(!a.attune(neck, 2)); // over the cap
        assert!(!a.is_attuned(neck));
        assert!(a.is_attuned(r1));
    }

    #[test]
    fn attune_is_idempotent_and_unattune_is_instant() {
        let mut a = AttunedItems::default();
        let r1 = EquipSlot::Armor(ArmorSlot::Ring1);
        assert!(a.attune(r1, 1));
        assert!(a.attune(r1, 1)); // already attuned → still true, not double-counted
        assert_eq!(a.count(), 1);
        assert!(a.unattune(r1));
        assert!(!a.is_attuned(r1));
        assert!(!a.unattune(r1)); // not attuned → false
    }

    // ---- reconcile_attunement (the auto-attune-on-equip brain) ----

    const R1: EquipSlot = EquipSlot::Armor(ArmorSlot::Ring1);
    const R2: EquipSlot = EquipSlot::Armor(ArmorSlot::Ring2);

    #[test]
    fn equipping_a_requires_item_starts_a_channel() {
        let mut attuning = Attuning::default();
        let mut attuned = AttunedItems::default();
        // common = 3s; equipped at t=0 → finishes at t=3.
        reconcile_attunement(
            &[(R1, true, Quality::Common)],
            10,
            Time(0.0),
            &mut attuning,
            &mut attuned,
        );
        assert_eq!(attuning.0.len(), 1);
        assert_eq!(attuning.0[0].0, R1);
        assert!((attuning.0[0].1.0 - 3.0).abs() < 1e-6);
        assert_eq!(attuned.count(), 0); // not done yet
    }

    #[test]
    fn non_attunement_item_is_ignored() {
        let mut attuning = Attuning::default();
        let mut attuned = AttunedItems::default();
        reconcile_attunement(
            &[(R1, false, Quality::Common)],
            10,
            Time(0.0),
            &mut attuning,
            &mut attuned,
        );
        assert_eq!(attuning.0.len(), 0);
        assert_eq!(attuned.count(), 0);
    }

    #[test]
    fn channel_completes_under_cap() {
        let mut attuning = Attuning(vec![(R1, Time(3.0))]);
        let mut attuned = AttunedItems::default();
        // now >= finish; level 10 → cap 1.
        reconcile_attunement(
            &[(R1, true, Quality::Common)],
            10,
            Time(3.0),
            &mut attuning,
            &mut attuned,
        );
        assert!(attuned.is_attuned(R1));
        assert_eq!(attuning.0.len(), 0);
    }

    #[test]
    fn channel_over_cap_stays_pending_then_attunes_when_a_slot_frees() {
        // r1 already attuned; level 10 → cap 1; r2's channel completes but cap is full.
        let mut attuned = AttunedItems(vec![R1]);
        let mut attuning = Attuning(vec![(R2, Time(3.0))]);
        reconcile_attunement(
            &[(R1, true, Quality::Common), (R2, true, Quality::Common)],
            10,
            Time(5.0),
            &mut attuning,
            &mut attuned,
        );
        assert!(attuned.is_attuned(R1));
        assert!(!attuned.is_attuned(R2)); // inert — cap full
        assert_eq!(attuning.0.len(), 1); // channel stays pending (finished), retries — no re-channel

        // Free R1's slot (unequip it): only R2 remains equipped → R2 attunes now.
        reconcile_attunement(
            &[(R2, true, Quality::Common)],
            10,
            Time(6.0),
            &mut attuning,
            &mut attuned,
        );
        assert!(!attuned.is_attuned(R1)); // cleared (unequipped)
        assert!(attuned.is_attuned(R2)); // auto-attuned the moment the slot freed
        assert_eq!(attuning.0.len(), 0);
    }

    #[test]
    fn unequip_clears_attuned_and_attuning_instantly() {
        let mut attuned = AttunedItems(vec![R1]);
        let mut attuning = Attuning(vec![(R2, Time(10.0))]);
        // neither slot holds an attunement item anymore.
        reconcile_attunement(&[], 12, Time(1.0), &mut attuning, &mut attuned);
        assert_eq!(attuned.count(), 0);
        assert_eq!(attuning.0.len(), 0);
    }

    #[test]
    fn already_attuned_slot_does_not_restart_a_channel() {
        let mut attuned = AttunedItems(vec![R1]);
        let mut attuning = Attuning::default();
        reconcile_attunement(
            &[(R1, true, Quality::Common)],
            10,
            Time(100.0),
            &mut attuning,
            &mut attuned,
        );
        assert!(attuned.is_attuned(R1));
        assert_eq!(attuning.0.len(), 0); // no new channel for an already-attuned slot
    }

    #[test]
    fn channel_in_progress_is_left_running() {
        let mut attuning = Attuning(vec![(R1, Time(3.0))]);
        let mut attuned = AttunedItems::default();
        // t=1 < finish 3 → still running, not yet attuned, not restarted.
        reconcile_attunement(
            &[(R1, true, Quality::Common)],
            10,
            Time(1.0),
            &mut attuning,
            &mut attuned,
        );
        assert_eq!(attuning.0.len(), 1);
        assert!((attuning.0[0].1.0 - 3.0).abs() < 1e-6);
        assert_eq!(attuned.count(), 0);
    }

    #[test]
    fn item_effects_active_gates_only_unattuned_requires() {
        let attuned = AttunedItems(vec![R1]);
        // Non-attunement items are always active.
        assert!(item_effects_active(R2, false, Some(&attuned)));
        assert!(item_effects_active(R2, false, None));
        // Attunement items are active only while their slot is attuned.
        assert!(item_effects_active(R1, true, Some(&attuned)));
        assert!(!item_effects_active(R2, true, Some(&attuned)));
        assert!(!item_effects_active(R1, true, None));
    }
}
