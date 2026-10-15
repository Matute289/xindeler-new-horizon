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
//! Process exit codes: 0 every step succeeded; 1 the script ran to the end
//! but at least one step failed; 2 the run could not start (script, server or
//! login error); 3 a screenshot could not be written. A panic ends with 101
//! (Rust's default). Code 1 is produced only by [`driver::finished_exit_code`]
//! for a failed step: the shot-script paths in `main` never return an `Err`
//! (which would also give 1), they exit through [`login::exit_code`].
//!
//! Screenshot files are named `NNN_<name>.png` (1-based, three digits).

pub mod capture;
pub mod driver;
pub mod login;
pub mod report;
pub mod schema;

pub use capture::{ShotError, ShotJob, ShotResult};
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
pub fn load_from_env() -> Option<Result<ShotScript, ScriptError>> {
    script_path_from_env().map(|path| ShotScript::load(&path))
}

#[cfg(test)] mod driver_tests;
#[cfg(test)] mod login_tests;
#[cfg(test)] mod tests;
