//! Data model for a cast disguise: a cosmetic-only lie about which `Body` an
//! entity appears to have.
//!
//! 🔴 **This is deliberately not a polymorph.** `BuffKind::Polymorphed`
//! (`common/src/comp/buff.rs`) rewrites the wearer's real `Body`, `Mass`,
//! `Density` and `Collider` via `ChangeBodyEvent` — that changes hitbox,
//! mass, gait, eye height (LOS) and movement, because `Body` genuinely drives
//! all of those for the real entity. A disguise must change none of it: the
//! wearer keeps its own hitbox, mass, reach, eye height and combat stats, and
//! only the *model/skeleton observers render* and the *name they read* lie.
//!
//! This module defines the component shape only. It is inserted/updated/
//! removed by the shared buff system (`common/systems/src/buff.rs`) in
//! lock-step with whether a `BuffKind::Disguised` buff is currently
//! contributing a `BuffEffect::Disguise` — the same "no cleanup code needed,
//! the tick after the buff dies nothing declares a disguise" lifecycle
//! `Stats`' own per-tick fields already use. It is **not** a `Stats` field
//! itself: `Stats` is rebuilt from scratch every tick
//! (`Stats::reset_temp_modifiers`), so a `Body`-sized payload living there
//! would be thrown away and rebuilt every tick for no reason whenever nobody
//! is disguised — a real cost paid by every entity, not just disguised ones.

use crate::{comp::body::Body, uid::Uid};
use common_i18n::Content;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

/// An active disguise: what observers see instead of the wearer's real
/// `Body`/name.
///
/// Synced `SyncFrom::AnyEntity` (`common/net/src/synced_components.rs`) —
/// a disguise is meant to be visible (as a disguise, i.e. its *lie*) to
/// everyone nearby: that is the entire point of casting one. Nothing about
/// who is disguised as what is a secret from other clients; the only thing
/// gated server-side is whether an *observing NPC* mechanically sees through
/// it (a later mechanism, not built here).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disguise {
    /// The body observers *believe* they are looking at. Read by voxygen's
    /// figure-rendering model/skeleton dispatch instead of the real `Body`.
    /// Never written to the entity's actual `Body` component.
    pub apparent_body: Body,
    /// The name observers see over its head. `None` keeps the real
    /// `Stats.name` (e.g. a disguise that changes appearance but not the
    /// name shown, or before spell content authors a fake name).
    pub apparent_name: Option<Content>,
    /// Who cast the disguise. Not this entity's own `Uid` unless
    /// self-disguised. Feeds a later-added suspicion roll, which needs the
    /// caster's `magic_accuracy` — see `cast_accuracy` below for why that
    /// value is snapshotted rather than looked up from this `Uid` every
    /// time.
    pub caster: Uid,
    /// Snapshot of the caster's `Stats.magic_accuracy` at cast time. Baked in
    /// once so a later-added suspicion roll needs no second entity lookup —
    /// an observer resolving the roll off a disguise that has been active
    /// for a while must not depend on the caster still being loaded, still
    /// alive, or still having the same accuracy they had when they cast it.
    pub cast_accuracy: f32,
}

impl Disguise {
    /// The `Body` a model/skeleton/render lookup should resolve to: the
    /// apparent body while disguised, the real body otherwise.
    ///
    /// This is the one seam every consumer of "which body does this entity
    /// render as" must go through — voxygen's model-selection dispatch, its
    /// render dispatch, and its per-entity skeleton-state cache lookup all
    /// call this same function rather than re-deriving the choice locally,
    /// so a future edit to any one of those three sites cannot silently
    /// diverge from the other two: keying the skeleton-state cache by the
    /// wrong body panics or renders the wrong skeleton type.
    ///
    /// 🔴 Never call this for physics, line-of-sight, or any other consumer
    /// of the entity's *real* body — those must keep reading the real `Body`
    /// component directly. This function exists solely for the cosmetic
    /// render path.
    pub fn render_body(disguise: Option<&Disguise>, real_body: Body) -> Body {
        disguise.map_or(real_body, |disguise| disguise.apparent_body)
    }
}

impl Component for Disguise {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::body::{humanoid, quadruped_medium};
    use std::num::NonZeroU64;

    fn uid(n: u64) -> Uid { Uid(NonZeroU64::new(n).unwrap()) }

    fn disguise(apparent_body: Body) -> Disguise {
        Disguise {
            apparent_body,
            apparent_name: None,
            caster: uid(1),
            cast_accuracy: 0.5,
        }
    }

    #[test]
    fn no_disguise_resolves_to_the_real_body() {
        let real = Body::Humanoid(humanoid::Body::random());
        assert_eq!(Disguise::render_body(None, real), real);
    }

    #[test]
    fn a_same_category_disguise_overrides_the_real_body() {
        let real = Body::Humanoid(humanoid::Body::random());
        let apparent = Body::Humanoid(humanoid::Body::random());
        let d = disguise(apparent);
        assert_eq!(Disguise::render_body(Some(&d), real), apparent);
        assert_ne!(
            Disguise::render_body(Some(&d), real),
            real,
            "regression guard: a same-category disguise must still override, not fall back to the \
             real body just because both are humanoid"
        );
    }

    #[test]
    fn a_cross_category_disguise_overrides_the_real_body() {
        // The exact risk this component is designed to guard against:
        // humanoid disguised as a quadruped (or vice versa) must resolve to
        // the *apparent* body's category, not silently keep the real one's.
        let real = Body::Humanoid(humanoid::Body::random());
        let apparent = Body::QuadrupedMedium(quadruped_medium::Body::random());
        let d = disguise(apparent);
        let resolved = Disguise::render_body(Some(&d), real);
        assert_eq!(resolved, apparent);
        assert!(
            matches!(resolved, Body::QuadrupedMedium(_)),
            "a humanoid disguised as a quadruped must resolve to a quadruped body for \
             model/skeleton selection, never the real humanoid body"
        );
    }
}
