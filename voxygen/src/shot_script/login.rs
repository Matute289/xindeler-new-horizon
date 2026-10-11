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
//!   resolved, so no DNS request ever leaves the machine.
//! - [`OUT_ENV`]: absolute output directory for screenshots and the report.
//!   Created owner-only if missing; refused if it is a symlink or not a
//!   directory.
//!
//! Setup or login failures end the process with [`SETUP_EXIT_CODE`].

use super::{ScriptError, ShotScript};
use client::Client;
use common::character::CharacterId;
use std::{
    ffi::{OsStr, OsString},
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tracing::{error, info};

/// Environment variable with the scratch server address (`host:port`).
pub const SERVER_ENV: &str = "XINDELER_SHOT_SERVER";
/// Environment variable with the absolute output directory.
pub const OUT_ENV: &str = "XINDELER_SHOT_OUT";
/// Username sent to the scratch server (which runs without authentication).
pub const USERNAME: &str = "t13shot";
/// Alias of the dedicated character, created if missing.
pub const CHARACTER_ALIAS: &str = "t13shot";
/// Process exit code for script, setup or login errors.
pub const SETUP_EXIT_CODE: i32 = 2;

/// Everything the driver needs, validated once at startup.
#[derive(Debug)]
pub struct ShotSetup {
    pub script: ShotScript,
    pub server: SocketAddr,
    pub out_dir: PathBuf,
}

#[derive(Debug)]
pub enum SetupError {
    Script(ScriptError),
    MissingEnv(&'static str),
    NotUnicode(&'static str),
    BadServer { value: String, reason: &'static str },
    NotLoopback { value: String },
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
            SetupError::OutDir { path, reason } => {
                write!(f, "{OUT_ENV}={}: {reason}", path.display())
            },
        }
    }
}

impl std::error::Error for SetupError {}

/// Parse `host:port` and accept it only if it is a loopback address.
///
/// IP literals are checked directly; the only accepted hostname is
/// `localhost` (any case), which maps to `127.0.0.1` without a lookup. Every
/// other hostname is refused unresolved.
pub fn parse_loopback_server(value: &str) -> Result<SocketAddr, SetupError> {
    let bad = |reason| SetupError::BadServer {
        value: value.to_owned(),
        reason,
    };
    if value.is_empty() || value.trim() != value {
        return Err(bad("expected host:port without surrounding whitespace"));
    }
    let addr = if let Ok(addr) = value.parse::<SocketAddr>() {
        let loopback = match addr.ip() {
            IpAddr::V4(ip) => ip.is_loopback(),
            IpAddr::V6(ip) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
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

/// Validate the output directory, creating it (owner-only) if missing.
///
/// The path must be absolute, without `..` components, and its last component
/// must not be a symlink. Parent directories may be symlinks: on macOS the
/// system temp directories themselves live behind one.
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
    if meta.permissions().readonly() {
        return Err(refuse("is read-only".into()));
    }
    Ok(path)
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
    let out = var(OUT_ENV)
        .filter(|v| !v.is_empty())
        .ok_or(SetupError::MissingEnv(OUT_ENV))?;
    let out_dir = prepare_out_dir(&out)?;
    Ok(ShotSetup {
        script,
        server,
        out_dir,
    })
}

static ACTIVE: OnceLock<ShotSetup> = OnceLock::new();
static LOGIN_STARTED: AtomicBool = AtomicBool::new(false);

/// Read and validate the shot environment once, at startup.
///
/// `Ok(())` when no script is set (the driver stays inert) or the setup is
/// valid; on error it is logged and the exit code to use is returned.
pub fn init_from_env() -> Result<(), i32> {
    let Some(script) = super::load_from_env() else {
        return Ok(());
    };
    match setup_with(script, |k| std::env::var_os(k)) {
        Ok(setup) => {
            info!(
                server = %setup.server,
                out_dir = %setup.out_dir.display(),
                steps = setup.script.steps.len(),
                "Shot script active: unattended login to the scratch server"
            );
            // A second call would be a programming error; keep the first.
            let _ = ACTIVE.set(setup);
            Ok(())
        },
        Err(e) => {
            error!("{e}");
            eprintln!("error: {e}");
            Err(SETUP_EXIT_CODE)
        },
    }
}

/// Turn off settings that would open a modal nobody is there to close (the
/// first-session intro). In memory only; the runner's config dir is scratch.
pub fn suppress_modals(settings: &mut crate::settings::Settings) {
    if active().is_some() {
        settings.interface.intro_show = crate::hud::Intro::Never;
    }
}

/// The validated setup, if a shot script is running.
pub fn active() -> Option<&'static ShotSetup> { ACTIVE.get() }

fn fail(msg: &str) -> ! {
    error!("Shot script login failed: {msg}");
    eprintln!("error: shot script login failed: {msg}");
    std::process::exit(SETUP_EXIT_CODE)
}

/// Called by the main menu while no connection is in progress. Returns the
/// server to connect to the first time; any later call means the connection
/// or login failed and the menu came back, which ends the run.
pub fn next_login(info_message: Option<&str>) -> Option<SocketAddr> {
    let setup = active()?;
    if LOGIN_STARTED.swap(true, Ordering::SeqCst) {
        fail(info_message.unwrap_or("returned to the main menu (see the log)"));
    }
    info!(server = %setup.server, "Shot script: connecting");
    Some(setup.server)
}

/// The server asked to authenticate: a scratch server never does, so this is
/// not one. Ends the run before any auth server is contacted.
pub fn refuse_auth() {
    if active().is_some() {
        fail("the server requires authentication; expected a scratch server without auth");
    }
}

/// Character selection reported an error (creation or entering the world).
pub fn character_error(error: &str) {
    if active().is_some() {
        fail(&format!("character selection: {error}"));
    }
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
            .filter(|(alias, id, _)| *alias == CHARACTER_ALIAS && id.is_some())
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

    /// Run one step against the client. No-op when no shot script is active.
    pub fn tick(&mut self, client: &mut Client, view_distances: common::ViewDistances) {
        if active().is_none() || !client.registered() {
            return;
        }
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
            CharAction::Fail(msg) => fail(&msg),
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
