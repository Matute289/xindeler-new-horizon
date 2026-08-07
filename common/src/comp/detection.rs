//! Data model for the detection/sensing/reveal system: what an observer has
//! currently perceived through an active magical sense (see, e.g.,
//! `Detect Magic` or `True Sight`).
//!
//! This module defines the component shape only. The system that populates
//! `Detected` by spatially querying the world runs server-side and lives
//! elsewhere; nothing in this file computes a reveal set.

use crate::{comp::stats::Stats, uid::Uid};
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage, NullStorage};
use vek::Vec3;

/// The set of things this entity currently perceives through a magical sense.
///
/// 🔴 **Owner-private.** This component must be synced `SyncFrom::ClientEntity`
/// (see `common/net/src/synced_components.rs`) — it must NEVER be broadcast to
/// every nearby client (`SyncFrom::AnyEntity`). A concealment-piercing reveal
/// broadcast to every client in range would tell everyone where the concealed
/// thing is, which is exactly the leak this component's sync scope exists to
/// avoid.
///
/// Owned and rewritten wholesale (never patched) by the server's detection
/// system. Never written client-side.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detected {
    /// Revealed entities, each tagged with the sense that revealed them.
    pub entities: Vec<DetectedEntity>,
    /// Revealed world points that are not entities (e.g. a revealed waypoint
    /// or location). Kept separate from `entities` because the block/sprite
    /// highlight surface is a single global uniform and structurally cannot
    /// express a set of points.
    pub points: Vec<DetectedPoint>,
}

impl Component for Detected {
    type Storage = DerefFlaggedStorage<Self, DenseVecStorage<Self>>;
}

/// A single revealed entity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DetectedEntity {
    pub uid: Uid,
    pub sense: SenseKind,
    /// Present only for single-target Identify-style spells. Selects which
    /// tooltip the client is permitted to open.
    ///
    /// 🟡 This is a UI-permission flag, not a secret: every field an Identify
    /// tooltip would show is already synced to every client for every entity
    /// (`Stats`, `Buffs`, `Health`, `Energy`, `Alignment`, `Body`, … are all
    /// `SyncFrom::AnyEntity`) because they drive the overhead nametag. The
    /// spell gates whether the *inspect card UI* opens, not whether the
    /// underlying data exists on the client. Do not build anti-cheat theatre
    /// around this field.
    pub detail: Option<DetectDetail>,
}

/// A single revealed world point (not an entity) — e.g. a revealed waypoint.
/// `Vec3<f32>`, deliberately not a 2-D map-pin shape: this renders through an
/// in-world label surface, not the minimap.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DetectedPoint {
    pub pos: Vec3<f32>,
    pub sense: SenseKind,
}

/// What kind of sense revealed a thing. Drives the server-side predicate, the
/// client-side glow colour, and the i18n string. One variant per
/// *presentation* family — not one per spell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SenseKind {
    /// Detects active magic effects or magic items.
    Magic,
    /// Detects fiends and undead.
    Aberrant,
    /// Detects poison and disease afflictions.
    Affliction,
    /// Detects sapient thought (humanoid minds).
    Thought,
    /// Detects portals.
    Portal,
    /// Detects a described/named creature.
    Creature,
    /// Detects animals.
    Fauna,
    /// Detects plants.
    Flora,
    /// Detects a specific object.
    Object,
    /// Wide-radius fauna/flora/water summary sense.
    Nature,
    /// A revealed path/waypoint (points only, see `DetectedPoint`).
    Path,
    /// The illusion-piercing reveal half of an always-active true-sight
    /// sense: reveals concealed entities such as disguised creatures.
    True,
}

