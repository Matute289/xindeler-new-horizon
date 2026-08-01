use crate::{
    DamageSource, combat::DamageContributor, comp, comp::ability::MagicSource, resources::Time,
    uid::Uid,
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};
use std::{convert::TryFrom, ops::Mul};

/// Number of `MagicSource` (`comp::ability::MagicSource`) variants. A local
/// alias for `MagicSource::COUNT`, so every per-source fixed-size array in
/// this module reads the same name rather than a literal.
const MAGIC_SOURCE_COUNT: usize = MagicSource::COUNT;

/// Specifies what and how much changed current health
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HealthChange {
    /// The amount of the health change, negative is damage, positive is healing
    pub amount: f32,
    /// The individual or group who caused the health change (None if the
    /// damage wasn't caused by an entity)
    pub by: Option<DamageContributor>,
    /// The category of action that resulted in the health change
    pub cause: Option<DamageSource>,
    /// The magic source of the ability that caused this change, if any.
    /// `None` for weapon swings, falls, environment, and sourceless
    /// abilities. Deliberately **not** `#[serde(skip)]`: unlike
    /// `Health.damage_contributors`, this struct is `Health.last_change`, a
    /// public field on a net-synced component, and every other field on it
    /// is kept in sync so a client's predicted `last_change` matches the
    /// server's — skipping only this field would silently diverge that
    /// invariant.
    pub magic_source: Option<MagicSource>,
    /// The time that the health change occurred at
    pub time: Time,
    /// A boolean that tells you if the change was a precsie hit
    pub precise: bool,
    /// A random ID, used to group up health changes from the same attack
    pub instance: u64,
}

impl HealthChange {
    pub fn damage_by(&self) -> Option<DamageContributor> {
        self.cause.is_some().then_some(self.by).flatten()
    }
}

/// A single damage contributor's running total, and the per-magic-source
/// split of that total. `by_source` sums to at most `total` — weapon and
/// other untagged damage counts toward `total` but has no source, so it is
/// not represented in the array. Fixed-size and allocation-free so this can
/// live inline inside the `damage_contributors` map entry with no extra
/// hashing or heap traffic on the damage hot path.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ContributorDamage {
    total: u64,
    last: Time,
    by_source: [u64; MAGIC_SOURCE_COUNT],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Health is represented by u32s within the module, but treated as a float by
/// the rest of the game.
// As a general rule, all input and output values to public functions should be
// floats rather than integers.
pub struct Health {
    // Current and base_max are scaled by 256 within this module compared to what is visible to
    // outside this module. The scaling is done to allow health to function as a fixed point while
    // still having the advantages of being an integer. The scaling of 256 was chosen so that max
    // health could be u16::MAX - 1, and then the scaled health could fit inside an f32 with no
    // precision loss
    /// Current health is how much health the entity currently has. Current
    /// health *must* be lower than or equal to maximum health.
    current: u32,
    /// Base max is the amount of health the entity has without considering
    /// temporary modifiers such as buffs
    base_max: u32,
    /// Maximum is the amount of health the entity has after temporary modifiers
    /// are considered
    maximum: u32,
    /// Temp-HP / absorb pool (BL-05 RD-6). Scaled like `current`. Incoming
    /// damage is soaked here first; only the overflow reaches real HP. Not real
    /// HP: it is not refilled by healing and does not count toward `maximum`.
    /// Granted by the `Shielded` buff (set on buff-add); removed when depleted.
    absorb: u32,
    /// The last change to health
    pub last_change: HealthChange,
    pub is_dead: bool,
    /// If this entity supports having death protection.
    pub can_have_death_protection: bool,
    /// If death protection is true, any damage that would kill instead leaves
    /// the entity at 1 health.
    pub death_protection: bool,

    /// Keeps track of damage per DamageContributor (including its
    /// per-magic-source split) and the last time they caused damage, used
    /// for EXP sharing
    #[serde(skip)]
    damage_contributors: HashMap<DamageContributor, ContributorDamage>,
}

