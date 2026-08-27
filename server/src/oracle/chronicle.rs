//! `DmEvent.narrative.world_rumor` feeds the real chronicle/lore system,
//! which does not exist yet. [`ChronicleLog`] is an explicit stand-in: a
//! plain in-memory, append-only, bounded-length log. The real system
//! replaces this resource later without touching `common_oracle::DmEvent`.
//!
//! The resource itself is still purely in-memory and knows nothing about the
//! database: it is warmed once at startup from [`ChronicleLog::load`], and
//! kept durable afterwards by the trigger call site, which mirrors each
//! [`ChronicleLog::push`] into the `chronicle_log` table from a background
//! slow-job. Keeping persistence at the call site rather than inside `push`
//! is what lets this type stay free of `DatabaseSettings`, I/O, and any
//! error path at all.

use crate::persistence::{self, DatabaseSettings};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use tracing::error;

/// Clamp bounds for [`ChronicleLog`] (anti-chaos, same posture as
/// `common_oracle::dm_event::bounds`): a misbehaving or malicious ORACLE
/// event stream dropping many `world_rumor`-carrying files must not grow
/// this resource unboundedly — oldest entries are dropped once the cap is
/// hit.
pub mod bounds {
    /// Maximum number of entries [`super::ChronicleLog`] retains.
    pub const MAX_ENTRIES: usize = 1024;
}

/// In-memory, append-only chronicle-hook log. Bounded to
/// [`bounds::MAX_ENTRIES`] (oldest entries drop first) so this resource
/// cannot grow without limit for the lifetime of a long-running server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChronicleLog(VecDeque<String>);

impl ChronicleLog {
    /// Reads the persisted chronicle back from the database, newest
    /// [`bounds::MAX_ENTRIES`] entries, oldest-first. Called once at server
    /// startup; a read failure is logged and treated as "nothing was
    /// chronicled yet" rather than aborting startup, since an empty log after
    /// a database hiccup is no worse than the always-empty-after-restart
    /// behaviour this replaces, whereas a startup panic here would take down
    /// the whole server over a narrative cache failing to warm.
    pub fn load(settings: &DatabaseSettings) -> Self {
        match persistence::load_chronicle_log_tail(settings) {
            Ok(entries) => Self(entries),
            Err(err) => {
                error!(
                    ?err,
                    "Failed to load the chronicle log from the database; starting with an empty \
                     log"
                );
                Self::default()
            },
        }
    }

    /// Appends `text`, dropping the oldest entry first if already at
    /// [`bounds::MAX_ENTRIES`].
    pub fn push(&mut self, text: impl Into<String>) {
        if self.0.len() >= bounds::MAX_ENTRIES {
            self.0.pop_front();
        }
        self.0.push_back(text.into());
    }

    /// Iterates entries oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &str> { self.0.iter().map(String::as_str) }

    #[must_use]
    pub fn len(&self) -> usize { self.0.len() }

    #[must_use]
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

/// Counts chronicle database writes that have been handed to the slow-job
/// pool but have not finished committing yet, so shutdown can wait for them.
///
/// The write path is deliberately fire-and-forget from the tick thread's
/// point of view (see `oracle::trigger`), which leaves one gap: a rumor
/// triggered moments before a restart could still be sitting in the job queue
/// when the process exits, and would be lost -- precisely the case persisting
/// the chronicle exists to cover. Nothing in [`common::slowjob::SlowJobPool`]
/// drains outstanding jobs on drop, and giving it general drain semantics
/// would change behaviour for every other job name on the pool, so the wait
/// is scoped to chronicle writes alone through this counter.
///
/// Cheap to clone (one `Arc`); clones all observe the same count.
#[derive(Clone, Default)]
pub struct ChronicleWrites(Arc<ChronicleWritesInner>);

#[derive(Default)]
struct ChronicleWritesInner {
    in_flight: Mutex<usize>,
    idle: Condvar,
}

impl ChronicleWrites {
    /// How long shutdown is willing to wait for outstanding chronicle writes.
    ///
    /// Bounded on purpose: losing the last rumor or two is a far smaller
    /// problem than a server that will not exit, which is the same trade
    /// [`ChronicleLog::load`] makes in the other direction at startup.
    pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

