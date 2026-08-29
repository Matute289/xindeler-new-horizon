//! `trigger_dm_event` — the single seam that resolves an ORACLE `DmEvent`
//! id into real, ceiling-checked spawns. Both the in-game `/oracle_trigger`
//! admin command and any future non-chat trigger path (an HTTP route, a
//! console button) are meant to call through here rather than reimplement
//! the lookup/ceiling/spawn sequence, so the two are bounded identically.
//!
//! Takes `&specs::World` rather than `&mut Server`: everything this
//! function touches (`OracleWatcher`, `EventBus<CreateNpcEvent>`,
//! `ChronicleLog`) is an ECS resource, and staying `Server`-free keeps the
//! ceiling/spawn logic reachable from a plain `specs::World` in tests.
//!
//! ## The live-entity ceiling and the warning threshold
//!
//! [`common_oracle::MAX_LIVE_ORACLE_ENTITIES`] is a hard ceiling on how many
//! ORACLE-attributed entities may be alive at once. What happens when a
//! trigger's request would cross it is **not** decided silently inside this
//! function — that decision belongs to the caller, made explicit through
//! [`CeilingPolicy`]:
//!
//! - Below [`common_oracle::CEILING_WARNING_FRACTION`] of the ceiling: the
//!   trigger proceeds normally, no warning.
//! - At or above that fraction but still under the ceiling: the trigger still
//!   proceeds and spawns everything requested, but the `Ok` result carries a
//!   [`CeilingWarning`] the caller should surface so an operator sees the
//!   situation before it becomes a hard refusal.
//! - At or over the ceiling: [`trigger_dm_event`] does not choose a fallback on
//!   its own. With [`CeilingPolicy::Refuse`] (the default for
//!   `/oracle_trigger`) it returns [`TriggerError::WouldExceedCeiling`] with
//!   all three numbers and creates **zero** entities. With
//!   [`CeilingPolicy::Clamp`] — which a caller opts into only after an operator
//!   has explicitly asked to proceed anyway — it spawns as many as fit under
//!   the ceiling and reports `clamped: true` in the outcome. There is
//!   deliberately no way to raise the ceiling at runtime; if the default is
//!   genuinely too low, that is a constant to change and redeploy, not a live
//!   knob.

use crate::persistence::{self, DatabaseSettings};
use chrono::Utc;
use common::{
    event::{CreateNpcEvent, EventBus, NpcBuilder},
    slowjob::SlowJobPool,
};
use common_oracle::{CEILING_WARNING_FRACTION, DmEvent, MAX_LIVE_ORACLE_ENTITIES};
use rand::rng;
use specs::{World, WorldExt};
use std::sync::{Arc, RwLock};
use tracing::error;
use vek::Vec3;

use super::{ChronicleLog, ChronicleWrites, OracleWatcher, factory, spawned};

/// What a caller wants `trigger_dm_event` to do when the requested spawn
/// would push the live count over [`MAX_LIVE_ORACLE_ENTITIES`]. Never
/// chosen automatically — see the module doc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CeilingPolicy {
    /// Create zero entities and return
    /// [`TriggerError::WouldExceedCeiling`]. The safe default.
    #[default]
    Refuse,
    /// Spawn as many of the planned entities as fit under the ceiling
    /// (`ceiling - live`, which may be zero) instead of refusing outright.
    /// Only meant to be reached after an operator has seen the exceeded-by
    /// numbers and explicitly asked to proceed anyway.
    Clamp,
}

/// A non-fatal heads-up: the trigger this accompanies still spawned
/// everything it requested, but the live count is now at or above
/// [`CEILING_WARNING_FRACTION`] of the ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CeilingWarning {
    /// Live ORACLE-entity count before this trigger's spawns were added.
    pub live: usize,
    /// How many entities this trigger just added (or, for a `dry_run`, is
    /// about to add).
    pub planned: usize,
    pub ceiling: usize,
    pub threshold: usize,
}