impl Health {
    /// Used when comparisons to health are needed outside this module.
    // This value is chosen as anything smaller than this is more precise than our
    // units of health.
    pub const HEALTH_EPSILON: f32 = 0.5 / Self::MAX_SCALED_HEALTH as f32;
    /// Maximum value allowed for health before scaling
    const MAX_HEALTH: u16 = u16::MAX - 1;
    /// The maximum value allowed for current and maximum health
    /// Maximum value is (u16:MAX - 1) * 256, which only requires 24 bits. This
    /// can fit into an f32 with no loss to precision
    // Cast to u32 done as u32::from cannot be called inside constant
    const MAX_SCALED_HEALTH: u32 = Self::MAX_HEALTH as u32 * Self::SCALING_FACTOR_INT;
    /// The amount health is scaled by within this module
    const SCALING_FACTOR_FLOAT: f32 = 256.;
    const SCALING_FACTOR_INT: u32 = Self::SCALING_FACTOR_FLOAT as u32;

    /// Returns the current value of health casted to a float
    pub fn current(&self) -> f32 { self.current as f32 / Self::SCALING_FACTOR_FLOAT }

    /// Returns the base maximum value of health casted to a float
    pub fn base_max(&self) -> f32 { self.base_max as f32 / Self::SCALING_FACTOR_FLOAT }

    /// Returns the maximum value of health casted to a float
    pub fn maximum(&self) -> f32 { self.maximum as f32 / Self::SCALING_FACTOR_FLOAT }

    /// Returns the current temp-HP / absorb pool (BL-05 RD-6).
    pub fn absorb(&self) -> f32 { self.absorb as f32 / Self::SCALING_FACTOR_FLOAT }

    /// Raises the absorb pool to at least `amount` (BL-05 RD-6). Take-higher so
    /// re-applying the same shield refreshes rather than stacks (same-kind
    /// no-stack); a future second shield *kind* would instead add. Clamped to
    /// the valid scaled range.
    pub fn raise_absorb_to(&mut self, amount: f32) {
        let scaled = (amount.max(0.0) * Self::SCALING_FACTOR_FLOAT)
            .min(Self::MAX_SCALED_HEALTH as f32) as u32;
        self.absorb = self.absorb.max(scaled);
    }

    /// Clears the absorb pool (BL-05 RD-6) — used when the granting buff is
    /// removed/dispelled.
    pub fn clear_absorb(&mut self) { self.absorb = 0; }

    /// Returns the fraction of health an entity has remaining
    pub fn fraction(&self) -> f32 { self.current() / self.maximum().max(1.0) }

    /// Instantly set the health fraction.
    pub fn set_fraction(&mut self, fraction: f32) {
        self.current =
            (self.maximum() * fraction.clamp(0.0, 1.0) * Self::SCALING_FACTOR_FLOAT).ceil() as u32;
    }

    pub fn set_amount(&mut self, amount: f32) {
        self.current = (amount * Self::SCALING_FACTOR_FLOAT)
            .clamp(0.0, self.maximum())
            .ceil() as u32;
    }

    /// Calculates a new maximum value and returns it if the value differs from
    /// the current maximum.
    ///
    /// Note: The returned value uses an internal format so don't expect it to
    /// be useful for anything other than a parameter to
    /// [`Self::update_internal_integer_maximum`].
    pub fn needs_maximum_update(&self, modifiers: comp::stats::StatsModifier) -> Option<u32> {
        let maximum = modifiers
            .compute_maximum(self.base_max())
            .mul(Self::SCALING_FACTOR_FLOAT)
            // NaN does not need to be handled here as rust will automatically change to 0 when casting to u32
            .clamp(0.0, Self::MAX_SCALED_HEALTH as f32) as u32;

        (maximum != self.maximum).then_some(maximum)
    }

    /// Updates the maximum value for health.
    ///
    /// Note: The accepted `u32` value is in the internal format of this type.
    /// So attempting to pass values that weren't returned from
    /// [`Self::needs_maximum_update`] can produce strange or unexpected
    /// results.
    pub fn update_internal_integer_maximum(&mut self, maximum: u32) {
        self.maximum = maximum;
        // Clamp the current health to enforce the current <= maximum invariant.
        self.current = self.current.min(self.maximum);
    }

    pub fn new(body: comp::Body) -> Self {
        let health = u32::from(body.base_health()) * Self::SCALING_FACTOR_INT;
        let death_protection = body.has_death_protection();
        Health {
            current: health,
            base_max: health,
            maximum: health,
            absorb: 0,
            last_change: HealthChange {
                amount: 0.0,
                by: None,
                cause: None,
                magic_source: None,
                precise: false,
                time: Time(0.0),
                instance: rand::random(),
            },
            is_dead: false,
            can_have_death_protection: death_protection,
            death_protection,
            damage_contributors: HashMap::new(),
        }
    }