    /// Registers one write as in flight. The returned guard marks it finished
    /// when dropped, so every exit path out of the job closure -- commit,
    /// database error, poisoned lock, panic -- decrements exactly once.
    #[must_use]
    pub(crate) fn begin(&self) -> ChronicleWriteGuard {
        *lock(&self.0.in_flight) += 1;
        ChronicleWriteGuard(Arc::clone(&self.0))
    }

    /// Blocks until no chronicle write is in flight, or `timeout` elapses.
    ///
    /// Returns `true` if everything drained, `false` on timeout -- callers are
    /// expected to log and carry on rather than retry, since the only thing a
    /// longer wait buys is a stuck shutdown.
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut in_flight = lock(&self.0.in_flight);
        while *in_flight > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (guard, timed_out) = self
                .0
                .idle
                .wait_timeout(in_flight, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            in_flight = guard;
            if timed_out.timed_out() && *in_flight > 0 {
                return false;
            }
        }
        true
    }

    /// How many writes are in flight right now. Test-only: production code
    /// waits on the count rather than sampling it, since any sampled value is
    /// stale the moment it is read.
    #[cfg(test)]
    fn in_flight(&self) -> usize { *lock(&self.0.in_flight) }
}

/// Marks one chronicle write finished when dropped. See
/// [`ChronicleWrites::begin`].
pub(crate) struct ChronicleWriteGuard(Arc<ChronicleWritesInner>);

impl Drop for ChronicleWriteGuard {
    fn drop(&mut self) {
        let mut in_flight = lock(&self.0.in_flight);
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 {
            self.0.idle.notify_all();
        }
    }
}