/// The result of a successful (possibly clamped) trigger.
#[derive(Clone, Debug)]
pub struct TriggerOutcome {
    pub event_id: Arc<str>,
    pub at: Vec3<f32>,
    /// How many entities the resolved spawn plan called for, before any
    /// [`CeilingPolicy::Clamp`] truncation.
    pub requested: usize,
    /// How many entities were actually (or, for a `dry_run`, would be)
    /// created. Equal to `requested` unless `clamped` is true.
    pub spawned: usize,
    /// True if [`CeilingPolicy::Clamp`] cut the plan short to fit under the
    /// ceiling.
    pub clamped: bool,
    /// Set when `spawned` didn't need clamping but pushed the live count at
    /// or above the warning threshold.
    pub warning: Option<CeilingWarning>,
    /// The event's `on_enter_message`, if any — the caller decides whether
    /// and how to deliver it (the in-game chat command has a client entity
    /// to greet; a future non-chat trigger path may not).
    pub on_enter_message: Option<String>,
    /// The outer `Body` variant of every entity actually (or, for a
    /// `dry_run`, would be) spawned — e.g. `"QuadrupedMedium"`, not the
    /// specific species. A preview aid, not player-facing text.
    pub bodies: Vec<String>,
}

/// Why `trigger_dm_event` could not produce a [`TriggerOutcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerError {
    /// No currently-loaded `.dmevent.ron`/`.dmevent.json` matches this id.
    UnknownEvent { event_id: String },
    /// The event's `spawning_rules.entity_templates` names at least one id,
    /// but none of them matched a currently-loaded `EntityTemplate` — the
    /// resolved spawn plan is empty and firing it would be a silent no-op.
    NoTemplatesMatched { event_id: String },
    /// Spawning the resolved plan on top of the current live count would
    /// exceed the ceiling, and the caller's [`CeilingPolicy`] was `Refuse`.
    /// Zero entities were created.
    WouldExceedCeiling {
        live: usize,
        planned: usize,
        ceiling: usize,
    },
    /// The resolved spawn plan alone (before any aggregate-ceiling check)
    /// exceeds the caller's `per_event_cap`. Checked first, independently of
    /// [`CeilingPolicy`] — a caller-supplied per-trigger cap is not a
    /// clampable ceiling, it is a hard refusal. Zero entities were created.
    WouldExceedPerEventCap { requested: usize, cap: usize },
}

/// Resolves `event_id` against currently-loaded ORACLE assets, checks
/// `per_event_cap` and the live-entity ceiling (per `ceiling_policy`), and —
/// unless `dry_run` — emits the resulting `CreateNpcEvent`s (each tagged
/// with `event_id`) and pushes the event's `world_rumor` to the chronicle
/// log.
///
/// `per_event_cap` bounds this single trigger's resolved spawn count,
/// independently of and checked before the aggregate `MAX_LIVE_ORACLE_ENTITIES`
/// ceiling — pass `usize::MAX` for "no additional cap beyond whatever
/// `DmEvent::sanitize` already clamped `spawn_count` to".
///
/// Does **not** send any message to a player: no client entity is assumed
/// to exist. The caller renders `on_enter_message` and any operator-facing
/// warning/error itself.
pub fn trigger_dm_event(
    ecs: &World,
    event_id: &str,
    pos: Vec3<f32>,
    dry_run: bool,
    ceiling_policy: CeilingPolicy,
    per_event_cap: usize,
) -> Result<TriggerOutcome, TriggerError> {
    let dm_event = find_loaded_event(ecs, event_id)?;
    let templates: Vec<_> = ecs
        .read_resource::<OracleWatcher>()
        .events()
        .entity_templates()
        .map(|(_, template)| template.clone())
        .collect();

    let mut spawn_rng = rng();
    let mut spawns = factory::spawn_dm_event(
        &dm_event.spawning_rules,
        templates.iter(),
        pos,
        &mut spawn_rng,
    );

    // `spawns` can be empty for two different reasons: no `entity_templates`
    // id matched a loaded template (a real configuration problem, worth its
    // own error), or every id matched fine but `spawning_rules.spawn_count`
    // sanitizes to zero (a legitimate zero-entity request). Checking
    // `factory::has_matching_templates` directly, rather than inferring the
    // cause from `spawns.is_empty()` alone, tells the two apart so the
    // second case doesn't get a misleading "none of them are currently
    // loaded" error.
    if spawns.is_empty()
        && !dm_event.spawning_rules.entity_templates.is_empty()
        && !factory::has_matching_templates(&dm_event.spawning_rules, templates.iter())
    {
        return Err(TriggerError::NoTemplatesMatched {
            event_id: event_id.to_string(),
        });
    }

    let requested = spawns.len();
    if requested > per_event_cap {
        return Err(TriggerError::WouldExceedPerEventCap {
            requested,
            cap: per_event_cap,
        });
    }

    let live = spawned::live_count(ecs);
    let resolution = resolve_ceiling(
        live,
        requested,
        MAX_LIVE_ORACLE_ENTITIES,
        CEILING_WARNING_FRACTION,
        ceiling_policy,
    )?;
    spawns.truncate(resolution.spawn_count);

    let event_id: Arc<str> = Arc::from(event_id);
    let spawned_count = spawns.len();
    let bodies = spawns
        .iter()
        .map(|(_, _, npc)| npc.body.to_string())
        .collect();

    if !dry_run {
        let event_bus = ecs.read_resource::<EventBus<CreateNpcEvent>>();
        for (pos, ori, npc) in spawns {
            event_bus.emit_now(CreateNpcEvent {
                pos,
                ori,
                npc: tag(npc, &event_id),
            });
        }
        drop(event_bus);

        if let Some(rumor) = &dm_event.narrative.world_rumor {
            ecs.write_resource::<ChronicleLog>().push(rumor.clone());
            persist_chronicle_entry(ecs, rumor);
        }
    }

    Ok(TriggerOutcome {
        event_id,
        at: pos,
        requested,
        spawned: spawned_count,
        clamped: resolution.clamped,
        warning: resolution.warning,
        on_enter_message: dm_event.narrative.on_enter_message.clone(),
        bodies,
    })
}

