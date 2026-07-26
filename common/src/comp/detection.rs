//! Data model for the detection/sensing/reveal system: what an observer has
//! currently perceived through an active magical sense (see, e.g.,
//! `Detect Magic` or `True Sight`).
//!
//! This module defines the component shape only. The system that populates
//! `Detected` by spatially querying the world runs server-side and lives
//! elsewhere; nothing in this file computes a reveal set.

use crate::uid::Uid;
use serde::{Deserialize, Serialize};
use specs::{Component, DenseVecStorage, DerefFlaggedStorage};
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

/// The kind of Identify-style inspect card a `DetectedEntity` may open. Carries
/// no data — see the doc comment on `DetectedEntity::detail`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DetectDetail {
    Item,
    Creature,
}
