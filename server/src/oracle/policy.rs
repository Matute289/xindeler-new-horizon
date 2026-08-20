//! Anti-chaos gates in front of [`trigger::trigger_dm_event`] for the
//! HTTP-reachable ORACLE-trigger path.
//!
//! A human typing `/oracle_trigger` in chat is self-limiting: they can only
//! type so fast, and a bad trigger is immediately visible to the admin who
//! typed it. An HTTP endpoint removes that limiter entirely, so this module
//! adds the checks that limiter used to provide for free: a rate ceiling,
//! a per-event-id cooldown, and a kill switch. The in-game chat command
//! (`server/src/cmd.rs::handle_oracle_trigger`) does **not** go through
//! this module — it calls `trigger::trigger_dm_event` directly, unchanged,
//! since a human typing a command is still its own rate limit.
//!
//! Gateway-side limits (if any exist upstream of this) are a UI convenience
//! only. If the gateway is ever compromised, its own checks evaporate, so
//! the checks here are the authoritative backstop and must never be treated
//! as redundant with something enforced only client-side of this process.

use super::trigger::{self, CeilingPolicy, TriggerError, TriggerOutcome};
use specs::{World, WorldExt};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use vek::Vec3;

/// Maximum triggers allowed within a trailing one-hour window.
pub const MAX_TRIGGERS_PER_HOUR: usize = 6;
/// Maximum triggers allowed within a trailing 24-hour window.
pub const MAX_TRIGGERS_PER_DAY: usize = 30;
/// Minimum time between two fires of the same `event_id`.
pub const EVENT_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// Default per-trigger spawn-count cap, tighter than the sanitize-time
/// `spawn_count` clamp (200) `DmEvent::sanitize` already enforces on the
/// file itself. A caller may raise the effective cap to that same 200 by
/// setting `high_impact_override` -- see [`gated_trigger_dm_event`].
pub const DEFAULT_PER_EVENT_CAP: usize = 50;

/// Kill switch for the HTTP-reachable ORACLE-trigger path specifically.
///
/// **Never** wire this to `common::resources::OracleLive` -- that flag
/// gates player spellcasting authority and shares nothing with this one but
/// a name. This resource governs only whether [`gated_trigger_dm_event`]
/// accepts new triggers (real or `dry_run`); it has no bearing on any spell
/// a player casts.
///
/// Defaults to enabled, so a freshly started server does not silently
/// refuse a feature nobody explicitly turned off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OracleEventsEnabled(pub bool);

impl Default for OracleEventsEnabled {
    fn default() -> Self { Self(true) }
}

/// Why [`gated_trigger_dm_event`] refused before ever reaching
/// [`trigger::trigger_dm_event`], or the underlying trigger error it passed
/// through unchanged.
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyError {
    /// [`OracleEventsEnabled`] is currently `false`.
    Disabled,
    /// The trailing-hour or trailing-day trigger count is already at its
    /// limit.
    RateLimited { window: &'static str, limit: usize },
    /// `event_id` last fired less than [`EVENT_COOLDOWN`] ago.
    Cooldown {
        event_id: String,
        remaining: Duration,
    },
    /// Passed through from [`trigger::trigger_dm_event`] unchanged.
    Trigger(TriggerError),
}

impl From<TriggerError> for PolicyError {
    fn from(err: TriggerError) -> Self { Self::Trigger(err) }
}

/// Server-side rate/cooldown ledger backing [`gated_trigger_dm_event`].
/// Restart-volatile by design, the same posture as `ChronicleLog` and the
/// `OracleSpawned`-derived live count: a fresh process legitimately starts
/// with a clean rate-limit window, not a persisted one.
#[derive(Debug, Default)]
pub struct OracleTriggerLedger {
    hourly: VecDeque<Instant>,
    daily: VecDeque<Instant>,
    last_fired: HashMap<Arc<str>, Instant>,
}

impl OracleTriggerLedger {
    fn prune(&mut self, now: Instant) {
        while self
            .hourly
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3600))
        {
            self.hourly.pop_front();
        }
        while self
            .daily
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(24 * 3600))
        {
            self.daily.pop_front();
        }
    }

    /// Checks the rate ceilings and the per-event cooldown without
    /// recording a new entry -- callers that only need to know "would this
    /// be refused right now" (e.g. a future status endpoint) can call this
    /// without side effects.
    fn check(&mut self, event_id: &str, now: Instant) -> Result<(), PolicyError> {
        self.prune(now);
        if self.hourly.len() >= MAX_TRIGGERS_PER_HOUR {
            return Err(PolicyError::RateLimited {
                window: "hour",
                limit: MAX_TRIGGERS_PER_HOUR,
            });
        }
        if self.daily.len() >= MAX_TRIGGERS_PER_DAY {
            return Err(PolicyError::RateLimited {
                window: "day",
                limit: MAX_TRIGGERS_PER_DAY,
            });
        }
        if let Some(last) = self.last_fired.get(event_id) {
            let elapsed = now.duration_since(*last);
            if elapsed < EVENT_COOLDOWN {
                return Err(PolicyError::Cooldown {
                    event_id: event_id.to_owned(),
                    remaining: EVENT_COOLDOWN - elapsed,
                });
            }
        }
        Ok(())
    }

    fn record(&mut self, event_id: &str, now: Instant) {
        self.hourly.push_back(now);
        self.daily.push_back(now);
        self.last_fired.insert(Arc::from(event_id), now);
    }
}

