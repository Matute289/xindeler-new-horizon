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
//! Currently the schema, its validator, the named-screenshot capture, the
//! unattended login and the executor for the time, teleport and waiting steps
//! exist; camera, minimap, sample and screenshot steps are not executed yet.

pub mod capture;
pub mod driver;
pub mod login;
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
