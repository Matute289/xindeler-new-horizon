//! Unattended login for shot scripts.
//!
//! When a shot script is active the client skips the main menu, connects to a
//! throwaway local server without authentication, creates (if missing) and
//! selects a dedicated character, and enters the world. Nothing here reads or
//! stores real account credentials: the username is fixed, the password is
//! empty, and a server that asks for an auth server is refused.
//!
//! Environment:
//! - [`SERVER_ENV`]: `host:port` of the scratch server. Only loopback addresses
//!   are accepted (`127.0.0.0/8`, `[::1]`, `localhost`); hostnames are never
//!   resolved, so no DNS request ever leaves the machine. Loopback does not
//!   guarantee the server is local (an SSH tunnel to 127.0.0.1 passes); that is
//!   accepted for a development-only tool whose operator sets up the
//!   environment.
//! - [`OUT_ENV`]: absolute output directory for screenshots and the report.
//!   Created owner-only if missing; refused if it is a symlink, not a
//!   directory, not owned by this user or world-writable. Files from an earlier
//!   run (`NNN_*.png`, `report.json`) are deleted at startup.
//! - [`CONNECT_TIMEOUT_ENV`]: optional overall connect deadline in seconds
//!   (default [`DEFAULT_CONNECT_TIMEOUT_S`]), so a scratch server that is not
//!   running fails the run quickly instead of after the menu's long retries.
//!
//! Phase deadlines keep an unattended run from hanging: the connect deadline
//! above, [`CHAR_SELECT_TIMEOUT_S`] for character selection and
//! [`WORLD_ENTRY_TIMEOUT_S`] for the player to appear in the world (each
//! raised to the connect deadline if that is larger). Missing one ends the run
//! with [`SETUP_EXIT_CODE`] and an abort report.
//!
//! A shot run never writes the user's settings or profile at exit.
//!
//! Setup or login failures end the run with [`SETUP_EXIT_CODE`]: at startup
//! directly, later through a normal shutdown (see [`exit_code`]) so the logs
//! are flushed.

use super::{ScriptError, ShotScript};
use client::Client;
use common::character::CharacterId;
use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Write as _},
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tracing::{error, info};

/// Environment variable with the scratch server address (`host:port`).
pub const SERVER_ENV: &str = "XINDELER_SHOT_SERVER";
/// Environment variable with the absolute output directory.
pub const OUT_ENV: &str = "XINDELER_SHOT_OUT";
/// Environment variable with the connect deadline in whole seconds.
pub const CONNECT_TIMEOUT_ENV: &str = "XINDELER_SHOT_CONNECT_TIMEOUT_S";
/// Connect deadline when [`CONNECT_TIMEOUT_ENV`] is unset.
pub const DEFAULT_CONNECT_TIMEOUT_S: u64 = 30;
/// How long character selection may take, from its first frame to entering
/// the world, in seconds (never less than the connect deadline).
pub const CHAR_SELECT_TIMEOUT_S: u64 = 60;
/// How long the player may take to appear in the world once the session
/// started, in seconds (never less than the connect deadline).
pub const WORLD_ENTRY_TIMEOUT_S: u64 = 120;
/// Largest accepted connect deadline.
pub const MAX_CONNECT_TIMEOUT_S: u64 = 600;
/// Username sent to the scratch server (which runs without authentication).
pub const USERNAME: &str = "t13shot";
/// Alias of the dedicated character, created if missing (matched ignoring
/// ASCII case, in case a server normalises aliases).
pub const CHARACTER_ALIAS: &str = "t13shot";
/// Process exit code for script, setup or login errors.
pub const SETUP_EXIT_CODE: i32 = 2;
/// Process exit code when a step failed (the run continued and captured the
/// rest) or the session ended before the last step (see the report's
/// `abort_reason`).
pub const STEP_FAILED_EXIT_CODE: i32 = 1;
/// Process exit code when a screenshot could not be written.
pub const SCREENSHOT_FAILED_EXIT_CODE: i32 = 3;

