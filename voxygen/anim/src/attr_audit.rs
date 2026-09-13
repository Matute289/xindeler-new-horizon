//! `SkeletonAttr` fall-through audit — the animation-side twin of
//! `common::comp::body::attr_audit`.
//!
//! Every body kind builds its `SkeletonAttr` from a stack of per-species
//! matches. Most of those matches are exhaustive, so adding a species simply
//! will not compile until every bone offset is filled in — which is exactly
//! the behaviour you want. A minority end in a wildcard instead:
//!
//! ```ignore
//! scaler: match (body.species, body.body_type) {
//!     (Mammoth, _) => 3.0,
//!     // …
//!     _ => 0.9,
//! },
//! ```
//!
//! Those are the dangerous ones: a new species compiles clean, renders, and is
//! quietly the wrong size with the wrong gait. [`attr_fallback!`] wraps each
//! such wildcard so that, under `cfg(test)`, the fall-through is recorded and
//! `attr_audit_test.rs` can diff the whole creature roster against the
//! checked-in ledger `attr_fallback_ledger.txt`.
//!
//! Outside `cfg(test)` the macro expands to the value and nothing else — no
//! branch, no atomic, no code. `SkeletonAttr::from` runs once per figure per
//! frame, so that matters.

// Records every `SkeletonAttr` fall-through taken on this thread.  A single
// `SkeletonAttr::from` evaluates many audited fields at once, so unlike the
// `common` twin this collects rather than holding one slot.
#[cfg(test)]
thread_local! {
    static HITS: std::cell::RefCell<Vec<(&'static str, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Called by [`attr_fallback!`] when a per-species wildcard arm is taken.
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

/// Marks a per-species `SkeletonAttr` wildcard arm as an *audited*
/// fall-through. Expands to `$value` alone outside `cfg(test)`.
macro_rules! attr_fallback {
    ($body_kind:literal, $attr:literal, $value:expr) => {{
        #[cfg(test)]
        $crate::attr_audit::record_fallback($body_kind, $attr);
        $value
    }};
}