/// The kind of Identify-style inspect card a `DetectedEntity` may open.
///
/// 🟡 **UI affordance, not a secret.** Every field either inspect card can
/// ever show is *already* fully synced to every client for every entity
/// (`Stats`, `SkillSet`, `Buffs`, `Health`/`Energy`/`Poise`, `Alignment`,
/// `Body`, `CharacterClass`, … are all `SyncFrom::AnyEntity`, the same data
/// that already drives the overhead nametag). Casting Identify does not grant
/// the caster's client any information it did not already have — it grants
/// permission to open a *UI panel* presenting a subset of that
/// already-present information in one place. `DetectDetail` (and the
/// server-side `IdentifyLinks` tier that gates which subset, see
/// `server/src/sys/detection.rs`) exist purely to answer "should the inspect
/// card render for this observer right now", never "what can this observer
/// possibly know" — the latter question has nothing left to gate. A future
/// change that turns this into a real server-side secret (e.g. withholding
/// `Stats`/`Buffs` sync from non-casters) would be undoing that deliberate
/// choice, not "fixing" a leak — do not build anti-cheat theatre around this
/// field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DetectDetail {
    /// Opens `ItemTooltip` (reused verbatim) for the target's `PickupItem`.
    Item,
    /// Opens the creature inspect card. `tier` selects which subset of the
    /// card's rows the observer is currently permitted to see (see
    /// `server/src/sys/detection.rs`'s `IdentifyLinks` doc comment for the
    /// tier→field mapping this number indexes into) — set server-side at
    /// creature-card-payload-construction time and carried here rather than
    /// through a second sync channel, per this enum's own doc comment above.
    Creature { tier: u8 },
}

/// Marks an entity as invisible to normal perception and revealed only to an
/// observer whose own `Stats.senses` currently includes `SenseKind::True`
/// (see `observer_pierces_concealment`). Used for objects that must exist as
/// real, positioned, server-authoritative entities (so the world data behind
/// them streams correctly) while never being visible or targetable by a
/// player without that sense.
///
/// A property of the *entity itself*, not a secret about who is observing it,
/// so it is synced `SyncFrom::AnyEntity` like `Body`/`Pos` — it reveals no
/// more than those already do. What differs per observer is purely local,
/// client-side render and target-acquisition filtering: this marker just
/// tells that filtering to apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcealedUnlessTrueSight;

impl Component for ConcealedUnlessTrueSight {
    type Storage = DerefFlaggedStorage<Self, NullStorage<Self>>;
}

/// Whether an observer with the given `Stats` currently pierces concealment —
/// i.e. can perceive and target an entity marked `ConcealedUnlessTrueSight`.
/// Reuses the same "does this entity have an active `SenseKind::True` sense"
/// signal the true-sight buff itself already populates
/// (`BuffEffect::Sense`/`Stats.senses`) rather than inventing a second
/// visibility mechanism.
pub fn observer_pierces_concealment(stats: &Stats) -> bool {
    stats
        .senses
        .iter()
        .any(|sense| sense.kind == SenseKind::True)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::{Body, buff::SenseMode, humanoid, stats::ActiveSense};

    fn stats_with_senses(senses: Vec<ActiveSense>) -> Stats {
        let body = Body::Humanoid(humanoid::Body::random());
        let mut stats = Stats::empty(body);
        stats.senses = senses;
        stats
    }

    #[test]
    fn true_sight_sense_pierces_concealment() {
        let stats = stats_with_senses(vec![ActiveSense {
            kind: SenseKind::True,
            radius: 30.0,
            mode: SenseMode::Continuous,
        }]);
        assert!(observer_pierces_concealment(&stats));
    }

    #[test]
    fn unrelated_sense_does_not_pierce_concealment() {
        let stats = stats_with_senses(vec![ActiveSense {
            kind: SenseKind::Magic,
            radius: 30.0,
            mode: SenseMode::Snapshot,
        }]);
        assert!(!observer_pierces_concealment(&stats));
    }

    #[test]
    fn no_active_senses_does_not_pierce_concealment() {
        let stats = stats_with_senses(vec![]);
        assert!(!observer_pierces_concealment(&stats));
    }
}
