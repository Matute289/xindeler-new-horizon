//! `report.json`: the machine-readable result of a shot-script run.
//!
//! Written into the output directory when the run ends, and also when it is
//! aborted where the process can still write (login failure, the session
//! closing before the last step). The `exit_code` field always equals the
//! process exit code.
//!
//! Schema (version [`SCHEMA_VERSION`]); every field is always present, `null`
//! when unknown or not applicable:
//!
//! ```text
//! {
//!   "schema_version": 1,
//!   "script":  { "name": <file name only>, "sha256": <hex of the file> },
//!   "server":  "127.0.0.1:14004",            // loopback only
//!   "started_at": "<RFC 3339 UTC>", "ended_at": "<RFC 3339 UTC>",
//!   "duration_s": <f64>,
//!   "tool_version": <string|null>, "engine_commit": <hex|null>,
//!   "view_distance": { "script": <u32>, "client": <u32|null> },
//!   "window": { "requested": [w, h],          // logical pixels (the script)
//!               "actual": [w, h] | null,      // renderer resolution, physical
//!               "scale_factor": <f64|null>,   // physical / logical
//!               "mismatch": <string|null> },  // set when actual differs from
//!                                             // requested * scale_factor
//!   // Sizes the script asks for are logical pixels: on a HiDPI display a
//!   // 1920x1080 request gives 3840x2160 screenshots at scale factor 2.
//!   "steps_total", "steps_ok", "steps_failed", "steps_skipped",
//!   "steps_aborted": <u32>,                  // 0 or 1
//!   "exit_code": <i32>,
//!   "abort_reason": <string|null>,           // set when the run did not
//!                                            // reach the end of the script
//!   "steps": [ {
//!     "index": <0-based>, "kind": "Teleport", "name": <string|null>,
//!     "status": "ok" | "failed" | "skipped" | "aborted",
//!     "failure_reason": <string|null>,
//!     "started_s", "ended_s", "duration_s": <f64|null>, // run clock
//!     "screenshot": { "file", "written", "bytes", "sha256" } | null,
//!     "terrain": { "chunks_total", "chunks_loaded", "chunks_meshed",
//!                  "ready", "timed_out" } | null,
//!     "position_before": [x,y,z] | null, "position_after": [x,y,z] | null
//!   } ]
//! }
//! ```
//!
//! `name` is the screenshot name, time-of-day name or sample label. Step times
//! are seconds on the driver's clock, which starts when the player enters the
//! world (it is the only value that uses that injected clock; `started_at`,
//! `ended_at` and `duration_s` are wall-clock). `position_*` are filled for
//! the two teleport kinds only, and are `null` as a whole when any component
//! is not finite. A skipped step never ran; its times and details are `null`.
//! An aborted step was running when the run ended: it has `started_s` and a
//! `failure_reason` but no end time. At most one step is aborted, and only
//! when `abort_reason` is set. Only file names appear, never absolute paths.
//!
//! Consumers must check `abort_reason` first: when it is non-null the
//! script did not run to its end and the step list is partial. `exit_code`
//! is 0 all steps ok, 1 a step failed or the session ended early, 2 setup or
//! login error, 3 a screenshot could not be written, 101 a panic.
//! `engine_commit` is the truncated 32-bit git hash the build embeds, or
//! `null` when the build has none.
//!
//! Reserved / not produced yet (consumers must not wait on them): camera pose
//! per step, frame-time percentiles, pending mesh jobs, RSS (the runner
//! samples RSS from outside the process) and a distinct "drift" or "died"
//! failure kind (both currently appear as a `failed` step with a
//! `failure_reason` text).

