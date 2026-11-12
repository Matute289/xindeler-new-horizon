//! Species-attribute fallback audit.
//!
//! Some per-species attribute lookups on [`super::Body`] end in a wildcard
//! arm:
//!
//! ```ignore
//! Body::Object(object) => match object {
//!     object::Body::BarrelOrgan => 500,
//!     // …
//!     _ => 1000,
//! },
//! ```
//!
//! That arm makes adding a body *compile clean and be quietly wrong*: a new
//! one silently becomes a 1000 HP prop with no signal to the author, in code
//! review, or in CI.
//!
//! **This is now the smaller half of the problem.** Every creature's `mass`,
//! `base_health`, `base_poise` and `base_energy` moved to
//! `assets/common/body_stats.ron`, whose per-species struct fields are
//! required, so omitting one fails the load by name; `threat_tier` and
//! `magic_resist_tier` became exhaustive matches, so omitting one fails the
//! build. Both beat an audit. What is left here is `Body::Object` — props,
//! projectiles and turrets, which are not a species roster and so have no
//! `AllSpecies` struct to make required — plus three attributes whose
//! catch-all is genuinely the neutral value (`scale`, `spacing_radius`,
//! `combat_multiplier`).
//!
//! This module turns those wildcards from silent into **auditable**. Each one
//! is wrapped in [`attr_fallback!`], which is a no-op expression in every
//! normal build and, under `cfg(test)`, records *which* body kind and
//! attribute fell through. The test in
//! `common/src/comp/body/attr_audit_test.rs` then walks the entire creature
//! roster, collects every fallback that fires, and diffs it against the
//! checked-in ledger `attr_fallback_ledger.txt`.
//!
//! The consequence is the one that matters: **a newly added species that
//! forgets an explicit value fails `cargo test -p xindeler-common` with a
//! message naming the species and the attribute.** The pre-existing roster is
//! grandfathered by the ledger rather than silently blessed, so the debt is
//! visible and countable instead of invisible.
//!
//! Wrapping a wildcard changes no value and no behaviour — it only makes the
//! fall-through observable.

// Records every attribute fall-through taken on this thread.  A getter that
// nests wildcards (`spacing_radius` already calls into `dimensions`) can take
// more than one per call, so this collects rather than keeping one slot.
#[cfg(test)]
thread_local! {
    static HITS: std::cell::RefCell<Vec<(&'static str, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Called by [`attr_fallback!`] when an audited wildcard arm is taken.
#[cfg(test)]
pub(crate) fn record_fallback(body_kind: &'static str, attr: &'static str) {
    HITS.with(|hits| hits.borrow_mut().push((body_kind, attr)));
}

/// Runs `f` and reports every wildcard arm it took.
#[cfg(test)]
pub(crate) fn probe<T>(f: impl FnOnce() -> T) -> Vec<(&'static str, &'static str)> {
    HITS.with(|hits| hits.borrow_mut().clear());
    let _ = f();
    HITS.with(|hits| hits.borrow().clone())
}

/// Marks an audited wildcard arm as a fall-through.
///
/// Expands to `$value` and nothing else outside of `cfg(test)`; there is no
/// runtime cost, no branch and no code size in a shipped build.
///
/// ```ignore
/// Body::QuadrupedMedium(body) => match body.species {
///     quadruped_medium::Species::Bear => 500.0,
///     _ => attr_fallback!("QuadrupedMedium", "mass", 200.0),
/// },
/// ```
///
/// `$body_kind` is the `Body` variant the arm lives under, and the test
/// asserts it against the body it was called with, so a copy-pasted label is
/// caught. Use `"*"` for a wildcard that sits at the outer `match self` level
/// and therefore catches several body kinds at once.
///
/// ⚠️ **The `pub(crate) use` below is load-bearing, not incidental.** The macro
/// must only ever expand inside this crate, so that its `#[cfg(test)]` is
/// resolved against `xindeler-common`'s own compilation. Export it and the
/// recorder would vanish from this crate's test binary while every call site
/// still compiled — a silently dead audit. (`the_audit_mechanism_is_wired_up`
/// in `attr_audit_test.rs` would catch that, but do not rely on it.)
macro_rules! attr_fallback {
    ($body_kind:literal, $attr:literal, $value:expr) => {{
        #[cfg(test)]
        $crate::comp::body::attr_audit::record_fallback($body_kind, $attr);
        $value
    }};
}

pub(crate) use attr_fallback;