/// Mirrors an in-memory [`ChronicleLog`] append into its database table.
///
/// `trigger_dm_event` is reachable from chat-command dispatch, which runs as
/// part of an ECS system during the tick, so the write must never happen
/// here: enqueueing onto the [`SlowJobPool`] is a cheap mutex-guarded push
/// that touches no disk, and the actual connection/transaction happens on a
/// pool thread.
///
/// **This is write-behind, and deliberately weaker than the write-through
/// convention `CharacterSuspensions` uses.** There, the database write
/// happens first and the in-memory cache is only updated once it succeeds, so
/// the two can never disagree. Here the in-memory push has already happened,
/// synchronously, by the time this is called; the database write is
/// asynchronous and may fail or be dropped on its own. So a rumor can exist
/// in the live [`ChronicleLog`] and never reach the table -- it would then
/// vanish at the next restart. That is acceptable for a narrative log in a
/// way it would not be for moderation state, but a caller must not read the
/// two as equivalent guarantees.
///
/// A failed write is therefore logged and dropped rather than propagated: the
/// caller's trigger genuinely happened, and the only consequence is one lost
/// rumor -- not something worth failing an operator's trigger over, and
/// certainly not worth unwinding a job-pool worker thread.
///
/// Ordering: the job name is configured with concurrency 1, so the writes
/// themselves never overlap. That the *enqueue* order matches the in-memory
/// push order additionally relies on every current caller reaching here from
/// the same thread (the chat-command dispatch and `Message::OracleTrigger`
/// both funnel through the server's single-threaded tick). Nothing enforces
/// that; a future genuinely concurrent caller would need its own ordering
/// story, since concurrency 1 alone only serializes the writes, it does not
/// decide which of two racing callers queued first.
fn persist_chronicle_entry(ecs: &World, rumor: &str) {
    let settings = {
        let settings = ecs.read_resource::<Arc<RwLock<DatabaseSettings>>>();
        Arc::clone(&settings)
    };
    // Registered before the spawn, so a shutdown that begins between here and
    // the job actually running still sees this write as outstanding.
    let in_flight = ecs.read_resource::<ChronicleWrites>().begin();
    let text = rumor.to_owned();
    let created_at = Utc::now();

    ecs.read_resource::<SlowJobPool>()
        .spawn("CHRONICLE_LOG", move || {
            // Dropped on every exit path below, so the shutdown drain can
            // never be left waiting on a write that already gave up.
            let _in_flight = in_flight;
            let settings = match settings.read() {
                // Unlike every other `DatabaseSettings` call site, this one
                // does not `.expect()` on a poisoned lock: the release profile
                // is `panic = "abort"`, so panicking here -- on a background
                // job-pool thread, over a narrative log -- would take the
                // whole server process down rather than just this one write.
                Ok(settings) => settings,
                Err(_) => {
                    error!("DatabaseSettings RwLock was poisoned; chronicle entry not persisted");
                    return;
                },
            };
            if let Err(err) = persistence::append_chronicle_entry(&text, created_at, &settings) {
                error!(?err, "Failed to persist a chronicle entry");
            }
        });
}

