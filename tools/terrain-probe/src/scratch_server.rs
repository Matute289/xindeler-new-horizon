//! A throw-away local game server for the client dump.
//!
//! Everything lives in one scratch directory under the OS temp dir (userdata,
//! saves, settings, log) and the server listens on loopback ports chosen free
//! at start, so it can never touch a real save or collide with a game the user
//! has open. Only the child process this module spawned is ever killed.

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::probe::{MAP_ASSET, Res};

/// Name of the admin bot account (the server runs with `--no-auth`).
pub const BOT_USER: &str = "tprobebot";
const READY_LINE: &str = "Server is ready to accept connections";
/// Prefix of the scratch directory; [`Inner::cleanup`] refuses to delete
/// anything that does not carry it.
const DIR_PREFIX: &str = "tprobe-client-dump-";

pub struct ServerOpts {
    pub server_bin: PathBuf,
    pub assets: PathBuf,
    pub seed: u32,
    pub view_distance: u32,
    pub ready_timeout: Duration,
    /// Copy the server log here on cleanup instead of deleting it.
    pub keep_log_to: Option<PathBuf>,
}

/// Process and directory state shared with the signal handler thread.
pub struct Inner {
    child: Mutex<Option<Child>>,
    dir: PathBuf,
    log: PathBuf,
    keep_log_to: Option<PathBuf>,
}

