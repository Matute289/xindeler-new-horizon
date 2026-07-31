use serde::{Deserialize, Serialize};
use specs::{Component, VecStorage};
use vek::Vec3;

/// Key of a persisted banishment record (see `rtsim::data::Banishments`).
/// Stable across a server restart — the ECS entity a banishment rides on is
/// not, which is exactly why the record is keyed separately.
pub type BanishmentId = u64;

/// Frozen/limbo marker: this entity has been temporarily removed from the
/// world and comes back at `returns_at_unix_secs`.
///
/// The marker alone changes nothing. Parking is done by
/// `server::banishment::maintain`, which removes the components every
/// simulating system joins on — `Pos` (physics, client entity-sync and
/// therefore rendering, and every spatial-grid target query) and `Agent`
/// (AI) — and puts them back on return. That is how this ECS already
/// expresses "not simulated"; there is no generic disabled-entity flag to
/// reuse.
///
/// Deliberately **not** net-synced: clients are never told an entity is
/// banished, they simply stop receiving it, because an entity without `Pos`
/// belongs to no region (`common/src/region.rs:11` documents that `Pos`
/// removal is an anticipated state).
///
/// Reusable primitive: any future effect that needs "remove this creature
/// now, bring it back later, durably" inserts this component plus a matching
/// `BanishedCreature` record.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Banished {
    /// Key of this entity's persisted record.
    pub id: BanishmentId,
    /// Where the creature stood when it was banished. It returns exactly
    /// here.
    pub return_pos: Vec3<f32>,
    /// Deadline as **wall-clock seconds since the UNIX epoch**.
    ///
    /// Wall clock, not `Time` or `TimeOfDay`, on purpose: the design calls
    /// for a 1–7 day *real time* absence; `Time` is reset to `Time(0.0)` on
    /// every server start (`common/state/src/state.rs:398`) and is never
    /// restored from any save, and `TimeOfDay` advances at
    /// `day_cycle_coefficient ×` real time
    /// (`common/state/src/state.rs:881-883`). Only a wall-clock instant means
    /// the same thing before and after a restart.
    pub returns_at_unix_secs: u64,
}

impl Banished {
    /// Whether the creature is due to return, given the current wall clock.
    pub fn is_due(&self, now_unix_secs: u64) -> bool { now_unix_secs >= self.returns_at_unix_secs }
}

impl Component for Banished {
    type Storage = VecStorage<Self>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use vek::Vec3;

    fn banished(returns_at_unix_secs: u64) -> Banished {
        Banished {
            id: 7,
            return_pos: Vec3::new(100.0, 200.0, 40.0),
            returns_at_unix_secs,
        }
    }

    #[test]
    fn a_banishment_is_due_once_the_wall_clock_deadline_is_reached() {
        let b = banished(1_000);
        assert!(!b.is_due(999));
        assert!(b.is_due(1_000));
        assert!(b.is_due(1_001));
    }

    /// The record is serialised into the rtsim save file, so it must survive
    /// a serde round trip unchanged — including the position it returns to.
    #[test]
    fn a_banishment_round_trips_through_serde() {
        let b = banished(1_700_000_000);
        let encoded = ron::ser::to_string(&b).expect("serialise");
        let decoded: Banished = ron::de::from_str(&encoded).expect("deserialise");
        assert_eq!(decoded, b);
    }
}