/// The outcome of resolving `requested` new entities against `live` already
/// on top of `ceiling`, under `policy`.
#[derive(Debug)]
struct CeilingResolution {
    /// How many of the `requested` entities to actually spawn — equal to
    /// `requested` unless `clamped`.
    spawn_count: usize,
    clamped: bool,
    warning: Option<CeilingWarning>,
}

/// The pure ceiling/warning decision, with no ECS/IO involved, so it can be
/// unit tested directly rather than only through a full trigger + `World`.
/// See the module doc for the semantics.
fn resolve_ceiling(
    live: usize,
    requested: usize,
    ceiling: usize,
    warning_fraction: f32,
    policy: CeilingPolicy,
) -> Result<CeilingResolution, TriggerError> {
    if live + requested > ceiling {
        return match policy {
            CeilingPolicy::Refuse => Err(TriggerError::WouldExceedCeiling {
                live,
                planned: requested,
                ceiling,
            }),
            CeilingPolicy::Clamp => {
                let available = ceiling.saturating_sub(live);
                Ok(CeilingResolution {
                    spawn_count: available,
                    clamped: true,
                    // A clamp notice already tells the caller the situation;
                    // no need to also attach a soft warning.
                    warning: None,
                })
            },
        };
    }

    let threshold = (ceiling as f32 * warning_fraction) as usize;
    let warning = (live + requested >= threshold).then_some(CeilingWarning {
        live,
        planned: requested,
        ceiling,
        threshold,
    });

    Ok(CeilingResolution {
        spawn_count: requested,
        clamped: false,
        warning,
    })
}

fn tag(npc: NpcBuilder, event_id: &Arc<str>) -> NpcBuilder {
    npc.with_oracle_event_id(Arc::clone(event_id))
}

fn find_loaded_event(ecs: &World, event_id: &str) -> Result<DmEvent, TriggerError> {
    ecs.read_resource::<OracleWatcher>()
        .events()
        .dm_events()
        .find(|(path, _)| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == format!("{event_id}.dmevent.ron")
                        || name == format!("{event_id}.dmevent.json")
                })
        })
        .map(|(_, event)| event.clone())
        .ok_or_else(|| TriggerError::UnknownEvent {
            event_id: event_id.to_string(),
        })
}

#[cfg(test)]
mod resolve_ceiling_tests {
    use super::*;

    #[test]
    fn under_the_warning_fraction_is_silent() {
        let r = resolve_ceiling(10, 5, 100, 0.8, CeilingPolicy::Refuse).expect("fits");
        assert_eq!(r.spawn_count, 5);
        assert!(!r.clamped);
        assert!(r.warning.is_none());
    }

    #[test]
    fn at_or_above_the_warning_fraction_but_under_ceiling_warns_and_still_spawns_all() {
        // live 75 + requested 10 = 85, >= 80% of 100, but <= 100.
        let r = resolve_ceiling(75, 10, 100, 0.8, CeilingPolicy::Refuse).expect("fits");
        assert_eq!(r.spawn_count, 10, "a soft warning must never reduce spawns");
        assert!(!r.clamped);
        let warning = r.warning.expect("expected a warning at 85/100");
        assert_eq!(warning.live, 75);
        assert_eq!(warning.planned, 10);
        assert_eq!(warning.ceiling, 100);
        assert_eq!(warning.threshold, 80);
    }

    #[test]
    fn exceeding_the_ceiling_with_refuse_policy_errors_and_spawns_nothing() {
        let err = resolve_ceiling(295, 10, 300, 0.8, CeilingPolicy::Refuse)
            .expect_err("295 + 10 > 300 must refuse");
        assert_eq!(err, TriggerError::WouldExceedCeiling {
            live: 295,
            planned: 10,
            ceiling: 300,
        });
    }