/// Everything the driver needs, validated once at startup.
#[derive(Debug)]
pub struct ShotSetup {
    pub script: ShotScript,
    pub server: SocketAddr,
    pub out_dir: PathBuf,
    pub connect_timeout: Duration,
    /// File name (no directory) of the script, for the report.
    pub script_name: Option<String>,
    /// SHA-256 of the script file, for the report.
    pub script_sha256: Option<String>,
}

#[derive(Debug)]
pub enum SetupError {
    Script(ScriptError),
    MissingEnv(&'static str),
    NotUnicode(&'static str),
    BadServer { value: String, reason: &'static str },
    NotLoopback { value: String },
    BadTimeout { value: String },
    OutDir { path: PathBuf, reason: String },
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::Script(e) => write!(f, "shot script: {e}"),
            SetupError::MissingEnv(var) => write!(f, "{var} must be set when a shot script runs"),
            SetupError::NotUnicode(var) => write!(f, "{var} is not valid UTF-8"),
            SetupError::BadServer { value, reason } => {
                write!(f, "{SERVER_ENV}={value:?}: {reason}")
            },
            SetupError::NotLoopback { value } => write!(
                f,
                "{SERVER_ENV}={value:?} is not a loopback address; shot scripts only connect to a \
                 local scratch server (127.0.0.0/8, [::1] or localhost)"
            ),
            SetupError::BadTimeout { value } => write!(
                f,
                "{CONNECT_TIMEOUT_ENV}={value:?}: expected whole seconds from 1 to \
                 {MAX_CONNECT_TIMEOUT_S}"
            ),
            SetupError::OutDir { path, reason } => {
                write!(f, "{OUT_ENV}={}: {reason}", path.display())
            },
        }
    }
}

impl std::error::Error for SetupError {}

/// Parse `host:port` and accept it only if it is a loopback address.
///
/// IP literals are checked directly (no IPv6 zone ids); the only accepted
/// hostname is `localhost` (any case), which maps to `127.0.0.1` without a
/// lookup. Every other hostname, including numeric shorthands such as
/// `127.1`, is refused unresolved.
pub fn parse_loopback_server(value: &str) -> Result<SocketAddr, SetupError> {
    let bad = |reason| SetupError::BadServer {
        value: value.to_owned(),
        reason,
    };
    if value.is_empty() || value.trim() != value {
        return Err(bad("expected host:port without surrounding whitespace"));
    }
    let addr = if let Ok(addr) = value.parse::<SocketAddr>() {
        let loopback = match addr {
            SocketAddr::V4(a) => a.ip().is_loopback(),
            SocketAddr::V6(a) => {
                a.scope_id() == 0
                    && (a.ip().is_loopback()
                        || a.ip().to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()))
            },
        };
        if !loopback {
            return Err(SetupError::NotLoopback {
                value: value.to_owned(),
            });
        }
        addr
    } else {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| bad("expected host:port"))?;
        if host.contains(':') {
            return Err(bad("IPv6 addresses need brackets, e.g. [::1]:14004"));
        }
        let port: u16 = port
            .parse()
            .map_err(|_| bad("port must be a number from 1 to 65535"))?;
        if host.is_empty() {
            return Err(bad("expected host:port"));
        }
        if !host.eq_ignore_ascii_case("localhost") {
            return Err(SetupError::NotLoopback {
                value: value.to_owned(),
            });
        }
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    };
    if addr.port() == 0 {
        return Err(bad("port must be a number from 1 to 65535"));
    }
    Ok(addr)
}

/// A phase deadline: the default, raised to the connect deadline when the
/// operator set that higher (a slow machine needs both).
fn phase_limit(default_s: u64, connect_timeout: Duration) -> Duration {
    Duration::from_secs(default_s).max(connect_timeout)
}

/// Deadline of character selection.
pub fn char_select_limit(connect_timeout: Duration) -> Duration {
    phase_limit(CHAR_SELECT_TIMEOUT_S, connect_timeout)
}

/// Deadline for the player to appear in the world.
pub fn world_entry_limit(connect_timeout: Duration) -> Duration {
    phase_limit(WORLD_ENTRY_TIMEOUT_S, connect_timeout)
}