/// Takes the count lock, recovering the guard if a previous holder panicked.
///
/// A poisoned lock here means some job thread unwound mid-write; the count
/// itself is a plain `usize` that the [`ChronicleWriteGuard`] keeps correct
/// across an unwind, so there is no torn state to protect and refusing to
/// take the lock would only turn a lost write into a hung shutdown.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chronicle_log_is_bounded() {
        let mut log = ChronicleLog::default();
        for i in 0..bounds::MAX_ENTRIES + 10 {
            log.push(format!("entry {i}"));
        }
        assert_eq!(log.len(), bounds::MAX_ENTRIES);
        // The oldest 10 entries ("entry 0".."entry 9") must have been
        // dropped; the log now starts at "entry 10".
        assert_eq!(log.iter().next(), Some("entry 10"));
    }

    #[test]
    fn push_then_iter_preserves_order() {
        let mut log = ChronicleLog::default();
        log.push("first");
        log.push("second");
        assert_eq!(log.iter().collect::<Vec<_>>(), vec!["first", "second"]);
    }

    /// A fresh database with every migration applied, exactly as a server
    /// would produce on first start. The `TempDir` is held so it isn't
    /// deleted before the test finishes with it.
    fn test_db() -> (tempfile::TempDir, DatabaseSettings) {
        let dir = tempfile::tempdir().expect("temp dir");
        let settings = DatabaseSettings {
            db_dir: dir.path().to_path_buf(),
            sql_log_mode: crate::persistence::SqlLogMode::Disabled,
        };
        persistence::run_migrations(&settings);
        (dir, settings)
    }

    #[test]
    fn load_reads_persisted_entries_oldest_first() {
        let (_dir, settings) = test_db();
        for text in ["first", "second", "third"] {
            persistence::append_chronicle_entry(text, chrono::Utc::now(), &settings)
                .expect("append");
        }

        let log = ChronicleLog::load(&settings);
        assert_eq!(log.iter().collect::<Vec<_>>(), vec![
            "first", "second", "third"
        ]);
    }

    #[test]
    fn load_of_an_untouched_database_is_empty() {
        let (_dir, settings) = test_db();
        assert!(ChronicleLog::load(&settings).is_empty());
    }

    /// The whole point of persisting this resource: entries pushed before a
    /// restart are still there after it. "Restart" here is the in-memory log
    /// being dropped and rebuilt from the same database, which is exactly
    /// what a real restart does to it.
    #[test]
    fn entries_survive_a_simulated_restart() {
        let (_dir, settings) = test_db();

        // Pre-restart: the live resource holds the entries in memory, and
        // each one is mirrored to the database the way the trigger call site
        // does.
        let mut before_restart = ChronicleLog::default();
        for text in ["a rumor", "another rumor", "a third rumor"] {
            before_restart.push(text);
            persistence::append_chronicle_entry(text, chrono::Utc::now(), &settings)
                .expect("append");
        }
        let before: Vec<String> = before_restart.iter().map(str::to_owned).collect();
        drop(before_restart);

        // Post-restart: a brand-new resource, warmed from the same database.
        let after_restart = ChronicleLog::load(&settings);
        let after: Vec<String> = after_restart.iter().map(str::to_owned).collect();

        assert_eq!(
            after, before,
            "the chronicle must come back identical, and still oldest-first, across a restart"
        );
    }

    #[test]
    fn waiting_on_nothing_returns_immediately() {
        let writes = ChronicleWrites::default();
        let started = Instant::now();
        assert!(writes.wait_until_idle(Duration::from_secs(30)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an idle tracker must not consume any of its timeout"
        );
    }

    #[test]
    fn waiting_blocks_until_an_in_flight_write_finishes() {
        let writes = ChronicleWrites::default();
        let guard = writes.begin();
        assert_eq!(writes.in_flight(), 1);

        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let released = Arc::clone(&released);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                released.store(true, std::sync::atomic::Ordering::SeqCst);
                drop(guard);
            })
        };

        assert!(
            writes.wait_until_idle(Duration::from_secs(30)),
            "the write finished well inside the timeout, so the wait must succeed"
        );
        assert!(
            released.load(std::sync::atomic::Ordering::SeqCst),
            "wait_until_idle returned before the write actually released its guard"
        );
        assert_eq!(writes.in_flight(), 0);
        worker.join().expect("worker thread");
    }

    #[test]
    fn waiting_gives_up_at_the_timeout_rather_than_hanging() {
        let writes = ChronicleWrites::default();
        // Never dropped inside the measured window: this stands in for a write
        // wedged on a stuck database.
        let guard = writes.begin();

        let started = Instant::now();
        assert!(
            !writes.wait_until_idle(Duration::from_millis(200)),
            "a still-pending write must be reported as not drained"
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(200),
            "returned before the timeout elapsed: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(5),
            "a stuck write must never hang shutdown past the timeout: {waited:?}"
        );

        drop(guard);
    }

    #[test]
    fn a_guard_decrements_even_when_its_thread_panics() {
        let writes = ChronicleWrites::default();
        let guard = writes.begin();

        let panicked = std::thread::spawn(move || {
            let _guard = guard;
            panic!("a chronicle write blew up mid-flight");
        })
        .join();
        assert!(panicked.is_err(), "the worker was supposed to panic");

        assert_eq!(
            writes.in_flight(),
            0,
            "an unwinding write must still release its slot, or shutdown waits forever"
        );
        assert!(writes.wait_until_idle(Duration::from_secs(1)));
    }

    /// The durable window mirrors the in-memory one: pushing past the cap
    /// must not leave the database holding entries the resource itself has
    /// already dropped.
    #[test]
    fn the_persisted_window_is_bounded_the_same_way_the_resource_is() {
        let (_dir, settings) = test_db();
        for i in 0..bounds::MAX_ENTRIES + 3 {
            persistence::append_chronicle_entry(
                &format!("entry {i}"),
                chrono::Utc::now(),
                &settings,
            )
            .expect("append");
        }

        let log = ChronicleLog::load(&settings);
        assert_eq!(log.len(), bounds::MAX_ENTRIES);
        assert_eq!(log.iter().next(), Some("entry 3"));
    }
}