    #[test]
    fn exceeding_the_ceiling_with_clamp_policy_spawns_only_what_fits() {
        let r = resolve_ceiling(295, 10, 300, 0.8, CeilingPolicy::Clamp).expect("clamp succeeds");
        assert_eq!(r.spawn_count, 5, "only 5 of 300 - 295 fit");
        assert!(r.clamped);
        assert!(
            r.warning.is_none(),
            "a clamp notice supersedes the soft warning"
        );
    }

    #[test]
    fn clamp_policy_at_zero_headroom_spawns_nothing_but_does_not_error() {
        let r = resolve_ceiling(300, 10, 300, 0.8, CeilingPolicy::Clamp).expect("clamp succeeds");
        assert_eq!(r.spawn_count, 0);
        assert!(r.clamped);
    }

    #[test]
    fn exactly_at_the_ceiling_is_not_an_over_ceiling_case() {
        let r = resolve_ceiling(290, 10, 300, 0.8, CeilingPolicy::Refuse)
            .expect("290 + 10 == 300 must not refuse");
        assert_eq!(r.spawn_count, 10);
        assert!(!r.clamped);
        assert!(
            r.warning.is_some(),
            "300/300 is well past the 80% warning line"
        );
    }
}

#[cfg(test)]
mod trigger_dm_event_tests {
    use super::*;
    use crate::persistence::SqlLogMode;
    use common_oracle::{EntityTemplate, Narrative, SpawningRules};
    use std::time::{Duration, Instant};

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

    /// A `World` wired with a real (tempdir-backed) `OracleWatcher` carrying
    /// one loaded `DmEvent` ("fixture_event") and one matching
    /// `EntityTemplate`, plus every resource/component `trigger_dm_event`
    /// touches. The `TempDir` is returned too so it isn't dropped (and
    /// deleted) before the test finishes with it.
    fn world_with_loaded_fixture_event() -> (World, tempfile::TempDir) {
        world_with_event("fixture_event", SpawningRules {
            entity_templates: vec!["test_template".to_owned()],
            spawn_count: 3.0,
            spawn_radius: 5.0,
            ..Default::default()
        })
    }

    /// Same fixture as `world_with_loaded_fixture_event`, but with the
    /// `DmEvent`'s `spawning_rules` swapped out for `spawning_rules` — the
    /// "test_template" `EntityTemplate` is always loaded, so tests can name
    /// or omit it from `entity_templates` themselves.
    fn world_with_event(
        event_id: &str,
        spawning_rules: SpawningRules,
    ) -> (World, tempfile::TempDir) {
        world_with_dm_event(event_id, DmEvent {
            spawning_rules,
            ..Default::default()
        })
    }

