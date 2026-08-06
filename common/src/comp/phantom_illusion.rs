//! Marker for a "phantasm": a shared-illusion entity — a real, positioned,
//! server-authoritative entity that everyone nearby sees, with no `Health`,
//! no `Agent`, and a collision-less `Collider::Point`.
//!
//! A phantasm is `SummonInfo::Npc` with `alignment: Some(Alignment::Passive)`,
//! `with_agent: false`, `incorporeal: true` (→ `Collider::Point`, never an
//! absent `Collider` — see `NpcBuilder::incorporeal`'s own doc comment) and
//! `has_health: false`. This marker is what lets a later system (the dispel
//! hook in `common::combat::Attack::apply_attack`, and
//! `server/src/sys/phantasm.rs`) find "this entity is a phantasm" without
//! re-deriving that from the combination of those other flags.
//!
//! 🔴 **There is no per-observer state, ever.** The sync layer packages
//! components once per *region*, not per subscriber, so a per-observer "who
//! has seen through this" bit cannot be expressed. A phantasm is a real
//! entity everyone sees, and when it dispels, it dispels **for everyone**
//! watching — not just for whoever attacked it. This is an intentional
//! consequence of the shared-illusion model, not a bug.
//!
//! 🔴 **Attacking is the only reveal trigger.** `has_health: false` means
//! there is no `Health` for `apply_attack` to write, so no `HealthChangeEvent`
//! is ever produced to react to — the dispel hook is a dedicated early-out in
//! `apply_attack` instead (see `combat::TargetInfo::phantom_illusion` and
//! `event::DispelIllusionEvent`). Non-combat interaction
//! (`event::NpcInteractEvent`) is deliberately **not** a second trigger: that
//! path requires an `Agent`, which phantasms deliberately never carry
//! (`with_agent: false` exists specifically to remove that AI surface) —
//! adding one to make a phantasm "pokeable" would reintroduce it.

use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, NullStorage};

/// Marks an entity as a phantasm: a shared-illusion decoy with no `Health`,
/// no `Agent`, and `Collider::Point`. See the module doc for the full
/// invariant list.
///
/// Synced `SyncFrom::AnyEntity` (`common/net/src/synced_components.rs`) —
/// like `Disguise`, a phantasm's whole purpose is to be seen by everyone
/// nearby; there is no "who is watching" secret to protect (contrast
/// `RemoteSense`/`Detected`, which are owner-private for exactly that
/// reason).
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhantomIllusion;

impl Component for PhantomIllusion {
    // `NullStorage`: zero-sized marker, the same choice `Sticky`/`Immovable`
    // (`common/src/comp/phys.rs`) already made for this exact shape.
    // `DerefFlaggedStorage` wraps it for `Tracked`, required for net sync.
    type Storage = DerefFlaggedStorage<Self, NullStorage<Self>>;
}