/// The single seam the HTTP `OracleTrigger` message handler calls into.
/// Wraps [`trigger::trigger_dm_event`] with the rate limit, per-event
/// cooldown, per-event spawn cap, and kill switch this module exists for.
///
/// A `dry_run` is exempt from the rate limit and cooldown (it never mutates
/// the world, so metering it against the same budget a real fire consumes
/// would make "preview safely first" cost the very budget it exists to
/// protect) but is **not** exempt from the kill switch -- an operator who
/// disabled triggers does not want previews either.
///
/// `high_impact_override` raises the effective per-trigger spawn cap from
/// [`DEFAULT_PER_EVENT_CAP`] up to the sanitize-time ceiling `DmEvent`
/// files are already clamped to. This function does not itself gate who
/// may set that flag -- the caller (the gateway, after its own re-auth) is
/// responsible for only setting it when that re-auth actually happened.
pub fn gated_trigger_dm_event(
    ecs: &World,
    event_id: &str,
    pos: Vec3<f32>,
    dry_run: bool,
    high_impact_override: bool,
) -> Result<TriggerOutcome, PolicyError> {
    if !ecs.read_resource::<OracleEventsEnabled>().0 {
        return Err(PolicyError::Disabled);
    }

    let now = Instant::now();
    if !dry_run {
        ecs.write_resource::<OracleTriggerLedger>()
            .check(event_id, now)?;
    }

    let per_event_cap = if high_impact_override {
        common_oracle::dm_event::bounds::SPAWN_COUNT.1 as usize
    } else {
        DEFAULT_PER_EVENT_CAP
    };

    let outcome = trigger::trigger_dm_event(
        ecs,
        event_id,
        pos,
        dry_run,
        CeilingPolicy::Refuse,
        per_event_cap,
    )?;

    if !dry_run {
        ecs.write_resource::<OracleTriggerLedger>()
            .record(event_id, now);
    }

    Ok(outcome)
}

#[cfg(test)]
mod ledger_tests {
    use super::*;

    #[test]
    fn stays_silent_under_every_limit() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        for _ in 0..MAX_TRIGGERS_PER_HOUR - 1 {
            ledger.check("e", now).expect("under the limit");
            ledger.record("e2", now);
        }
    }

    #[test]
    fn refuses_once_the_hourly_ceiling_is_hit() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        for i in 0..MAX_TRIGGERS_PER_HOUR {
            ledger
                .check(&format!("e{i}"), now)
                .expect("under the limit");
            ledger.record(&format!("e{i}"), now);
        }
        let err = ledger
            .check("one_more", now)
            .expect_err("the hourly ceiling was just hit");
        assert_eq!(err, PolicyError::RateLimited {
            window: "hour",
            limit: MAX_TRIGGERS_PER_HOUR,
        });
    }

    #[test]
    fn hourly_entries_older_than_an_hour_are_pruned() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        for i in 0..MAX_TRIGGERS_PER_HOUR {
            ledger.record(&format!("e{i}"), now);
        }
        let later = now + Duration::from_secs(3601);
        ledger
            .check("fresh_slot", later)
            .expect("the earlier entries are now over an hour old");
    }

    #[test]
    fn same_event_id_within_the_cooldown_is_refused() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        ledger.check("mist_bound", now).expect("first fire");
        ledger.record("mist_bound", now);

        let soon = now + Duration::from_secs(60);
        let err = ledger
            .check("mist_bound", soon)
            .expect_err("only 60s of a 5min cooldown elapsed");
        assert!(matches!(err, PolicyError::Cooldown { event_id, .. } if event_id == "mist_bound"));
    }

    #[test]
    fn a_different_event_id_is_unaffected_by_another_ones_cooldown() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        ledger.record("mist_bound", now);
        ledger
            .check("wraith_choir", now)
            .expect("a different event id has no cooldown yet");
    }

    #[test]
    fn the_cooldown_clears_once_it_elapses() {
        let mut ledger = OracleTriggerLedger::default();
        let now = Instant::now();
        ledger.record("mist_bound", now);
        let after = now + EVENT_COOLDOWN + Duration::from_secs(1);
        ledger
            .check("mist_bound", after)
            .expect("the cooldown window has fully elapsed");
    }
}