    /// Returns a boolean if the delta was not zero.
    pub fn change_by(&mut self, mut change: HealthChange) -> bool {
        // BL-05 RD-6: a damaging change is soaked by the absorb (temp-HP) pool
        // first; only the overflow reaches real HP. Positive changes (healing)
        // bypass the pool entirely — temp-HP is separate from real HP.
        if change.amount < 0.0 && self.absorb > 0 {
            let damage = -change.amount;
            let absorb_real = self.absorb();
            let soaked = damage.min(absorb_real);
            // BL-05 RD-6: when the hit consumes the whole pool, zero it exactly.
            // Going through the f32 round-trip (`absorb/256 * 256 as u32`) can
            // truncate to a 1-unit residual (~0.004 HP), which leaves `absorb()`
            // just above 0 — so the "absorb depleted → remove Shielded" check
            // never fires and the shield lingers, silently re-absorbing damage
            // until its timer expires. Zeroing on full depletion keeps the
            // invariant "absorb > 0 ⟺ Shielded active" exact.
            self.absorb = if damage >= absorb_real {
                0
            } else {
                self.absorb
                    .saturating_sub((soaked * Self::SCALING_FACTOR_FLOAT) as u32)
            };
            change.amount += soaked;
        }
        let prev_health = i64::from(self.current);
        self.current = (((self.current() + change.amount).clamp(0.0, f32::from(Self::MAX_HEALTH))
            * Self::SCALING_FACTOR_FLOAT) as u32)
            .min(self.maximum);
        let delta = i64::from(self.current) - prev_health;

        self.last_change = change;

        // If damage is applied by an entity, update the damage contributors
        if delta < 0 {
            if let Some(attacker) = change.by {
                let amount = u64::try_from(-delta).unwrap_or(0);
                let entry = self
                    .damage_contributors
                    .entry(attacker)
                    .or_insert(ContributorDamage {
                        total: 0,
                        last: change.time,
                        by_source: [0; MAGIC_SOURCE_COUNT],
                    });
                entry.total += amount;
                entry.last = change.time;
                // Split the same amount by the ability's magic source, if it
                // has one. Untagged (weapon/environment) damage still counts
                // toward `total` above but has no source bucket to add to.
                if let Some(source) = change.magic_source {
                    entry.by_source[source as usize] += amount;
                }
            }

            // Prune any damage contributors who haven't contributed damage for over the
            // threshold - this enforces a maximum period that an entity will receive EXP
            // for a kill after they last damaged the killed entity. The per-source split
            // lives inside the same map entry, so it is pruned by this same retain with
            // no separate timer.
            const DAMAGE_CONTRIB_PRUNE_SECS: f64 = 600.0;
            self.damage_contributors
                .retain(|_, entry| (change.time.0 - entry.last.0) < DAMAGE_CONTRIB_PRUNE_SECS);
        }
        delta != 0
    }

    pub fn damage_contributions(&self) -> impl Iterator<Item = (&DamageContributor, &u64)> {
        self.damage_contributors
            .iter()
            .map(|(damage_contrib, entry)| (damage_contrib, &entry.total))
    }

    /// Sibling of [`Self::damage_contributions`], exposing each
    /// contributor's damage split by magic source instead of the flat
    /// total. Index with a `MagicSource` cast to `usize`; entries with no
    /// magic source (weapon/environment damage) are not represented and so
    /// do not appear in any bucket.
    pub fn damage_contributions_by_source(
        &self,
    ) -> impl Iterator<Item = (&DamageContributor, &[u64; MAGIC_SOURCE_COUNT])> {
        self.damage_contributors
            .iter()
            .map(|(damage_contrib, entry)| (damage_contrib, &entry.by_source))
    }

