//! Operational limits on the *live population* PROJECT ORACLE is allowed to
//! keep spawned at once. Distinct from
//! [`dm_event::bounds`](crate::dm_event::bounds), which clamps individual
//! `DmEvent` fields during `DmEvent::sanitize` (an anti-chaos defense against a
//! hostile/buggy authored file) — these constants instead bound the *runtime
//! effect* of triggering an already-sanitized event, and are shared between the
//! server (which enforces the ceiling) and the future ops-console gateway
//! (which needs the same number to render "how much headroom is left" without
//! duplicating it and risking drift).
//!
//! This crate has no engine/`specs` dependency (see the crate-level doc on
//! `dm_event`), so these are plain `usize`/`f32` constants with no ECS types
//! involved.

/// Ceiling on how many entities PROJECT ORACLE may have alive at once,
/// summed across every triggered `DmEvent`. Enforced by
/// `server::oracle::trigger::trigger_dm_event`, the single seam both the
/// in-game `/oracle_trigger` admin command and the future HTTP trigger path
/// call into.
pub const MAX_LIVE_ORACLE_ENTITIES: usize = 300;

/// Fraction of [`MAX_LIVE_ORACLE_ENTITIES`] at which a trigger that still
/// fits under the ceiling nonetheless earns a soft warning instead of
/// spawning silently. 0.8 (80%, i.e. 240 of the default 300) was chosen so
/// the operator sees the warning while there is still a real, actionable
/// margin — 60 entities of headroom at the default ceiling — rather than a
/// warning that only fires once the situation is already tight. Getting the
/// warning is never a refusal: the trigger this fraction gates still
/// proceeds and spawns everything requested.
pub const CEILING_WARNING_FRACTION: f32 = 0.8;