/// Has `limit` passed since `started`? (`now` is injected for tests.)
pub fn phase_expired(started: Instant, now: Instant, limit: Duration) -> bool {
    now.saturating_duration_since(started) > limit
}

/// Parse the optional connect deadline.
pub fn parse_connect_timeout(value: Option<&OsStr>) -> Result<Duration, SetupError> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_S));
    };
    let bad = || SetupError::BadTimeout {
        value: value.to_string_lossy().into_owned(),
    };
    let secs: u64 = value.to_str().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    if !(1..=MAX_CONNECT_TIMEOUT_S).contains(&secs) {
        return Err(bad());
    }
    Ok(Duration::from_secs(secs))
}

/// Problem with a directory's owner or mode, if any: it must belong to `me`
/// and must not be writable by others. Group-writable is accepted (a shared
/// group on a developer machine is common, e.g. macOS's `staff`); only the
/// world-writable case lets an unrelated user plant or replace files.
pub fn owner_mode_problem(dir_uid: u32, mode: u32, me: u32) -> Option<&'static str> {
    if dir_uid != me {
        Some("is not owned by the current user")
    } else if mode & 0o002 != 0 {
        Some("is writable by others")
    } else {
        None
    }
}

/// Validate the output directory, creating it (owner-only) if missing.
///
/// The path must be absolute, without `..` components, and its last component
/// must not be a symlink. Parent directories may be symlinks: on macOS the
/// system temp directories themselves live behind one. On unix the directory
/// must be owned by this process's user and not world-writable.
pub fn prepare_out_dir(raw: &OsStr) -> Result<PathBuf, SetupError> {
    let path = PathBuf::from(raw);
    let refuse = |reason: String| SetupError::OutDir {
        path: path.clone(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refuse("must be an absolute path".into()));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(refuse("must not contain '..'".into()));
    }
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {},
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            create_private_dir(&path).map_err(|e| refuse(format!("couldn't create it: {e}")))?;
        },
        Err(e) => return Err(refuse(format!("couldn't inspect it: {e}"))),
    }
    // Checked again after creation so a symlink swapped in meanwhile is
    // still refused.
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| refuse(format!("couldn't inspect it: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(refuse("is a symlink".into()));
    }
    if !meta.is_dir() {
        return Err(refuse("exists but is not a directory".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let me = current_uid(&path).map_err(|e| refuse(format!("isn't writable: {e}")))?;
        if let Some(problem) = owner_mode_problem(meta.uid(), meta.mode(), me) {
            return Err(refuse(problem.into()));
        }
    }
    #[cfg(not(unix))]
    if meta.permissions().readonly() {
        return Err(refuse("is read-only".into()));
    }
    Ok(path)
}

/// This process's effective uid, read from a probe file created (and
/// removed) inside `dir`, so no platform API or `unsafe` is needed. Also
/// proves the directory is writable.
#[cfg(unix)]
fn current_uid(dir: &Path) -> io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    let probe = dir.join(format!(".shot-owner-probe-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let uid = file.metadata().map(|m| m.uid());
    drop(file);
    let removed = std::fs::remove_file(&probe);
    let uid = uid?;
    removed?;
    Ok(uid)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Is `name` an output of an earlier run: a numbered screenshot
/// (`NNN_<name>.png`, at least three digits) or the report (or a leftover
/// temporary report file)?
pub fn is_run_output(name: &str) -> bool {
    if name == super::report::REPORT_FILE
        || (name.starts_with("report.json.") && name.ends_with(".partial"))
    {
        return true;
    }
    let digits = name.bytes().take_while(u8::is_ascii_digit).count();
    digits >= 3 && name[digits..].starts_with('_') && name.ends_with(".png")
}

/// The screenshot files a previous `report.json` names (bare file names).
fn files_named_by_report(report: &[u8]) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(report).ok()?;
    let steps = value.get("steps")?.as_array()?;
    Some(
        steps
            .iter()
            .filter_map(|s| s.get("screenshot")?.get("file")?.as_str())
            .map(str::to_owned)
            .collect(),
    )
}

/// Delete the outputs of an earlier run from `dir`, so a report can never list
/// a stale file as written. Only regular files directly inside it (never
/// symlinks) that the previous `report.json` names, plus that report and its
/// leftover temporary files, are removed. A non-empty directory without a
/// readable `report.json` is refused: it is not known to be a shot output
/// directory, and nothing in it is deleted. Returns how many files were
/// removed.
pub fn clean_run_outputs(dir: &Path) -> io::Result<usize> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        if let Some(name) = entry?.file_name().to_str() {
            names.push(name.to_owned());
        } else {
            names.push(String::from("<non-unicode name>"));
        }
    }
    if names.is_empty() {
        return Ok(0);
    }
    let named = std::fs::read(dir.join(super::report::REPORT_FILE))
        .ok()
        .and_then(|bytes| files_named_by_report(&bytes));
    let Some(named) = named else {
        return Err(io::Error::other(format!(
            "{} is not empty and has no readable {}: refusing to delete anything; use an empty \
             directory or remove the old files yourself",
            dir.display(),
            super::report::REPORT_FILE
        )));
    };
    let mut removed = 0;
    for name in names {
        let ours = name == super::report::REPORT_FILE
            || (name.starts_with("report.json.") && name.ends_with(".partial"))
            || (is_run_output(&name) && named.contains(&name));
        if !ours {
            continue;
        }
        let path = dir.join(&name);
        if std::fs::symlink_metadata(&path)?.file_type().is_file() {
            std::fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Build the setup from a loaded script and an environment lookup.
pub fn setup_with(
    script: Result<ShotScript, ScriptError>,
    var: impl Fn(&str) -> Option<OsString>,
) -> Result<ShotSetup, SetupError> {
    let script = script.map_err(SetupError::Script)?;
    let server = var(SERVER_ENV)
        .filter(|v| !v.is_empty())
        .ok_or(SetupError::MissingEnv(SERVER_ENV))?;
    let server = server.to_str().ok_or(SetupError::NotUnicode(SERVER_ENV))?;
    let server = parse_loopback_server(server)?;
    let connect_timeout = parse_connect_timeout(var(CONNECT_TIMEOUT_ENV).as_deref())?;
    let out = var(OUT_ENV)
        .filter(|v| !v.is_empty())
        .ok_or(SetupError::MissingEnv(OUT_ENV))?;
    let out_dir = prepare_out_dir(&out)?;
    Ok(ShotSetup {
        script,
        server,
        out_dir,
        connect_timeout,
        script_name: None,
        script_sha256: None,
    })
}

static ACTIVE: OnceLock<ShotSetup> = OnceLock::new();
static EXIT_CODE: OnceLock<i32> = OnceLock::new();
/// When the login was started; `None` until the first attempt.
static LOGIN_STARTED: Mutex<Option<Instant>> = Mutex::new(None);

fn report(msg: &str) {
    error!("{msg}");
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "error: {msg}");
    let _ = stderr.flush();
}

/// Read and validate the shot environment once, at startup.
///
/// `Ok(())` when no script is set (the driver stays inert) or the setup is
/// valid; on error it is logged and the exit code to use is returned.
pub fn init_from_env() -> Result<(), i32> {
    let Some(script) = super::load_from_env() else {
        return Ok(());
    };
    super::report::mark_run_start();
    let (script, sha256) = match script {
        Ok((script, sha)) => (Ok(script), Some(sha)),
        Err(e) => (Err(e), None),
    };
    match setup_with(script, |k| std::env::var_os(k)) {
        Ok(mut setup) => {
            if let Some(path) = super::script_path_from_env() {
                setup.script_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                setup.script_sha256 = sha256;
            }
            info!(
                server = %setup.server,
                out_dir = %setup.out_dir.display(),
                connect_timeout_s = setup.connect_timeout.as_secs(),
                steps = setup.script.steps.len(),
                "Shot script active: unattended login to the scratch server"
            );
            match clean_run_outputs(&setup.out_dir) {
                Ok(0) => {},
                Ok(n) => info!(removed = n, "Removed the outputs of an earlier shot run"),
                Err(e) => {
                    report(&format!(
                        "couldn't clear the output directory of earlier results: {e}"
                    ));
                    return Err(SETUP_EXIT_CODE);
                },
            }
            // A second call would be a programming error; keep the first.
            let _ = ACTIVE.set(setup);
            Ok(())
        },
        Err(e) => {
            report(&e.to_string());
            Err(SETUP_EXIT_CODE)
        },
    }
}

/// The validated setup, if a shot script is running.
pub fn active() -> Option<&'static ShotSetup> { ACTIVE.get() }

/// Exit code the process must end with, once the run was aborted. `main`
/// checks it after the normal shutdown, when the log writers can be flushed.
pub fn exit_code() -> Option<i32> { EXIT_CODE.get().copied() }

/// Set the exit code the process ends with (see [`exit_code`]). The first
/// code set wins, so an earlier, more specific failure is kept.
pub fn set_exit_code(code: i32) { let _ = EXIT_CODE.set(code); }

/// Report a login failure and request shutdown with [`SETUP_EXIT_CODE`].
/// The caller returns `PlayStateResult::Shutdown`.
fn fail(msg: &str) {
    report(&format!("shot script login failed: {msg}"));
    let _ = EXIT_CODE.set(SETUP_EXIT_CODE);
    super::report::write_abort_report(msg, SETUP_EXIT_CODE);
}

/// The user's own `minimap_show` value, remembered before the script first
/// changed it, so that a run never leaves the minimap switched in the real
/// settings (they are saved on exit).
static MINIMAP_ORIGINAL: Mutex<Option<bool>> = Mutex::new(None);

/// Remember `current` as the value to restore, unless one is already kept.
pub fn remember_minimap(current: bool) {
    if let Ok(mut g) = MINIMAP_ORIGINAL.lock() {
        g.get_or_insert(current);
    }
}

/// The user's own `intro_show`, remembered before `suppress_modals` changed it.
static INTRO_ORIGINAL: Mutex<Option<crate::hud::Intro>> = Mutex::new(None);

/// Put back what a script changed in the settings, as defence in depth: a
/// shot run does not save the settings at all (see [`persist_on_exit`]), so
/// this only matters if something else saves them.
pub fn restore_settings(settings: &mut crate::settings::Settings) {
    restore_minimap(&mut settings.interface.minimap_show);
    if let Ok(mut g) = INTRO_ORIGINAL.lock()
        && let Some(original) = g.take()
    {
        settings.interface.intro_show = original;
    }
}

/// Whether the settings and profile are written back when the client exits.
/// A shot run changes settings in memory only (window size, minimap, intro
/// modal, view distance), so it must not write the user's real files at all.
pub fn persist_on_exit() -> bool { persist_decision(active().is_some()) }

fn persist_decision(shot_script_active: bool) -> bool { !shot_script_active }

#[cfg(test)]
pub(crate) fn persist_decision_for_test(shot_script_active: bool) -> bool {
    persist_decision(shot_script_active)
}

pub(crate) fn restore_minimap(minimap_show: &mut bool) {
    if let Ok(mut g) = MINIMAP_ORIGINAL.lock()
        && let Some(original) = g.take()
    {
        *minimap_show = original;
    }
}

/// Turn off settings that would open a modal nobody is there to close (the
/// first-session intro), and apply the script's window size. In memory only:
/// the settings file is not written during a shot run. Call it through
/// [`prepare_settings`], which fixes the order against the first-run setup.
pub fn suppress_modals(settings: &mut crate::settings::Settings) {
    suppress_modals_with(active(), settings);
}

pub(crate) fn suppress_modals_with(
    setup: Option<&ShotSetup>,
    settings: &mut crate::settings::Settings,
) {
    if let Some(setup) = setup {
        if let Ok(mut g) = INTRO_ORIGINAL.lock() {
            g.get_or_insert(settings.interface.intro_show);
        }
        settings.interface.intro_show = crate::hud::Intro::Never;
        // The window is created from these settings. In memory only: with a
        // shot script active nothing is saved (see `persist_on_exit`).
        let (w, h) = setup.script.window;
        settings.graphics.window.size = [w, h];
        settings.graphics.window.maximised = false;
        settings.graphics.fullscreen.enabled = false;
    }
}

/// Startup settings preparation: the first-run graphics preset first (saved to
/// disk through `save`), then the shot script's in-memory overrides.
///
/// The order is load-bearing. Applying the overrides first would let the
/// first-run preset overwrite the script's window size, and the first-run
/// save would write the overrides (such as `intro_show = Never`) into the
/// user's settings file. `detect` is only called on a first run.
pub fn prepare_settings(
    settings: &mut crate::settings::Settings,
    is_first_run: bool,
    detect: impl FnOnce() -> Option<crate::settings::GraphicsSettings>,
    save: impl FnOnce(&crate::settings::Settings),
) {
    prepare_settings_with(active(), settings, is_first_run, detect, save);
}

pub(crate) fn prepare_settings_with(
    setup: Option<&ShotSetup>,
    settings: &mut crate::settings::Settings,
    is_first_run: bool,
    detect: impl FnOnce() -> Option<crate::settings::GraphicsSettings>,
    save: impl FnOnce(&crate::settings::Settings),
) {
    if is_first_run && let Some(graphics) = detect() {
        settings.graphics = graphics;
        save(settings);
    }
    suppress_modals_with(setup, settings);
}

/// What the main menu should do this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Wait,
    Connect(SocketAddr),
    Abort(String),
}

/// Main-menu decision. `idle`: no connection in progress; `connecting`: the
/// client is still connecting (not yet creating render pipelines).
pub fn menu_decide(
    started: Option<Instant>,
    now: Instant,
    idle: bool,
    connecting: bool,
    server: SocketAddr,
    timeout: Duration,
    info_message: Option<&str>,
) -> MenuAction {
    match started {
        None if idle => MenuAction::Connect(server),
        // Back to idle after an attempt: the connection or login failed.
        Some(_) if idle => MenuAction::Abort(
            info_message
                .unwrap_or("returned to the main menu (see the log)")
                .to_owned(),
        ),
        Some(t) if connecting && now.saturating_duration_since(t) > timeout => {
            MenuAction::Abort(format!(
                "couldn't connect to {server} within {} s; is the scratch server running?",
                timeout.as_secs()
            ))
        },
        _ => MenuAction::Wait,
    }
}

/// Main-menu tick. Returns the server to connect to (once), `Some(Err(()))`
/// when the run must shut down, `None` otherwise.
pub fn menu_tick(
    idle: bool,
    connecting: bool,
    info_message: Option<&str>,
) -> Option<Result<SocketAddr, ()>> {
    let setup = active()?;
    let mut started = LOGIN_STARTED.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match menu_decide(
        *started,
        now,
        idle,
        connecting,
        setup.server,
        setup.connect_timeout,
        info_message,
    ) {
        MenuAction::Wait => None,
        MenuAction::Connect(server) => {
            *started = Some(now);
            info!(%server, "Shot script: connecting");
            Some(Ok(server))
        },
        MenuAction::Abort(msg) => {
            fail(&msg);
            Some(Err(()))
        },
    }
}

/// The server asked to authenticate: a scratch server never does, so this is
/// not one. Returns `true` (shut down) before any auth server is contacted.
pub fn refuse_auth() -> bool {
    if active().is_some() {
        fail("the server requires authentication; expected a scratch server without auth");
        return true;
    }
    false
}

/// Character selection reported an error (creation or entering the world).
/// Returns `true` when the run must shut down.
pub fn character_error(error: &str) -> bool {
    if active().is_some() {
        fail(&format!("character selection: {error}"));
        return true;
    }
    false
}

/// What character selection should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharAction {
    Wait,
    Create,
    Play(CharacterId),
    Fail(String),
}

/// Character-selection half of the auto-login: create the dedicated
/// character if missing, then enter the world with it.
#[derive(Debug, Default)]
pub struct CharSelectDriver {
    create_requested: bool,
    play_requested: bool,
    /// First frame of character selection, for the deadline.
    started: Option<Instant>,
}

impl CharSelectDriver {
    /// Decide from the character list: `(alias, id, suspended)` per entry.
    pub fn decide<'a>(
        &mut self,
        loading: bool,
        characters: impl IntoIterator<Item = (&'a str, Option<CharacterId>, bool)>,
    ) -> CharAction {
        if self.play_requested || loading {
            return CharAction::Wait;
        }
        let ours = characters
            .into_iter()
            .filter(|(alias, id, _)| alias.eq_ignore_ascii_case(CHARACTER_ALIAS) && id.is_some())
            .min_by_key(|(_, id, _)| id.map(|id| id.0));
        match ours {
            Some((_, _, true)) => CharAction::Fail(format!(
                "the character {CHARACTER_ALIAS:?} is suspended on this server"
            )),
            Some((_, Some(id), false)) => {
                self.play_requested = true;
                CharAction::Play(id)
            },
            // Creation was requested: wait until the list shows it (errors
            // arrive separately through `character_error`).
            _ if self.create_requested => CharAction::Wait,
            _ => {
                self.create_requested = true;
                CharAction::Create
            },
        }
    }

    /// Has character selection taken longer than `limit`? The clock starts at
    /// the first call. (`now` is injected for tests.)
    pub fn expired(&mut self, now: Instant, limit: Duration) -> bool {
        let started = *self.started.get_or_insert(now);
        phase_expired(started, now, limit)
    }

    /// Run one step against the client. No-op when no shot script is active.
    /// Returns `true` when the run must shut down.
    pub fn tick(&mut self, client: &mut Client, settings_distances: common::ViewDistances) -> bool {
        let Some(setup) = active() else {
            return false;
        };
        let limit = char_select_limit(setup.connect_timeout);
        if self.expired(Instant::now(), limit) {
            fail(&format!(
                "character selection did not finish within {} s",
                limit.as_secs()
            ));
            return true;
        }
        if !client.registered() {
            return false;
        }
        let view_distances =
            script_view_distances(setup.script.defaults.view_distance, settings_distances);
        let list = client.character_list();
        let action = self.decide(
            list.loading,
            list.characters.iter().map(|c| {
                (
                    c.character.alias.as_str(),
                    c.character.id,
                    c.suspended.is_some(),
                )
            }),
        );
        match action {
            CharAction::Wait => {},
            CharAction::Fail(msg) => {
                fail(&msg);
                return true;
            },
            CharAction::Create => {
                info!("Shot script: creating the character {CHARACTER_ALIAS:?}");
                let site = client.possible_starting_sites().first().copied();
                client.create_character(
                    CHARACTER_ALIAS.to_owned(),
                    Some("common.items.weapons.sword.starter".into()),
                    None,
                    default_body().into(),
                    false,
                    site,
                    common::comp::class::ClassKind::Warrior,
                    common::comp::Ethos::default(),
                    common::comp::Background::default(),
                );
                client.load_character_list();
            },
            CharAction::Play(id) => {
                info!(?id, "Shot script: entering the world");
                client.request_character(id, view_distances);
            },
        }
        false
    }
}

/// The view distances a script run enters the world with: the script's
/// `defaults.view_distance` for terrain, instead of whatever the (possibly
/// auto-detected, low-preset) graphics settings of the machine say. The entity
/// distance is capped by the settings value so a script never raises entity
/// streaming cost. The server and the client still clamp to their own maximum.
pub(crate) fn script_view_distances(
    script_view_distance: u32,
    settings: common::ViewDistances,
) -> common::ViewDistances {
    common::ViewDistances {
        terrain: script_view_distance,
        entity: script_view_distance.min(settings.entity),
    }
}

fn default_body() -> common::comp::humanoid::Body {
    use common::comp::humanoid::{Body, BodyType, Species};
    Body {
        species: Species::Human,
        body_type: BodyType::Male,
        hair_style: 0,
        beard: 0,
        eyes: 0,
        accessory: 0,
        hair_color: 0,
        skin: 0,
        eye_color: 0,
        height_scale: 0,
    }
}