    pub fn recent_damagers(&self) -> impl Iterator<Item = (Uid, Time)> + '_ {
        self.damage_contributors
            .iter()
            .map(|(contrib, entry)| (contrib.uid(), entry.last))
    }

    pub fn should_die(&self) -> bool { self.current == 0 }

    pub fn kill(&mut self) {
        self.current = 0;
        self.death_protection = false;
    }

    pub fn revive(&mut self) {
        self.current = self.maximum;
        self.is_dead = false;
        self.death_protection = self.can_have_death_protection;
    }

    pub fn consume_death_protection(&mut self) {
        if self.death_protection {
            self.death_protection = false;
            if self.current() < 1.0 {
                self.set_amount(1.0);
            }
        }
    }

    pub fn refresh_death_protection(&mut self) {
        if self.can_have_death_protection {
            self.death_protection = true;
        }
    }

    pub fn has_consumed_death_protection(&self) -> bool {
        self.can_have_death_protection && !self.death_protection
    }

    #[cfg(test)]
    pub fn empty() -> Self {
        Health {
            current: 0,
            base_max: 0,
            maximum: 0,
            absorb: 0,
            last_change: HealthChange {
                amount: 0.0,
                by: None,
                cause: None,
                magic_source: None,
                precise: false,
                time: Time(0.0),
                instance: rand::random(),
            },
            is_dead: false,
            can_have_death_protection: false,
            death_protection: false,
            damage_contributors: HashMap::new(),
        }
    }
}

/// Returns true if an entity is downed, their character state is `Crawl` and
/// their death protection has been consumed.
pub fn is_downed(health: Option<&Health>, character_state: Option<&super::CharacterState>) -> bool {
    health.is_some_and(|health| !health.is_dead && health.has_consumed_death_protection())
        && matches!(character_state, Some(super::CharacterState::Crawl))
}

pub fn is_downed_or_dead(
    health: Option<&Health>,
    character_state: Option<&super::CharacterState>,
) -> bool {
    health.is_some_and(|health| health.is_dead) || is_downed(health, character_state)
}

impl Component for Health {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use crate::{
        combat::DamageContributor,
        comp::{Health, HealthChange, ability::MagicSource},
        resources::Time,
        uid::Uid,
    };
    use std::num::NonZeroU64;