    /// The general form of `world_with_event`, for tests that need to set
    /// more of the `DmEvent` than just its `spawning_rules` (its `narrative`,
    /// say).
    fn world_with_dm_event(event_id: &str, event: DmEvent) -> (World, tempfile::TempDir) {
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

        std::fs::write(
            root.join(format!("{event_id}.dmevent.ron")),
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
        (world, dir)
    }

    #[test]
    fn unknown_event_id_errors_without_touching_the_event_bus() {
        let (world, _dir) = world_with_loaded_fixture_event();
        let err = trigger_dm_event(
            &world,
            "no_such_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect_err("id was never loaded");
        assert_eq!(err, TriggerError::UnknownEvent {
            event_id: "no_such_event".to_owned(),
        });
        assert_eq!(
            world
                .read_resource::<EventBus<CreateNpcEvent>>()
                .recv_all()
                .len(),
            0
        );
    }

    #[test]
    fn a_fitting_trigger_emits_one_tagged_create_npc_event_per_spawn() {
        let (world, _dir) = world_with_loaded_fixture_event();
        let outcome = trigger_dm_event(
            &world,
            "fixture_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect("fits comfortably under the ceiling");

        assert_eq!(outcome.spawned, 3);
        assert_eq!(outcome.requested, 3);
        assert!(!outcome.clamped);
        assert!(outcome.warning.is_none());
        assert_eq!(&*outcome.event_id, "fixture_event");
        assert_eq!(outcome.bodies.len(), 3);

        let emitted: Vec<_> = world
            .read_resource::<EventBus<CreateNpcEvent>>()
            .recv_all()
            .collect();
        assert_eq!(emitted.len(), 3);
        for ev in &emitted {
            assert_eq!(ev.npc.oracle_event_id.as_deref(), Some("fixture_event"));
        }
    }

    #[test]
    fn dry_run_reports_the_outcome_but_emits_nothing() {
        let (world, _dir) = world_with_loaded_fixture_event();
        let outcome = trigger_dm_event(
            &world,
            "fixture_event",
            Vec3::zero(),
            true,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect("fits comfortably under the ceiling");

        assert_eq!(outcome.spawned, 3);
        assert_eq!(
            outcome.bodies.len(),
            3,
            "dry_run still reports what would spawn"
        );
        assert_eq!(
            world
                .read_resource::<EventBus<CreateNpcEvent>>()
                .recv_all()
                .len(),
            0,
            "dry_run must never emit a CreateNpcEvent"
        );
        assert!(world.read_resource::<ChronicleLog>().is_empty());
    }

    #[test]
    fn an_unmatched_template_id_errors_as_no_templates_matched() {
        let (world, _dir) = world_with_event("unmatched_event", SpawningRules {
            entity_templates: vec!["no_such_template".to_owned()],
            spawn_count: 3.0,
            spawn_radius: 5.0,
            ..Default::default()
        });
        let err = trigger_dm_event(
            &world,
            "unmatched_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect_err("no loaded template matches \"no_such_template\"");
        assert_eq!(err, TriggerError::NoTemplatesMatched {
            event_id: "unmatched_event".to_owned(),
        });
    }

    #[test]
    fn a_matched_template_with_zero_spawn_count_succeeds_with_nothing_spawned() {
        // `spawn_count: 0.0` is `SpawningRules::default()`'s own value (and
        // within `bounds::SPAWN_COUNT`), so an event whose author simply
        // omits `spawn_count` hits this path — it must not be misreported as
        // "no templates matched" just because the resolved spawn list is
        // also empty.
        let (world, _dir) = world_with_event("zero_count_event", SpawningRules {
            entity_templates: vec!["test_template".to_owned()],
            spawn_count: 0.0,
            spawn_radius: 5.0,
            ..Default::default()
        });
        let outcome = trigger_dm_event(
            &world,
            "zero_count_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect("a matched template with spawn_count 0 is a legitimate no-op, not an error");

        assert_eq!(outcome.requested, 0);
        assert_eq!(outcome.spawned, 0);
        assert_eq!(
            world
                .read_resource::<EventBus<CreateNpcEvent>>()
                .recv_all()
                .len(),
            0
        );
    }

    #[test]
    fn a_per_event_cap_below_the_resolved_plan_refuses_and_spawns_nothing() {
        let (world, _dir) = world_with_loaded_fixture_event();
        let err = trigger_dm_event(
            &world,
            "fixture_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            2,
        )
        .expect_err("3 requested exceeds a cap of 2");
        assert_eq!(err, TriggerError::WouldExceedPerEventCap {
            requested: 3,
            cap: 2,
        });
        assert_eq!(
            world
                .read_resource::<EventBus<CreateNpcEvent>>()
                .recv_all()
                .len(),
            0,
            "a per-event-cap refusal must not fall back to spawning a clamped amount"
        );
    }

    #[test]
    fn a_per_event_cap_checked_before_the_aggregate_ceiling() {
        // A per_event_cap of 2 is tighter than a ceiling that would
        // otherwise happily clamp the request -- the per-event refusal must
        // win, not be silently superseded by CeilingPolicy::Clamp.
        let (world, _dir) = world_with_loaded_fixture_event();
        let err = trigger_dm_event(
            &world,
            "fixture_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Clamp,
            2,
        )
        .expect_err("the per-event cap is checked independently of CeilingPolicy");
        assert_eq!(err, TriggerError::WouldExceedPerEventCap {
            requested: 3,
            cap: 2,
        });
    }

    #[test]
    fn a_per_event_cap_at_or_above_the_resolved_plan_does_not_refuse() {
        let (world, _dir) = world_with_loaded_fixture_event();
        let outcome = trigger_dm_event(
            &world,
            "fixture_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            3,
        )
        .expect("exactly at the cap must not refuse");
        assert_eq!(outcome.spawned, 3);
    }

    /// A fixture `World` for the chronicle-persistence path: the standard
    /// event fixture plus a `world_rumor` to chronicle, a real (small,
    /// concurrency-1) `SlowJobPool`, and a tempdir-backed, fully migrated
    /// database registered the way the server registers it.
    fn world_with_rumor_and_database(
        rumor: &str,
    ) -> (
        World,
        tempfile::TempDir,
        tempfile::TempDir,
        DatabaseSettings,
    ) {
        let (mut world, event_dir) = world_with_dm_event("rumor_event", DmEvent {
            spawning_rules: SpawningRules {
                entity_templates: vec!["test_template".to_owned()],
                spawn_count: 1.0,
                spawn_radius: 5.0,
                ..Default::default()
            },
            narrative: Narrative {
                world_rumor: Some(rumor.to_owned()),
                ..Default::default()
            },
            ..Default::default()
        });

        let db_dir = tempfile::tempdir().expect("db tempdir");
        let settings = DatabaseSettings {
            db_dir: db_dir.path().to_path_buf(),
            sql_log_mode: SqlLogMode::Disabled,
        };
        persistence::run_migrations(&settings);

        let threadpool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .expect("rayon pool"),
        );
        let pool = SlowJobPool::new(2, 100, threadpool);
        pool.configure("CHRONICLE_LOG", |_| 1);

        world.insert(pool);
        world.insert(Arc::new(RwLock::new(settings.clone())));
        world.insert(ChronicleWrites::default());

        (world, event_dir, db_dir, settings)
    }

    #[test]
    fn a_triggered_rumor_reaches_both_the_resource_and_the_database() {
        let (world, _event_dir, _db_dir, settings) =
            world_with_rumor_and_database("A cold mist swallows the village.");

        trigger_dm_event(
            &world,
            "rumor_event",
            Vec3::zero(),
            false,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect("fits comfortably under the ceiling");

        assert_eq!(
            world
                .read_resource::<ChronicleLog>()
                .iter()
                .collect::<Vec<_>>(),
            vec!["A cold mist swallows the village."],
            "the in-memory push is synchronous and must have already happened"
        );

        // The database write is deliberately off the tick thread. Draining it
        // the same way the server's shutdown path does, rather than polling
        // the table, is what proves that path actually covers this write --
        // if the job were never registered as in-flight, this would return
        // instantly and the assert below would catch the empty table.
        assert!(
            world
                .read_resource::<ChronicleWrites>()
                .wait_until_idle(Duration::from_secs(5)),
            "the queued chronicle write never drained"
        );
        assert_eq!(
            persistence::load_chronicle_log_tail(&settings)
                .expect("load")
                .front()
                .map(String::as_str),
            Some("A cold mist swallows the village."),
            "the rumor never reached the chronicle_log table"
        );
    }

    #[test]
    fn a_dry_run_persists_nothing_and_needs_no_job_pool() {
        // The dry-run branch returns before the chronicle is touched at all,
        // so it must not even fetch the `SlowJobPool` or the database
        // settings -- this fixture deliberately registers neither, and a
        // regression that moved the persistence call outside `if !dry_run`
        // would panic here on the missing resource.
        let (world, _dir) = world_with_dm_event("dry_rumor_event", DmEvent {
            spawning_rules: SpawningRules {
                entity_templates: vec!["test_template".to_owned()],
                spawn_count: 1.0,
                spawn_radius: 5.0,
                ..Default::default()
            },
            narrative: Narrative {
                world_rumor: Some("A rumor that must not be recorded.".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        });

        let outcome = trigger_dm_event(
            &world,
            "dry_rumor_event",
            Vec3::zero(),
            true,
            CeilingPolicy::Refuse,
            usize::MAX,
        )
        .expect("fits comfortably under the ceiling");

        assert_eq!(outcome.spawned, 1, "dry_run still reports what would spawn");
        assert!(world.read_resource::<ChronicleLog>().is_empty());
    }
}