#[cfg(test)]
mod gated_trigger_tests {
    use super::*;
    use crate::oracle::{ChronicleLog, OracleWatcher, spawned};
    use common::event::{CreateNpcEvent, EventBus};
    use common_oracle::{EntityTemplate, SpawningRules};

    fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    fn world_with_fixture() -> (World, tempfile::TempDir) { world_with_spawn_count(3.0) }

    /// Same fixture as `world_with_fixture`, but with `spawn_count`
    /// parameterized so tests can exercise the per-event cap directly at
    /// ingestion time (there is no mutable accessor onto an already-loaded
    /// `OracleEvents` table -- entries are only ever replaced by re-ingesting
    /// a changed file, which is the real, intended lifecycle).
    fn world_with_spawn_count(spawn_count: f32) -> (World, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        let mut watcher = OracleWatcher::new(&root);

        let template = EntityTemplate {
            entity_template_id: "test_template".to_owned(),
            body: "pig".to_owned(),
            faction: "wild".to_owned(),
            ..Default::default()
        };
        std::fs::write(
            root.join("test_template.entity_template.ron"),
            ron::ser::to_string(&template).expect("EntityTemplate serializes"),
        )
        .expect("write template fixture");

        let event = common_oracle::DmEvent {
            spawning_rules: SpawningRules {
                entity_templates: vec!["test_template".to_owned()],
                spawn_count,
                spawn_radius: 5.0,
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(
            root.join("fixture_event.dmevent.ron"),
            ron::ser::to_string(&event).expect("DmEvent serializes"),
        )
        .expect("write event fixture");

        let ingested = wait_until(
            || {
                watcher.poll();
                watcher.events().dm_events().count() >= 1
                    && watcher.events().entity_templates().count() >= 1
            },
            Duration::from_secs(2),
        );
        assert!(ingested, "fixture files were not ingested in time");

        let mut world = World::new();
        world.register::<spawned::OracleSpawned>();
        world.insert(watcher);
        world.insert(EventBus::<CreateNpcEvent>::default());
        world.insert(ChronicleLog::default());
        world.insert(OracleEventsEnabled::default());
        world.insert(OracleTriggerLedger::default());
        (world, dir)
    }

    #[test]
    fn the_kill_switch_refuses_a_real_fire() {
        let (world, _dir) = world_with_fixture();
        *world.write_resource::<OracleEventsEnabled>() = OracleEventsEnabled(false);
        let err = gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), false, false)
            .expect_err("kill switch is off");
        assert_eq!(err, PolicyError::Disabled);
    }

    #[test]
    fn the_kill_switch_also_refuses_a_dry_run() {
        let (world, _dir) = world_with_fixture();
        *world.write_resource::<OracleEventsEnabled>() = OracleEventsEnabled(false);
        let err = gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), true, false)
            .expect_err("kill switch blocks previews too, not just real fires");
        assert_eq!(err, PolicyError::Disabled);
    }

    #[test]
    fn a_dry_run_does_not_spend_rate_limit_or_cooldown_budget() {
        let (world, _dir) = world_with_fixture();
        for _ in 0..MAX_TRIGGERS_PER_HOUR + 5 {
            gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), true, false)
                .expect("dry runs never consume the rate-limit budget");
        }
    }

    #[test]
    fn a_real_fire_records_into_the_ledger() {
        let (world, _dir) = world_with_fixture();
        gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), false, false)
            .expect("first real fire succeeds");
        let err = gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), false, false)
            .expect_err("immediate re-fire of the same event id hits the cooldown");
        assert!(matches!(err, PolicyError::Cooldown { .. }));
    }

    #[test]
    fn default_cap_refuses_a_plan_over_fifty_without_the_override() {
        // spawn_count 60 is under sanitize()'s own 200 clamp but over
        // DEFAULT_PER_EVENT_CAP (50).
        let (world, _dir) = world_with_spawn_count(60.0);
        let err = gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), false, false)
            .expect_err("60 > DEFAULT_PER_EVENT_CAP without an override");
        assert!(matches!(
            err,
            PolicyError::Trigger(TriggerError::WouldExceedPerEventCap { .. })
        ));
    }

    #[test]
    fn high_impact_override_allows_a_plan_over_the_default_cap() {
        let (world, _dir) = world_with_spawn_count(60.0);
        gated_trigger_dm_event(&world, "fixture_event", Vec3::zero(), false, true)
            .expect("override raises the cap up to the sanitize-time ceiling");
    }
}