    #[test]
    fn test_change_by_negative_health_change_adds_to_damage_contributors() {
        let mut health = Health::empty();
        health.current = 100 * Health::SCALING_FACTOR_INT;
        health.maximum = health.current;

        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let health_change = HealthChange {
            amount: -5.0,
            time: Time(123.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };

        health.change_by(health_change);

        let entry = health.damage_contributors.get(&damage_contrib).unwrap();

        assert_eq!(
            health_change.amount.abs() as u64 * Health::SCALING_FACTOR_INT as u64,
            entry.total
        );
        assert_eq!(health_change.time, entry.last);
    }

    #[test]
    fn test_change_by_positive_health_change_does_not_add_damage_contributor() {
        let mut health = Health::empty();
        health.maximum = 100 * Health::SCALING_FACTOR_INT;
        health.current = (health.maximum as f32 * 0.5) as u32;

        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let health_change = HealthChange {
            amount: 20.0,
            time: Time(123.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };

        health.change_by(health_change);

        assert!(health.damage_contributors.is_empty());
    }

    #[test]
    fn test_change_by_multiple_damage_from_same_damage_contributor() {
        let mut health = Health::empty();
        health.current = 100 * Health::SCALING_FACTOR_INT;
        health.maximum = health.current;

        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let health_change = HealthChange {
            amount: -5.0,
            time: Time(123.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(health_change);
        health.change_by(health_change);

        let entry = health.damage_contributors.get(&damage_contrib).unwrap();

        assert_eq!(
            (health_change.amount.abs() * 2.0) as u64 * Health::SCALING_FACTOR_INT as u64,
            entry.total
        );
        assert_eq!(1, health.damage_contributors.len());
    }

    #[test]
    fn test_change_by_damage_contributor_pruning() {
        let mut health = Health::empty();
        health.current = 100 * Health::SCALING_FACTOR_INT;
        health.maximum = health.current;

        let damage_contrib1 = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let health_change = HealthChange {
            amount: -5.0,
            time: Time(10.0),
            by: Some(damage_contrib1),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(health_change);

        let damage_contrib2 = DamageContributor::Solo(Uid(NonZeroU64::new(2).unwrap()));
        let health_change = HealthChange {
            amount: -5.0,
            time: Time(100.0),
            by: Some(damage_contrib2),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(health_change);

        assert!(health.damage_contributors.contains_key(&damage_contrib1));
        assert!(health.damage_contributors.contains_key(&damage_contrib2));

        // Apply damage 610 seconds after the damage from damage_contrib1 - this should
        // result in the damage from damage_contrib1 being pruned.
        let health_change = HealthChange {
            amount: -5.0,
            time: Time(620.0),
            by: Some(damage_contrib2),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(health_change);

        assert!(!health.damage_contributors.contains_key(&damage_contrib1));
        assert!(health.damage_contributors.contains_key(&damage_contrib2));
    }

    fn damage(amount: f32) -> HealthChange {
        HealthChange {
            amount,
            time: Time(0.0),
            by: None,
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        }
    }

    /// A `HealthChange` carrying `magic_source` survives a serde round-trip
    /// with the field intact, the same way every other field on it does —
    /// this is the property the field's own doc comment promises by
    /// deliberately not being `#[serde(skip)]`.
    #[test]
    fn health_change_magic_source_survives_serde_round_trip() {
        let with_source = HealthChange {
            amount: -12.0,
            by: None,
            cause: None,
            magic_source: Some(MagicSource::Divine),
            time: Time(1.0),
            precise: false,
            instance: 42,
        };
        let json = serde_json::to_string(&with_source).unwrap();
        let round_tripped: HealthChange = serde_json::from_str(&json).unwrap();
        assert_eq!(with_source, round_tripped);
        assert_eq!(round_tripped.magic_source, Some(MagicSource::Divine));

        let without_source = HealthChange {
            magic_source: None,
            ..with_source
        };
        let json = serde_json::to_string(&without_source).unwrap();
        let round_tripped: HealthChange = serde_json::from_str(&json).unwrap();
        assert_eq!(without_source.magic_source, round_tripped.magic_source);
    }

    /// Two damage instances from the same contributor, one attributed to a
    /// magic source and one untagged (a weapon hit, `magic_source: None`),
    /// accumulate into the same running `total` while only the tagged
    /// portion lands in that source's `by_source` bucket; every other
    /// bucket stays zero.
    #[test]
    fn per_source_split_accumulates_alongside_the_flat_total() {
        let mut health = Health::empty();
        health.current = 100 * Health::SCALING_FACTOR_INT;
        health.maximum = health.current;

        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let divine_hit = HealthChange {
            amount: -5.0,
            time: Time(1.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: Some(MagicSource::Divine),
            precise: false,
            instance: rand::random(),
        };
        let weapon_hit = HealthChange {
            amount: -3.0,
            time: Time(2.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(divine_hit);
        health.change_by(weapon_hit);

        let entry = health.damage_contributors.get(&damage_contrib).unwrap();
        let divine_scaled = 5 * Health::SCALING_FACTOR_INT as u64;
        let weapon_scaled = 3 * Health::SCALING_FACTOR_INT as u64;
        assert_eq!(entry.total, divine_scaled + weapon_scaled);
        assert_eq!(entry.by_source[MagicSource::Divine as usize], divine_scaled);
        for (i, &bucket) in entry.by_source.iter().enumerate() {
            if i != MagicSource::Divine as usize {
                assert_eq!(bucket, 0, "source index {i} should carry no damage");
            }
        }

        let (_, by_source) = health.damage_contributions_by_source().next().unwrap();
        assert_eq!(by_source[MagicSource::Divine as usize], divine_scaled);
    }

    /// The per-source split lives inside the same map entry as the flat
    /// total, so it is pruned by the exact same 600s retain — a contributor
    /// whose last damage is older than the window is dropped entirely, not
    /// merely zeroed.
    #[test]
    fn per_source_split_prunes_with_its_parent_entry() {
        let mut health = Health::empty();
        health.current = 100 * Health::SCALING_FACTOR_INT;
        health.maximum = health.current;

        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));
        let divine_hit = HealthChange {
            amount: -5.0,
            time: Time(10.0),
            by: Some(damage_contrib),
            cause: None,
            magic_source: Some(MagicSource::Divine),
            precise: false,
            instance: rand::random(),
        };
        health.change_by(divine_hit);
        assert!(health.damage_contributors.contains_key(&damage_contrib));

        // A later, unrelated contributor's hit 610s after damage_contrib's
        // last hit triggers the prune sweep and should drop it completely,
        // `by_source` included.
        let other_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(2).unwrap()));
        let later_hit = HealthChange {
            amount: -5.0,
            time: Time(620.0),
            by: Some(other_contrib),
            cause: None,
            magic_source: None,
            precise: false,
            instance: rand::random(),
        };
        health.change_by(later_hit);

        assert!(
            !health.damage_contributors.contains_key(&damage_contrib),
            "pruned contributor must be gone entirely, by_source included"
        );
    }

    // BL-05 RD-6: damage is soaked by the absorb pool first; only the overflow
    // reaches real HP, and healing never refills the pool.
    #[test]
    fn absorb_soaks_damage_before_health() {
        let mut health = Health::empty();
        health.maximum = 100 * Health::SCALING_FACTOR_INT;
        health.current = health.maximum;
        health.raise_absorb_to(30.0);
        assert_eq!(health.absorb(), 30.0);

        // 20 damage: fully soaked, HP untouched, 10 absorb left.
        health.change_by(damage(-20.0));
        assert_eq!(health.current(), 100.0);
        assert_eq!(health.absorb(), 10.0);

        // 25 damage: 10 soaked, 15 hits HP, absorb depleted.
        health.change_by(damage(-25.0));
        assert_eq!(health.absorb(), 0.0);
        assert_eq!(health.current(), 85.0);

        // Healing does not refill the absorb pool.
        health.change_by(damage(10.0));
        assert_eq!(health.absorb(), 0.0);
        assert_eq!(health.current(), 95.0);
    }

    // BL-05 RD-6 regression: a hit that consumes the whole pool must leave
    // `absorb()` *exactly* 0, even for values whose f32 round-trip would
    // otherwise truncate to a 1-unit residual (which made the shield linger and
    // keep absorbing until its timer expired). Tests several awkward magnitudes.
    #[test]
    fn full_depletion_zeroes_absorb_exactly() {
        for &amount in &[0.1_f32, 3.3, 7.7, 12.34, 49.9, 0.004, 99.99] {
            let mut health = Health::empty();
            health.maximum = 1000 * Health::SCALING_FACTOR_INT;
            health.current = health.maximum;
            health.raise_absorb_to(amount);
            // Exactly enough damage to drain the pool.
            health.change_by(damage(-amount));
            assert_eq!(
                health.absorb(),
                0.0,
                "absorb residual after exact depletion of {amount}"
            );
            // Overkill damage must also zero it, never leave a residual.
            health.raise_absorb_to(amount);
            health.change_by(damage(-(amount + 5.0)));
            assert_eq!(
                health.absorb(),
                0.0,
                "absorb residual after overkill of {amount}"
            );
        }
    }

    #[test]
    fn raise_absorb_is_take_higher_and_clearable() {
        let mut health = Health::empty();
        health.raise_absorb_to(20.0);
        health.raise_absorb_to(10.0); // lower → ignored (take-higher)
        assert_eq!(health.absorb(), 20.0);
        health.raise_absorb_to(50.0); // higher → refreshes
        assert_eq!(health.absorb(), 50.0);
        health.clear_absorb();
        assert_eq!(health.absorb(), 0.0);
    }

    /// Not a strict perf assertion (wall-clock varies by machine and load) —
    /// this is a manually-run smoke measurement for a hot-path change:
    /// `change_by`'s damage-contributor split is a fixed-size array write
    /// inside an existing map entry, so 100k calls should stay in the
    /// low-single-digit milliseconds with no allocation growth. Run with
    /// `cargo test -p xindeler-common --lib -- --ignored
    /// change_by_100k_calls_stay_cheap --nocapture` to see the printed
    /// timing.
    #[test]
    #[ignore = "manual perf smoke, not a CI assertion"]
    fn change_by_100k_calls_stay_cheap() {
        use std::time::Instant;

        let mut health = Health::empty();
        health.maximum = u32::MAX / 2;
        health.current = health.maximum;
        let damage_contrib = DamageContributor::Solo(Uid(NonZeroU64::new(1).unwrap()));

        let start = Instant::now();
        for i in 0..100_000u32 {
            let change = HealthChange {
                amount: -1.0,
                by: Some(damage_contrib),
                cause: None,
                magic_source: Some(if i % 2 == 0 {
                    MagicSource::Divine
                } else {
                    MagicSource::Primordial
                }),
                time: Time(f64::from(i)),
                precise: false,
                instance: u64::from(i),
            };
            health.change_by(change);
        }
        let elapsed = start.elapsed();
        println!("100_000 change_by calls (with per-source split): {elapsed:?}");
    }
}