use super::{
    driver::{StepOutcome, StepRecord, StepRunner, finished_exit_code},
    login,
    schema::Step,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub const SCHEMA_VERSION: u32 = 1;
/// File name of the report inside the output directory.
pub const REPORT_FILE: &str = "report.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub script: ScriptInfo,
    pub server: String,
    pub started_at: String,
    pub ended_at: String,
    pub duration_s: f64,
    pub tool_version: Option<String>,
    pub engine_commit: Option<String>,
    pub view_distance: ViewDistance,
    pub window: WindowReport,
    pub steps_total: u32,
    pub steps_ok: u32,
    pub steps_failed: u32,
    pub steps_skipped: u32,
    /// The step that was running when the run ended (0 or 1).
    pub steps_aborted: u32,
    pub exit_code: i32,
    pub abort_reason: Option<String>,
    pub steps: Vec<StepReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptInfo {
    pub name: Option<String>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewDistance {
    /// `defaults.view_distance` of the script.
    pub script: u32,
    /// What the client reported when the run ended.
    pub client: Option<u32>,
}

/// The window size the script asked for and what the renderer got.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowReport {
    /// `window` of the script, in logical pixels.
    pub requested: [u32; 2],
    /// The renderer's resolution in physical pixels when the run ended;
    /// equals `requested` times `scale_factor` when the request was honoured.
    pub actual: Option<[u32; 2]>,
    pub scale_factor: Option<f64>,
    /// A note when `actual` is not `requested * scale_factor` (the window
    /// manager gave another size); `null` when it matches or is unknown.
    #[serde(default)]
    pub mismatch: Option<String>,
}

/// Describe a difference between the requested window (logical pixels) and
/// the renderer's resolution (physical pixels), `None` when they agree to
/// within a pixel of rounding.
pub fn window_mismatch(
    requested: [u32; 2],
    actual: Option<[u32; 2]>,
    scale_factor: Option<f64>,
) -> Option<String> {
    let (actual, scale) = (actual?, scale_factor?);
    let expected = requested.map(|v| (f64::from(v) * scale).round());
    let ok = (0..2).all(|i| (f64::from(actual[i]) - expected[i]).abs() <= 1.0);
    (!ok).then(|| {
        format!(
            "renderer is {}x{} but {}x{} logical at scale factor {scale} would be {}x{}",
            actual[0], actual[1], requested[0], requested[1], expected[0], expected[1]
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Ok,
    Failed,
    /// Never ran.
    Skipped,
    /// Was running when the run ended.
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub index: u32,
    pub kind: String,
    pub name: Option<String>,
    pub status: StepStatus,
    pub failure_reason: Option<String>,
    pub started_s: Option<f64>,
    pub ended_s: Option<f64>,
    pub duration_s: Option<f64>,
    pub screenshot: Option<ScreenshotReport>,
    pub terrain: Option<TerrainReport>,
    pub position_before: Option<[f32; 3]>,
    pub position_after: Option<[f32; 3]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotReport {
    pub file: String,
    /// The file exists in the output directory.
    pub written: bool,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainReport {
    pub chunks_total: u32,
    pub chunks_loaded: u32,
    pub chunks_meshed: u32,
    pub ready: bool,
    pub timed_out: bool,
}

/// Facts about the run that the runner does not know.
#[derive(Clone, Debug)]
pub struct RunInfo {
    pub script_name: Option<String>,
    pub script_sha256: Option<String>,
    pub server: String,
    pub started_at: String,
    pub ended_at: String,
    pub duration_s: f64,
    pub tool_version: Option<String>,
    pub engine_commit: Option<String>,
    pub client_view_distance: Option<u32>,
    /// `window` of the script (logical pixels).
    pub window_requested: [u32; 2],
    /// Renderer resolution (physical pixels) and scale factor, last seen.
    pub window_actual: Option<([u32; 2], f64)>,
    /// Why the run ended before the script did, if it did.
    pub abort_reason: Option<String>,
    /// Exit code to record for an aborted run (ignored once the runner is
    /// finished: then [`finished_exit_code`] decides).
    pub abort_exit_code: i32,
}

/// What is known about a screenshot file: size and hash, `None` if missing.
pub type FileFacts = Option<(u64, String)>;

fn step_name(step: &Step) -> Option<String> {
    match step {
        Step::Time(n) => Some(n.clone()),
        Step::Sample { label } => Some(label.clone()),
        Step::Screenshot { name } => Some(name.clone()),
        _ => None,
    }
}

fn is_teleport(kind: &str) -> bool { matches!(kind, "Teleport" | "CromatolisTeleport") }

/// A position as an array; `None` as a whole if any component is not finite
/// (JSON has no NaN, it would otherwise become a `null` inside the array).
fn pos(p: vek::Vec3<f32>) -> Option<[f32; 3]> {
    [p.x, p.y, p.z]
        .iter()
        .all(|c| c.is_finite())
        .then_some([p.x, p.y, p.z])
}

fn count(n: usize) -> u32 { u32::try_from(n).unwrap_or(u32::MAX) }

fn step_report(
    record: &StepRecord,
    step: Option<&Step>,
    facts: &dyn Fn(&str) -> FileFacts,
) -> StepReport {
    let (status, failure_reason) = match &record.outcome {
        StepOutcome::Failed(reason) => (StepStatus::Failed, Some(reason.clone())),
        // A step is only recorded once it ended; `Waiting` cannot be stored.
        StepOutcome::Done | StepOutcome::Waiting => (StepStatus::Ok, None),
    };
    let teleport = is_teleport(record.kind);
    StepReport {
        index: count(record.index),
        kind: record.kind.to_owned(),
        name: step.and_then(step_name),
        status,
        failure_reason,
        started_s: Some(record.start_s),
        ended_s: Some(record.end_s),
        duration_s: Some((record.end_s - record.start_s).max(0.0)),
        screenshot: record.screenshot.as_deref().map(|file| {
            let facts = facts(file);
            ScreenshotReport {
                file: file.to_owned(),
                written: facts.is_some(),
                bytes: facts.as_ref().map(|f| f.0),
                sha256: facts.map(|f| f.1),
            }
        }),
        terrain: record.terrain.map(|(t, timed_out)| TerrainReport {
            chunks_total: count(t.total),
            chunks_loaded: count(t.loaded),
            chunks_meshed: count(t.meshed),
            ready: t.ready(),
            timed_out,
        }),
        position_before: record.pos_before.filter(|_| teleport).and_then(pos),
        position_after: record.pos_after.filter(|_| teleport).and_then(pos),
    }
}

/// Build the report of a run. `facts` looks up a screenshot file by name.
pub fn build_report(
    runner: &StepRunner,
    info: &RunInfo,
    facts: &dyn Fn(&str) -> FileFacts,
) -> Report {
    let steps_def = runner.steps();
    let mut steps: Vec<StepReport> = runner
        .records()
        .iter()
        .map(|r| step_report(r, steps_def.get(r.index), facts))
        .collect();
    let in_flight = runner.in_flight();
    for (index, step) in steps_def.iter().enumerate().skip(steps.len()) {
        let running = in_flight.filter(|(i, _)| *i == index).map(|(_, s)| s);
        steps.push(StepReport {
            index: count(index),
            kind: step.kind().to_owned(),
            name: step_name(step),
            status: if running.is_some() {
                StepStatus::Aborted
            } else {
                StepStatus::Skipped
            },
            failure_reason: running.map(|_| "the run ended while this step was running".to_owned()),
            started_s: running,
            ended_s: None,
            duration_s: None,
            screenshot: None,
            terrain: None,
            position_before: None,
            position_after: None,
        });
    }
    let of = |s: StepStatus| count(steps.iter().filter(|r| r.status == s).count());
    let skipped = of(StepStatus::Skipped);
    // The same code the process exits with: an unfinished run (aborted)
    // uses the code of the abort unless a failed screenshot or step already
    // decided it.
    let exit_code = finished_exit_code(runner).unwrap_or(if runner.is_finished() {
        0
    } else {
        info.abort_exit_code
    });
    Report {
        schema_version: SCHEMA_VERSION,
        script: ScriptInfo {
            name: info.script_name.clone(),
            sha256: info.script_sha256.clone(),
        },
        server: info.server.clone(),
        started_at: info.started_at.clone(),
        ended_at: info.ended_at.clone(),
        duration_s: info.duration_s,
        tool_version: info.tool_version.clone(),
        engine_commit: info.engine_commit.clone(),
        view_distance: ViewDistance {
            script: runner.defaults().view_distance,
            client: info.client_view_distance,
        },
        window: WindowReport {
            requested: info.window_requested,
            actual: info.window_actual.map(|(size, _)| size),
            scale_factor: info.window_actual.map(|(_, f)| f),
            mismatch: window_mismatch(
                info.window_requested,
                info.window_actual.map(|(size, _)| size),
                info.window_actual.map(|(_, f)| f),
            ),
        },
        steps_total: count(steps.len()),
        steps_ok: of(StepStatus::Ok),
        steps_failed: of(StepStatus::Failed),
        steps_skipped: skipped,
        steps_aborted: of(StepStatus::Aborted),
        exit_code,
        abort_reason: info.abort_reason.clone(),
        steps,
    }
}

/// Size and SHA-256 of `<out_dir>/<file>`, `None` if it can't be read.
/// Refuses names with a path separator so only files directly in the output
/// directory are ever read.
pub fn file_facts(out_dir: &Path, file: &str) -> FileFacts {
    if file.is_empty() || file.contains(['/', '\\']) || file == ".." {
        return None;
    }
    let path = out_dir.join(file);
    let before = std::fs::symlink_metadata(&path).ok()?;
    if !before.is_file() {
        return None;
    }
    let handle = std::fs::File::open(&path).ok()?;
    // The opened handle must be the plain file seen above (not a symlink
    // swapped in between the check and the open).
    let opened = handle.metadata().ok()?;
    if !opened.is_file() || opened.len() > MAX_FACTS_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
            return None;
        }
    }
    let mut bytes = Vec::new();
    handle
        .take(MAX_FACTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_FACTS_BYTES {
        return None;
    }
    Some((bytes.len() as u64, hex(&Sha256::digest(&bytes))))
}

/// Largest file `file_facts` reads (a screenshot is a few MiB).
const MAX_FACTS_BYTES: u64 = 256 * 1024 * 1024;

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

/// SHA-256 of `bytes`, as hex.
pub fn sha256_hex(bytes: &[u8]) -> String { hex(&Sha256::digest(bytes)) }

/// Write `report` as `<out_dir>/report.json` atomically: a temporary file in
/// the same directory (exclusive create, mode 0600) is renamed into place, so
/// a reader sees the old or the new file, never half of one. The directory is
/// re-validated with the same checks as at startup.
pub fn write_report(out_dir: &Path, report: &Report) -> std::io::Result<PathBuf> {
    let dir = login::prepare_out_dir(out_dir.as_os_str())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    json.push(b'\n');
    let target = dir.join(REPORT_FILE);
    let partial = dir.join(format!("{REPORT_FILE}.{}.partial", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&partial)?;
    let written = file
        .write_all(&json)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&partial, &target)
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    Ok(target)
}

/// When the run started (monotonic and wall clock), set once at startup.
static RUN_START: OnceLock<(Instant, chrono::DateTime<chrono::Utc>)> = OnceLock::new();

/// Remember the start of the run. The first call wins.
pub fn mark_run_start() { let _ = RUN_START.set((Instant::now(), chrono::Utc::now())); }

fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Hex of the truncated 32-bit git hash a build embeds; the all-zero hash
/// means the build had none, which is `None`.
pub fn engine_commit(hash: u32) -> Option<String> { (hash != 0).then(|| format!("{hash:08x}")) }

/// The run's facts for a report written now. `abort_reason` is `Some` when
/// the script did not run to its end.
pub fn run_info(
    setup: &login::ShotSetup,
    client_view_distance: Option<u32>,
    window_actual: Option<([u32; 2], f64)>,
    abort_reason: Option<String>,
    abort_exit_code: i32,
) -> RunInfo {
    let now = chrono::Utc::now();
    let (started, duration_s) = match RUN_START.get() {
        Some((instant, wall)) => (*wall, instant.elapsed().as_secs_f64()),
        None => (now, 0.0),
    };
    // The version statics can panic on a malformed build environment; while
    // unwinding that must not happen again (a panic inside `Drop`).
    let (tool_version, engine_commit) = if std::thread::panicking() {
        (None, None)
    } else {
        (
            Some(common::util::DISPLAY_VERSION.clone()),
            engine_commit(*common::util::GIT_HASH),
        )
    };
    RunInfo {
        script_name: setup.script_name.clone(),
        script_sha256: setup.script_sha256.clone(),
        server: setup.server.to_string(),
        started_at: rfc3339(started),
        ended_at: rfc3339(now),
        duration_s,
        tool_version,
        engine_commit,
        client_view_distance,
        window_requested: [setup.script.window.0, setup.script.window.1],
        window_actual,
        abort_reason,
        abort_exit_code,
    }
}

/// Write the report for a run that ended before any step ran (login or
/// connection failure): every step is `skipped`. Never panics; failures are
/// logged.
pub fn write_abort_report(reason: &str, exit_code: i32) {
    let Some(setup) = login::active() else { return };
    // A report that already exists (a run that was cut short writes its own,
    // with the steps that did run) is never replaced by an all-skipped one.
    if !claim_report() {
        return;
    }
    let runner = StepRunner::new(&setup.script);
    let info = run_info(setup, None, None, Some(reason.to_owned()), exit_code);
    let report = build_report(&runner, &info, &|_| None);
    log_write(&setup.out_dir, &report);
}

/// Write `report`, logging instead of failing.
pub fn log_write(out_dir: &Path, report: &Report) {
    match write_report(out_dir, report) {
        Ok(path) => tracing::info!(path = %path.display(), "Shot report written"),
        Err(e) => tracing::error!(?e, "Couldn't write the shot report"),
    }
}

/// Set once a report was written (or is being written) for this run; the
/// first writer wins, so a later abort path can neither replace a report nor
/// make it disagree with the process exit code.
static REPORT_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Claim the right to write the run's report. `true` for the first caller
/// only.
pub fn claim_report() -> bool { claim(&REPORT_CLAIMED) }

fn claim(flag: &AtomicBool) -> bool { !flag.swap(true, Ordering::SeqCst) }

#[cfg(test)]
pub(crate) fn claim_for_test(flag: &AtomicBool) -> bool { claim(flag) }

/// `abort_reason` recorded for a panic.
pub const PANIC_REASON: &str = "panic";

/// The report as of the last finished step, kept so the panic hook (which
/// cannot reach the session driver) can still write the steps that ran.
static SNAPSHOT: Mutex<Option<Report>> = Mutex::new(None);

/// Keep `report` for [`write_panic_report`].
pub fn set_snapshot(report: Report) {
    *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
}

/// Turn a report into the one a panic ends with.
pub fn finalize_panic(report: &mut Report, ended_at: String, duration_s: f64) {
    report.abort_reason = Some(PANIC_REASON.to_owned());
    report.exit_code = super::driver::PANIC_EXIT_CODE;
    report.ended_at = ended_at;
    report.duration_s = duration_s;
}

/// Write the report of a run that ended in a panic, from the panic hook.
/// Uses the last snapshot, or an all-skipped report when there is none. Writes
/// even if a report exists: the process is about to exit with code 101, and
/// the report must say so. Returns whether a report was written. Best effort:
/// it avoids panicking itself and a poisoned snapshot lock still yields the
/// steps, but `catch_unwind` is a no-op under `panic=abort` (the shipped
/// profiles), so a panic in here would abort rather than be caught.
pub fn write_panic_report() -> bool {
    std::panic::catch_unwind(|| {
        let setup = login::active()?;
        let info = run_info(setup, None, None, Some(PANIC_REASON.to_owned()), 101);
        let snapshot = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut report = snapshot
            .unwrap_or_else(|| build_report(&StepRunner::new(&setup.script), &info, &|_| None));
        finalize_panic(&mut report, info.ended_at.clone(), info.duration_s);
        REPORT_CLAIMED.store(true, Ordering::SeqCst);
        Some(write_report(&setup.out_dir, &report).is_ok())
    })
    .ok()
    .flatten()
    .unwrap_or(false)
}
