//! Data model for a live remote-sensing link: an entity's point of view is
//! currently anchored somewhere other than its own body (e.g. seeing through a
//! bonded animal, a planted sensor, or a piloted scouting mote).
//!
//! This module defines the component shape only. The server-side authority
//! system that resolves and re-validates the anchor every tick, and that
//! drives the shared spectate/camera path from it, lives in
//! `server/src/sys/remote_sense.rs`. Nothing in this file grants a viewpoint
//! by itself.

use crate::uid::Uid;
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage};

/// An active remote-sensing link: this entity's *point of view* is currently
/// anchored somewhere other than its own body.
///
/// 🔴 **Owner-private.** This component must be synced `SyncFrom::ClientEntity`
/// (see `common/net/src/synced_components.rs`) — it must NEVER be broadcast to
/// every nearby client (`SyncFrom::AnyEntity`), which would leak *who is
/// watching what* to every other player in range, and it must NEVER be synced
/// via `SyncFrom::ClientSpectatorEntity` (that scope carries a copy of the
/// *spectated* entity's own components to the spectator — the opposite
/// direction from this component's purpose).
///
/// Owned and rewritten wholesale (never patched) by the server's remote-sense
/// system. Never written client-side — the client's copy exists only so the
/// HUD/camera know a link is active and legal; the server re-validates every
/// tick and is the sole source of truth for whether it still holds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteSense {
    pub anchor: SenseAnchor,
    /// May the player look around freely from the anchor, or is the view
    /// locked to the anchor's own orientation?
    pub free_look: bool,
    /// May the player *drive* the anchor. Only ever true for
    /// `SenseAnchor::Piloted`.
    pub piloted: bool,
    /// The `Uid` of the buff-bearing caster this link belongs to, so a system
    /// iterating sensor entities can find the link that governs them without
    /// a second lookup.
    pub caster: Uid,
    /// Horizontal/vertical cruise speed of a `SenseAnchor::Piloted` anchor,
    /// in blocks per second -- forwarded here from the cast-time ability RON
    /// (`comp::buff::MiscBuffData::RemoteSense::flight_speed`) so
    /// `common/systems/src/pilot.rs` can read it back every tick off the
    /// caster's own (already-synced) component, without needing to touch
    /// `Buffs` at all. Meaningless for `SenseAnchor::Existing`/`Sensor`,
    /// which are never driven.
    pub flight_speed: f32,
}

impl RemoteSense {
    /// The `Uid` this viewpoint is currently anchored to, regardless of which
    /// kind of anchor it is.
    pub fn anchor_uid(&self) -> Uid { self.anchor.uid() }
}

impl Component for RemoteSense {
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

/// What a remote-sensing viewpoint is anchored to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SenseAnchor {
    /// An entity that already existed before the link was formed (e.g. a
    /// bonded animal). Not spawned by the link, not despawned when it ends.
    Existing(Uid),
    /// A sensor entity spawned specifically for this link. Despawned when the
    /// link ends.
    Sensor(Uid),
    /// A sensor entity spawned for this link, which the caster also drives.
    Piloted(Uid),
}

impl SenseAnchor {
    /// The `Uid` this anchor points at, regardless of variant.
    pub fn uid(&self) -> Uid {
        match self {
            SenseAnchor::Existing(uid) | SenseAnchor::Sensor(uid) | SenseAnchor::Piloted(uid) => {
                *uid
            },
        }
    }

    /// Whether this anchor is a sensor entity spawned for the link (as
    /// opposed to an entity that existed independently of it), and so should
    /// be despawned when the link ends.
    pub fn is_spawned_sensor(&self) -> bool {
        matches!(self, SenseAnchor::Sensor(_) | SenseAnchor::Piloted(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    #[test]
    fn anchor_uid_matches_every_variant() {
        assert_eq!(SenseAnchor::Existing(uid(1)).uid(), uid(1));
        assert_eq!(SenseAnchor::Sensor(uid(2)).uid(), uid(2));
        assert_eq!(SenseAnchor::Piloted(uid(3)).uid(), uid(3));
    }

    #[test]
    fn only_spawned_anchors_are_reaped() {
        assert!(!SenseAnchor::Existing(uid(1)).is_spawned_sensor());
        assert!(SenseAnchor::Sensor(uid(2)).is_spawned_sensor());
        assert!(SenseAnchor::Piloted(uid(3)).is_spawned_sensor());
    }

    #[test]
    fn remote_sense_anchor_uid_delegates() {
        let rs = RemoteSense {
            anchor: SenseAnchor::Sensor(uid(42)),
            free_look: true,
            piloted: false,
            caster: uid(1),
            flight_speed: 0.0,
        };
        assert_eq!(rs.anchor_uid(), uid(42));
    }
}