impl Inner {
    /// Kill our own server child (by handle, never by name), keep or drop its
    /// log and delete the scratch directory. Idempotent.
    pub fn cleanup(&self) {
        if let Some(mut c) = self.child.lock().ok().and_then(|mut g| g.take()) {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(dst) = &self.keep_log_to {
            let _ = fs::copy(&self.log, dst);
        }
        let ours = self
            .dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(DIR_PREFIX))
            && self.dir.starts_with(std::env::temp_dir());
        if ours {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

pub struct ScratchServer {
    pub inner: Arc<Inner>,
    pub port: u16,
    pub log: PathBuf,
}

impl Drop for ScratchServer {
    fn drop(&mut self) { self.inner.cleanup(); }
}

/// Two distinct free loopback ports (game, web/metrics).
fn free_ports() -> Res<(u16, u16)> {
    let a = TcpListener::bind(("127.0.0.1", 0))?;
    let b = TcpListener::bind(("127.0.0.1", 0))?;
    Ok((a.local_addr()?.port(), b.local_addr()?.port()))
}

/// `server_config/settings.ron` for the throw-away server. The settings struct
/// is `#[serde(default)]`, so only what matters is written.
pub fn game_settings_ron(port: u16, seed: u32, view_distance: u32) -> String {
    format!(
        "(\n    gameserver_protocols: [Tcp(address: \"127.0.0.1:{port}\")],\n    \
         auth_server_address: None,\n    auth_service_address: None,\n    query_address: None,\n    \
         max_players: 4,\n    world_seed: {seed},\n    server_name: \"terrain-probe\",\n    \
         map_file: Some(LoadAsset(\"{MAP_ASSET}\")),\n    max_view_distance: Some({max_vd}),\n    \
         calendar_mode: None,\n)\n",
        max_vd = view_distance + 8
    )
}

/// `server-cli/settings.ron`: moves the metrics/web endpoint off its default
/// port (the user's own server may hold it) and drops the signal handlers.
pub fn cli_settings_ron(web_port: u16) -> String {
    format!("(\n    web_address: \"127.0.0.1:{web_port}\",\n    shutdown_signals: [],\n)\n")
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn log_is_ready(log: &str) -> bool { strip_ansi(log).contains(READY_LINE) }

fn tail(path: &Path, lines: usize) -> String {
    let text = strip_ansi(&fs::read_to_string(path).unwrap_or_default());
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

impl ScratchServer {
    pub fn start(o: &ServerOpts) -> Res<Self> {
        if !o.server_bin.is_file() {
            return Err(format!(
                "server binary {} not found: build it (`cargo build -p xindeler-server-cli`) or \
                 pass --server-bin / $TPROBE_SERVER_BIN",
                o.server_bin.display()
            )
            .into());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("{DIR_PREFIX}{}-{nanos}", std::process::id()));
        let ud = dir.join("userdata");
        fs::create_dir_all(ud.join("server/server_config"))?;
        fs::create_dir_all(ud.join("server-cli"))?;
        let (port, web_port) = free_ports()?;
        fs::write(
            ud.join("server/server_config/settings.ron"),
            game_settings_ron(port, o.seed, o.view_distance),
        )?;
        fs::write(
            ud.join("server-cli/settings.ron"),
            cli_settings_ron(web_port),
        )?;
        let log = dir.join("server.log");
        let inner = Arc::new(Inner {
            child: Mutex::new(None),
            dir: dir.clone(),
            log: log.clone(),
            keep_log_to: o.keep_log_to.clone(),
        });
        // From here on Drop cleans up the directory on every exit path.
        let mut me = Self {
            inner: inner.clone(),
            port,
            log: log.clone(),
        };
        let cmd = |args: &[&str]| -> Command {
            let mut c = Command::new(&o.server_bin);
            c.args(args)
                .env("VELOREN_ASSETS", &o.assets)
                .env("VELOREN_USERDATA", &ud)
                .stdin(Stdio::null());
            c
        };
        // Register the bot as admin (no auth, so the name is the identity).
        let out = cmd(&[
            "--no-auth",
            "--non-interactive",
            "admin",
            "add",
            BOT_USER,
            "admin",
        ])
        .output()?;
        if !out.status.success() {
            return Err(format!(
                "`admin add` failed: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        let logf = fs::File::create(&log)?;
        let child = cmd(&["--no-auth", "--non-interactive"])
            .stdout(Stdio::from(logf.try_clone()?))
            .stderr(Stdio::from(logf))
            .spawn()?;
        eprintln!(
            "scratch server pid {} on 127.0.0.1:{port} (userdata {})",
            child.id(),
            ud.display()
        );
        *inner.child.lock().map_err(|e| e.to_string())? = Some(child);

        let t0 = Instant::now();
        loop {
            if log_is_ready(&fs::read_to_string(&log).unwrap_or_default()) {
                eprintln!("server ready in {:.0}s", t0.elapsed().as_secs_f32());
                return Ok(me);
            }
            if let Some(status) = me.exited()? {
                return Err(format!(
                    "server exited early ({status}); log tail:\n{}",
                    tail(&log, 25)
                )
                .into());
            }
            if t0.elapsed() > o.ready_timeout {
                return Err(format!(
                    "server not ready after {:.0}s; log tail:\n{}",
                    o.ready_timeout.as_secs_f32(),
                    tail(&log, 25)
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// `Some(status)` if our server child is gone.
    pub fn exited(&mut self) -> Res<Option<std::process::ExitStatus>> {
        let mut g = self.inner.child.lock().map_err(|e| e.to_string())?;
        match g.as_mut() {
            Some(c) => Ok(c.try_wait()?),
            None => Err("server already cleaned up".into()),
        }
    }

    pub fn log_tail(&self, lines: usize) -> String { tail(&self.log, lines) }
}

/// Kill our own server and delete the scratch dir when SIGINT/SIGTERM arrives,
/// so an interrupted run never leaves an orphan server behind.
#[cfg(unix)]
pub fn install_signal_cleanup(inner: Arc<Inner>) {
    use signal_hook::{consts::signal::*, iterator::Signals};
    if let Ok(mut sigs) = Signals::new([SIGINT, SIGTERM, SIGHUP]) {
        std::thread::spawn(move || {
            if sigs.forever().next().is_some() {
                eprintln!("interrupted: stopping the scratch server");
                inner.cleanup();
                std::process::exit(130);
            }
        });
    }
}

#[cfg(not(unix))]
pub fn install_signal_cleanup(_inner: Arc<Inner>) {}

/// `xindeler-server-cli` next to this executable, unless overridden.
pub fn default_server_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("TPROBE_SERVER_BIN") {
        return PathBuf::from(p);
    }
    let name = format!("xindeler-server-cli{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(&name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_carry_the_port_seed_map_and_no_auth_or_query() {
        let s = game_settings_ron(40123, 7, 24);
        assert!(s.contains("127.0.0.1:40123"));
        assert!(s.contains("world_seed: 7"));
        assert!(s.contains("LoadAsset(\"world.map.cromatolis_v0\")"));
        assert!(s.contains("auth_server_address: None"));
        assert!(s.contains("query_address: None"));
        assert!(s.contains("calendar_mode: None"));
        assert!(s.contains("max_view_distance: Some(32)"));
        let c = cli_settings_ron(40124);
        assert!(c.contains("127.0.0.1:40124") && c.contains("shutdown_signals: []"));
    }

    #[test]
    fn ready_line_is_found_through_ansi_colours() {
        let log = "\x1b[2m2026\x1b[0m INFO Server is ready to accept connections.\n";
        assert!(log_is_ready(log));
        assert!(!log_is_ready("generating world\n"));
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m"), "ok");
    }

    #[test]
    fn free_ports_are_distinct_and_loopback_bindable() {
        let (a, b) = free_ports().unwrap();
        assert_ne!(a, b);
        assert!(a > 1023 && b > 1023);
    }

    #[test]
    fn cleanup_refuses_foreign_directories() {
        let dir = std::env::temp_dir().join("tprobe-not-ours-test-dir");
        fs::create_dir_all(&dir).unwrap();
        let inner = Inner {
            child: Mutex::new(None),
            dir: dir.clone(),
            log: dir.join("x.log"),
            keep_log_to: None,
        };
        inner.cleanup();
        assert!(dir.exists(), "a directory without our prefix must survive");
        fs::remove_dir_all(&dir).unwrap();
    }
}
