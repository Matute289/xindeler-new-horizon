//! Unattended shot scripts (development tool).
//!
//! A shot script is a RON file listing positions to visit and screenshots to
//! take, so terrain can be checked in the real renderer without anybody at the
//! keyboard. See `voxygen/shot-script/example.ron` for the format.
//!
//! This module is compiled only with the `shot-script` cargo feature, which is
//! in neither `default` nor `default-publish`. Even when compiled in, it does
//! nothing unless the [`SCRIPT_ENV`] environment variable names a script file.
//!
//! The schema, its validator, the named-screenshot capture, the unattended
//! login, the executor for every step kind and the `report.json` writer exist.
//!
//! Process exit codes: 0 every step succeeded; 1 a step failed, or the
//! session ended before the last step (the report's `abort_reason` tells
//! which); 2 the run could not start or progress (script, server, login or
//! world-entry error, including a phase deadline); 3 a screenshot could not
//! be written; 101 a panic. The shipped profiles abort on panic (no
//! unwinding, no `Drop`), so the 101 comes from the panic hook (see
//! [`on_panic`]), which also skips the crash dialog nobody could close.
//! Code 1 comes from [`driver::abort_code`] (and [`driver::finished_exit_code`]
//! for a failed step): the shot-script paths in `main` never return an `Err`
//! (which would also give 1), they exit through [`login::exit_code`].
//!
//! Screenshot files are named `NNN_<name>.png` (1-based, three digits; a script
//! may therefore hold at most 999 `Screenshot` steps). Files of that shape and
//! `report.json` are deleted from the output directory when a run starts.

pub mod capture;
pub mod driver;
pub mod login;
pub mod report;
pub mod schema;

pub use capture::{ShotError, ShotJob, ShotResult};
/// Panic hook part: with a shot script active, write the abort report
/// (`abort_reason: "panic"`, exit code 101) and end the process with 101
/// instead of showing a modal crash dialog. Returns when no script is active.
pub fn on_panic(reason: &str) {
    if login::active().is_none() {
        return;
    }
    // Never leave the hook unattended-hostile: report what is known, then go.
    eprintln!("error: shot script run panicked: {reason}");
    let _ = report::write_panic_report();
    std::process::exit(driver::PANIC_EXIT_CODE);
}

pub use schema::{Defaults, ScriptError, ShotScript, Step};

use std::path::PathBuf;

/// Environment variable naming the shot script to run.
pub const SCRIPT_ENV: &str = "XINDELER_SHOT_SCRIPT";

/// The script path from [`SCRIPT_ENV`], if set and non-empty.
pub fn script_path_from_env() -> Option<PathBuf> {
    std::env::var_os(SCRIPT_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Load and validate the script named by [`SCRIPT_ENV`]. `None` when the
/// variable is unset, i.e. the shot driver stays inert.
///
/// The script is read once; the second value is the SHA-256 (hex) of the
/// bytes that were parsed.
pub fn load_from_env() -> Option<Result<(ShotScript, String), ScriptError>> {
    script_path_from_env().map(|path| ShotScript::load_hashed(&path))
}

#[cfg(test)] mod driver_tests;
#[cfg(test)] mod login_tests;
#[cfg(test)] mod tests;
